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
    let hs = util::analyze_tsx("function C(){ return <button onClick={() => fetch('/x')}/>; }");

    // AFFIRMATIVE: the inline handler arrow appears as its own non-suppressed hotspot
    // carrying the net.fs.db effect — proving the fetch was scored on the handler unit,
    // NOT on the component, and that scanning actually ran.
    let handler = hs
        .iter()
        .find(|h| h.symbol.starts_with("<arrow@"))
        .expect("onClick handler arrow must appear as its own hotspot");
    assert!(
        handler
            .effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
        "handler arrow carries NetFsDb effect (the fetch)"
    );
    assert!(
        handler
            .risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "handler arrow itself is not tagged EffectInRender (it is event-time)"
    );

    // ABSENCE: the component C must not carry EffectInRender — the absence assertion
    // is now discriminating because we confirmed scanning ran and the fetch scored.
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
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

/// `useCallback` bodies run on invocation (event-time), NOT during render.
/// A `fetch` inside a `useCallback` callback must inherit to the component at
/// honest baseline — no `EffectInRender` risk.
#[test]
fn usecallback_fetch_is_not_effect_in_render() {
    let hs = util::analyze_tsx(
        "function C(){ const f = useCallback(() => { fetch('/x'); }, []); return <div onClick={f}/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("component C");
    // The fetch effect must be inherited (net.fs.db class 7).
    assert!(
        c.effects
            .iter()
            .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
        "C must inherit the NetFsDb effect from the useCallback callback"
    );
    // But it must NOT carry EffectInRender — useCallback is event-phase.
    assert!(
        c.risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "useCallback body is event-time, not render-time: C must not carry EffectInRender"
    );
}

/// Regression guard: `useMemo` still correctly fires `EffectInRender`.
/// A `fetch` inside a `useMemo` callback runs during render → must carry the risk.
#[test]
fn usememo_fetch_still_effect_in_render() {
    let hs = util::analyze_tsx(
        "function C(){ const v = useMemo(() => fetch('/x'), []); return <div/>; }",
    );
    let c = hs.iter().find(|h| h.symbol == "C").expect("component C");
    assert!(
        c.risk_features
            .iter()
            .any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender),
        "useMemo callback runs during render: C must carry EffectInRender"
    );
}

/// Snapshot the full hotspot list for the three React fixture files.
///
/// Dynamic snapshot suffix (the `with_settings!` form) is used because the
/// 2-arg `assert_json_snapshot!(name, value)` form is version-sensitive and
/// the 1-arg form uses the test function name as the key.
#[test]
fn snapshot_react_fixtures() {
    for name in ["counter", "effects", "uncontrolled_cell"] {
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
