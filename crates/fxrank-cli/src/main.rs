use clap::{Parser, Subcommand};
use fxrank_core::frontend::{FrontendOutput, SourceFile};
use fxrank_core::model::{Diagnostic, Report, Scope};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "fxrank", about = "Effect-rank your Rust codebase")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyze Rust source files and emit a ranked JSON report to stdout.
    Scan {
        /// File or directory to scan. Omit to read from stdin.
        path: Option<PathBuf>,
        /// Limit output to the top-N hotspots (summary still covers all).
        #[arg(long)]
        limit: Option<usize>,
        /// Include test functions and modules in the analysis (skipped by default).
        #[arg(long)]
        include_tests: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Cmd::Scan {
        path,
        limit,
        include_tests,
    } = cli.cmd;

    match run_scan(path, limit, include_tests) {
        Ok(report) => {
            // Compact JSON: no trailing newline issues — println! adds exactly one.
            println!(
                "{}",
                serde_json::to_string(&report).expect("serialize report")
            );
            ExitCode::SUCCESS
        }
        Err(msg) => {
            // JSON error object to stdout so agent pipelines still get machine-readable output.
            println!("{}", serde_json::json!({ "error": msg }));
            ExitCode::FAILURE
        }
    }
}

fn run_scan(
    path: Option<PathBuf>,
    limit: Option<usize>,
    include_tests: bool,
) -> Result<Report, String> {
    // Accumulated read-error diagnostics (files that exist but couldn't be read).
    let mut read_errors: Vec<Diagnostic> = Vec::new();

    let (input_label, sources) = match path {
        None => {
            // Read all of stdin into one synthetic SourceFile.
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| format!("read stdin: {e}"))?;
            (
                "stdin".to_owned(),
                vec![SourceFile {
                    path: "stdin".into(),
                    text,
                }],
            )
        }
        Some(ref p) if !p.exists() => {
            return Err(format!("path not found: {}", p.display()));
        }
        Some(ref p) if p.is_file() => {
            let text =
                std::fs::read_to_string(p).map_err(|e| format!("read {}: {e}", p.display()))?;
            let label = p.to_string_lossy().into_owned();
            (label.clone(), vec![SourceFile { path: label, text }])
        }
        Some(ref p) => {
            // Directory: walk recursively collecting *.rs files.
            let label = p.to_string_lossy().into_owned();
            let sources = collect_rs_files(p, &mut read_errors);
            (label, sources)
        }
    };

    let read_error_count = read_errors.len();

    // Dispatch to language frontend(s).
    let output = dispatch(&sources, include_tests);

    // Count parse diagnostics from the frontend (not read errors).
    let parse_diag_count = output.diagnostics.iter().filter(|d| !d.parsed).count();

    // Merge frontend diagnostics with file-read diagnostics.
    let mut all_diagnostics = output.diagnostics;
    all_diagnostics.extend(read_errors);

    let scope = Scope {
        input: input_label,
        files: sources.len() + read_error_count,
        parsed: sources.len().saturating_sub(parse_diag_count),
        functions: output.functions.len(),
        skipped_tests: output.skipped_tests,
        risk_features: output.module_risks,
    };

    Ok(Report::build(
        scope,
        output.functions,
        all_diagnostics,
        limit,
    ))
}

/// Walk `dir` recursively, collecting every `*.rs` file as a `SourceFile`.
/// Files that can't be read are pushed to `read_errors` instead.
fn collect_rs_files(dir: &PathBuf, read_errors: &mut Vec<Diagnostic>) -> Vec<SourceFile> {
    let mut sources = Vec::new();
    walk_dir(dir, &mut sources, read_errors);
    sources
}

fn walk_dir(dir: &PathBuf, sources: &mut Vec<SourceFile>, read_errors: &mut Vec<Diagnostic>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            read_errors.push(Diagnostic {
                path: dir.to_string_lossy().into_owned(),
                parsed: false,
                error: format!("{e}"),
            });
            return;
        }
    };
    for entry_result in entries {
        match entry_result {
            Err(e) => {
                // Entry-level error (e.g. permission denied on a dir entry): attribute
                // to the directory being read since the entry name is unavailable.
                read_errors.push(Diagnostic {
                    path: dir.to_string_lossy().into_owned(),
                    parsed: false,
                    error: format!("read_dir entry: {e}"),
                });
            }
            Ok(entry) => {
                // Use the DirEntry's own file type to avoid following symlinks.
                // `path.is_dir()` resolves symlinks and can cause infinite
                // recursion on symlink cycles or inadvertently scan `target/`.
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(e) => {
                        read_errors.push(Diagnostic {
                            path: entry.path().to_string_lossy().into_owned(),
                            parsed: false,
                            error: format!("file_type: {e}"),
                        });
                        continue;
                    }
                };
                if file_type.is_symlink() {
                    // Skip symlinks entirely — no recursion, no read.
                    continue;
                }
                let path = entry.path();
                if file_type.is_dir() {
                    walk_dir(&path, sources, read_errors);
                } else if file_type.is_file()
                    && path.extension().map(|e| e == "rs").unwrap_or(false)
                {
                    match std::fs::read_to_string(&path) {
                        Ok(text) => sources.push(SourceFile {
                            path: path.to_string_lossy().into_owned(),
                            text,
                        }),
                        Err(e) => read_errors.push(Diagnostic {
                            path: path.to_string_lossy().into_owned(),
                            parsed: false,
                            error: format!("{e}"),
                        }),
                    }
                }
            }
        }
    }
}

/// Feature-gated dispatch: run all sources through the Rust frontend when
/// the `rust` feature is enabled; otherwise emit a "no frontend" diagnostic
/// per file so the binary still compiles and produces valid (empty) output.
#[cfg(feature = "rust")]
fn dispatch(sources: &[SourceFile], include_tests: bool) -> FrontendOutput {
    use fxrank_core::frontend::Frontend;
    use fxrank_lang_rust::RustFrontend;
    RustFrontend { include_tests }.analyze(sources)
}

#[cfg(not(feature = "rust"))]
fn dispatch(sources: &[SourceFile], _include_tests: bool) -> FrontendOutput {
    let mut output = FrontendOutput::default();
    for src in sources {
        output.diagnostics.push(fxrank_core::model::Diagnostic {
            path: src.path.clone(),
            parsed: false,
            error: "no frontend available for .rs (built without 'rust' feature)".into(),
        });
    }
    output
}
