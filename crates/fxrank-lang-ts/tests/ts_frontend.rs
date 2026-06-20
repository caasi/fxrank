//! Integration tests for the swc-based TypeScript frontend.

use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_core::model::Hotspot;
use fxrank_lang_ts::TsFrontend;
use fxrank_lang_ts::detect::{self, calls, mutation, risk};
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
    //   <arrow@L5C…> (inline x => x), D.get v, D.set v  — 8 units total.
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

// ── Task 10: risk detection ──────────────────────────────────────────────────

/// Parse a fixture, find the unit named `fn_name`, run `risk::detect` directly,
/// and return the wire kinds of the risk features found.
fn risk_kinds(fixture: &str, fn_name: &str) -> Vec<String> {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let (module, cm) = functions::parse_module(&src, fixture, Lang::Ts).expect("parse fixture");
    let lines = SpanLines::new(cm);
    let units = functions::collect(&module, fixture, &lines);
    let unit = units
        .iter()
        .find(|u| u.symbol == fn_name)
        .expect("function unit not found");
    risk::detect(&unit.body, fixture, &lines)
        .iter()
        .map(|r| r.kind.wire().to_string())
        .collect()
}

#[test]
fn detects_risks() {
    // eval(...) → dynamic.code (exact)
    assert!(
        risk_kinds("risk.ts", "dyn").contains(&"dynamic.code".into()),
        "dyn: eval should yield dynamic.code"
    );
    // new Function(...) → dynamic.code (exact)
    assert!(
        risk_kinds("risk.ts", "dynFunc").contains(&"dynamic.code".into()),
        "dynFunc: new Function should yield dynamic.code"
    );
    // Object.setPrototypeOf(...) → proto.pollution (path)
    assert!(
        risk_kinds("risk.ts", "proto").contains(&"proto.pollution".into()),
        "proto: Object.setPrototypeOf should yield proto.pollution"
    );
    // .innerHTML = ... → html.injection (heuristic)
    assert!(
        risk_kinds("risk.ts", "html").contains(&"html.injection".into()),
        "html: .innerHTML = should yield html.injection"
    );
    // x! → type.escape (exact), owned by risk::detect (not coverage)
    assert!(
        risk_kinds("risk.ts", "nonNull").contains(&"type.escape".into()),
        "nonNull: x! should yield type.escape"
    );
    // Dedup split: risk::detect must NOT emit type.escape for a pure `as any`.
    // Coverage owns that; risk detecting it here would double-count.
    assert!(
        !risk_kinds("risk.ts", "pureAsAny").contains(&"type.escape".into()),
        "pureAsAny: risk::detect must NOT emit type.escape for `as any` (coverage owns it)"
    );
}

#[test]
fn dyn_hotspot_has_dynamic_code_and_max_class_7() {
    // End-to-end: risk folds into the hotspot score.
    // dyn() calls eval() → DynamicCode (class 7) → max_class 7.
    let h = analyze_fixture_unit("risk.ts", "dyn");
    assert!(
        h.risk_features
            .iter()
            .any(|r| r.kind.wire() == "dynamic.code"),
        "dyn hotspot must carry a dynamic.code risk feature"
    );
    assert_eq!(
        h.max_class, 7,
        "dynamic.code (class 7) must dominate max_class"
    );
}

#[test]
fn pure_as_any_has_exactly_one_type_escape() {
    // pureAsAny uses `as any` but no `x!`. Coverage emits one type.escape;
    // risk::detect must NOT add a second one.
    let h = analyze_fixture_unit("risk.ts", "pureAsAny");
    let count = h
        .risk_features
        .iter()
        .filter(|r| r.kind.wire() == "type.escape")
        .count();
    assert_eq!(
        count, 1,
        "pureAsAny must have exactly one type.escape (from coverage, not doubled by risk)"
    );
}

// ── P1: detectors must not descend into nested function bodies ───────────────

fn analyze_inline_units(src: &str) -> Vec<Hotspot> {
    let (module, cm) = functions::parse_module(src, "inline.ts", Lang::Ts).expect("parse");
    let lines = SpanLines::new(cm);
    let imports = ImportTable::from_module(&module);
    let units = functions::collect(&module, "inline.ts", &lines);
    units
        .iter()
        .map(|unit| detect::analyze_unit(unit, &imports, &lines))
        .collect()
}

#[test]
fn nested_fn_effects_not_attributed_to_parent() {
    let src = "function outer(): void { function inner(): void { fetch('/x'); } }";
    let hotspots = analyze_inline_units(src);
    let outer = hotspots
        .iter()
        .find(|h| h.symbol == "outer")
        .expect("outer");
    let inner = hotspots
        .iter()
        .find(|h| h.symbol == "inner")
        .expect("inner");
    assert_eq!(
        outer.max_class, 0,
        "outer has no own effects; got: {:?}",
        outer.effects
    );
    assert!(
        outer.effects.is_empty(),
        "outer effects must be empty; got: {:?}",
        outer.effects
    );
    assert!(inner.max_class > 0, "inner must have net.fs.db effect");
}

#[test]
fn callback_effects_not_attributed_to_parent() {
    let src = "function withCb(): void { [1].forEach((x: number) => { fetch('/y'); }); }";
    let hotspots = analyze_inline_units(src);
    let with_cb = hotspots
        .iter()
        .find(|h| h.symbol == "withCb")
        .expect("withCb");
    let arrow = hotspots
        .iter()
        .find(|h| h.symbol.starts_with("<arrow@"))
        .expect("arrow unit");
    assert_eq!(
        with_cb.max_class, 0,
        "withCb has no own effects; got: {:?}",
        with_cb.effects
    );
    assert!(
        with_cb.effects.is_empty(),
        "withCb effects must be empty; got: {:?}",
        with_cb.effects
    );
    assert!(arrow.max_class > 0, "arrow must have net.fs.db effect");
}

/// Parse a `.js` fixture (non-strict ES syntax), find the unit named `fn_name`,
/// run `risk::detect`, and return the wire kinds of the risk features found.
///
/// `with (…) {}` is strict-mode-illegal and therefore unparseable in `.ts`
/// (TS modules are always strict). This helper uses `Lang::Js` so the fixture
/// is parsed as non-strict ES, making `visit_with_stmt` reachable.
fn risk_kinds_js(fixture: &str, fn_name: &str) -> Vec<String> {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let (module, cm) = functions::parse_module(&src, fixture, Lang::Js).expect("parse fixture");
    let lines = SpanLines::new(cm);
    let units = functions::collect(&module, fixture, &lines);
    let unit = units
        .iter()
        .find(|u| u.symbol == fn_name)
        .expect("function unit not found");
    risk::detect(&unit.body, fixture, &lines)
        .iter()
        .map(|r| r.kind.wire().to_string())
        .collect()
}

#[test]
fn detects_with_stmt_via_js() {
    // `with (o) {}` is illegal in strict mode, so it cannot appear in a `.ts`
    // file (TS modules are always strict — swc rejects it at parse time).
    // `Lang::Js` uses non-strict ES syntax, so `visit_with_stmt` IS reachable
    // from a `.js` fixture. This test exercises that path.
    assert!(
        risk_kinds_js("risk_with.js", "usesWith").contains(&"dynamic.code".into()),
        "usesWith: with(...){{}} should yield dynamic.code"
    );
}

// ── P1 extension: coverage any-scan must not descend into nested function bodies ──

#[test]
fn nested_any_in_inner_fn_not_attributed_to_outer() {
    // `outer` has no `any` of its own; only `inner` has `const x: any = 1`.
    // `outer`'s `has_any` must be false → no type.escape risk, max_class 0.
    // `inner`'s `has_any` must be true → type.escape risk present.
    let src = "function outer(): void { function inner(): void { const x: any = 1; } }";
    let hotspots = analyze_inline_units(src);
    let outer = hotspots
        .iter()
        .find(|h| h.symbol == "outer")
        .expect("outer");
    let inner = hotspots
        .iter()
        .find(|h| h.symbol == "inner")
        .expect("inner");

    assert!(
        outer.risk_features.is_empty(),
        "outer must have no risk features (inner's any must not bleed up); got: {:?}",
        outer.risk_features
    );
    assert_eq!(
        outer.max_class, 0,
        "outer max_class must be 0; got {}",
        outer.max_class
    );

    assert!(
        inner
            .risk_features
            .iter()
            .any(|r| r.kind.wire() == "type.escape"),
        "inner must carry a type.escape risk (its own `const x: any`); got: {:?}",
        inner.risk_features
    );
}

// ── P2: test-file skipping by path ───────────────────────────────────────────

/// Build a `SourceFile` with a synthetic path and text (no disk I/O).
fn make_source(path: &str, text: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        text: text.into(),
    }
}

const TEST_SRC: &str = "function t(): void { fetch('/'); }";

#[test]
fn test_file_skipped_by_default() {
    // A .test.ts file should be skipped when include_tests == false (the default).
    let frontend = TsFrontend::default();
    let out = frontend.analyze(&[make_source("x.test.ts", TEST_SRC)]);
    assert!(
        out.functions.is_empty(),
        "test file must be skipped; got: {:?}",
        out.functions.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
    assert_eq!(out.skipped_tests, 1, "skipped_tests must be 1");
}

#[test]
fn test_file_included_when_include_tests_true() {
    // include_tests == true: even .test.ts files are analyzed.
    let frontend = TsFrontend {
        lang: Lang::Ts,
        include_tests: true,
    };
    let out = frontend.analyze(&[make_source("x.test.ts", TEST_SRC)]);
    assert_eq!(
        out.functions.len(),
        1,
        "include_tests=true must analyze the function"
    );
    assert_eq!(out.skipped_tests, 0, "skipped_tests must be 0");
}

#[test]
fn normal_ts_file_not_skipped() {
    // A plain .ts file is always analyzed regardless of include_tests.
    let frontend = TsFrontend::default();
    let out = frontend.analyze(&[make_source("app.ts", TEST_SRC)]);
    assert_eq!(out.functions.len(), 1, "normal .ts file must be analyzed");
    assert_eq!(out.skipped_tests, 0);
}

#[test]
fn is_test_file_recognizes_patterns() {
    use fxrank_lang_ts::is_test_file;
    // Must be recognized as test files.
    assert!(is_test_file("foo.test.ts"), "foo.test.ts");
    assert!(is_test_file("foo.spec.tsx"), "foo.spec.tsx");
    assert!(is_test_file("__tests__/foo.ts"), "__tests__/foo.ts");
    assert!(is_test_file("src/__tests__/bar.ts"), "src/__tests__/bar.ts");
    // Windows backslash path separators: __tests__ segment must still be found.
    assert!(
        is_test_file("src\\__tests__\\foo.ts"),
        "src\\\\__tests__\\\\foo.ts (Windows path)"
    );
    // Must NOT be recognized as test files.
    assert!(!is_test_file("foo.ts"), "foo.ts");
    assert!(!is_test_file("stdin"), "stdin");
    assert!(
        !is_test_file("testimony.ts"),
        "testimony.ts: no .test. infix"
    );
    assert!(!is_test_file("spectral.ts"), "spectral.ts: no .spec. infix");
    // A `.test.` in a Windows DIRECTORY name must not false-match the file.
    assert!(
        !is_test_file("C:\\work\\my.test.project\\src\\app.ts"),
        "windows dir with .test. infix: file is app.ts"
    );
}

#[test]
fn anonymous_fns_on_same_line_get_distinct_ids() {
    // Two anonymous arrows on one physical line — issue #9.
    let src = "foo().then(() => {}).catch(() => {});";
    let units = functions::parse_and_collect(src, "t.ts", Lang::Ts).expect("parse");

    let arrows: Vec<_> = units
        .iter()
        .filter(|u| u.symbol.starts_with("<arrow@L"))
        .collect();
    assert_eq!(arrows.len(), 2, "both arrows collected");

    // The bug: identical ids. The fix: distinct (column disambiguates).
    assert_ne!(
        arrows[0].id, arrows[1].id,
        "same-line arrows must have distinct ids"
    );

    // Every id in the report is unique.
    let ids: Vec<&String> = units.iter().map(|u| &u.id).collect();
    let unique: std::collections::HashSet<&&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "all hotspot ids are unique");

    // 4-field shape `path:line:col:symbol` and the symbol carries C{col}.
    for a in &arrows {
        assert!(
            a.symbol.starts_with("<arrow@L") && a.symbol.contains('C'),
            "anonymous symbol carries column: {}",
            a.symbol
        );
        assert!(
            a.id.starts_with("t.ts:1:"),
            "id is path:line:col:symbol: {}",
            a.id
        );
    }
}
