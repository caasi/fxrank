//! Per-function effect detection and `Hotspot` assembly for the TypeScript frontend.
//!
//! [`analyze_unit`] is the single entry point: it runs each detector over a
//! [`FnUnit`]'s body to gather a `Vec<Effect>`, then folds those effects into a
//! [`Hotspot`] using the core scoring functions. This is the swc analog of
//! `fxrank-lang-rust`'s `detect::analyze_unit`.
//!
//! Adding a detector is a one-line addition to the `gather` step.

pub mod calls;
pub mod fnvalues;
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
/// [`adopt_effects`] can fold them into the owning component's `await_count`
/// and `async_boundary` fields, enabling the `0.8` await-confidence penalty to
/// apply even when the awaits live inside an inherited callback.
pub struct RawSignals {
    /// (effect, contained) — the gather tuple is PRESERVED (RAW = undiscounted).
    /// `adopt_effects` reads the `contained` flag and runs the React containment
    /// classifier over it; it must not be dropped here. See spec 027 §4.4.
    pub effects: Vec<(Effect, bool)>,
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
    // Pre-discount effects: PRESERVE the `(Effect, contained)` tuple. The
    // absorbing component's `adopt_effects` reads `contained` and runs the React
    // containment classifier over it (spec 027 §4.4) — the flag must survive.
    let effects: Vec<(Effect, bool)> = gather(unit, imports, lines, module_bindings, ref_bindings);

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

    // Capture the callback's async metadata so adopt_effects can fold it into
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
/// `panic` are class 4, so a `class >= 5` test would miss them.
/// `Logging` is **excluded** — `console.*` is a benign annotation (class 2)
/// and does not constitute the wrong-place IO that `EffectInRender` flags.
/// `TimeRead`/`Random` stay: nondeterminism during render is a legitimate concern.
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
/// Pushes `state_transitions` (CONTAINED — bounded internal state, the React
/// `&mut self`) and `context_reads` (ESCAPING — `useContext` reaches outside
/// the component) effects, adds an `EffectInRender` risk for each of the
/// component's OWN-body world effects, then recomputes the score.
///
/// `contained` is set EXPLICITLY per kind (it drives `Effect::escapes()` and the
/// cross-file fold). The constructors in `react.rs` default `contained = false`;
/// we override only `state.transition` to `true` here. `discounted_to` stays
/// `None`: these synthetic render-body effects have no signature boundary.
pub fn augment_component(h: &mut Hotspot, unit: &FnUnit, lines: &SpanLines) {
    for mut e in react::state_transitions(&unit.body, lines) {
        // Bounded internal state ⇒ contained ⇒ stays in own, does not propagate.
        e.contained = true;
        h.effects.push(e);
    }
    for e in react::context_reads(&unit.body, lines) {
        // useContext reaches outside the component ⇒ escaping (contained = false,
        // already the constructor default).
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

/// Apply the React containment classifier (spec 027 §2.2 / §4.4) to one
/// adopted effect, overriding the raw `contained` flag for the React-specific
/// kinds the gather step cannot classify on its own.
///
/// CONTAINED (`true`): `useState`/`useReducer` state transitions — the React
/// analog of `&mut self`, bounded internal state declared by the component.
/// ESCAPING (`false`): every world effect (`world_effect`); `useContext`
/// ambient reads (they reach outside the component); and — conservatively —
/// `ref-cell-write`. A precise "ref forwarded to a DOM node ⇒ escaping vs. used
/// as private storage ⇒ contained" classifier needs ref-forwarding-chain
/// analysis (spec 027 §6, deferred); until then a `ref-cell-write` stays
/// `contained = false` (escaping). We do NOT claim DOM precision here.
///
/// Any other kind keeps the raw `contained` flag the gather step produced (e.g.
/// a body-local `local.mutation` inside a callback stays contained).
fn react_contained(effect: &Effect, raw_contained: bool) -> bool {
    // Anti-Goodhart: a world effect is never contained, whatever the raw flag says.
    if world_effect(effect.kind) {
        return false;
    }
    match effect.kind {
        // Bounded internal state — contained (the React `&mut self`).
        EffectKind::StateTransition => true,
        // useContext reaches outside the component → escaping (matches today).
        EffectKind::AmbientRead => false,
        // Conservative: a useRef().current write stays escaping until a precise
        // DOM-forward classifier exists (§6). Private-storage vs DOM-attach is
        // indistinguishable without ref-forwarding-chain analysis.
        EffectKind::HiddenMutation if effect.subreason.as_deref() == Some("ref-cell-write") => {
            false
        }
        // Everything else keeps the gathered containment (body-local writes, etc).
        _ => raw_contained,
    }
}

/// Apply the conditionality (phase) discount to one adopted effect (spec 027
/// §2.4 — the C1 fix).
///
/// This is a NEW, ORTHOGONAL axis to the spec-003 containment discount: it is a
/// DIRECT write of `discounted_to`/`discount`/`subreason`, never a call to
/// `apply_discount` (the Rust `&mut`/`&self` mutation-channel discount, no React
/// analog) nor `apply_boundary_discount` (contained-only, refuses escaping
/// effects). An event-phase effect runs only on interaction, so it drops ONE
/// class — but it is **capped at 1 class and floored at class 1**: a world effect
/// is nudged one notch, NEVER erased to 0. It never touches `contained`.
///
/// For a class-1 effect the formula is a no-op: `1.saturating_sub(1) = 0`,
/// `0.max(1) = 1`, and `1 < 1` is false ⇒ no discount written (correct — a
/// class-1 effect cannot be nudged below the floor).
///
/// An `Unknown` phase additionally lowers `confidence` (the invocation schedule
/// is not known) and records the unknown-schedule rationale.
fn apply_conditionality_discount(e: &mut Effect, phase: HookPhase) {
    let base = e.effective_class();
    let down = base.saturating_sub(1).max(1); // cap 1 class, floor 1
    if down < base {
        e.discounted_to = Some(down);
        // `subreason` is the CLASSIFICATION axis (e.g. "ref-cell-write",
        // "captured-binding"). Only set it if no prior classification exists; the
        // phase rationale always goes into `discount` (the DISCOUNT axis).
        if e.subreason.is_none() {
            e.subreason = Some("phase:event".to_string());
        }
        e.discount = Some(match phase {
            HookPhase::Unknown => {
                "phase:event — conditional on interaction (unknown callback schedule)".to_string()
            }
            _ => "phase:event — conditional on interaction".to_string(),
        });
    }
    if phase == HookPhase::Unknown {
        // Unknown invocation schedule ⇒ lower confidence (applies whether or not
        // a class downshift was written, e.g. for a class-1 effect).
        e.confidence *= 0.8;
    }
}

/// Fold inherited hook-callback raw signals into the owning component, then
/// recompute the score.
///
/// Each inherited effect's REAL `contained` flag is preserved from the gather
/// tuple, then refined by the React containment classifier ([`react_contained`]):
/// a `state.transition` is contained (own internal state, stays in `own`); a
/// world effect / `useContext` / `ref-cell-write` escapes. `contained` drives
/// `Effect::escapes()`, which the shared cross-file fold reads — so a contained
/// adopted effect stays in the component's own score and does not propagate.
///
/// Adopted effects have no signature, so the spec-003 boundary discount does not
/// apply: `discounted_to` starts `None`. The **conditionality (phase) discount**
/// (spec 027 §2.4 — the C1 fix) is then layered on as a NEW, ORTHOGONAL axis:
/// for an event-phase (`HookPhase::Event`) or unknown-schedule (`HookPhase::Unknown`)
/// effect — one that runs only on interaction — we DIRECTLY write `discounted_to`
/// (NOT `apply_discount`/`apply_boundary_discount`, which key on `&mut`/containment
/// and have no React analog). The discount is capped at one class and floored at
/// class 1, so a world effect is *nudged one notch, never erased* — never to 0.
/// It never touches `contained` (orthogonal to spec 003).
///
/// For a `HookPhase::Render` callback (`useMemo` and the `useState`/`useReducer`
/// lazy initializers — bodies that run during render), each inherited world
/// effect additionally earns an `EffectInRender` risk; `HookPhase::Effect`
/// callbacks (`useEffect` / `useLayoutEffect`) do not — their bodies run outside
/// render, so running effects there is the honest baseline (≈ full, no discount).
/// An `Unknown` phase additionally lowers the effect's `confidence` (unknown
/// invocation schedule).
pub fn adopt_effects(h: &mut Hotspot, raws: Vec<(HookPhase, RawSignals)>) {
    for (phase, raw) in raws {
        for (mut e, raw_contained) in raw.effects {
            e.contained = react_contained(&e, raw_contained);
            // Adopted effects carry no signature ⇒ no spec-003 boundary discount.
            e.discounted_to = None;
            // Conditionality (phase) discount (spec 027 §2.4): event/unknown-phase
            // effects run only on interaction ⇒ ONE-class downshift, floored at 1.
            // Direct write — NOT apply_discount/apply_boundary_discount.
            if matches!(phase, HookPhase::Event | HookPhase::Unknown) {
                apply_conditionality_discount(&mut e, phase);
            }
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
/// Shared by [`augment_component`] and [`adopt_effects`] after they mutate
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
/// signals (via [`adopt_effects`]) and its own render-body signals (via
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
    module_map: &crate::module_map::TsModuleMap,
) -> fxrank_core::record::UnitRecord {
    let module_key = module_map.module_of(&unit.path);
    let mut canonical_path = vec![module_key];
    canonical_path.extend(symbol_segments(&unit.symbol));
    let mut refs = refs::extract(&unit.body, imports, lines, &unit.path, module_map);
    refs.extend_from_slice(extra_refs);
    fxrank_core::record::UnitRecord {
        unit_id: h.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false, // root is set by the CLI for explicit-file entries
        canonical_path,
        aliases: vec![], // barrel AliasFacts deferred (§9)
        effects: h.effects.clone(),
        risks: h.risk_features.clone(),
        refs,
        async_boundary: h.async_boundary,
        await_count: h.await_count,
        language: fxrank_core::frontend::Language::Ts,
    }
}

/// Split a TS display symbol into path segments (`C.method` → ["C","method"]).
fn symbol_segments(symbol: &str) -> Vec<String> {
    symbol.split('.').map(|s| s.to_string()).collect()
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
    fn record_from_hotspot_sets_canonical_path() {
        use crate::module_map::TsModuleMap;
        use fxrank_core::frontend::SourceFile;
        // The existing `unit_and_ctx` helper parses at path "t.ts" → module key "t".
        let mmap = TsModuleMap::build(&[SourceFile {
            path: "t.ts".into(),
            text: String::new(),
        }]);
        let src = "export function fetchUser() {}";
        let (units, imports, module_bindings, lines, idx) = unit_and_ctx(src, "fetchUser");
        let unit = &units[idx];
        let h = analyze_unit(unit, &imports, &lines, &module_bindings);
        let rec = record_from_hotspot(unit, &h, &imports, &lines, &[], &mmap);
        assert_eq!(
            rec.canonical_path,
            vec!["t".to_string(), "fetchUser".into()]
        );
    }

    #[test]
    fn record_from_hotspot_copies_final_hotspot_own_data() {
        use crate::module_map::TsModuleMap;
        use fxrank_core::frontend::SourceFile;
        let mmap = TsModuleMap::build(&[SourceFile {
            path: "t.ts".into(),
            text: String::new(),
        }]);
        let src = "import fs from 'node:fs';\n\
                   function writer(p: string) { fs.writeFileSync(p, 'x'); helper(); }";
        let (units, imports, module_bindings, lines, idx) = unit_and_ctx(src, "writer");
        let unit = &units[idx];
        let h = analyze_unit(unit, &imports, &lines, &module_bindings);
        let rec = record_from_hotspot(unit, &h, &imports, &lines, &[], &mmap);

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

    // ----- Task 4: React containment classifier -----

    /// Build a `RawSignals` carrying the effects gathered from `fn_name`'s body
    /// (tuple-preserving), threading an empty ref set. Helper for adoption tests.
    fn raw_for(src: &str, fn_name: &str) -> RawSignals {
        let (units, imports, mb, lines, idx) = unit_and_ctx(src, fn_name);
        raw_signals(&units[idx], &imports, &lines, &mb, &HashSet::new())
    }

    /// A useState-derived `StateTransition` adopted onto a component must be
    /// `contained` (own internal state) ⇒ does NOT escape ⇒ stays in `own`.
    #[test]
    fn adopted_state_transition_is_contained() {
        // Component holds state; the StateTransition is augmented onto it.
        let src = "function C(){ const [v,setV]=useState(0); return v; }";
        let (units, imports, mb, lines, idx) = unit_and_ctx(src, "C");
        let mut h = analyze_unit(&units[idx], &imports, &lines, &mb);
        augment_component(&mut h, &units[idx], &lines);
        let st = h
            .effects
            .iter()
            .find(|e| e.kind == EffectKind::StateTransition)
            .expect("expected a state.transition effect");
        assert!(
            st.contained,
            "state.transition is bounded internal state ⇒ contained"
        );
        assert!(!st.escapes(), "contained state.transition must not escape");
    }

    /// A `NetFsDb` adopted from a hook callback must be `contained = false` ⇒
    /// it escapes and propagates through the fold.
    #[test]
    fn adopted_fetch_is_escaping_not_contained() {
        // Component with an empty body; we adopt a fetching effect callback.
        let src_c = "function C(){ return null; }";
        let (units_c, imports_c, mb_c, lines_c, idx_c) = unit_and_ctx(src_c, "C");
        let mut h = analyze_unit(&units_c[idx_c], &imports_c, &lines_c, &mb_c);

        let cb_src = "function cb(){ fetch('/x'); }";
        let raw = raw_for(cb_src, "cb");
        adopt_effects(&mut h, vec![(HookPhase::Effect, raw)]);

        let net = h
            .effects
            .iter()
            .find(|e| e.kind == EffectKind::NetFsDb)
            .expect("expected an adopted net.fs.db effect");
        assert!(
            !net.contained,
            "a world effect (fetch) is never contained; got contained={}",
            net.contained
        );
        assert!(net.escapes(), "adopted fetch must escape");
    }

    /// `raw_signals` preserves the `(Effect, contained)` tuple: a body-local
    /// write inside an adopted callback keeps `contained = true`.
    #[test]
    fn raw_signals_preserves_contained_tuple() {
        let cb_src = "function cb(){ let x = 0; x = 1; }";
        let raw = raw_for(cb_src, "cb");
        let (e, contained) = raw
            .effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::LocalMutation)
            .expect("expected a local.mutation effect in raw signals");
        assert!(
            *contained,
            "body-local write must keep contained=true in RawSignals"
        );
        // And the flag is honoured when the effect is later adopted.
        let src_c = "function C(){ return null; }";
        let (units_c, imports_c, mb_c, lines_c, idx_c) = unit_and_ctx(src_c, "C");
        let mut h = analyze_unit(&units_c[idx_c], &imports_c, &lines_c, &mb_c);
        adopt_effects(&mut h, vec![(HookPhase::Effect, raw_for(cb_src, "cb"))]);
        let local = h
            .effects
            .iter()
            .find(|e| e.kind == EffectKind::LocalMutation)
            .expect("expected adopted local.mutation");
        assert!(
            local.contained && !local.escapes(),
            "adopted body-local write stays contained"
        );
        let _ = e; // pattern-bind use
    }

    /// A `ref-cell-write` adopted from a callback stays `contained = false`
    /// (escaping) — the conservative 027 default (no DOM-forward classifier).
    #[test]
    fn adopted_ref_cell_write_is_escaping_conservative() {
        // The component declares the ref; the callback writes `.current`.
        let src_c = "function C(){ const r = useRef(null); return null; }";
        let (units_c, imports_c, mb_c, lines_c, idx_c) = unit_and_ctx(src_c, "C");
        let mut h = analyze_unit(&units_c[idx_c], &imports_c, &lines_c, &mb_c);

        // Harvest the callback with the component's ref binding threaded in so
        // `r.current = …` classifies as ref-cell-write.
        let cb_src = "function cb(){ r.current = 1; }";
        let (units_cb, imports_cb, mb_cb, lines_cb, idx_cb) = unit_and_ctx(cb_src, "cb");
        let mut refs = HashSet::new();
        refs.insert("r".to_string());
        let raw = raw_signals(&units_cb[idx_cb], &imports_cb, &lines_cb, &mb_cb, &refs);
        let has_ref_cell = raw
            .effects
            .iter()
            .any(|(e, _)| e.subreason.as_deref() == Some("ref-cell-write"));
        assert!(has_ref_cell, "expected a ref-cell-write in raw signals");

        adopt_effects(&mut h, vec![(HookPhase::Effect, raw)]);
        let rc = h
            .effects
            .iter()
            .find(|e| e.subreason.as_deref() == Some("ref-cell-write"))
            .expect("expected adopted ref-cell-write");
        assert!(
            !rc.contained,
            "027 keeps ref-cell-write conservatively escaping (no DOM precision)"
        );
        assert!(rc.escapes(), "conservative ref-cell-write must escape");
    }

    /// Finding A (Copilot round-3): `apply_conditionality_discount` must NOT
    /// overwrite an existing `subreason` (the classification axis) when it applies
    /// the phase discount. The phase rationale belongs in `discount`, not
    /// `subreason`. If the effect already has a classification subreason (e.g.
    /// `"ref-cell-write"` on a `HiddenMutation`), that classification is preserved;
    /// `discount` always records the event-phase rationale.
    #[test]
    fn conditionality_discount_preserves_prior_subreason() {
        // Build a component with an empty body; adopt a ref-cell-write effect at
        // event-phase (simulating a `r.current = x` inside a JSX onClick handler
        // that was adopted into the component).
        let src_c = "function C(){ const r = useRef(null); return null; }";
        let (units_c, imports_c, mb_c, lines_c, idx_c) = unit_and_ctx(src_c, "C");
        let mut h = analyze_unit(&units_c[idx_c], &imports_c, &lines_c, &mb_c);

        // Construct a RawSignals carrying a HiddenMutation with subreason
        // "ref-cell-write" (class 3, escaping) — as produced by mutation::detect
        // for a `r.current = x` write.
        let cb_src = "function cb(){ r.current = 1; }";
        let (units_cb, imports_cb, mb_cb, lines_cb, idx_cb) = unit_and_ctx(cb_src, "cb");
        let mut refs = HashSet::new();
        refs.insert("r".to_string());
        let raw = raw_signals(&units_cb[idx_cb], &imports_cb, &lines_cb, &mb_cb, &refs);

        // Confirm the raw signal has a ref-cell-write subreason before adoption.
        assert!(
            raw.effects
                .iter()
                .any(|(e, _)| e.subreason.as_deref() == Some("ref-cell-write")),
            "pre-condition: raw must carry a ref-cell-write effect"
        );

        // Adopt at HookPhase::Event → conditionality discount applies.
        adopt_effects(&mut h, vec![(HookPhase::Event, raw)]);

        let rc = h
            .effects
            .iter()
            .find(|e| e.kind == EffectKind::HiddenMutation)
            .expect("expected adopted HiddenMutation");

        // The classification subreason must survive — discount must NOT clobber it.
        assert_eq!(
            rc.subreason.as_deref(),
            Some("ref-cell-write"),
            "conditionality discount must not clobber a prior classification subreason"
        );
        // The phase rationale must be in discount, not overwritten into subreason.
        assert!(
            rc.discount.is_some(),
            "discount must carry the event-phase rationale"
        );
        assert!(
            rc.discount.as_deref().unwrap_or("").contains("phase:event"),
            "discount text must mention phase:event"
        );
        // Class-3 effect → discounted to 2 (1-class cap, floor 1).
        assert_eq!(
            rc.discounted_to,
            Some(2),
            "class-3 ref-cell-write in event phase: down to 2"
        );
    }
}
