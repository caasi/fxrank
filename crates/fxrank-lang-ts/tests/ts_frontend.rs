//! Integration tests for the swc-based TypeScript frontend.

use fxrank_core::model::Hotspot;
use fxrank_lang_ts::detect::{self, calls};
use fxrank_lang_ts::functions;
use fxrank_lang_ts::imports::ImportTable;
use fxrank_lang_ts::source::{Lang, SpanLines};

/// Read a fixture, parse it (keeping the `SourceMap`), build `SpanLines` +
/// `ImportTable`, find the unit named `fn_name`, and run the full
/// `detect::analyze_unit` pipeline — returning a scored `Hotspot`.
fn analyze_fixture_unit(fixture: &str, fn_name: &str) -> Hotspot {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let (module, cm) = functions::parse_module(&src, fixture, Lang::Ts).expect("parse fixture");
    let lines = SpanLines::new(cm);
    let imports = ImportTable::from_module(&module);
    let units = functions::collect(&module, fixture, &lines);
    let unit = units
        .iter()
        .find(|u| u.symbol == fn_name)
        .expect("function unit not found");
    detect::analyze_unit(unit, &imports, &lines)
}

/// Read a fixture from `tests/fixtures/<name>`, parse it as TypeScript, run
/// `functions::collect`, and return the unit symbols.
fn collect_symbols(fixture: &str) -> Vec<String> {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let units = functions::parse_and_collect(&src, fixture, Lang::Ts).expect("parse fixture");
    units.into_iter().map(|u| u.symbol).collect()
}

/// Read a fixture, parse it (keeping the `SourceMap`), build `SpanLines` +
/// `ImportTable`, find the unit named `fn_name`, run `calls::detect` on its
/// body, and return the wire kinds of the effects found.
fn effect_kinds(fixture: &str, fn_name: &str) -> Vec<String> {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let (module, cm) = functions::parse_module(&src, fixture, Lang::Ts).expect("parse fixture");
    let lines = SpanLines::new(cm);
    let imports = ImportTable::from_module(&module);
    let units = functions::collect(&module, fixture, &lines);
    let unit = units
        .iter()
        .find(|u| u.symbol == fn_name)
        .expect("function unit not found");
    calls::detect(&unit.body, &imports, &lines)
        .iter()
        .map(|e| e.kind.wire().to_string())
        .collect()
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

#[test]
fn detects_world_effects() {
    let kinds = effect_kinds("calls.ts", "io");
    for k in [
        "net.fs.db",
        "logging",
        "time.read",
        "random",
        "env.read",
        "panic",
    ] {
        assert!(kinds.contains(&k.to_string()), "missing {k}");
    }
}

#[test]
fn detects_ctor_and_method_effects() {
    let kinds = effect_kinds("calls.ts", "ctorsAndMethods");
    // db.query → net.fs.db (heuristic, unknown-receiver method)
    // new WebSocket → net.fs.db (constructor)
    assert!(
        kinds.contains(&"net.fs.db".to_string()),
        "expected net.fs.db (query + WebSocket), got: {kinds:?}"
    );
    // new Date() with no args → time.read
    assert!(
        kinds.contains(&"time.read".to_string()),
        "expected time.read from new Date(), got: {kinds:?}"
    );
}

// ── Task 7: analyze_unit folds world effects into scored hotspots ──

#[test]
fn analyze_unit_scores_world_effects() {
    let h = analyze_fixture_unit("calls.ts", "io");
    // fetch → net.fs.db → class 7, weight 21
    assert_eq!(h.max_class, 7, "max_class should be 7 (net.fs.db)");
    assert!(
        h.own_score >= 21.0,
        "own_score should be >= 21.0 (weight_for_class(7) == 21), got {}",
        h.own_score
    );
    // io() is declared `async` and contains `await fetch(...)`
    assert!(h.async_boundary, "io() must be an async boundary");
    assert!(h.await_count >= 1, "io() has at least one await");
    assert!(!h.effects.is_empty(), "io() must have detected effects");
}

#[test]
fn analyze_unit_pure_fn_scores_zero() {
    // A function with no effects should have max_class 0, own_score 0.0.
    let src = "function pure(x: number): number { return x * 2; }";
    let (module, cm) = functions::parse_module(src, "pure.ts", Lang::Ts).expect("parse");
    let lines = SpanLines::new(cm);
    let imports = ImportTable::from_module(&module);
    let units = functions::collect(&module, "pure.ts", &lines);
    let unit = units.iter().find(|u| u.symbol == "pure").expect("unit");
    let h = detect::analyze_unit(unit, &imports, &lines);
    assert_eq!(h.max_class, 0);
    assert_eq!(h.own_score, 0.0);
    assert!(h.effects.is_empty());
    assert!(!h.async_boundary);
}
