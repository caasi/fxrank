//! Integration tests for the swc-based TypeScript frontend.

use fxrank_lang_ts::functions;
use fxrank_lang_ts::source::Lang;

/// Read a fixture from `tests/fixtures/<name>`, parse it as TypeScript, run
/// `functions::collect`, and return the unit symbols.
fn collect_symbols(fixture: &str) -> Vec<String> {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let units = functions::parse_and_collect(&src, fixture, Lang::Ts).expect("parse fixture");
    units.into_iter().map(|u| u.symbol).collect()
}

#[test]
fn collects_all_function_forms() {
    let symbols = collect_symbols("functions.ts");
    assert!(symbols.contains(&"topLevel".to_string()));
    assert!(symbols.contains(&"arrowConst".to_string()));
    assert!(symbols.contains(&"C.method".to_string()));
    // Getter renamed: was "C.g", now includes "get " prefix to avoid collisions.
    assert!(symbols.contains(&"C.get g".to_string()));
    assert!(symbols.contains(&"exported".to_string()));
    assert!(symbols.iter().any(|s| s.starts_with("<arrow@L"))); // the inline x => x
    // Class D exercises both getter and setter of the same name — must not collide.
    assert!(symbols.contains(&"D.get v".to_string()));
    assert!(symbols.contains(&"D.set v".to_string()));
    // Total count guards against future double-emit regressions.
    // functions.ts yields: topLevel, arrowConst, C.method, C.get g, exported,
    //   <arrow@L5> (inline x => x), D.get v, D.set v  — 8 units total.
    assert_eq!(symbols.len(), 8);
}
