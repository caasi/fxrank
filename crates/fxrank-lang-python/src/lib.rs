//! Python (libcst-based) frontend for FxRank's effect-cost profiler.

pub mod coverage;
pub mod detect;
pub mod functions;
pub mod imports;
pub mod source;

use fxrank_core::CorpusProfile;
use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::Diagnostic;
use libcst_native::parse_module;

/// Python corpus hygiene.
///
/// Virtual-environment and build-artifact directories are pruned so that
/// `fxrank scan .` on a Python project doesn't descend into installed packages.
/// `pyvenv.cfg` marks a venv root for content-based pruning (`prune_marker_files`).
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &[
        ".venv",
        "venv",
        ".tox",
        ".nox",
        "__pycache__",
        ".eggs",
        "build",
        "dist",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        "site-packages",
    ],
    exclude_file_globs: &["*_pb2.py", "*_pb2_grpc.py"],
    test_file_globs: &["test_*.py", "*_test.py", "conftest.py", "tests"],
    prune_marker_files: &["pyvenv.cfg"],
};

/// The Python language frontend.
///
/// When `include_tests` is `false` (the default), two skip mechanisms apply:
///
/// - **Path-based**: entire files whose base name matches `test_*.py` / `*_test.py` /
///   `conftest.py`, or whose path contains a `tests/` directory segment, are skipped.
/// - **Source-based**: within an otherwise-scanned file, units that are a `test_*`-named
///   function, a method of a `Test*`-named class, or a method of a `unittest.TestCase`
///   subclass are skipped.
///
/// Skipped units are counted in `FrontendOutput::skipped_tests`.
/// `--include-tests` (`include_tests: true`) disables both skip mechanisms.
pub struct PythonFrontend {
    pub include_tests: bool,
}

impl Frontend for PythonFrontend {
    fn language(&self) -> Language {
        Language::Python
    }

    fn corpus_profile(&self) -> CorpusProfile {
        CORPUS_PROFILE
    }

    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
        let mut output = FrontendOutput::default();
        for file in files {
            let src = source::strip_bom(&file.text);
            match parse_module(src, None) {
                Err(e) => {
                    output.diagnostics.push(Diagnostic {
                        path: file.path.clone(),
                        parsed: false,
                        error: format!("{e}"),
                    });
                }
                Ok(module) => {
                    // Single borrowed pass: collect + analyze while `module`/`src`
                    // are both alive. `analyze_unit` emits owned `Hotspot`s.
                    let imports = imports::Imports::build(&module);
                    let module_bindings = crate::imports::module_bindings(&module);
                    // Build SpanIndex once per file; pass it into both `collect`
                    // (for lambda-anchor line/col) and `analyze_unit` (for effect
                    // line resolution) — no duplicate O(n) line-start indexing.
                    let span = source::SpanIndex::new(src);

                    // Tokenize once to obtain lambda anchors. `parse_module`
                    // succeeded above, so tokenization is a strict subset and
                    // must also succeed — `None` is a theoretically-impossible
                    // state. We propagate failure rather than swallowing it with
                    // `unwrap_or_default()`: an empty anchor vec would make the
                    // count guard see `0 == 0` and silently drop every lambda.
                    let anchors = match source::lambda_anchors(src) {
                        Some(a) => a,
                        None => {
                            output.diagnostics.push(Diagnostic {
                                path: file.path.clone(),
                                parsed: true,
                                error: "lambda anchoring unavailable: tokenizer failed on \
                                        a file that parsed successfully; hotspots for this \
                                        file are omitted to avoid mis-anchored output"
                                    .into(),
                            });
                            continue;
                        }
                    };

                    // Pass the pre-computed anchors into collect so tokenization
                    // runs exactly once per file (no second tokenizer pass for the
                    // mismatch guard).
                    let (units, lambda_node_count) =
                        functions::collect(&module, src, &span, &anchors);

                    // Runtime lambda-anchor mismatch guard (node count, not emitted count).
                    // `collect` guards the bijection with a `debug_assert_eq!`
                    // (loud in tests/debug builds). In a release build that assert
                    // is stripped, so we add a non-panicking runtime check here.
                    //
                    // CRITICAL: we compare the Lambda-NODE count (`lambda_node_count`,
                    // which `collect` increments on every Lambda node visited, even when
                    // `anchors.get(idx)` returns `None` and no unit is emitted) against
                    // `anchors.len()`. Comparing emitted-unit count instead would miss
                    // the N>M case: if there are more Lambda nodes (N) than anchors (M),
                    // the first M emit normally, the remaining N−M are silently skipped,
                    // and emitted(M)==anchors(M) → guard passes → silent drop. Using the
                    // node count detects the mismatch in both directions (N<M and N>M).
                    {
                        let anchor_count = anchors.len();
                        if lambda_node_count != anchor_count {
                            output.diagnostics.push(Diagnostic {
                                path: file.path.clone(),
                                parsed: true,
                                error: format!(
                                    "lambda-anchor mismatch: CST walk found {lambda_node_count} Lambda node(s) \
                                     but tokenizer found {anchor_count} lambda keyword(s); \
                                     hotspots for this file are omitted to avoid mis-anchored output"
                                ),
                            });
                            continue;
                        }
                    }

                    if !self.include_tests && is_test_file(&file.path) {
                        // Path-based skip: the entire file is test code.
                        output.skipped_tests += units.len();
                    } else {
                        for unit in &units {
                            if !self.include_tests && unit.is_test_unit {
                                // Source-based skip: individual test unit within a
                                // non-test-named file.
                                output.skipped_tests += 1;
                            } else {
                                output.functions.push(detect::analyze_unit(
                                    unit,
                                    &file.path,
                                    &imports,
                                    &module_bindings,
                                    &span,
                                ));
                                output.records.push(detect::build_record(
                                    unit,
                                    &file.path,
                                    &imports,
                                    &module_bindings,
                                    &span,
                                ));
                            }
                        }

                        // Module-init unit: score the module's top-level executable
                        // statements as a synthetic `<module>` unit. Emitted only
                        // when the module has ≥1 effect (import-time IO, effectful
                        // top-level call, etc.). A pure module (imports + function/
                        // class definitions only) produces no `<module>` entry.
                        //
                        // Root-ness is CLI-level, not a frontend heuristic: the
                        // frontend always emits `record.is_root = false`; the CLI sets
                        // `root` for units whose file was an explicit FILE arg (the
                        // agent's observation focus). So the `<module>` unit is a root
                        // iff its file is explicit, like any other unit — NOT
                        // automatically. (Guideline: *Roots — the agent's observation
                        // focus*.)
                        if let Some(init_unit) = functions::module_init_unit(&module) {
                            let h = detect::analyze_unit(
                                &init_unit,
                                &file.path,
                                &imports,
                                &module_bindings,
                                &span,
                            );
                            if !h.effects.is_empty() {
                                let rec = detect::build_record(
                                    &init_unit,
                                    &file.path,
                                    &imports,
                                    &module_bindings,
                                    &span,
                                );
                                output.records.push(rec);
                                output.functions.push(h);
                            }
                        }
                    }
                }
            }
        }
        output
    }
}

/// Return `true` if `path` identifies a test file by Python convention.
///
/// Delegates to a `CorpusMatcher` built from `CORPUS_PROFILE.test_file_globs`:
/// - `test_*.py` / `*_test.py` match by base-name glob (pytest conventions), OR
/// - `conftest.py` matches exactly (pytest fixtures / configuration), OR
/// - `tests` as a bare literal matches any path segment (e.g. `src/tests/foo.py`).
pub fn is_test_file(path: &str) -> bool {
    use std::sync::OnceLock;
    static M: OnceLock<fxrank_core::CorpusMatcher> = OnceLock::new();
    M.get_or_init(|| fxrank_core::CorpusMatcher::test_matcher(CORPUS_PROFILE.test_file_globs))
        .matches_test_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `FrontendOutput` from the given fixture file paths.
    ///
    /// The path used for `is_test_file` detection is the same as the read path.
    fn analyze_files(paths: &[&str], include_tests: bool) -> FrontendOutput {
        let files: Vec<SourceFile> = paths
            .iter()
            .map(|p| {
                let text =
                    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("cannot read {p}: {e}"));
                SourceFile {
                    path: p.to_string(),
                    text,
                }
            })
            .collect();
        PythonFrontend { include_tests }.analyze(&files)
    }

    /// Build a `FrontendOutput` from a fixture file, but report it under a
    /// different `logical_path` for `is_test_file` detection.
    ///
    /// This lets source-based skip tests use a fixture that lives under
    /// `tests/fixtures/` (which would otherwise trigger the `tests/` path rule)
    /// while controlling the path that the skip logic sees.
    fn analyze_fixture_as(
        fixture: &str,
        logical_path: &str,
        include_tests: bool,
    ) -> FrontendOutput {
        let text = std::fs::read_to_string(fixture)
            .unwrap_or_else(|e| panic!("cannot read {fixture}: {e}"));
        PythonFrontend { include_tests }.analyze(&[SourceFile {
            path: logical_path.to_string(),
            text,
        }])
    }

    #[test]
    fn skips_test_code_by_default_and_counts() {
        // File named test_sample.py → path-based file skip.
        let out = analyze_files(
            &["tests/fixtures/test_sample.py"],
            /*include_tests=*/ false,
        );
        assert_eq!(out.functions.len(), 0);
        assert!(out.skipped_tests >= 1);

        // With --include-tests, all units are scored.
        let inc = analyze_files(
            &["tests/fixtures/test_sample.py"],
            /*include_tests=*/ true,
        );
        assert!(inc.functions.len() >= 3);
    }

    #[test]
    fn source_based_skip_independent_of_path_skip() {
        // The fixture is read from tests/fixtures/ but we report it under
        // "src/mixed_tests.py" so the path-based skip rule doesn't apply
        // (no test_*/conftest base name, no `tests/` segment in the logical path).
        // Source-based rules must skip test_something, TestWidget.test_render,
        // TestWidget.helper (ALL methods of a Test* class are skipped, even
        // non-test_*-named ones), and MyCase.test_case (unittest.TestCase
        // subclass method).
        let out = analyze_fixture_as(
            "tests/fixtures/mixed_tests.py",
            "src/mixed_tests.py",
            /*include_tests=*/ false,
        );
        let symbols: Vec<&str> = out.functions.iter().map(|h| h.symbol.as_str()).collect();

        // normal_function is always kept.
        assert!(
            symbols.contains(&"normal_function"),
            "normal_function must not be skipped; got: {symbols:?}"
        );
        // test_* function is skipped.
        assert!(
            !symbols.contains(&"test_something"),
            "test_something must be skipped; got: {symbols:?}"
        );
        // TestWidget.test_render is skipped (method of Test* class).
        assert!(
            !symbols.contains(&"test_render"),
            "test_render must be skipped; got: {symbols:?}"
        );
        // TestWidget.helper is skipped too: ALL methods of a Test* class are
        // skipped, including non-test_*-named ones (spec: "Test* class → its
        // methods skipped"). Keeping helper would violate the spec.
        assert!(
            !symbols.contains(&"helper"),
            "helper must be skipped (method of Test* class TestWidget); got: {symbols:?}"
        );
        // MyCase.test_case is skipped (unittest.TestCase subclass method).
        assert!(
            !symbols.contains(&"test_case"),
            "test_case must be skipped; got: {symbols:?}"
        );
        // Some units must have been skipped.
        assert!(
            out.skipped_tests >= 1,
            "expected skipped_tests >= 1; got: {}",
            out.skipped_tests
        );

        // With --include-tests, all units including test ones are returned.
        let inc = analyze_fixture_as(
            "tests/fixtures/mixed_tests.py",
            "src/mixed_tests.py",
            /*include_tests=*/ true,
        );
        let inc_symbols: Vec<&str> = inc.functions.iter().map(|h| h.symbol.as_str()).collect();
        assert!(
            inc_symbols.contains(&"test_something"),
            "with include_tests, test_something must be scored; got: {inc_symbols:?}"
        );
    }

    #[test]
    fn corpus_profile_method_returns_const() {
        use fxrank_core::frontend::Frontend;
        let p = PythonFrontend {
            include_tests: false,
        }
        .corpus_profile();
        assert_eq!(p.prune_dirs, CORPUS_PROFILE.prune_dirs);
        assert_eq!(p.test_file_globs, CORPUS_PROFILE.test_file_globs);
    }

    #[test]
    fn is_test_file_characterization() {
        for p in [
            "test_views.py",
            "views_test.py",
            "conftest.py",
            "pkg/tests/helpers.py",
            "tests/x.py",
        ] {
            assert!(is_test_file(p), "expected test file: {p}");
        }
        for p in [
            "views.py",
            "pkg/mytests/foo.py",
            "tests.py",
            "contest.py",
            "test_views.txt",
        ] {
            assert!(!is_test_file(p), "expected NON-test file: {p}");
        }
    }
}
