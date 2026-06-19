//! Per-function effect detection and `Hotspot` assembly for the TypeScript frontend.
//!
//! [`analyze_unit`] is the single entry point: it runs each detector over a
//! [`FnUnit`]'s body to gather a `Vec<Effect>`, then folds those effects into a
//! [`Hotspot`] using the core scoring functions. This is the swc analog of
//! `fxrank-lang-rust`'s `detect::analyze_unit`.
//!
//! Adding a detector is a one-line addition to the `gather` step.

pub mod calls;

use crate::functions::{FnBodyOwned, FnUnit};
use crate::imports::ImportTable;
use crate::source::SpanLines;
use fxrank_core::confidence::function_confidence;
use fxrank_core::effect::Effect;
use fxrank_core::model::Hotspot;
use fxrank_core::score::{max_class, own_score};
use swc_ecma_ast::AwaitExpr;
use swc_ecma_visit::{Visit, VisitWith};

/// Run every detector over `unit.body` and assemble a scored [`Hotspot`].
///
/// The gather step is the extension point: each detector returns `Vec<Effect>`
/// and they are concatenated. `imports` and `lines` are threaded in from the
/// calling `TsFrontend::analyze`, which keeps the `SourceMap` alive so spans
/// can be resolved.
pub fn analyze_unit(unit: &FnUnit, imports: &ImportTable, lines: &SpanLines) -> Hotspot {
    let effects: Vec<Effect> = gather(unit, imports, lines);

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

    // TODO(Task 10): risk::detect_fn_risks; for now risk_class and risk_weight are 0.
    let risk_class: u8 = 0;
    let risk_weight: u32 = 0;

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
        // TODO(Task 10): risk_features from risk::detect_fn_risks.
        risk_features: vec![],
    }
}

/// Gather effects from all detectors. New detectors plug in here.
fn gather(unit: &FnUnit, imports: &ImportTable, lines: &SpanLines) -> Vec<Effect> {
    let mut effects = Vec::new();
    effects.extend(calls::detect(&unit.body, imports, lines));
    // TODO(Task 8): mutation::detect — self/param write-through effects.
    // TODO(Task 9): boundary-containment discount applied here after all effects are gathered.
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
    match body {
        FnBodyOwned::Block(stmts) => {
            for s in stmts {
                s.visit_with(&mut counter);
            }
        }
        FnBodyOwned::Expr(e) => e.visit_with(&mut counter),
    }
    counter.0
}
