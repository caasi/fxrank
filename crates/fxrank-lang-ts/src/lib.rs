// fxrank-lang-ts: TypeScript frontend for FxRank (swc-based)

pub mod coverage;
pub mod detect;
pub mod functions;
pub mod imports;
pub mod source;

use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::Diagnostic;

use crate::imports::ImportTable;
use crate::source::{Lang, SpanLines};

/// The TypeScript/JavaScript language frontend.
///
/// `TsFrontend { lang }.analyze()` parses each `SourceFile` with the configured
/// `lang` dialect via `functions::parse_module`, builds a `SpanLines` from the
/// same `SourceMap` used for parsing (so effect-line resolution works), then
/// maps each `FnUnit` to a scored `Hotspot` via `detect::analyze_unit`.
/// Un-parseable files become `Diagnostic`s, not panics.
///
/// `lang` is the dialect used for *all* this frontend's sources; the CLI groups
/// sources by resolved `Lang` so each group gets a `TsFrontend` with the right
/// dialect. When `include_tests` is `false` (the default), whole files whose path
/// contains `.test.` or `.spec.` (e.g. `foo.test.ts`, `bar.spec.tsx`) or any
/// path segment equals `__tests__` are skipped; their unit count is tallied in
/// `FrontendOutput::skipped_tests`. JS/TS convention keeps tests in separate
/// files, so skipping is by file path (not by detecting `describe`/`it` inside
/// app code), mirroring the Rust frontend's `skipped_tests` contract.
#[derive(Default)]
pub struct TsFrontend {
    pub lang: Lang,
    pub include_tests: bool,
}

impl Frontend for TsFrontend {
    fn language(&self) -> Language {
        Language::Ts
    }

    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
        let mut output = FrontendOutput::default();

        for source in files {
            match functions::parse_module(&source.text, &source.path, self.lang) {
                Err(e) => {
                    // FIXME(diagnostic-UX): swc Error has no Display; Debug output is
                    // verbose — extract just the message in a later pass.
                    output.diagnostics.push(Diagnostic {
                        path: source.path.clone(),
                        parsed: false,
                        error: format!("{e:?}"),
                    });
                }
                Ok((module, cm)) => {
                    // Keep the SourceMap alive through detection: swc spans are
                    // bare BytePos offsets, and SpanLines needs the same cm that
                    // parsed the file to resolve them to line numbers.
                    let lines = SpanLines::new(cm);
                    let imports = ImportTable::from_module(&module);
                    let units = functions::collect(&module, &source.path, &lines);
                    if !self.include_tests && is_test_file(&source.path) {
                        output.skipped_tests += units.len();
                    } else {
                        for unit in &units {
                            output
                                .functions
                                .push(detect::analyze_unit(unit, &imports, &lines));
                        }
                    }
                }
            }
        }

        output
    }
}

/// Return `true` if `path` identifies a test file by convention.
///
/// A file is a test file when:
/// - the file name contains `.test.` or `.spec.` (e.g. `foo.test.ts`, `bar.spec.tsx`), OR
/// - any path segment is exactly `__tests__` (e.g. `src/__tests__/foo.ts`).
///
/// Only these two well-established JS/TS conventions are checked. Stdin
/// (`"stdin"`) and ordinary `.ts`/`.js` files are never test files.
pub fn is_test_file(path: &str) -> bool {
    // Use the file name only for the infix check (avoid matching `.test.` in a
    // directory component like `my.test.project/app.ts`). Split on both `/` and
    // `\` so a Windows directory like `my.test.project\app.ts` isn't false-matched.
    let file_name = path.split(['/', '\\']).next_back().unwrap_or(path);
    if file_name.contains(".test.") || file_name.contains(".spec.") {
        return true;
    }
    // Any path segment equal to `__tests__` marks the whole file as a test file.
    // Split on both `/` and `\` to handle Windows paths (e.g. `src\__tests__\foo.ts`).
    path.split(['/', '\\']).any(|seg| seg == "__tests__")
}
