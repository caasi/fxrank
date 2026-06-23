use clap::{Parser, Subcommand};
use fxrank_core::frontend::{FrontendOutput, SourceFile};
use fxrank_core::model::{Diagnostic, Report, Scope};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "fxrank",
    about = "Effect-rank your Rust, TypeScript/JavaScript, and Python codebase"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyze source files and emit a ranked JSON report to stdout.
    Scan {
        /// File or directory to scan. Omit (or pass `-`) to read from stdin.
        path: Option<PathBuf>,
        /// Limit output to the top-N hotspots (summary still covers all).
        #[arg(long)]
        limit: Option<usize>,
        /// Include test functions and modules in the analysis (skipped by default).
        #[arg(long)]
        include_tests: bool,
        /// Language dialect for stdin (`ts`, `tsx`, `js`, `jsx`, `python`). Only meaningful
        /// for stdin; for files/directories the extension decides the frontend.
        #[arg(long)]
        lang: Option<String>,
        /// Patterns to skip during directory scans (comma-separated; REPLACES the
        /// default union of the enabled frontends' corpus profiles when provided).
        /// Classified by `/` (spec 004): no-`/` literal prunes a dir + excludes a file;
        /// no-`/` glob excludes files only; `/`-bearing glob filters files by path.
        #[arg(long, value_delimiter = ',')]
        exclude: Option<Vec<String>>,
    },
}

/// Which frontend a source file should be routed to.
///
/// Feature-independent: TS sources carry their extension (without the dot) so
/// the (feature-gated) TS dispatch can resolve the `Lang` dialect itself. The
/// CLI never references `fxrank_lang_ts::Lang` directly, so the binary still
/// compiles without the `ts` feature.
#[derive(Clone)]
enum Route {
    Rust,
    /// TS-family source; the `String` is the file extension (e.g. `"ts"`, `"tsx"`).
    Ts(String),
    /// Python source.
    Python,
}

/// A source file paired with the frontend it should be routed to.
struct RoutedSource {
    source: SourceFile,
    route: Route,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Cmd::Scan {
        path,
        limit,
        include_tests,
        lang,
        exclude,
    } = cli.cmd;

    match run_scan(path, limit, include_tests, lang, exclude) {
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
    lang: Option<String>,
    exclude: Option<Vec<String>>,
) -> Result<Report, String> {
    // Accumulated read-error diagnostics (files that exist but couldn't be read).
    let mut read_errors: Vec<Diagnostic> = Vec::new();
    let mut skipped_excluded = 0usize; // 0 for stdin/single-file (no-op)

    // `-` is the conventional "read stdin" path; treat it like an omitted path.
    let is_stdin = match &path {
        None => true,
        Some(p) => p.as_os_str() == "-",
    };

    // `--lang` is only valid for stdin; for a real file/dir the extension decides.
    if lang.is_some() && !is_stdin {
        return Err(
            "--lang is only valid when reading from stdin; for files the extension determines the language".into()
        );
    }

    let (input_label, routed) = if is_stdin {
        // Read all of stdin into one synthetic SourceFile.
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("read stdin: {e}"))?;
        let source = SourceFile {
            path: "stdin".into(),
            text,
        };
        // Back-compat: stdin defaults to Rust. `--lang` selects the frontend
        // for the given dialect: `ts`, `tsx`, `js`, `jsx` (TS frontend) or
        // `python` (Python frontend). There is no `--lang rust`.
        let route = match lang.as_deref() {
            None => Route::Rust,
            Some(flag) => {
                // Accept the TS dialects (`ts`/`tsx`/`js`/`jsx`) and `python`; reject anything else.
                match flag {
                    "ts" | "tsx" | "js" | "jsx" => Route::Ts(flag.to_owned()),
                    "python" => Route::Python,
                    other => {
                        return Err(format!(
                            "unknown --lang value '{other}' (expected ts, tsx, js, jsx, or python)"
                        ));
                    }
                }
            }
        };
        ("stdin".to_owned(), vec![RoutedSource { source, route }])
    } else {
        let p = path.expect("path present when not stdin");
        if !p.exists() {
            return Err(format!("path not found: {}", p.display()));
        }
        if p.is_file() {
            // Single explicit file: route by its extension. --exclude is a no-op here.
            let route = route_for_path(&p)
                .ok_or_else(|| format!("unsupported file extension: {}", p.display()))?;
            let label = p.to_string_lossy().into_owned();
            let text =
                std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
            let source = SourceFile {
                path: label.clone(),
                text,
            };
            (label, vec![RoutedSource { source, route }])
        } else {
            // Directory branch — the ONLY place the matcher is built.
            // A bad glob surfaces here as a JSON error (non-zero exit).
            // Single-file/stdin branches above never consult --exclude.
            let exclude_entries = exclude.unwrap_or_else(default_exclude_entries);
            let matcher = fxrank_core::CorpusMatcher::build(&exclude_entries)?; // invalid glob → JSON error
            // Content-marker prunes are computed independently of --exclude (always on).
            let markers = default_prune_markers();
            let label = p.to_string_lossy().into_owned();
            let routed = collect_source_files(
                &p,
                &p,
                &mut read_errors,
                &matcher,
                &mut skipped_excluded,
                &markers,
            );
            (label, routed)
        }
    };

    let read_error_count = read_errors.len();
    let source_count = routed.len();

    // Dispatch to the appropriate frontend(s) and merge outputs.
    let output = dispatch(routed, include_tests);

    // Count parse diagnostics from the frontend (not read errors).
    let parse_diag_count = output.diagnostics.iter().filter(|d| !d.parsed).count();

    // Merge frontend diagnostics with file-read diagnostics.
    let mut all_diagnostics = output.diagnostics;
    all_diagnostics.extend(read_errors);

    let scope = Scope {
        input: input_label,
        files: source_count + read_error_count,
        parsed: source_count.saturating_sub(parse_diag_count),
        functions: output.functions.len(),
        skipped_tests: output.skipped_tests,
        skipped_excluded,
        risk_features: output.module_risks,
    };

    Ok(Report::build(
        scope,
        output.functions,
        all_diagnostics,
        limit,
    ))
}

// `allow(unused_mut)`: in a no-feature build all the `push`es below are cfg'd out,
// leaving `v` never-mutated. Slim builds run `cargo build`, not clippy -D warnings,
// but keep them warning-clean anyway.
#[allow(unused_mut)]
fn default_corpus_profiles() -> Vec<fxrank_core::CorpusProfile> {
    let mut v = vec![fxrank_core::CorpusProfile::COMMON];
    #[cfg(feature = "rust")]
    v.push(fxrank_lang_rust::CORPUS_PROFILE);
    #[cfg(feature = "ts")]
    v.push(fxrank_lang_ts::CORPUS_PROFILE);
    #[cfg(feature = "python")]
    v.push(fxrank_lang_python::CORPUS_PROFILE);
    v
}

/// Union the prune-dir + exclude-file channels into a sorted, deduped entry list
/// for `CorpusMatcher::build` (the default when `--exclude` is absent).
fn default_exclude_entries() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in default_corpus_profiles() {
        out.extend(p.prune_dirs.iter().map(|s| s.to_string()));
        out.extend(p.exclude_file_globs.iter().map(|s| s.to_string()));
    }
    out.sort();
    out.dedup();
    out
}

/// Union of content-marker file names whose presence prunes a directory.
fn default_prune_markers() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in default_corpus_profiles() {
        out.extend(p.prune_marker_files.iter().map(|s| s.to_string()));
    }
    out.sort();
    out.dedup();
    out
}

/// Decide which frontend a path's extension routes to. Returns `None` for
/// extensions no frontend handles.
fn route_for_path(path: &std::path::Path) -> Option<Route> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some(Route::Rust),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Route::Ts(ext.to_owned())),
        "py" => Some(Route::Python),
        _ => None,
    }
}

/// Walk `dir` recursively, collecting every routable source file (`.rs` plus the
/// JS/TS family) as a `RoutedSource`. Files that can't be read are pushed to
/// `read_errors` instead. The `CorpusMatcher` prunes directories and excludes
/// files; `root` is the scan root used to compute root-relative paths for path globs.
fn collect_source_files(
    dir: &std::path::Path,
    root: &std::path::Path,
    read_errors: &mut Vec<Diagnostic>,
    matcher: &fxrank_core::CorpusMatcher,
    skipped_excluded: &mut usize,
    markers: &[String],
) -> Vec<RoutedSource> {
    let mut sources = Vec::new();
    walk_dir(
        dir,
        root,
        &mut sources,
        read_errors,
        matcher,
        skipped_excluded,
        markers,
    );
    sources
}

/// Recursively collects routable source files under `dir`, skipping symlinks,
/// pruning directories whose base name is a literal exclude entry, and excluding
/// files that match any exclude pattern (after extension routing).
fn walk_dir(
    dir: &std::path::Path,
    root: &std::path::Path,
    sources: &mut Vec<RoutedSource>,
    read_errors: &mut Vec<Diagnostic>,
    matcher: &fxrank_core::CorpusMatcher,
    skipped_excluded: &mut usize,
    markers: &[String],
) {
    // Content-marker prune: a dir containing e.g. `pyvenv.cfg` is a venv root,
    // regardless of its name (`myenv/`, `.env3/`). Catches arbitrarily-named venvs,
    // and (because this runs at walk_dir entry) the scan root itself.
    if markers.iter().any(|m| dir.join(m).is_file()) {
        return;
    }

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
                    // Prune iff the dir base name is a literal exclude entry.
                    // Wildcard globs and path globs never prune directories (spec 004).
                    let dir_name = entry.file_name();
                    if matcher.dir_pruned(&dir_name.to_string_lossy()) {
                        continue;
                    }
                    walk_dir(
                        &path,
                        root,
                        sources,
                        read_errors,
                        matcher,
                        skipped_excluded,
                        markers,
                    );
                } else if file_type.is_file() {
                    // Route by extension; skip files no frontend handles.
                    if let Some(route) = route_for_path(&path) {
                        // Exclusion runs AFTER routing (spec 004 invariant).
                        let rel = path.strip_prefix(root).unwrap_or(&path);
                        let rel_lossy = rel.to_string_lossy();
                        // Normalize the OS separator to '/' for glob matching ONLY on Windows.
                        // On Unix '\' is a valid filename char, so we must not rewrite it (and the
                        // separator is already '/', so the borrowed path is used verbatim — zero alloc).
                        let rel_str: std::borrow::Cow<str> = if cfg!(windows) {
                            std::borrow::Cow::Owned(rel_lossy.replace('\\', "/"))
                        } else {
                            rel_lossy
                        };
                        let file_name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if matcher.file_excluded(&file_name, &rel_str) {
                            *skipped_excluded += 1;
                            continue;
                        }
                        match std::fs::read_to_string(&path) {
                            Ok(text) => sources.push(RoutedSource {
                                source: SourceFile {
                                    path: path.to_string_lossy().into_owned(),
                                    text,
                                },
                                route,
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
}

/// Merge `other` into `acc` (concatenate functions/risks/diagnostics, sum
/// skipped_tests).
fn merge_output(acc: &mut FrontendOutput, mut other: FrontendOutput) {
    acc.functions.append(&mut other.functions);
    acc.module_risks.append(&mut other.module_risks);
    acc.diagnostics.append(&mut other.diagnostics);
    acc.skipped_tests += other.skipped_tests;
}

/// Route each source to its frontend, run the right frontend(s), and merge the
/// per-frontend `FrontendOutput`s into one.
///
/// Rust sources need the `rust` feature; TS sources need the `ts` feature. When
/// a source's frontend feature is not compiled in, a "no frontend available"
/// diagnostic is emitted per file (mirroring the slim-build behavior).
fn dispatch(routed: Vec<RoutedSource>, include_tests: bool) -> FrontendOutput {
    // Partition by route.
    let mut rust_sources: Vec<SourceFile> = Vec::new();
    // TS sources keyed by extension so each `Lang` dialect group runs separately.
    let mut ts_sources: Vec<(String, SourceFile)> = Vec::new();
    let mut python_sources: Vec<SourceFile> = Vec::new();

    for r in routed {
        match r.route {
            Route::Rust => rust_sources.push(r.source),
            Route::Ts(ext) => ts_sources.push((ext, r.source)),
            Route::Python => python_sources.push(r.source),
        }
    }

    let mut output = FrontendOutput::default();
    merge_output(&mut output, dispatch_rust(rust_sources, include_tests));
    merge_output(&mut output, dispatch_ts(ts_sources, include_tests));
    merge_output(&mut output, dispatch_python(python_sources, include_tests));
    output
}

#[cfg(feature = "rust")]
fn dispatch_rust(sources: Vec<SourceFile>, include_tests: bool) -> FrontendOutput {
    use fxrank_core::frontend::Frontend;
    use fxrank_lang_rust::RustFrontend;
    if sources.is_empty() {
        return FrontendOutput::default();
    }
    RustFrontend { include_tests }.analyze(&sources)
}

#[cfg(not(feature = "rust"))]
fn dispatch_rust(sources: Vec<SourceFile>, _include_tests: bool) -> FrontendOutput {
    let mut output = FrontendOutput::default();
    for src in sources {
        output.diagnostics.push(Diagnostic {
            path: src.path,
            parsed: false,
            error: "no frontend available for .rs (built without 'rust' feature)".into(),
        });
    }
    output
}

#[cfg(feature = "ts")]
fn dispatch_ts(sources: Vec<(String, SourceFile)>, include_tests: bool) -> FrontendOutput {
    use fxrank_core::frontend::Frontend;
    use fxrank_lang_ts::TsFrontend;
    use fxrank_lang_ts::source::Lang;
    use std::collections::HashMap;

    // Group by resolved `Lang` so each dialect runs with its own syntax. The
    // grouping key is the `Lang` (a `.ts` and a `.tsx` in one dir differ).
    let mut groups: HashMap<Lang, Vec<SourceFile>> = HashMap::new();
    for (ext, source) in sources {
        // Every collected extension is one `Lang::from_extension` recognizes.
        let lang = Lang::from_extension(&ext).unwrap_or_else(|| {
            unreachable!("route_for_path only routes extensions from_extension recognizes")
        });
        groups.entry(lang).or_default().push(source);
    }

    let mut output = FrontendOutput::default();
    for (lang, group) in groups {
        let frontend = TsFrontend {
            lang,
            include_tests,
        };
        merge_output(&mut output, frontend.analyze(&group));
    }
    output
}

#[cfg(not(feature = "ts"))]
fn dispatch_ts(sources: Vec<(String, SourceFile)>, _include_tests: bool) -> FrontendOutput {
    let mut output = FrontendOutput::default();
    for (ext, src) in sources {
        output.diagnostics.push(Diagnostic {
            path: src.path,
            parsed: false,
            error: format!("no frontend available for .{ext} (built without 'ts' feature)"),
        });
    }
    output
}

#[cfg(feature = "python")]
fn dispatch_python(sources: Vec<SourceFile>, include_tests: bool) -> FrontendOutput {
    use fxrank_core::frontend::Frontend;
    use fxrank_lang_python::PythonFrontend;
    if sources.is_empty() {
        return FrontendOutput::default();
    }
    PythonFrontend { include_tests }.analyze(&sources)
}

#[cfg(not(feature = "python"))]
fn dispatch_python(sources: Vec<SourceFile>, _include_tests: bool) -> FrontendOutput {
    let mut output = FrontendOutput::default();
    for src in sources {
        output.diagnostics.push(Diagnostic {
            path: src.path,
            parsed: false,
            error: "no frontend available for .py (built without 'python' feature)".into(),
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(all(feature = "rust", feature = "ts", feature = "python"))]
    fn full_build_union_equals_old_default_value() {
        // The verbatim pre-#21 default_value (spec 004 + the #14 interim Python add).
        let mut old: Vec<String> = "node_modules,.git,target,*.min.js,*.min.mjs,*.min.cjs,\
*.stories.*,mockServiceWorker.js,jest.setup.*,jest.config.*,__mocks__,.venv,venv,.tox,.nox,\
__pycache__,.eggs,build,dist,.mypy_cache,.pytest_cache,.ruff_cache,site-packages,*_pb2.py,\
*_pb2_grpc.py"
            .split(',')
            .map(|s| s.to_string())
            .collect();
        old.sort();
        old.dedup();
        assert_eq!(
            default_exclude_entries(),
            old,
            "union default drifted from the old --exclude list"
        );
        assert_eq!(default_prune_markers(), vec!["pyvenv.cfg".to_string()]);
    }

    #[test]
    #[cfg(all(feature = "ts", not(feature = "rust"), not(feature = "python")))]
    fn ts_only_union_excludes_other_ecosystems() {
        let e = default_exclude_entries();
        assert!(e.iter().any(|x| x == "node_modules") && e.iter().any(|x| x == ".git"));
        assert!(
            !e.iter().any(|x| x == "target"),
            "Rust default leaked into TS-only build"
        );
        assert!(
            !e.iter().any(|x| x == ".venv"),
            "Python default leaked into TS-only build"
        );
        assert!(
            default_prune_markers().is_empty(),
            "pyvenv.cfg leaked into TS-only build"
        );
    }
}
