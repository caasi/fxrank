//! Integration tests for the swc-based TypeScript frontend.

use fxrank_core::model::Hotspot;
use fxrank_lang_ts::detect::{self, calls, mutation};
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

// ── Task 8: mutation detection with escape analysis ──

/// Read a fixture, find the unit named `fn_name`, run `mutation::detect`, and
/// return each effect's `(wire kind, contained)` pair.
fn mutation_effects(fixture: &str, fn_name: &str) -> Vec<(String, bool)> {
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
    mutation::detect(&unit.body, &unit.sig, unit.is_constructor, &lines, &imports)
        .into_iter()
        .map(|(e, contained)| (e.kind.wire().to_string(), contained))
        .collect()
}

/// Just the wire kinds of a unit's mutation effects.
fn mutation_kinds(fixture: &str, fn_name: &str) -> Vec<String> {
    mutation_effects(fixture, fn_name)
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

#[test]
fn classifies_mutation_by_escape() {
    assert!(mutation_kinds("mutation.ts", "buildLocal").contains(&"local.mutation".into()));
    assert!(mutation_kinds("mutation.ts", "mutParam").contains(&"param.mutation".into()));
    assert!(mutation_kinds("mutation.ts", "viaClosure").contains(&"hidden.mutation".into()));
    assert!(mutation_kinds("mutation.ts", "Box.set").contains(&"this.mutation".into()));
    assert!(mutation_kinds("mutation.ts", "viaGlobal").contains(&"global.mutation".into()));
}

#[test]
fn contained_flag_tracks_escape() {
    // A write to a body-local binding is contained (Task 9 will discount it).
    let local = mutation_effects("mutation.ts", "buildLocal");
    let local_mut = local
        .iter()
        .find(|(k, _)| k == "local.mutation")
        .expect("buildLocal has a local.mutation");
    assert!(local_mut.1, "local.mutation must be contained == true");

    // A write to a param escapes the function — not contained.
    let param = mutation_effects("mutation.ts", "mutParam");
    let param_mut = param
        .iter()
        .find(|(k, _)| k == "param.mutation")
        .expect("mutParam has a param.mutation");
    assert!(!param_mut.1, "param.mutation must be contained == false");

    // `this.x = …` inside a constructor is local initialization — contained.
    let ctor = mutation_effects("mutation.ts", "WithCtor.constructor");
    let ctor_mut = ctor
        .iter()
        .find(|(k, _)| k == "local.mutation")
        .expect("WithCtor.constructor has a local.mutation (this.x = 1)");
    assert!(
        ctor_mut.1,
        "constructor this.x init must be local.mutation, contained == true"
    );
}

#[test]
fn delete_operator_detected_as_mutation() {
    // `delete o.a` where `o` is a body-local → local.mutation
    assert!(
        mutation_kinds("mutation.ts", "delLocal").contains(&"local.mutation".into()),
        "delLocal: delete on local should yield local.mutation"
    );
    // `delete o.x` where `o` is a param → param.mutation
    assert!(
        mutation_kinds("mutation.ts", "delParam").contains(&"param.mutation".into()),
        "delParam: delete on param should yield param.mutation"
    );
}

// ── Task 9: signature coverage + boundary-containment discount ──

use fxrank_core::score::BoundaryCoverage;
use fxrank_lang_ts::coverage;

/// Run `coverage::analyze` on a named unit of a fixture.
fn coverage_of(fixture: &str, fn_name: &str) -> coverage::Coverage {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let units = functions::parse_and_collect(&src, fixture, Lang::Ts).expect("parse fixture");
    let unit = units
        .iter()
        .find(|u| u.symbol == fn_name)
        .expect("function unit not found");
    coverage::analyze(&unit.sig, unit.is_constructor, &unit.body)
}

#[test]
fn coverage_counts_typed_slots_and_any() {
    let full = coverage_of("coverage.ts", "fullyTyped");
    assert_eq!(full.tier, BoundaryCoverage::Full);
    assert!(!full.has_any);

    let partial = coverage_of("coverage.ts", "partlyTyped");
    assert_eq!(partial.tier, BoundaryCoverage::Partial);
    assert!(!partial.has_any);

    let none = coverage_of("coverage.ts", "untyped");
    assert_eq!(none.tier, BoundaryCoverage::None);
    assert!(!none.has_any);

    let poisoned = coverage_of("coverage.ts", "poisoned");
    assert!(poisoned.has_any, "poisoned has `as any` in body");
    assert_eq!(poisoned.tier, BoundaryCoverage::None);
}

#[test]
fn boundary_discount_zeros_contained_local_mutation() {
    // fullyTyped: the local.mutation (a.push) is contained; Full coverage
    // discounts it to class 0 / weight 0 → own_score contribution 0.
    let full = analyze_fixture_unit("coverage.ts", "fullyTyped");
    let lm = full
        .effects
        .iter()
        .find(|e| e.kind.wire() == "local.mutation")
        .expect("fullyTyped has a local.mutation");
    assert_eq!(
        lm.discounted_to,
        Some(0),
        "Full coverage floors contained to 0"
    );
    assert_eq!(lm.effective_class(), 0);
    assert_eq!(lm.weight, 0);
    assert_eq!(full.own_score, 0.0, "the only effect discounts to weight 0");

    // untyped: None coverage → discount voided, local.mutation stays class 1.
    let untyped = analyze_fixture_unit("coverage.ts", "untyped");
    let ulm = untyped
        .effects
        .iter()
        .find(|e| e.kind.wire() == "local.mutation")
        .expect("untyped has a local.mutation");
    assert_eq!(ulm.discounted_to, None, "no typing → no discount");
    assert_eq!(ulm.effective_class(), 1);

    // poisoned: `as any` voids the discount AND emits a type.escape (class 3).
    let poisoned = analyze_fixture_unit("coverage.ts", "poisoned");
    let plm = poisoned
        .effects
        .iter()
        .find(|e| e.kind.wire() == "local.mutation")
        .expect("poisoned has a local.mutation");
    assert_eq!(plm.discounted_to, None, "any voids the boundary discount");
    assert_eq!(plm.effective_class(), 1);
    assert!(
        poisoned
            .risk_features
            .iter()
            .any(|r| r.kind.wire() == "type.escape"),
        "poisoned must carry a type.escape risk"
    );
    assert_eq!(
        poisoned.max_class, 3,
        "type.escape (class 3) dominates max_class"
    );
    assert_eq!(poisoned.risk_weight, 3, "weight_for_class(3) == 3");
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
