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
pub mod refs;
pub mod risk;

use crate::coverage;
use crate::functions::{FnBodyOwned, FnUnit};
use crate::imports::ImportTable;
use crate::react::{self, HookPhase};
use crate::source::SpanLines;
use fxrank_core::confidence::function_confidence;
use fxrank_core::effect::{Effect, EffectKind, RiskFeature, RiskKind, Tier};
use fxrank_core::model::Hotspot;
use fxrank_core::score::{
    BoundaryCoverage, apply_boundary_discount, max_class, own_score, weight_for_class,
};
use std::collections::HashSet;
use swc_ecma_ast::AwaitExpr;
use swc_ecma_visit::{Visit, VisitWith};

/// Run every detector over `unit.body` and assemble a scored [`Hotspot`].
///
/// The gather step is the extension point: each detector returns `Vec<Effect>`
/// and they are concatenated. `imports` and `lines` are threaded in from the
/// calling `TsFrontend::analyze`, which keeps the `SourceMap` alive so spans
/// can be resolved.
pub fn analyze_unit(
    unit: &FnUnit,
    imports: &ImportTable,
    lines: &SpanLines,
    module_bindings: &HashSet<String>,
) -> Hotspot {
    let gathered: Vec<(Effect, bool)> =
        gather(unit, imports, lines, module_bindings, &HashSet::new());

    // The project thesis: types lower the score. Measure how typed the
    // signature is, then discount CONTAINED effects by the boundary tier — a
    // contained write behind a fully-typed boundary floors to class 0 (free),
    // while an `any` anywhere voids the gate (`tier == None`, no shift).
    let cov = coverage::analyze(&unit.sig, unit.is_constructor, &unit.body);

    let effects: Vec<Effect> = gathered
        .into_iter()
        .map(|(mut effect, contained)| {
            // Wire the gather tuple's containment flag onto the Effect so that
            // `Effect::escapes()` and downstream propagation (cross-file fold) see
            // the real value — not the stub `false` that the default gives.
            effect.contained = contained;
            if contained && cov.tier != BoundaryCoverage::None {
                effect.discounted_to =
                    Some(apply_boundary_discount(effect.class, cov.tier, contained));
                // For class-1 effects (all contained effects in this milestone) Partial and Full
                // both floor to 0 — the discount value is identical. The label still records which
                // tier applied; it only separates a contained class->=2 effect (none exist yet).
                // See apply_boundary_discount / spec 003 "latent gradient".
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

    // The coverage gate owns the `any`-family `type.escape` risk. The risk
    // detector owns `!` / dynamic.code / proto.pollution / html.injection and
    // does NOT re-detect `any` (dedup split).
    let mut risks: Vec<RiskFeature> = Vec::new();
    if cov.has_any {
        let class = RiskKind::TypeEscape.class();
        risks.push(RiskFeature {
            kind: RiskKind::TypeEscape,
            class,
            weight: weight_for_class(class),
            path: unit.path.clone(),
            line: unit.line,
            col: unit.col,
            evidence: "any in signature or body".into(),
            tier: Tier::Heuristic,
        });
    }

    // Extend with per-body risks: non-null assertion, dynamic.code, proto.pollution, html.injection.
    risks.extend(risk::detect(&unit.body, &unit.path, lines));

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

/// Gather effects from all detectors, each paired with a `contained` flag.
///
/// New detectors plug in here. The bool is the boundary-containment signal Task
/// 9 consumes: world effects (calls) are never contained; mutation effects carry
/// their own per-write classification (a write to a local/constructor-`this` is
/// contained, an escaping write is not).
fn gather(
    unit: &FnUnit,
    imports: &ImportTable,
    lines: &SpanLines,
    module_bindings: &HashSet<String>,
    extra_refs: &HashSet<String>,
) -> Vec<(Effect, bool)> {
    let mut effects: Vec<(Effect, bool)> = Vec::new();
    // Call effects are world effects — never contained.
    effects.extend(
        calls::detect(&unit.body, imports, lines)
            .into_iter()
            .map(|e| (e, false)),
    );
    effects.extend(mutation::detect_with_refs(
        &unit.body,
        &unit.sig,
        unit.is_constructor,
        lines,
        imports,
        module_bindings,
        extra_refs,
    ));
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

        fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
        fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
        fn visit_constructor(&mut self, _n: &swc_ecma_ast::Constructor) {}
    }

    let mut counter = AwaitCounter(0);
    body.walk_with(&mut counter);
    counter.0
}

// ---------------------------------------------------------------------------
// React component assembly (Task 9): inheritance + suppression.
//
// A component absorbs the RAW (pre-discount) effects of the inline arrows it
// passes to built-in hooks; those arrow units are suppressed as standalone
// hotspots. Render-phase inherited world effects, plus the component's own-body
// world effects, gain an `EffectInRender` risk.
// ---------------------------------------------------------------------------

/// The pre-discount effects + risks of one function unit, harvested without
/// building a `Hotspot`.
///
/// Used by [`raw_signals`] to stash the signals of an inherited hook callback so
/// they can be folded into the owning component. RAW means **no boundary
/// discount has been applied** — the component absorbs the honest, undiscounted
/// effect of the callback it owns (it must not inherit a child's discounted
/// score, which the boundary discount could have floored to 0).
///
/// `await_count` and `is_async` carry the callback's async metadata so
/// [`absorb_inherited`] can fold them into the owning component's `await_count`
/// and `async_boundary` fields, enabling the `0.8` await-confidence penalty to
/// apply even when the awaits live inside an inherited callback.
pub struct RawSignals {
    pub effects: Vec<Effect>,
    pub risks: Vec<RiskFeature>,
    pub await_count: usize,
    pub is_async: bool,
}

/// Harvest the pre-discount effects + risks of `unit` (an inherited hook
/// callback), threading the owning component's `ref_bindings` into mutation
/// detection so a `r.current = …` write inside the callback still classifies as
/// `ref-cell-write`.
///
/// Returns the same effect/risk surface `analyze_unit` would gather, minus the
/// boundary discount and minus the `Hotspot` assembly: the call's world effects,
/// the mutation effects (now ref-aware), the coverage-owned `type.escape` risk,
/// and the per-body risks (`!`, dynamic.code, proto.pollution, html.injection).
pub fn raw_signals(
    unit: &FnUnit,
    imports: &ImportTable,
    lines: &SpanLines,
    module_bindings: &HashSet<String>,
    ref_bindings: &HashSet<String>,
) -> RawSignals {
    // Pre-discount effects: drop the `contained` flag — the absorbing component
    // forces `contained = false` on every inherited effect anyway.
    let effects: Vec<Effect> = gather(unit, imports, lines, module_bindings, ref_bindings)
        .into_iter()
        .map(|(e, _contained)| e)
        .collect();

    let mut risks: Vec<RiskFeature> = Vec::new();
    // The coverage gate owns the `any`-family `type.escape` risk.
    let cov = coverage::analyze(&unit.sig, unit.is_constructor, &unit.body);
    if cov.has_any {
        let class = RiskKind::TypeEscape.class();
        risks.push(RiskFeature {
            kind: RiskKind::TypeEscape,
            class,
            weight: weight_for_class(class),
            path: unit.path.clone(),
            line: unit.line,
            col: unit.col,
            evidence: "any in signature or body".into(),
            tier: Tier::Heuristic,
        });
    }
    // Per-body risks: non-null assertion, dynamic.code, proto.pollution, html.injection.
    risks.extend(risk::detect(&unit.body, &unit.path, lines));

    // Capture the callback's async metadata so absorb_inherited can fold it into
    // the owning component, propagating the await-confidence penalty.
    let await_count = count_awaits(&unit.body);
    let is_async = unit.is_async;

    RawSignals {
        effects,
        risks,
        await_count,
        is_async,
    }
}

/// Is `kind` a **world** effect — one that, in render phase, makes a component
/// impure (and so earns an `EffectInRender` risk)?
///
/// This is an **explicit kind match**, not a class threshold: `env.read` /
/// `logging` / `panic` are class 4, so a `class >= 5` test would miss them.
/// `AmbientRead` is deliberately EXCLUDED — `useContext` reuses it and must
/// never trip `EffectInRender`.
fn world_effect(kind: EffectKind) -> bool {
    matches!(
        kind,
        EffectKind::NetFsDb
            | EffectKind::ProcessControl
            | EffectKind::EnvWrite
            | EffectKind::Concurrency
            | EffectKind::TimeRead
            | EffectKind::Random
            | EffectKind::EnvRead
            | EffectKind::Logging
            | EffectKind::Panic
    )
}

/// Build an `EffectInRender` risk feature anchored at `path:line:col`.
fn effect_in_render_risk(path: &str, line: usize, col: usize) -> RiskFeature {
    let class = RiskKind::EffectInRender.class();
    RiskFeature {
        kind: RiskKind::EffectInRender,
        class,
        weight: weight_for_class(class),
        path: path.to_string(),
        line,
        col,
        evidence: "world effect during render phase".into(),
        tier: Tier::Heuristic,
    }
}

/// Augment a component's own `Hotspot` with its React render-body signals.
///
/// Pushes `state_transitions` + `context_reads` effects (all `contained =
/// false`, so the boundary discount never zeroes a class-2 `useContext`), adds
/// an `EffectInRender` risk for each of the component's OWN-body world effects,
/// then recomputes the score.
pub fn augment_component(h: &mut Hotspot, unit: &FnUnit, lines: &SpanLines) {
    for mut e in react::state_transitions(&unit.body, lines) {
        e.discounted_to = None;
        h.effects.push(e);
    }
    for mut e in react::context_reads(&unit.body, lines) {
        e.discounted_to = None;
        h.effects.push(e);
    }
    // The component's own-body world effects run during render: flag each.
    let own_world: Vec<(usize, usize)> = h
        .effects
        .iter()
        .filter(|e| world_effect(e.kind))
        .map(|e| (e.line, e.col))
        .collect();
    for (line, col) in own_world {
        h.risk_features
            .push(effect_in_render_risk(&h.path, line, col));
    }
    recompute(h);
}

/// Fold inherited hook-callback raw signals into the owning component, then
/// recompute the score.
///
/// Each inherited effect is forced `contained = false` / `discounted_to = None`
/// (the component never benefits from a child's boundary discount). For a
/// `HookPhase::Render` callback (`useMemo` and the `useState`/`useReducer` lazy
/// initializers — bodies that run during render), each inherited world effect
/// additionally earns an `EffectInRender` risk; `HookPhase::Effect` callbacks
/// (`useEffect` / `useLayoutEffect` / `useCallback`) do not — their bodies run
/// outside render (after commit, or on invocation), so running effects there is
/// the honest baseline.
pub fn absorb_inherited(h: &mut Hotspot, raws: Vec<(HookPhase, RawSignals)>) {
    for (phase, raw) in raws {
        for mut e in raw.effects {
            e.discounted_to = None;
            e.sync_weight();
            if phase == HookPhase::Render && world_effect(e.kind) {
                h.risk_features
                    .push(effect_in_render_risk(&h.path, e.line, e.col));
            }
            h.effects.push(e);
        }
        h.risk_features.extend(raw.risks);
        // Fold the callback's async metadata into the component so that:
        //   - recompute() sees h.await_count > 0 and pushes the 0.8 synthetic
        //     confidence entry (the "unresolved awaited call" penalty);
        //   - h.async_boundary reflects that the component now "owns" async IO
        //     through the inherited hook callback.
        h.await_count += raw.await_count;
        if raw.is_async || raw.await_count > 0 {
            h.async_boundary = true;
        }
    }
    recompute(h);
}

/// Recompute `own_score` / `max_class` / `risk_weight` / `confidence` from
/// `h.effects` + `h.risk_features`, using the core scoring functions.
///
/// Shared by [`augment_component`] and [`absorb_inherited`] after they mutate
/// the effect/risk vectors. Mirrors the fold in [`analyze_unit`] (it does not
/// touch `async_boundary` / `await_count`, which are body-structural and set
/// once at collection).
fn recompute(h: &mut Hotspot) {
    let weights: Vec<u32> = h.effects.iter().map(|e| e.weight).collect();
    let classes: Vec<u8> = h.effects.iter().map(|e| e.effective_class()).collect();
    let mut confidences: Vec<f64> = h.effects.iter().map(|e| e.confidence).collect();
    if h.await_count > 0 {
        confidences.push(0.8);
    }
    let risk_class = h.risk_features.iter().map(|r| r.class).max().unwrap_or(0);
    h.risk_weight = if h.risk_features.is_empty() {
        0
    } else {
        weight_for_class(risk_class)
    };
    h.max_class = max_class(&classes, risk_class);
    h.own_score = own_score(&weights);
    h.confidence = function_confidence(&confidences);
}

/// Build a language-neutral [`fxrank_core::record::UnitRecord`] whose own-body
/// data is **copied from the final [`Hotspot`] `h`** — not re-derived.
///
/// This is the React-two-pass-safe analog of the Rust/Python `build_record`:
/// because a component's Hotspot has already absorbed its inherited hook-callback
/// signals (via [`absorb_inherited`]) and its own render-body signals (via
/// [`augment_component`]) by the time we emit, copying `effects` / `risks` /
/// `async_boundary` / `await_count` straight off `h` guarantees the record's
/// own-data is *exactly* the Hotspot's own-data — including those absorbed
/// signals. `unit_id` is taken from `h.id` (so the cross-file fold's `apply_fold`
/// matches by id); `path`/`line`/`col`/`symbol` come from the `unit`; `refs` are
/// this unit's own outgoing call references, plus any `extra_refs` from suppressed
/// hook-callback arrows that have been absorbed into this component (pass `&[]` for
/// non-component units). Duplicate `CallSiteRef`s between own-body and absorbed
/// arrows are harmless (the cross-file fold deduplicates by `SiteKey`), but we
/// extend rather than dedup here for simplicity.
pub fn record_from_hotspot(
    unit: &FnUnit,
    h: &Hotspot,
    imports: &ImportTable,
    lines: &SpanLines,
    extra_refs: &[fxrank_core::record::CallSiteRef],
) -> fxrank_core::record::UnitRecord {
    let mut refs = refs::extract(&unit.body, imports, lines);
    refs.extend_from_slice(extra_refs);
    fxrank_core::record::UnitRecord {
        unit_id: h.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,         // root is set by the CLI for explicit-file entries
        canonical_path: vec![], // 025-3e: frontend not yet adopted → non-adopted partition
        aliases: vec![],
        effects: h.effects.clone(),
        risks: h.risk_features.clone(),
        refs,
        async_boundary: h.async_boundary,
        await_count: h.await_count,
        language: fxrank_core::frontend::Language::Ts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use crate::source::Lang;

    /// Parse `src`, build the analysis context, and return the unit named
    /// `fn_name` together with the context pieces needed to build a record.
    fn unit_and_ctx(
        src: &str,
        fn_name: &str,
    ) -> (Vec<FnUnit>, ImportTable, HashSet<String>, SpanLines, usize) {
        let (module, cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let idx = units
            .iter()
            .position(|u| u.symbol == fn_name)
            .expect("unit not found");
        (units, imports, module_bindings, lines, idx)
    }

    /// Verify that `analyze_unit` sets `Effect.contained` from the gather tuple.
    ///
    /// A body-local write (`let x = 0; x = 1;`) produces `local.mutation` with
    /// `contained == true` (and therefore `!escapes()`).  An escaping write to a
    /// normal method's `this` field produces `this.mutation` with `contained == false`
    /// (and therefore `escapes()`).
    #[test]
    fn analyze_unit_sets_effect_contained() {
        // --- contained: body-local write ---
        let src_local = "function f() { let x = 0; x = 1; }";
        let (units_l, imports_l, mb_l, lines_l, idx_l) = unit_and_ctx(src_local, "f");
        let unit_l = &units_l[idx_l];
        let h_l = analyze_unit(unit_l, &imports_l, &lines_l, &mb_l);
        let local_mut = h_l
            .effects
            .iter()
            .find(|e| e.kind == EffectKind::LocalMutation)
            .expect("expected a local.mutation effect");
        assert!(
            local_mut.contained,
            "local.mutation should be contained; got contained={}",
            local_mut.contained
        );
        assert!(
            !local_mut.escapes(),
            "contained local.mutation should not escape"
        );

        // --- escaping: this.mutation in a normal method ---
        let src_this = "class C { m() { this.x = 1; } }";
        let (units_t, imports_t, mb_t, lines_t, idx_t) = unit_and_ctx(src_this, "C.m");
        let unit_t = &units_t[idx_t];
        let h_t = analyze_unit(unit_t, &imports_t, &lines_t, &mb_t);
        let this_mut = h_t
            .effects
            .iter()
            .find(|e| e.kind == EffectKind::ThisMutation)
            .expect("expected a this.mutation effect");
        assert!(
            !this_mut.contained,
            "this.mutation should not be contained; got contained={}",
            this_mut.contained
        );
        assert!(this_mut.escapes(), "this.mutation should escape");
    }

    #[test]
    fn record_from_hotspot_copies_final_hotspot_own_data() {
        let src = "import fs from 'node:fs';\n\
                   function writer(p: string) { fs.writeFileSync(p, 'x'); helper(); }";
        let (units, imports, module_bindings, lines, idx) = unit_and_ctx(src, "writer");
        let unit = &units[idx];
        let h = analyze_unit(unit, &imports, &lines, &module_bindings);
        let rec = record_from_hotspot(unit, &h, &imports, &lines, &[]);

        assert_eq!(rec.unit_id, h.id, "unit_id must equal the Hotspot id");
        // `Effect`/`RiskFeature` are not `PartialEq`; compare the observable
        // own-data the record copies from the Hotspot via the effect kinds + lens.
        assert_eq!(
            rec.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
            h.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
            "effects copied from Hotspot"
        );
        assert_eq!(
            rec.risks.iter().map(|r| r.kind).collect::<Vec<_>>(),
            h.risk_features.iter().map(|r| r.kind).collect::<Vec<_>>(),
            "risks copied from Hotspot"
        );
        assert_eq!(rec.async_boundary, h.async_boundary);
        assert_eq!(rec.await_count, h.await_count);
        assert_eq!(rec.symbol, "writer");
        assert_eq!(rec.language, fxrank_core::frontend::Language::Ts);
        assert!(!rec.is_root);
        assert!(
            rec.refs.iter().any(|r| r.base == "helper"),
            "expected own-body refs to be populated, got: {:?}",
            rec.refs
        );
    }
}
