//! Shell (Bash/POSIX) frontend for fxrank — SPIKE scaffold (Task 0).
//!
//! # Confirmed `brush-parser` 0.4.0 API (spike findings)
//!
//! 1. **Tokenize → parse entry point** (both re-exported at the crate root):
//!    - `brush_parser::tokenize_str(text: &str) -> Result<Vec<Token>, TokenizerError>`
//!    - `brush_parser::parse_tokens(tokens: &[Token], options: &ParserOptions) -> Result<ast::Program, ParseError>`
//!      (2-arg, confirmed — no `SourceInfo` argument in this version).
//!    - `ast::Program { pub complete_commands: Vec<CompleteCommand> }` — the top-level
//!      command list.
//!
//! 2. **Line/col access**: every AST node that carries position info implements
//!    `brush_parser::ast::SourceLocation { fn location(&self) -> Option<SourceSpan> }`
//!    (e.g. `Program`, `Command`, `Pipeline`, `CompoundCommand`, …). A handful of struct
//!    variants (`ArithmeticCommand`, `SubshellCommand`, `ForClauseCommand`, `IfClauseCommand`,
//!    …) additionally carry a `pub loc: SourceSpan` field directly, but `.location()` is the
//!    uniform accessor. `SourceSpan { start: Arc<SourcePosition>, end: Arc<SourcePosition> }`
//!    and `SourcePosition { index: usize /* 0-based */, line: usize /* 1-based */, column:
//!    usize /* 1-based */ }` — so `location()` already yields 1-based line/col directly, no
//!    manual +1 needed. See the [`span`] helper below.
//!
//! 3. **`time` and redirect lists**:
//!    - `time` is *not* a free-standing command; it is carried on the pipeline it applies
//!      to: `Pipeline { pub timed: Option<PipelineTimed>, .. }` where
//!      `PipelineTimed::Timed(SourceSpan)` / `PipelineTimed::TimedWithPosixOutput(SourceSpan)`
//!      (bash `time` vs. POSIX `time -p`).
//!    - Redirect lists on compound commands are a **sibling of the command, not a field on
//!      it**: `Command::Compound(CompoundCommand, Option<RedirectList>)` (also
//!      `Command::ExtendedTest(ExtendedTestExprCommand, Option<RedirectList>)`). A `[ … ]`
//!      block-level redirect (`{ …; } > out.log`) is the second tuple element, not nested
//!      inside `CompoundCommand`.

pub mod bindings;
pub mod detect;
pub mod functions;
pub mod walk;

use fxrank_core::CorpusProfile;
use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::Diagnostic;

/// Shell corpus hygiene. No directories to prune and no exclude globs (a shell corpus
/// has no build-artifact convention like `target`/`node_modules`); test scripts are
/// identified by filename convention only (`*_test.sh` / `test_*.sh`) since shell has no
/// in-file test marker equivalent to `#[test]`/`test_*` function-naming inside a file.
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &[],
    exclude_file_globs: &[],
    test_file_globs: &["*_test.sh", "test_*.sh"],
    prune_marker_files: &[],
};

/// The Shell language frontend.
///
/// `ShellFrontend::default().analyze()` parses each `SourceFile` with [`parse`], runs
/// `functions::collect` to find every function unit (plus the synthetic `<script>` unit
/// for top-level executable statements), and maps each `FnUnit` to a scored `Hotspot`
/// via `detect::analyze_unit`. A parse failure never panics — it becomes an unparsed
/// `Diagnostic` for that file and analysis continues with the rest of the batch.
///
/// When `include_tests` is `false` (the default), whole files matching
/// [`CORPUS_PROFILE`]'s `test_file_globs` are skipped entirely (shell has no in-file test
/// marker, so the skip is file-name-based, not unit-based) and counted **once per file**
/// in `FrontendOutput::skipped_tests` — unlike Python, which parses a skipped test file
/// and counts its individual units; shell doesn't parse a skipped file at all, so its
/// unit count is unknown.
#[derive(Default)]
pub struct ShellFrontend {
    pub include_tests: bool,
}

impl Frontend for ShellFrontend {
    fn language(&self) -> Language {
        Language::Shell
    }

    fn corpus_profile(&self) -> CorpusProfile {
        CORPUS_PROFILE
    }

    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
        let mut output = FrontendOutput::default();

        for source in files {
            if !self.include_tests && is_test_file(&source.path) {
                output.skipped_tests += 1;
                continue;
            }

            match parse(&source.text) {
                Err(e) => {
                    output.diagnostics.push(Diagnostic {
                        path: source.path.clone(),
                        parsed: false,
                        error: e,
                    });
                }
                Ok(prog) => {
                    let fns = functions::defined_function_names(&prog);
                    let top = bindings::script_top_names(&prog);
                    for unit in functions::collect(&prog, &source.path) {
                        output
                            .functions
                            .push(detect::analyze_unit(&unit, &fns, &top));
                        output.records.push(detect::build_record(&unit, &fns, &top));
                    }
                }
            }
        }

        output
    }
}

/// Parse a shell script into a brush-parser AST, or a diagnostic string.
///
/// Never panics: tokenizer and parser errors are both mapped to `Err`.
pub fn parse(text: &str) -> Result<brush_parser::ast::Program, String> {
    let opts = brush_parser::ParserOptions::default();
    let tokens = brush_parser::tokenize_str(text).map_err(|e| e.to_string())?;
    brush_parser::parse_tokens(&tokens, &opts).map_err(|e| e.to_string())
}

/// Map a node's `SourceLocation` to a 1-based `(line, col)` pair, if known.
///
/// `SourceSpan`/`SourcePosition` are already 1-based for `line`/`column` (see the module
/// doc), so this is a direct passthrough over `Option`/`Arc` unwrapping — no offset math.
pub fn span(node: &impl brush_parser::ast::SourceLocation) -> Option<(usize, usize)> {
    node.location()
        .map(|span| (span.start.line, span.start.column))
}

/// Return `true` if `path` identifies a test file by shell filename convention.
///
/// Delegates to a `CorpusMatcher` built from `CORPUS_PROFILE.test_file_globs`
/// (`*_test.sh` / `test_*.sh`, base-name globs) — mirrors the sibling frontends'
/// `is_test_file` call pattern (`fxrank-lang-python/src/lib.rs`, `-ts/src/lib.rs`).
/// Takes the full path; the matcher extracts the basename internally.
pub fn is_test_file(path: &str) -> bool {
    use std::sync::OnceLock;
    static M: OnceLock<fxrank_core::CorpusMatcher> = OnceLock::new();
    M.get_or_init(|| fxrank_core::CorpusMatcher::test_matcher(CORPUS_PROFILE.test_file_globs))
        .matches_test_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_script_into_a_program() {
        let prog = parse("echo hi\nfoo() { rm -rf /tmp/x; }\n").expect("should parse");
        assert!(!prog.complete_commands.is_empty());
    }

    #[test]
    fn unparseable_input_is_an_err_not_a_panic() {
        // An obviously broken construct must return Err, never panic.
        let result = parse("if then fi fi )(");
        assert!(result.is_err());
    }

    #[test]
    fn span_reports_a_one_based_line_and_col() {
        let prog = parse("echo hi\n").expect("should parse");
        let (line, col) = span(&prog).expect("program should have a location");
        assert_eq!(line, 1);
        assert_eq!(col, 1);
    }

    #[test]
    fn analyze_never_panics_on_garbage() {
        let out = ShellFrontend::default().analyze(&[SourceFile {
            path: "g.sh".into(),
            text: "if then )(".into(),
        }]);
        assert_eq!(out.diagnostics.len(), 1);
        assert!(!out.diagnostics[0].parsed);
    }

    #[test]
    fn deploy_fixture_snapshot() {
        let src = std::fs::read_to_string("tests/fixtures/deploy.sh").unwrap();
        let out = ShellFrontend::default().analyze(&[SourceFile {
            path: "deploy.sh".into(),
            text: src,
        }]);
        let syms: Vec<_> = out
            .functions
            .iter()
            .map(|h| (h.symbol.clone(), h.max_class))
            .collect();
        insta::assert_debug_snapshot!(syms);
    }

    #[test]
    fn pipeline_fixture_snapshot() {
        let src = std::fs::read_to_string("tests/fixtures/pipeline.sh").unwrap();
        let out = ShellFrontend::default().analyze(&[SourceFile {
            path: "pipeline.sh".into(),
            text: src,
        }]);
        let syms: Vec<_> = out
            .functions
            .iter()
            .map(|h| (h.symbol.clone(), h.max_class))
            .collect();
        insta::assert_debug_snapshot!(syms);
    }

    #[test]
    fn corpus_profile_method_returns_const() {
        let p = ShellFrontend {
            include_tests: false,
        }
        .corpus_profile();
        assert_eq!(p.test_file_globs, CORPUS_PROFILE.test_file_globs);
    }

    #[test]
    fn is_test_file_characterization() {
        for p in ["deploy_test.sh", "test_deploy.sh", "pkg/test_run.sh"] {
            assert!(is_test_file(p), "expected test file: {p}");
        }
        for p in ["deploy.sh", "run_tests.sh", "testing.sh"] {
            assert!(!is_test_file(p), "expected NON-test file: {p}");
        }
    }

    #[test]
    fn include_tests_true_still_analyzes_test_named_files() {
        let out = ShellFrontend {
            include_tests: true,
        }
        .analyze(&[SourceFile {
            path: "test_deploy.sh".into(),
            text: "greet() { echo hi; }\n".into(),
        }]);
        assert_eq!(out.skipped_tests, 0);
        assert!(out.functions.iter().any(|h| h.symbol == "greet"));
    }

    #[test]
    fn test_named_file_is_skipped_by_default_and_counted_once() {
        let out = ShellFrontend::default().analyze(&[SourceFile {
            path: "test_deploy.sh".into(),
            text: "greet() { echo hi; }\n".into(),
        }]);
        assert_eq!(out.skipped_tests, 1);
        assert!(out.functions.is_empty());
        assert!(out.diagnostics.is_empty());
    }
}
