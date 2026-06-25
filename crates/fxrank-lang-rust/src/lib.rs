//! Rust language frontend for fxrank — walks syn ASTs and emits effect reports.

pub mod detect;
pub mod functions;
pub mod imports;
pub mod module_tree;

use fxrank_core::CorpusProfile;
use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::Diagnostic;
use imports::ImportTable;
use std::collections::HashSet;

/// Rust corpus hygiene. `target` is the build dir; unit tests are SOURCE-based
/// (`#[test]`/`#[cfg(test)]`, handled in `analyze`), so no `test_file_globs`.
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &["target"],
    exclude_file_globs: &[],
    test_file_globs: &[],
    prune_marker_files: &[],
};

/// The Rust language frontend.
///
/// `RustFrontend::default().analyze()` parses each `SourceFile` with `syn::parse_file`,
/// builds an `ImportTable`, collects the set of top-level `static` item names
/// (for `ambient.read` detection), runs `functions::collect` to find all
/// concrete function units, and maps each `FnUnit` to a scored `Hotspot` via
/// `detect::analyze_unit`. The call-effect detector (T11) is wired today;
/// detector tasks T12–T15 plug into `detect::analyze_unit`'s gather step.
///
/// When `include_tests` is `false` (the default), function units marked
/// `is_test` are excluded from scoring and counted in `FrontendOutput::skipped_tests`.
/// Module-level risks (`impl Drop`, `unsafe impl`, `extern` blocks) that carry
/// `#[cfg(test)]` are also suppressed.
#[derive(Default)]
pub struct RustFrontend {
    pub include_tests: bool,
}

impl Frontend for RustFrontend {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn corpus_profile(&self) -> CorpusProfile {
        CORPUS_PROFILE
    }

    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
        let mut output = FrontendOutput::default();
        let module_tree = module_tree::ModuleTree::build(files);

        for source in files {
            match syn::parse_file(&source.text) {
                Err(e) => {
                    output.diagnostics.push(Diagnostic {
                        path: source.path.clone(),
                        parsed: false,
                        error: format!("{e}"),
                    });
                }
                Ok(parsed) => {
                    let imports = ImportTable::from_file(&parsed);
                    let statics = collect_static_names(&parsed);
                    let units = functions::collect(&parsed, &source.path);
                    if self.include_tests {
                        for unit in &units {
                            output
                                .functions
                                .push(detect::analyze_unit(unit, &imports, &statics));
                            output.records.push(detect::build_record(
                                unit,
                                &imports,
                                &statics,
                                &module_tree,
                            ));
                        }
                    } else {
                        let mut skipped = 0usize;
                        for unit in &units {
                            if unit.is_test {
                                skipped += 1;
                            } else {
                                output
                                    .functions
                                    .push(detect::analyze_unit(unit, &imports, &statics));
                                output.records.push(detect::build_record(
                                    unit,
                                    &imports,
                                    &statics,
                                    &module_tree,
                                ));
                            }
                        }
                        output.skipped_tests += skipped;
                    }
                    output
                        .module_risks
                        .extend(detect::risk::detect_module_risks(
                            &parsed,
                            &source.path,
                            self.include_tests,
                        ));
                }
            }
        }

        output
    }
}

/// Collect the names of all top-level `static` items in a parsed file.
///
/// Only `static` items represent ambient runtime state whose bare-name reads
/// should be flagged. `const` items are compile-time copies and are excluded.
fn collect_static_names(file: &syn::File) -> HashSet<String> {
    file.items
        .iter()
        .filter_map(|item| {
            if let syn::Item::Static(s) = item {
                Some(s.ident.to_string())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_profile_method_returns_const() {
        use fxrank_core::frontend::Frontend;
        let p = RustFrontend {
            include_tests: false,
        }
        .corpus_profile();
        assert_eq!(p.prune_dirs, CORPUS_PROFILE.prune_dirs);
        assert_eq!(p.test_file_globs, CORPUS_PROFILE.test_file_globs);
    }

    /// End-to-end: a crate with a lone `Foo::write` and a `std::fs::write` caller.
    /// After adoption (canonical_path set), `resolve_ref_precise` must return
    /// `Edge::Opaque` for the std call — NOT `Edge::Resolved` to `Foo::write`.
    /// This proves the 025-3e false-resolve fix end to end. (025-3e §8)
    #[test]
    fn false_resolve_killed_std_write_not_resolved_to_local_write() {
        use fxrank_core::frontend::SourceFile;
        use fxrank_core::graph::Edge;
        use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};

        // Crate with a lone `Foo::write` and a caller that calls std::fs::write.
        let files = vec![SourceFile {
            path: "src/lib.rs".into(),
            text: "pub struct Foo;\n\
                   impl Foo { pub fn write(&self) {} }\n\
                   pub fn caller() { std::fs::write(\"a\", b\"b\").unwrap(); }"
                .into(),
        }];
        let out = RustFrontend::default().analyze(&files);
        let idx = CanonicalIndex::from_records(&out.records);
        assert!(
            idx.adopted(),
            "Rust partition must be adopted (canonical_path set)"
        );

        // Find the caller's std::fs::write ref and resolve it.
        let caller = out
            .records
            .iter()
            .find(|r| r.symbol == "caller")
            .expect("expected a 'caller' unit in records");
        let std_ref = caller
            .refs
            .iter()
            .find(|r| r.base.contains("fs") && r.base.ends_with("write"))
            .expect("expected a ref with base containing 'fs' and ending with 'write'");
        let edge = resolve_ref_precise(std_ref, &idx, &caller.path);
        // MUST be opaque (external), NOT Resolved to Foo::write.
        let is_opaque = matches!(edge, Some(Edge::Opaque(_)));
        assert!(
            is_opaque,
            "std::fs::write must resolve to Edge::Opaque (external), \
             not Edge::Resolved to Foo::write — false-resolve not killed"
        );
    }
}
