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
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
    assert!(
        c.risk_features
            .iter()
            .all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
        "event handler is not render-time"
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
