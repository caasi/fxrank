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

/// Run every detector over `unit.block` and assemble a scored [`Hotspot`].
///
/// The gather step is the extension point: each detector returns `Vec<Effect>`
/// and they are concatenated.
///
/// `statics` is the set of top-level `static` names from the same file, used by
/// `calls::detect` to flag bare static-name path expressions as `ambient.read`.
pub fn analyze_unit(unit: &FnUnit, imports: &ImportTable, statics: &HashSet<String>) -> Hotspot {
    let effects: Vec<Effect> = gather(unit, imports, statics);
    let risks = risk::detect_fn_risks(&unit.block, &unit.sig, &unit.path);

    let is_async = unit.sig.asyncness.is_some();
    let await_count = count_awaits(&unit.block);
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

/// Count `expr.await` sites in a block using a simple visitor.
fn count_awaits(block: &syn::Block) -> usize {
    struct AwaitCounter(usize);
    impl<'ast> Visit<'ast> for AwaitCounter {
        fn visit_expr_await(&mut self, _node: &'ast syn::ExprAwait) {
            self.0 += 1;
            syn::visit::visit_expr_await(self, _node);
        }
    }
    let mut counter = AwaitCounter(0);
    counter.visit_block(block);
    counter.0
}

/// Gather effects from all detectors. New detectors plug in here.
fn gather(unit: &FnUnit, imports: &ImportTable, statics: &HashSet<String>) -> Vec<Effect> {
    let mut effects = Vec::new();
    effects.extend(calls::detect(&unit.block, imports, &unit.path, statics));
    effects.extend(macros::detect(&unit.block));
    effects.extend(mutation::detect(&unit.block, &unit.sig, statics, imports));
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
) -> fxrank_core::record::UnitRecord {
    let effects = gather(unit, imports, statics);
    let risks = risk::detect_fn_risks(&unit.block, &unit.sig, &unit.path);
    let call_refs = refs::extract(&unit.block, imports);
    let await_count = count_awaits(&unit.block);
    let async_boundary = unit.sig.asyncness.is_some() || await_count > 0;

    fxrank_core::record::UnitRecord {
        unit_id: unit.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,
        export: None,
        effects,
        risks,
        refs: call_refs,
        async_boundary,
        await_count,
        language: fxrank_core::frontend::Language::Rust,
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
        let units = crate::functions::collect(&f, "a.rs");
        let rec = build_record(&units[0], &imports, &statics);
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
}
