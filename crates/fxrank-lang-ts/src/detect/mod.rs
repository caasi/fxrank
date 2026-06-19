//! Per-function effect detection and `Hotspot` assembly for the TypeScript frontend.
//!
//! [`analyze_unit`] is the single entry point: it runs each detector over a
//! [`FnUnit`]'s body to gather a `Vec<Effect>`, then folds those effects into a
//! [`Hotspot`] using the core scoring functions. This is the swc analog of
//! `fxrank-lang-rust`'s `detect::analyze_unit`.
//!
//! Adding a detector is a one-line addition to the `gather` step.

pub mod calls;
pub mod mutation;

use crate::coverage;
use crate::functions::{FnBodyOwned, FnUnit};
use crate::imports::ImportTable;
use crate::source::SpanLines;
use fxrank_core::confidence::function_confidence;
use fxrank_core::effect::{Effect, RiskFeature, RiskKind, Tier};
use fxrank_core::model::Hotspot;
use fxrank_core::score::{
    BoundaryCoverage, apply_boundary_discount, max_class, own_score, weight_for_class,
};
use swc_ecma_ast::AwaitExpr;
use swc_ecma_visit::{Visit, VisitWith};

/// Run every detector over `unit.body` and assemble a scored [`Hotspot`].
///
/// The gather step is the extension point: each detector returns `Vec<Effect>`
/// and they are concatenated. `imports` and `lines` are threaded in from the
/// calling `TsFrontend::analyze`, which keeps the `SourceMap` alive so spans
/// can be resolved.
pub fn analyze_unit(unit: &FnUnit, imports: &ImportTable, lines: &SpanLines) -> Hotspot {
    let gathered: Vec<(Effect, bool)> = gather(unit, imports, lines);

    // The project thesis: types lower the score. Measure how typed the
    // signature is, then discount CONTAINED effects by the boundary tier — a
    // contained write behind a fully-typed boundary floors to class 0 (free),
    // while an `any` anywhere voids the gate (`tier == None`, no shift).
    let cov = coverage::analyze(&unit.sig, unit.is_constructor, &unit.body);

    let effects: Vec<Effect> = gathered
        .into_iter()
        .map(|(mut effect, contained)| {
            if contained && cov.tier != BoundaryCoverage::None {
                effect.discounted_to =
                    Some(apply_boundary_discount(effect.class, cov.tier, contained));
                let label = if cov.tier == BoundaryCoverage::Full {
                    "fully-typed"
                } else {
                    "typed"
                };
                effect.discount = Some(format!(
                    "contained by {label} boundary (coverage {}/{})",
                    cov.typed_slots, cov.total_slots
                ));
                effect.sync_weight();
            }
            effect
        })
        .collect();

    // The coverage gate owns the `any`-family `type.escape` risk. Task 10's risk
    // detector owns `!` / dynamic.code / proto.pollution / html.injection and
    // will NOT re-detect `any`.
    // TODO(Task 10): merge with risk::detect (any-family owned here; ! and others owned there)
    let mut risks: Vec<RiskFeature> = Vec::new();
    if cov.has_any {
        let class = RiskKind::TypeEscape.class();
        risks.push(RiskFeature {
            kind: RiskKind::TypeEscape,
            class,
            weight: weight_for_class(class),
            path: unit.path.clone(),
            line: unit.line,
            evidence: "any in signature or body".into(),
            tier: Tier::Heuristic,
        });
    }

    let await_count = count_awaits(&unit.body);
    let async_boundary = unit.is_async || await_count > 0;

    let weights: Vec<u32> = effects.iter().map(|e| e.weight).collect();
    let classes: Vec<u8> = effects.iter().map(|e| e.effective_class()).collect();

    // Build confidence inputs: each effect's confidence, plus a 0.8 synthetic
    // entry when there are awaits (the "unresolved awaited call" approximation,
    // mirroring the Rust frontend: an async fn that awaits may hide IO effects
    // we cannot see statically).
    let mut confidences: Vec<f64> = effects.iter().map(|e| e.confidence).collect();
    if await_count > 0 {
        confidences.push(0.8);
    }

    // Fold risks into scoring, mirroring the Rust `analyze_unit`.
    let risk_class = risks.iter().map(|r| r.class).max().unwrap_or(0);
    let risk_weight = if risks.is_empty() {
        0
    } else {
        weight_for_class(risk_class)
    };

    Hotspot {
        id: unit.id.clone(),
        symbol: unit.symbol.clone(),
        path: unit.path.clone(),
        line: unit.line,
        max_class: max_class(&classes, risk_class),
        own_score: own_score(&weights),
        risk_weight,
        confidence: function_confidence(&confidences),
        async_boundary,
        await_count,
        effects,
        risk_features: risks,
    }
}

/// Gather effects from all detectors, each paired with a `contained` flag.
///
/// New detectors plug in here. The bool is the boundary-containment signal Task
/// 9 consumes: world effects (calls) are never contained; mutation effects carry
/// their own per-write classification (a write to a local/constructor-`this` is
/// contained, an escaping write is not).
fn gather(unit: &FnUnit, imports: &ImportTable, lines: &SpanLines) -> Vec<(Effect, bool)> {
    let mut effects: Vec<(Effect, bool)> = Vec::new();
    // Call effects are world effects — never contained.
    effects.extend(
        calls::detect(&unit.body, imports, lines)
            .into_iter()
            .map(|e| (e, false)),
    );
    effects.extend(mutation::detect(
        &unit.body,
        &unit.sig,
        unit.is_constructor,
        lines,
        imports,
    ));
    // TODO(Task 10): risk::detect — risk features (unsafe, etc.).
    effects
}

/// Count `expr.await` sites in a body using a simple visitor.
fn count_awaits(body: &FnBodyOwned) -> usize {
    struct AwaitCounter(usize);

    impl Visit for AwaitCounter {
        fn visit_await_expr(&mut self, node: &AwaitExpr) {
            self.0 += 1;
            node.visit_children_with(self);
        }
    }

    let mut counter = AwaitCounter(0);
    body.walk_with(&mut counter);
    counter.0
}
