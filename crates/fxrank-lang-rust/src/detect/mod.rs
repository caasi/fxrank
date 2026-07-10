//! Per-function effect detection and `Hotspot` assembly.
//!
//! [`analyze_unit`] is the single entry point: it runs each detector over a
//! [`FnUnit`]'s body to gather a `Vec<Effect>`, then folds those effects into a
//! [`Hotspot`] using the core scoring functions. Adding a detector (T12 macros,
//! T13 mutation, T14 risk, T15 async) is a one-line addition to the gather step.

pub mod calls;
pub mod macros;
pub mod mutation;
pub mod refs;
pub mod risk;

use crate::functions::FnUnit;
use crate::imports::ImportTable;
use fxrank_core::confidence::function_confidence;
use fxrank_core::effect::Effect;
use fxrank_core::model::Hotspot;
use fxrank_core::score::{max_class, own_score, weight_for_class};
use std::collections::HashSet;
use syn::visit::Visit;

/// Detectors take a borrowed import table; alias it so sibling detector modules
/// (calls, and later macros/mutation/risk) don't each hard-code `ImportTable`.
pub(crate) type Imports = ImportTable;

/// Run every detector over `unit.body` and assemble a scored [`Hotspot`].
///
/// The gather step is the extension point: each detector returns `Vec<Effect>`
/// and they are concatenated.
///
/// `statics` is the set of top-level `static` names from the same file, used by
/// `calls::detect` to flag bare static-name path expressions as `ambient.read`.
pub fn analyze_unit(unit: &FnUnit, imports: &ImportTable, statics: &HashSet<String>) -> Hotspot {
    let effects: Vec<Effect> = gather(unit, imports, statics);
    let risks = risk::detect_fn_risks(&unit.body, &unit.sig, &unit.path);

    let is_async = unit.sig.asyncness;
    let await_count = count_awaits(&unit.body);
    let async_boundary = is_async || await_count > 0;

    let weights: Vec<u32> = effects.iter().map(|e| e.weight).collect();
    let classes: Vec<u8> = effects.iter().map(|e| e.effective_class()).collect();
    // Build the confidence inputs: effect confidences plus, when there are awaits,
    // a 0.8 synthetic entry representing the "unresolved awaited call" approximation.
    // An async fn that awaits may hide IO effects we cannot see statically.
    let mut confidences: Vec<f64> = effects.iter().map(|e| e.confidence).collect();
    if await_count > 0 {
        confidences.push(0.8);
    }

    let risk_class = risks.iter().map(|r| r.class).max().unwrap_or(0);
    let risk_weight = if risks.is_empty() {
        0
    } else {
        weight_for_class(risk_class)
    };

    let mc = max_class(&classes, risk_class);
    let os = own_score(&weights);
    Hotspot {
        id: unit.id.clone(),
        symbol: unit.symbol.clone(),
        path: unit.path.clone(),
        line: unit.line,
        risk_weight,
        confidence: function_confidence(&confidences),
        async_boundary,
        await_count,
        effects,
        risk_features: risks,
        // Propagated fields default to own (cross-file folding overwrites them).
        ..Hotspot::own_seed(os, mc)
    }
}

/// Count `expr.await` sites in a body using a simple visitor.
fn count_awaits(body: &crate::functions::FnBodyOwned) -> usize {
    struct AwaitCounter(usize);
    impl<'ast> Visit<'ast> for AwaitCounter {
        fn visit_expr_await(&mut self, _node: &'ast syn::ExprAwait) {
            self.0 += 1;
            syn::visit::visit_expr_await(self, _node);
        }
    }
    let mut counter = AwaitCounter(0);
    body.walk_with(&mut counter);
    counter.0
}

/// Gather effects from all detectors. New detectors plug in here.
fn gather(unit: &FnUnit, imports: &ImportTable, statics: &HashSet<String>) -> Vec<Effect> {
    let mut effects = Vec::new();
    effects.extend(calls::detect(&unit.body, imports, &unit.path, statics));
    effects.extend(macros::detect(&unit.body));
    effects.extend(mutation::detect(&unit.body, &unit.sig, statics, imports));
    effects
}

/// Build a language-neutral [`fxrank_core::record::UnitRecord`] for `unit`.
///
/// The record carries the same own-body `effects` and `risks` as the
/// [`analyze_unit`] Hotspot (same `gather` + `risk::detect_fn_risks` calls),
/// plus outgoing call references from [`refs::extract`].  It is the phase-2
/// pass-1 intermediate that the cross-file fold consumes.
///
/// INVARIANT: this recomputes own-body via the same `gather` as `analyze_unit`.
/// This stays correct only while `analyze_unit` does NO post-`gather` mutation of
/// effects/risks (unlike TS, which absorbs React signals and so must copy from the
/// final Hotspot). If you add a post-gather step here, switch to copying from the
/// Hotspot or the record's own-body will drift from it.
pub fn build_record(
    unit: &FnUnit,
    imports: &ImportTable,
    statics: &HashSet<String>,
    module_tree: &crate::module_tree::ModuleTree,
) -> fxrank_core::record::UnitRecord {
    let effects = gather(unit, imports, statics);
    let risks = risk::detect_fn_risks(&unit.body, &unit.sig, &unit.path);
    let await_count = count_awaits(&unit.body);
    let async_boundary = unit.sig.asyncness || await_count > 0;
    let canonical_path = canonical_path_of(unit, module_tree);
    let referencing_mod = module_context_of(unit, module_tree);
    let call_refs = refs::extract(&unit.body, imports, &referencing_mod);

    fxrank_core::record::UnitRecord {
        unit_id: unit.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,
        canonical_path,
        aliases: vec![], // pub-use AliasFacts deferred (025-3e §9)
        effects,
        risks,
        refs: call_refs,
        async_boundary,
        await_count,
        language: fxrank_core::frontend::Language::Rust,
    }
}

/// canonical_path = ["crate"] ++ file-module ++ inline-mod nesting ++ symbol segments.
/// Returns empty when the file has no module path (no crate root in scope), OR
/// when the unit is nested (a nested fn/method or any closure): a nested unit has
/// no importable name, so an empty path keeps it out of `CanonicalIndex.primary`
/// (`from_records` skips empty paths) — it can neither false-resolve a
/// `use crate::m::helper` nor collide with a real module-level `helper`. (#83)
fn canonical_path_of(unit: &FnUnit, module_tree: &crate::module_tree::ModuleTree) -> Vec<String> {
    if unit.is_nested {
        return vec![];
    }
    let Some(file_mod) = module_tree.module_of(&unit.path) else {
        return vec![];
    };
    let mut segs = vec!["crate".to_string()];
    segs.extend(file_mod);
    segs.extend(unit.mod_path.iter().cloned());
    segs.extend(symbol_segments(&unit.symbol));
    segs
}

/// The module a unit lives in, computed DIRECTLY from the file module + inline
/// `mod` nesting — independent of the (possibly-empty) canonical path.
///
/// Blocker 3 (#83): a nested unit's canonical_path is empty, so deriving the
/// referencing module from it (the old `module_of_unit`) would strip a
/// `self::`/`super::` ref inside the nested body of its anchor. A nested unit
/// lives in the SAME module as its enclosing fn (Rust fn-local items open no new
/// module path — `mod_path` tracks only inline `mod` blocks), so we anchor refs
/// to the file module here regardless of canonical-path emptiness.
fn module_context_of(unit: &FnUnit, mt: &crate::module_tree::ModuleTree) -> Vec<String> {
    let Some(file_mod) = mt.module_of(&unit.path) else {
        return vec![];
    };
    let mut segs = vec!["crate".to_string()];
    segs.extend(file_mod);
    segs.extend(unit.mod_path.iter().cloned());
    segs // module, NOT the unit path — no symbol segments
}

/// Split a display symbol into path-meaningful segments:
/// `"f"` → ["f"]; `"S::method"` → ["S","method"];
/// `"<S as T>::method"` → ["S","method"] (trait qualifier dropped for identity).
///
/// Relies on `functions.rs` already normalizing generics out of the type name
/// (`last_segment_ident` returns the bare ident), so the LHS is `S`, `<S as T>`,
/// never `S<u32>`. The `<`/`>`/` as ` peeling below is robust to those forms.
fn symbol_segments(symbol: &str) -> Vec<String> {
    if let Some((lhs, method)) = symbol.rsplit_once("::") {
        // lhs is "S" (inherent) or "<S as T>" (trait impl). Strip the angle
        // brackets and the trait qualifier, leaving the self-type ident.
        let ty = lhs
            .trim_start_matches('<')
            .trim_end_matches('>')
            .split(" as ")
            .next()
            .unwrap_or(lhs)
            .trim();
        vec![ty.to_string(), method.to_string()]
    } else {
        vec![symbol.to_string()]
    }
}

#[cfg(test)]
mod symbol_segments_tests {
    use super::symbol_segments;

    #[test]
    fn segments_free_inherent_and_trait_impl() {
        assert_eq!(symbol_segments("free_fn"), vec!["free_fn".to_string()]);
        assert_eq!(
            symbol_segments("S::method"),
            vec!["S".to_string(), "method".into()]
        );
        // The trait-impl form: angle brackets + trait qualifier both stripped.
        assert_eq!(
            symbol_segments("<S as T>::method"),
            vec!["S".to_string(), "method".into()]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_record_captures_own_and_refs() {
        let f =
            syn::parse_file("use std::fs; fn writer(p: &str) { fs::write(p, b\"x\"); }").unwrap();
        let imports = crate::imports::ImportTable::from_file(&f);
        let statics = std::collections::HashSet::new();
        let mt = crate::module_tree::ModuleTree::build(&[]);
        let units = crate::functions::collect(&f, "a.rs");
        let rec = build_record(&units[0], &imports, &statics, &mt);
        assert_eq!(rec.symbol, "writer");
        assert!(
            rec.refs.iter().any(|r| r.base.contains("fs::write")),
            "expected a ref with base containing 'fs::write', got: {:?}",
            rec.refs
        );
        assert!(
            !rec.effects.is_empty(),
            "expected effects (fs::write → net.fs.db), got none"
        );
        assert_eq!(rec.unit_id, units[0].id);
        assert!(!rec.is_root);
    }

    #[test]
    fn build_record_sets_canonical_path_from_module_tree() {
        use crate::module_tree::ModuleTree;
        use fxrank_core::frontend::SourceFile;

        let mt = ModuleTree::build(&[
            SourceFile {
                path: "src/lib.rs".into(),
                text: String::new(),
            },
            SourceFile {
                path: "src/helpers.rs".into(),
                text: String::new(),
            },
        ]);
        let src = "fn write() {}";
        let file = syn::parse_file(src).unwrap();
        let unit = &crate::functions::collect(&file, "src/helpers.rs")[0];
        let imports = ImportTable::from_file(&file);
        let statics = std::collections::HashSet::new();
        let rec = build_record(unit, &imports, &statics, &mt);
        assert_eq!(
            rec.canonical_path,
            vec!["crate".to_string(), "helpers".into(), "write".into()]
        );
    }

    #[test]
    fn build_record_empty_canonical_when_no_root() {
        use crate::module_tree::ModuleTree;
        use fxrank_core::frontend::SourceFile;
        // No lib.rs/main.rs in the batch → no module path → empty canonical_path.
        let mt = ModuleTree::build(&[SourceFile {
            path: "src/helpers.rs".into(),
            text: String::new(),
        }]);
        let file = syn::parse_file("fn write() {}").unwrap();
        let unit = &crate::functions::collect(&file, "src/helpers.rs")[0];
        let imports = ImportTable::from_file(&file);
        let statics = std::collections::HashSet::new();
        let rec = build_record(unit, &imports, &statics, &mt);
        assert!(rec.canonical_path.is_empty());
    }

    // ── #83: nested-unit identity guard ─────────────────────────────────────

    #[test]
    fn nested_unit_gets_empty_canonical_path() {
        use crate::module_tree::ModuleTree;
        use fxrank_core::frontend::SourceFile;
        let mt = ModuleTree::build(&[SourceFile {
            path: "src/lib.rs".into(),
            text: String::new(),
        }]);
        let file = syn::parse_file("fn outer() { fn helper() {} }").unwrap();
        let imports = ImportTable::from_file(&file);
        let statics = std::collections::HashSet::new();
        let unit = crate::functions::collect(&file, "src/lib.rs")
            .into_iter()
            .find(|u| u.symbol == "helper")
            .unwrap();
        let rec = build_record(&unit, &imports, &statics, &mt);
        assert!(
            rec.canonical_path.is_empty(),
            "a nested fn must not get an importable canonical_path; got {:?}",
            rec.canonical_path
        );
    }

    #[test]
    fn same_name_module_level_fn_still_resolves_when_qualified() {
        // A real module-level `helper` + a nested `helper` in a sibling fn.
        // A `crate::helper()` (qualified) MUST still resolve to the module-level
        // unit via CanonicalIndex, not drop to ambiguous.
        use fxrank_core::frontend::{Frontend, SourceFile};
        use fxrank_core::graph::Edge;
        use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
        let files = vec![SourceFile {
            path: "src/lib.rs".into(),
            text: "pub fn helper() {}\n\
                   pub fn other() { fn helper() {} }\n\
                   pub fn caller() { crate::helper(); }"
                .into(),
        }];
        let out = crate::RustFrontend::default().analyze(&files);
        let idx = CanonicalIndex::from_records(&out.records);
        let caller = out.records.iter().find(|r| r.symbol == "caller").unwrap();
        let r = caller
            .refs
            .iter()
            .find(|r| r.base.ends_with("helper"))
            .unwrap();
        assert!(
            matches!(
                resolve_ref_precise(r, &idx, &caller.path),
                Some(Edge::Resolved(_))
            ),
            "qualified crate::helper must resolve to the module-level unit, not drop"
        );
    }

    #[test]
    fn nested_unit_ref_keeps_self_super_anchor_despite_empty_canonical() {
        use crate::module_tree::ModuleTree;
        use fxrank_core::frontend::SourceFile;
        // A nested fn whose body makes a `self::sibling()` call. Even though the
        // nested unit's canonical_path is empty, its refs must still anchor to the
        // file module (`crate::m`) so `self::sibling` resolves to crate::m::sibling.
        // F2: `ModuleTree::build` only recognizes a crate root at `src/lib.rs`/
        // `src/main.rs`. With ONLY `src/m.rs` in the batch there is no root, so
        // `module_of("src/m.rs")` is `None` → `module_context_of` returns `[]` and
        // the anchor assertion could never pass. Include `src/lib.rs` in the batch
        // so `module_of("src/m.rs") = Some(["m"])`.
        let mt = ModuleTree::build(&[
            SourceFile {
                path: "src/lib.rs".into(),
                text: String::new(),
            },
            SourceFile {
                path: "src/m.rs".into(),
                text: String::new(),
            },
        ]);
        let file = syn::parse_file("fn outer() { fn nested() { self::sibling(); } }").unwrap();
        let imports = ImportTable::from_file(&file);
        let statics = std::collections::HashSet::new();
        let nested = crate::functions::collect(&file, "src/m.rs")
            .into_iter()
            .find(|u| u.symbol == "nested")
            .unwrap();
        let rec = build_record(&nested, &imports, &statics, &mt);
        assert!(
            rec.canonical_path.is_empty(),
            "nested unit keeps empty canonical_path"
        );
        let r = rec.refs.iter().find(|r| r.base == "self::sibling").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["crate".into(), "m".into(), "sibling".into()]),
            "self:: inside a nested unit must anchor to crate::m via module_context_of, not lose its anchor"
        );
    }
}
