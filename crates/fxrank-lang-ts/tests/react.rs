//! Integration tests for the React two-pass: component score inheritance from
//! inline hook callbacks, arrow suppression, and the EffectInRender phase rule.

mod util;

/// Build a stable JSON summary of a single hotspot for snapshot testing.
/// Omits `line`/`id`/`path` which depend on fixture line numbers and break on unrelated edits.
/// Omits `evidence` and `discount` which are prose and may change without spec impact.
fn summarize(hs: &fxrank_core::model::Hotspot) -> serde_json::Value {
    serde_json::json!({
        "symbol": hs.symbol,
        "max_class": hs.max_class,
        "own_score": hs.own_score,
        "risk_weight": hs.risk_weight,
        "confidence": hs.confidence,
        "async_boundary": hs.async_boundary,
        "await_count": hs.await_count,
        "effects": hs.effects.iter().map(|e| serde_json::json!({
            "kind": e.kind.wire(),
            "class": e.class,
            "discounted_to": e.discounted_to,
            "tier": format!("{:?}", e.tier).to_lowercase(),
            "hidden": e.hidden,
            "subreason": e.subreason,
        })).collect::<Vec<_>>(),
        "risk_features": hs.risk_features.iter().map(|r| serde_json::json!({
            "kind": r.kind.wire(),
            "class": r.class,
        })).collect::<Vec<_>>(),
    })
}

#[test]
fn nested_handler_any_depth_is_adopted() {
    // fetch lives TWO hops down: component -> useEffect arrow -> inner arrow.
    let hs = util::analyze_tsx(
        "function C(){ useEffect(() => { const run = () => fetch('/x'); run(); }, []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    assert_eq!(
        c.max_class, 7,
        "C owns the depth-2 fetch (tree-aware, not single-hop)"
    );
    assert!(
        hs.iter().all(|h| !h.symbol.starts_with("<arrow@")),
        "all nested arrows suppressed (none float as orphan hotspots)"
    );
}

#[test]
fn named_local_handler_is_adopted_not_orphaned() {
    // The CURRENT bug: a named handler passed to onClick floats as its own hotspot.
    let hs = util::analyze_tsx(
        "function C(){ function onClick(){ fetch('/x'); } return <button onClick={onClick}/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    // The handler is a JSX onClick → event phase → its fetch (class 7) is owned by
    // C but earns the 1-class conditionality discount (spec 027 §2.4), so
    // max_class is the discounted 6 (conditional on interaction, never erased).
    assert_eq!(
        c.max_class, 6,
        "C owns its named handler's fetch, discounted to 6 (event-phase)"
    );
    assert!(
        !hs.iter().any(|h| h.symbol == "onClick"),
        "named local handler must NOT appear as an orphan hotspot"
    );
}

#[test]
fn useeffect_fetch_inherits_to_component_no_duplicate() {
    let hs = util::analyze_tsx(
        "function C(){ useEffect(() => { fetch('/api'); }, []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("component C");
    assert_eq!(c.max_class, 7, "C inherits the fetch at class 7");
    assert!(
        c.risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "effect-phase: no EffectInRender"
    );
    // the inline arrow must NOT also appear as a separate hotspot:
    assert!(
        hs.iter().all(|h| !h.symbol.starts_with("<arrow@")),
        "inherited arrow suppressed"
    );
}

#[test]
fn fetch_in_usememo_is_effect_in_render() {
    let hs = util::analyze_tsx(
        "function C(){ const x = useMemo(() => fetch('/b'), []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
    assert!(
        c.risk_features
            .iter()
            .any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender)
    );
}

#[test]
fn useref_write_outranks_setter() {
    let hs = util::analyze_tsx(
        "function R(){ const r = useRef(0); r.current = 1; return <i/>; } \
         function S(){ const [v,setV]=useState(0); return <i onClick={()=>setV(1)}/>; }",
    );
    let r = hs.iter().find(|h| h.symbol == "R").unwrap();
    let s = hs.iter().find(|h| h.symbol == "S").unwrap();
    assert!(
        r.max_class > s.max_class,
        "hidden ref ({}) > traced setter ({})",
        r.max_class,
        s.max_class
    );
}

#[test]
fn fetch_in_render_body_is_effect_in_render() {
    let hs = util::analyze_tsx("function C(){ fetch('/x'); return <div/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
    assert!(
        c.risk_features
            .iter()
            .any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender)
    );
}

#[test]
fn lifting_makes_child_pure_parent_holds_state() {
    let hs = util::analyze_tsx(
        "function Parent(){ const [v,setV]=useState(0); return <Child value={v} onChange={setV}/>; } \
         function Child({value,onChange}){ return <input value={value} onChange={onChange}/>; }",
    );
    let parent = hs.iter().find(|h| h.symbol == "Parent").unwrap();
    let child = hs.iter().find(|h| h.symbol == "Child").unwrap();
    assert!(
        parent
            .effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::StateTransition)
    );
    assert_eq!(child.max_class, 0, "presentational child is pure");
}

#[test]
fn onclick_handler_is_not_effect_in_render() {
    // Spec 027 §4.2: an inline JSX-prop handler is OWNED by the component — it is
    // adopted (suppressed as a standalone hotspot) and its effect re-parented onto
    // the component. (Pre-027 this arrow floated as its own `<arrow@…>` hotspot; the
    // adoption transform changed that — Task 3.)
    let hs = util::analyze_tsx("function C(){ return <button onClick={() => fetch('/x')}/>; }");

    // ADOPTION: the inline handler arrow is suppressed — it does NOT float as its
    // own hotspot. Its fetch is re-parented onto the component.
    assert!(
        hs.iter().all(|h| !h.symbol.starts_with("<arrow@")),
        "inline JSX handler must be adopted (suppressed), not float as an orphan; out={:?}",
        hs.iter().map(|h| &h.symbol).collect::<Vec<_>>(),
    );

    // AFFIRMATIVE: the component now carries the handler's net.fs.db effect —
    // proving the fetch was scored and re-parented onto the component.
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
    assert!(
        c.effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
        "C must carry the adopted handler's NetFsDb effect (the fetch)"
    );

    // ABSENCE: an event handler runs on invocation (event-time), NOT during the
    // render phase — so the component must NOT carry EffectInRender. The absence
    // assertion is discriminating because we confirmed the fetch scored above.
    assert!(
        c.risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "event handler is not render-time — C must not carry EffectInRender"
    );
}

/// An async inherited callback propagates its await metadata to the component.
///
/// Component `C` uses `useEffect(async () => { await fetch('/x'); }, [])`.
/// After inheritance + absorption, `C` must have `await_count > 0`,
/// `async_boundary == true`, and a confidence strictly lower than the sync variant
/// `D` (which has the same `fetch` effect but no `await`).
#[test]
fn async_inherited_callback_propagates_await_penalty() {
    // Async variant: useEffect callback is `async () => { await fetch('/x') }`
    let hs_async = util::analyze_tsx(
        "function C(){ useEffect(async () => { await fetch('/x'); }, []); return <div/>; }",
    );
    let c = hs_async
        .iter()
        .find(|h| h.symbol == "C")
        .expect("component C");

    assert!(
        c.await_count > 0,
        "C must have await_count > 0 (got {})",
        c.await_count
    );
    assert!(c.async_boundary, "C must have async_boundary == true");

    // Sync variant: same fetch but no async/await
    let hs_sync =
        util::analyze_tsx("function D(){ useEffect(() => { fetch('/x'); }, []); return <div/>; }");
    let d = hs_sync
        .iter()
        .find(|h| h.symbol == "D")
        .expect("component D");

    assert!(
        c.confidence < d.confidence,
        "async variant C confidence ({}) must be strictly lower than sync variant D confidence ({})",
        c.confidence,
        d.confidence
    );
}

/// `useCallback` bodies run on invocation (event-time), NOT during render — they
/// are EVENT phase (spec 027 §2.4). A `fetch` inside a `useCallback` callback must
/// inherit to the component with NO `EffectInRender` risk AND the 1-class
/// conditionality discount (class 7 → 6).
#[test]
fn usecallback_fetch_is_not_effect_in_render() {
    let hs = util::analyze_tsx(
        "function C(){ const f = useCallback(() => { fetch('/x'); }, []); return <div onClick={f}/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("component C");
    // The fetch effect must be inherited (net.fs.db class 7).
    let fetch = c
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("C must inherit the NetFsDb effect from the useCallback callback");
    // It must NOT carry EffectInRender — useCallback is event-phase.
    assert!(
        c.risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "useCallback body is event-time, not render-time: C must not carry EffectInRender"
    );
    // And — the new model — event-phase ⇒ conditionality discount (7 → 6).
    assert_eq!(fetch.class, 7, "base class unchanged");
    assert_eq!(
        fetch.discounted_to,
        Some(6),
        "useCallback is event-phase: conditional on interaction → down 1 class"
    );
    assert_eq!(fetch.subreason.as_deref(), Some("phase:event"));
}

/// Regression guard: `useMemo` still correctly fires `EffectInRender`.
/// A `fetch` inside a `useMemo` callback runs during render → must carry the risk.
#[test]
fn usememo_fetch_still_effect_in_render() {
    let hs = util::analyze_tsx(
        "function C(){ const v = useMemo(() => fetch('/x'), []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("component C");
    // The fetch effect must be inherited (net.fs.db class 7) — proving the render
    // risk is attached alongside a real inherited world effect, not a spurious path.
    assert!(
        c.effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
        "C must inherit the NetFsDb effect from the useMemo callback"
    );
    assert!(
        c.risk_features
            .iter()
            .any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender),
        "useMemo callback runs during render: C must carry EffectInRender"
    );
}

/// Two render-phase world effects in a `useMemo` callback on the same line must
/// produce `EffectInRender` risks with DISTINCT `col` values — not both `0`.
/// This guards against same-line collapse when risks are keyed by `(unit, line, col, kind)`.
#[test]
fn two_render_phase_effects_same_line_have_distinct_cols() {
    // Both fetch calls are on one source line inside useMemo → two world effects,
    // each carrying their own column → two EffectInRender risks with distinct cols.
    let src =
        "function C(){ const x = useMemo(() => { fetch('a'); fetch('b'); }, []); return <div/>; }";
    let hs = util::analyze_tsx(src);
    let c = hs.iter().find(|h| h.symbol == "C").expect("component C");
    let render_risks: Vec<_> = c
        .risk_features
        .iter()
        .filter(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender)
        .collect();
    assert!(
        render_risks.len() >= 2,
        "expected at least 2 EffectInRender risks, got {}",
        render_risks.len()
    );
    // All cols must be non-zero (no hardcoded col:0 placeholder).
    for r in &render_risks {
        assert_ne!(r.col, 0, "EffectInRender risk must carry real col, not 0");
    }
    // The two risks must have distinct cols (same-line, different positions).
    let cols: Vec<usize> = render_risks.iter().map(|r| r.col).collect();
    assert_ne!(
        cols[0], cols[1],
        "two same-line render-phase effects must produce distinct col values, got both {}",
        cols[0]
    );
}

/// Snapshot the full hotspot list for the three React fixture files.
///
/// Dynamic snapshot suffix (the `with_settings!` form) is used because the
/// 2-arg `assert_json_snapshot!(name, value)` form is version-sensitive and
/// the 1-arg form uses the test function name as the key.
#[test]
fn snapshot_react_fixtures() {
    for name in [
        "counter",
        "effects",
        "uncontrolled_cell",
        "attribution",
        "containment",
        "consumer_responsibility",
        "phase",
    ] {
        let path = format!(
            "{}/tests/fixtures/react/{name}.tsx",
            env!("CARGO_MANIFEST_DIR")
        );
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {name}.tsx: {e}"));
        let hs = util::analyze_tsx(&src);
        let snapshot: Vec<_> = hs.iter().map(summarize).collect();
        insta::with_settings!({ snapshot_suffix => name }, {
            insta::assert_json_snapshot!(snapshot);
        });
    }
}

/// `console.*` in render phase must NOT raise `EffectInRender`.
///
/// Logging is class 2 (benign annotation) — it is not the wrong-place IO
/// that `EffectInRender` flags (unlike `fetch`). A component or `useMemo`
/// callback that only calls `console.warn(...)` must be clean of the risk.
#[test]
fn console_warn_in_render_is_not_effect_in_render() {
    // Case 1: console.warn directly in the component render body.
    let hs_body = util::analyze_tsx("function C(){ console.warn('debug'); return <div/>; }");
    let c_body = hs_body
        .iter()
        .find(|h| h.symbol == "C")
        .expect("component C");
    assert!(
        c_body
            .risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "console.warn in render body must not raise EffectInRender"
    );

    // Case 2: console.warn inside a useMemo callback (render-phase hook).
    let hs_memo = util::analyze_tsx(
        "function C(){ const x = useMemo(() => { console.warn('debug'); return 1; }, []); return <div/>; }",
    );
    let c_memo = hs_memo
        .iter()
        .find(|h| h.symbol == "C")
        .expect("component C");
    assert!(
        c_memo
            .risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "console.warn in useMemo callback must not raise EffectInRender"
    );
}

// ---------------------------------------------------------------------------
// Task 5: JSX-prop & hook-arg function-value walker (spec 027 §4.5).
//
// A function passed as a VALUE (not called) routes by provenance:
//   inline arrow / LocalDefined-named → OWNED (adopted into the component);
//   Imported → graph EDGE (propagate, never copy);
//   Received / escaped → not charged, not adopted.
// ---------------------------------------------------------------------------

/// An imported handler passed as a JSX prop must become a graph EDGE on the
/// component (so the import's effects PROPAGATE through the fold), NOT be copied
/// into the component's own body.
#[test]
fn imported_handler_passed_as_prop_is_edge_not_copied() {
    let src = "import { handle } from './h';\n\
               function C(){ return <button onClick={handle}/>; }";
    let hs = util::analyze_tsx(src);
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    // own-body must NOT inline-copy the import's effects (no NetFsDb copied in).
    assert!(
        c.effects
            .iter()
            .all(|e| e.kind != fxrank_core::effect::EffectKind::NetFsDb),
        "imported handler is propagated via edge, not copied into own-body; effects={:?}",
        c.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
    );

    // Record level: C's record must carry an edge to the imported handler.
    let recs = util::analyze_tsx_records(src);
    let c_rec = recs.iter().find(|r| r.symbol == "C").expect("C record");
    assert!(
        c_rec
            .refs
            .iter()
            .any(|r| r.module.as_deref() == Some("./h") && r.base == "handle"),
        "C record must carry an edge to the imported handler `handle` (module './h'); refs={:?}",
        c_rec
            .refs
            .iter()
            .map(|r| (&r.base, &r.module))
            .collect::<Vec<_>>(),
    );
}

/// A received callback passed onward (`function C({onSave}){ … onClick={onSave} }`)
/// is NEVER charged to the component — origin wins (§2.3).
#[test]
fn received_handler_passed_onward_is_not_charged() {
    let hs = util::analyze_tsx("function C({onSave}){ return <button onClick={onSave}/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    assert_eq!(
        c.max_class, 0,
        "passing a received callback onward charges nothing"
    );
}

/// A LocalDefined named handler passed as a JSX prop is OWNED (adopted) by the
/// component — its effects are re-parented, it is suppressed as a standalone
/// hotspot.
#[test]
fn local_defined_named_handler_passed_as_prop_is_owned() {
    let hs = util::analyze_tsx(
        "function C(){ const onClick = () => { fetch('/x'); }; return <button onClick={onClick}/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    // Event-phase (JSX onClick): the owned fetch (class 7) is conditionality-
    // discounted one class → max_class 6 (spec 027 §2.4).
    assert_eq!(
        c.max_class, 6,
        "C owns its local named handler's fetch, discounted to 6 (event-phase)"
    );
    assert!(
        !hs.iter().any(|h| h.symbol == "onClick"),
        "local named handler must NOT float as an orphan hotspot; out={:?}",
        hs.iter().map(|h| &h.symbol).collect::<Vec<_>>(),
    );
}

/// Escape-gap closure (§4.5): a local function VALUE that escapes via being
/// passed to an UNKNOWN callee inside a hook callback must NOT be adopted — the
/// component does not own a value it handed to an opaque sink.
///
/// `const run = () => fetch('/x'); registerCallback(run);` — `run` is passed to
/// the unknown `registerCallback`, so its fetch must NOT be charged to C.
#[test]
fn local_value_escaped_to_unknown_callee_is_not_adopted() {
    let hs = util::analyze_tsx(
        "function C(){ useEffect(() => { const run = () => fetch('/x'); registerCallback(run); }, []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    assert!(
        c.effects
            .iter()
            .all(|e| e.kind != fxrank_core::effect::EffectKind::NetFsDb),
        "an escaped-to-unknown-callee value must NOT be adopted (no fetch charged to C); effects={:?}",
        c.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
    );
}

// ---------------------------------------------------------------------------
// Task 6: conditionality (phase) discount (spec 027 §2.4 — the C1 fix).
//
// Event-phase OwnedDeferred effects (JSX handlers, useCallback bodies) are
// CONDITIONAL on interaction, so they earn a NEW, orthogonal 1-class discount,
// floored at class 1 — a world effect is nudged one notch, never erased.
// Render-phase effects keep full weight (+ EffectInRender); effect-phase
// (useEffect/useLayoutEffect) ≈ full (no conditionality discount).
// ---------------------------------------------------------------------------

/// An inline JSX `onClick` handler's `fetch` runs only on interaction, so the
/// adopted `net.fs.db` (class 7) earns the 1-class conditionality discount
/// (→ 6), recorded with a `phase:event` rationale.
#[test]
fn event_handler_fetch_gets_conditionality_discount() {
    let hs = util::analyze_tsx("function C(){ return <button onClick={() => fetch('/x')}/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    let fetch = c
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("C owns the onClick fetch");
    assert_eq!(fetch.class, 7, "base class unchanged");
    assert_eq!(
        fetch.discounted_to,
        Some(6),
        "event-phase: down 1, floored at 1"
    );
    assert_eq!(fetch.subreason.as_deref(), Some("phase:event"));
    assert!(fetch.discount.is_some(), "rationale recorded");
}

/// A render-phase `fetch` (inside `useMemo`) must NOT get the conditionality
/// discount (it runs unconditionally during render) but DOES get EffectInRender.
#[test]
fn render_phase_fetch_keeps_full_weight_no_conditionality_discount() {
    let hs = util::analyze_tsx(
        "function C(){ const v = useMemo(() => fetch('/x'), []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    let fetch = c
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("C owns the useMemo fetch");
    assert_eq!(fetch.class, 7, "base class unchanged");
    assert_eq!(
        fetch.discounted_to, None,
        "render-phase fetch keeps full weight (no conditionality discount)"
    );
    assert!(
        c.risk_features
            .iter()
            .any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender),
        "render-phase fetch still earns EffectInRender"
    );
}

/// A class-1 owned-deferred effect in an event handler floors at 1 — never 0.
/// `setState` calls inside a `useCallback` produce a class-1 `state.transition`
/// style signal; whatever class-1 effect the handler owns must NOT be discounted
/// (1.saturating_sub(1)=0, but 0.max(1)=1, and 1 < 1 is false → no-op).
#[test]
fn event_discount_never_below_floor_one() {
    // A `useCallback` whose body has a body-local mutation (class 1) — adopted as
    // event-phase but class-1, so the conditionality discount is a no-op (floored).
    let hs = util::analyze_tsx(
        "function C(){ const f = useCallback(() => { let x = 0; x = 1; }, []); return <div onClick={f}/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    let local = c
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::LocalMutation)
        .expect("C owns the local.mutation");
    assert_eq!(local.class, 1, "base class 1");
    assert_eq!(
        local.discounted_to, None,
        "class-1 effect is never discounted below floor 1 (no-op)"
    );
}

/// A callback passed to an UNRECOGNIZED `use[A-Z]…` hook is still OWNED (the
/// component certainly passes it), but the unknown invocation schedule lowers
/// the component's confidence (treated as event-phase OwnedDeferred).
#[test]
fn unknown_hook_callback_is_owned_deferred_low_confidence() {
    let hs = util::analyze_tsx("function C(){ useMystery(() => fetch('/x')); return <div/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    assert!(
        c.effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
        "ownership is certain even for unknown hook"
    );
    assert!(
        c.confidence < 1.0,
        "unknown hook schedule lowers confidence (got {})",
        c.confidence
    );
}

/// Object-literal hook-arg callbacks (spec 027 §6, Task 9). A react-query-style
/// `useMutation({ mutationFn, onError })` hands its callbacks to the component's
/// own deferred subtree, so the component OWNS them: the `fetch` (net.fs.db
/// class 7) inside `mutationFn` is escaping + event-phase-discounted to 6, and
/// the `console.warn` inside `onError` is logging (class 2). No orphan arrow
/// hotspots; the unknown (non-built-in) hook schedule lowers confidence.
#[test]
fn object_literal_hook_arg_callbacks_owned_by_component() {
    let hs = util::analyze_tsx(
        "function PersonalTagCard(){ useMutation({ mutationFn: () => fetch('/x'), onError: () => console.warn('e') }); return <div/>; }",
    );
    let c = hs
        .iter()
        .find(|h| h.symbol == "PersonalTagCard")
        .expect("PersonalTagCard");

    // The fetch inside `mutationFn` is owned, escaping, event-phase discounted 7→6.
    let fetch = c
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("component owns the mutationFn fetch (net.fs.db)");
    assert_eq!(fetch.class, 7, "base class unchanged");
    assert_eq!(
        fetch.discounted_to,
        Some(6),
        "unknown-hook callback is event-like: conditionality discount 7→6"
    );

    // The console.warn inside `onError` is logging (class 2).
    assert!(
        c.effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::Logging),
        "component owns the onError console.warn (logging); effects={:?}",
        c.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
    );

    // own_score is non-zero — the previously-missed callbacks now count.
    assert!(
        c.own_score > 0.0,
        "component now owns the object-literal callbacks (own_score > 0); got {}",
        c.own_score
    );

    // No orphan arrow hotspots — both callbacks were adopted.
    assert!(
        hs.iter().all(|h| !h.symbol.starts_with("<arrow@")),
        "object-literal callbacks must be adopted, not float as orphans; out={:?}",
        hs.iter().map(|h| &h.symbol).collect::<Vec<_>>(),
    );

    // Unknown hook schedule lowers confidence.
    assert!(
        c.confidence < 1.0,
        "unknown (non-built-in) hook schedule lowers confidence (got {})",
        c.confidence
    );
}

/// The hook-vs-non-hook boundary holds at the integration level: an object arg
/// to a NON-hook callee is NOT descended — its callbacks escape (T3), so the
/// component is not charged.
#[test]
fn object_arg_to_non_hook_call_not_charged() {
    let hs = util::analyze_tsx(
        "function C(){ configureThing({ onSave: () => fetch('/x') }); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    assert!(
        c.effects
            .iter()
            .all(|e| e.kind != fxrank_core::effect::EffectKind::NetFsDb),
        "object arg to a non-hook call escapes — component not charged; effects={:?}",
        c.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
    );
}

// ---------------------------------------------------------------------------
// Spec 027 §5 per-principle acceptance tests (fixtures/react/*.tsx)
//
// These tests read the fixture files and verify the end-to-end behavior for
// each of the four core principles: attribution, containment,
// consumer-responsibility, and phase. They complement the inline-snippet tests
// above and serve as living documentation of the spec invariants.
// ---------------------------------------------------------------------------

/// Attribution (乙) — spec 027 §5 fixture `attribution.tsx`.
///
/// Two sub-cases:
///   1. Named inner handler (`handleSave`) passed to a JSX onClick prop →
///      adopted into `AttributionClick`; own_score > 0; no orphan hotspot.
///   2. Depth-2 nested callback (useEffect → inner arrow → fetch) →
///      adopted into `AttributionEffect`; own_score > 0; no orphans.
#[test]
fn attribution_fixture_components_own_effects() {
    let path = format!(
        "{}/tests/fixtures/react/attribution.tsx",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = std::fs::read_to_string(&path).expect("attribution.tsx");
    let hs = util::analyze_tsx(&src);

    // Sub-case 1: named handler adopted into component.
    let click = hs
        .iter()
        .find(|h| h.symbol == "AttributionClick")
        .expect("AttributionClick");
    assert!(
        click.own_score > 0.0,
        "AttributionClick must own its handler's fetch (own_score > 0); got {}",
        click.own_score
    );
    assert!(
        click.max_class >= 6,
        "AttributionClick owns event-phase fetch → max_class >= 6 (got {})",
        click.max_class
    );
    // No standalone `handleSave` hotspot — it was adopted.
    assert!(
        !hs.iter().any(|h| h.symbol == "handleSave"),
        "named handler `handleSave` must NOT appear as an orphan hotspot; out={:?}",
        hs.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );

    // Sub-case 2: depth-2 nested callback adopted.
    let effect = hs
        .iter()
        .find(|h| h.symbol == "AttributionEffect")
        .expect("AttributionEffect");
    assert!(
        effect.own_score > 0.0,
        "AttributionEffect must own depth-2 fetch (own_score > 0); got {}",
        effect.own_score
    );
    assert_eq!(
        effect.max_class, 7,
        "depth-2 useEffect fetch is effect-phase (class 7, no discount)"
    );

    // No orphan arrow hotspots from either component.
    assert!(
        hs.iter().all(|h| !h.symbol.starts_with("<arrow@")),
        "all nested arrows must be suppressed (none orphaned); out={:?}",
        hs.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
}

/// Containment — spec 027 §5 fixture `containment.tsx`.
///
/// `StateOnly` has only contained effects (state.transition class 1 + ref-cell
/// write class 3); `FetchingComponent` has an escaping world effect (class 7
/// discounted to 6 — event-phase). The contained component must score
/// substantially lower than the escaping one.
#[test]
fn containment_fixture_state_only_scores_lower_than_fetching() {
    let path = format!(
        "{}/tests/fixtures/react/containment.tsx",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = std::fs::read_to_string(&path).expect("containment.tsx");
    let hs = util::analyze_tsx(&src);

    let state_only = hs
        .iter()
        .find(|h| h.symbol == "StateOnly")
        .expect("StateOnly");
    let fetching = hs
        .iter()
        .find(|h| h.symbol == "FetchingComponent")
        .expect("FetchingComponent");

    // StateOnly: no world effects — only bounded state + ref write.
    assert!(
        state_only.max_class <= 3,
        "StateOnly must have no escaping world effects (max_class ≤ 3, got {})",
        state_only.max_class
    );
    assert!(
        state_only
            .effects
            .iter()
            .all(|e| e.kind != fxrank_core::effect::EffectKind::NetFsDb),
        "StateOnly must not carry a net.fs.db effect"
    );

    // FetchingComponent: world effect present and dominant.
    assert!(
        fetching
            .effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
        "FetchingComponent must carry net.fs.db"
    );
    assert!(
        fetching.own_score > state_only.own_score,
        "FetchingComponent must score higher than StateOnly ({} > {})",
        fetching.own_score,
        state_only.own_score
    );
}

/// Consumer-responsibility — spec 027 §5 fixture `consumer_responsibility.tsx`.
///
/// `CallbackChild` receives `onAction` from props and only passes it onward —
/// it is NEVER charged for that callback's effects. `CallbackParent` defines
/// the handler and holds the state, so it IS charged.
#[test]
fn consumer_responsibility_fixture_child_is_pure() {
    let path = format!(
        "{}/tests/fixtures/react/consumer_responsibility.tsx",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = std::fs::read_to_string(&path).expect("consumer_responsibility.tsx");
    let hs = util::analyze_tsx(&src);

    let child = hs
        .iter()
        .find(|h| h.symbol == "CallbackChild")
        .expect("CallbackChild");
    let parent = hs
        .iter()
        .find(|h| h.symbol == "CallbackParent")
        .expect("CallbackParent");

    // Child passes a received callback onward — must be completely pure.
    assert_eq!(
        child.max_class, 0,
        "CallbackChild only passes a received callback — must be pure (max_class 0)"
    );

    // Parent holds the state and defines the handler.
    assert!(
        parent.max_class > 0,
        "CallbackParent holds state + defines handler — must not be pure (got {})",
        parent.max_class
    );
    assert!(
        parent
            .effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::StateTransition),
        "CallbackParent must carry state.transition"
    );
}

/// Phase — spec 027 §5 fixture `phase.tsx`.
///
/// The same `fetch('/api')` call is scored differently depending on WHEN it runs:
///   - `RenderPhase` (useMemo): full-weight class 7, `discounted_to null`,
///     EffectInRender risk.
///   - `EffectPhase` (useEffect): full-weight class 7, `discounted_to null`,
///     no EffectInRender.
///   - `EventPhase` (onClick): conditionality discount → class 7 discounted to 6,
///     subreason "phase:event", no EffectInRender.
#[test]
fn phase_fixture_three_weightings_differ() {
    let path = format!(
        "{}/tests/fixtures/react/phase.tsx",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = std::fs::read_to_string(&path).expect("phase.tsx");
    let hs = util::analyze_tsx(&src);

    let render = hs
        .iter()
        .find(|h| h.symbol == "RenderPhase")
        .expect("RenderPhase");
    let effect = hs
        .iter()
        .find(|h| h.symbol == "EffectPhase")
        .expect("EffectPhase");
    let event = hs
        .iter()
        .find(|h| h.symbol == "EventPhase")
        .expect("EventPhase");

    // Render-phase: full weight + EffectInRender.
    let render_fetch = render
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("RenderPhase must carry net.fs.db");
    assert_eq!(render_fetch.class, 7, "render-phase base class 7");
    assert_eq!(
        render_fetch.discounted_to, None,
        "render-phase: no conditionality discount"
    );
    assert!(
        render
            .risk_features
            .iter()
            .any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender),
        "render-phase fetch must carry EffectInRender"
    );

    // Effect-phase: full weight, no EffectInRender, no conditionality discount.
    let effect_fetch = effect
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("EffectPhase must carry net.fs.db");
    assert_eq!(effect_fetch.class, 7, "effect-phase base class 7");
    assert_eq!(
        effect_fetch.discounted_to, None,
        "effect-phase: no conditionality discount"
    );
    assert!(
        effect
            .risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "effect-phase fetch must NOT carry EffectInRender"
    );

    // Event-phase: conditionality discount applied (7 → 6), phase:event rationale.
    let event_fetch = event
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("EventPhase must carry net.fs.db");
    assert_eq!(event_fetch.class, 7, "event-phase base class 7");
    assert_eq!(
        event_fetch.discounted_to,
        Some(6),
        "event-phase: conditionality discount → 6"
    );
    assert_eq!(
        event_fetch.subreason.as_deref(),
        Some("phase:event"),
        "event-phase discount must carry phase:event rationale"
    );
    assert!(
        event
            .risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "event-phase fetch must NOT carry EffectInRender"
    );

    // Aggregate: render and effect score equally high; event scores lower (discount).
    assert_eq!(
        render.max_class, 7,
        "render-phase max_class must be 7 (full weight)"
    );
    assert_eq!(
        effect.max_class, 7,
        "effect-phase max_class must be 7 (full weight)"
    );
    assert_eq!(
        event.max_class, 6,
        "event-phase max_class must be 6 (conditionality discount)"
    );
}

/// Finding 1 (Copilot, #37): a weakly-recognized component (PascalCase + `.tsx`
/// alone, no JSX/hook strong signal) carries the recognition confidence (0.8)
/// through to its emitted hotspot — `is_component`'s `confidence` field must be
/// min-clamped into `hotspots[].confidence`, not discarded.
#[test]
fn weak_component_recognition_lowers_hotspot_confidence() {
    // PascalCase name, `.tsx` file, but returns no JSX and uses no hooks: the only
    // recognition signal is PascalCase + extension ⇒ ComponentSignal.confidence 0.8.
    let hs = util::analyze_tsx("function Widget() { return 1; }");
    let c = hs.iter().find(|h| h.symbol == "Widget").expect("Widget");
    assert!(
        c.confidence < 1.0,
        "weak component recognition (0.8) must lower the hotspot confidence; got {}",
        c.confidence
    );
    assert!(
        (c.confidence - 0.8).abs() < f64::EPSILON,
        "hotspot confidence must reflect the 0.8 recognition confidence; got {}",
        c.confidence
    );
}

/// Finding 2 (Copilot, #37): a built-in hook whose args[0] is DATA (not a
/// callback) must NOT have that arg adopted as an owned deferred callback.
/// `useRef(() => fetch())` stores the arrow as a mutable cell value — React
/// never invokes it — so the component must NOT gain the `net.fs.db` effect.
#[test]
fn useref_data_arg_not_adopted_as_callback() {
    let hs =
        util::analyze_tsx("function C(){ const r = useRef(() => fetch('/x')); return <div/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    assert!(
        !c.effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
        "useRef args[0] is a stored data value, not a callback — component must \
         NOT gain net.fs.db; effects={:?}",
        c.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
    );
}

/// Finding 2 (Copilot, #37): `useInsertionEffect` IS an effect-phase hook (its
/// args[0] is an effect callback, exactly like `useEffect`) — the explicit arm
/// must keep adopting it so the regression is avoided.
#[test]
fn useinsertioneffect_callback_is_adopted_effect_phase() {
    let hs = util::analyze_tsx(
        "function C(){ useInsertionEffect(() => fetch('/x'), []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("C");
    let fetch = c
        .effects
        .iter()
        .find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
        .expect("useInsertionEffect callback is owned (net.fs.db)");
    // Effect-phase: full weight, no conditionality discount.
    assert_eq!(fetch.class, 7, "base class unchanged");
    assert_eq!(
        fetch.discounted_to, None,
        "effect-phase callback is the honest baseline (no event discount)"
    );
}
