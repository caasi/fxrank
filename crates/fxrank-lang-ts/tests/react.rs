//! Integration tests for the React two-pass: component score inheritance from
//! inline hook callbacks, arrow suppression, and the EffectInRender phase rule.

mod util;

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
