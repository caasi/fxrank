//! Integration tests for the swc-based TypeScript frontend.

use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_core::model::Hotspot;
use fxrank_lang_ts::TsFrontend;
use fxrank_lang_ts::detect::{self, calls, mutation, risk};
use fxrank_lang_ts::functions;
use fxrank_lang_ts::imports::{self, ImportTable};
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
    let module_bindings = imports::module_bindings(&module);
    let units = functions::collect(&module, fixture, &lines);
    let unit = units
        .iter()
        .find(|u| u.symbol == fn_name)
        .expect("function unit not found");
    detect::analyze_unit(unit, &imports, &lines, &module_bindings)
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
    let module_bindings = imports::module_bindings(&module);
    let units = functions::collect(&module, fixture, &lines);
    let unit = units
        .iter()
        .find(|u| u.symbol == fn_name)
        .expect("function unit not found");
    mutation::detect(
        &unit.body,
        &unit.sig,
        unit.is_constructor,
        &lines,
        &imports,
        &module_bindings,
    )
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
    // issue #29: a write to a MODULE-level binding (`counter`) escalates to
    // global.mutation (class 6), not hidden.mutation.
    assert!(mutation_kinds("mutation.ts", "viaModuleVar").contains(&"global.mutation".into()));
    // A captured ENCLOSING-FUNCTION local stays hidden.mutation (class 3).
    assert!(mutation_kinds("mutation.ts", "inner").contains(&"hidden.mutation".into()));
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
    let module_bindings = imports::module_bindings(&module);
    let units = functions::collect(&module, "pure.ts", &lines);
    let unit = units.iter().find(|u| u.symbol == "pure").expect("unit");
    let h = detect::analyze_unit(unit, &imports, &lines, &module_bindings);
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
    let module_bindings = imports::module_bindings(&module);
    let units = functions::collect(&module, "inline.ts", &lines);
    units
        .iter()
        .map(|unit| detect::analyze_unit(unit, &imports, &lines, &module_bindings))
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

// ── Phase-3b Task 2: is_root (frontend-level; CLI sets the real value) ──────────

/// Build a `SourceFile`, run `TsFrontend::analyze`, return `(hotspots, records)`.
fn analyze_source(path: &str, text: &str) -> (Vec<Hotspot>, Vec<fxrank_core::record::UnitRecord>) {
    let frontend = TsFrontend::default();
    let out = frontend.analyze(&[make_source(path, text)]);
    (out.functions, out.records)
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

    // Parse each id into exactly 4 fields `path:line:col:symbol` (the symbol
    // `<arrow@LnCm>` and the path `t.ts` contain no colons, so a plain split is
    // unambiguous here) and validate every field — not just a prefix, which the
    // old 3-field format would also satisfy.
    for a in &arrows {
        let fields: Vec<&str> = a.id.split(':').collect();
        assert_eq!(fields.len(), 4, "id has 4 colon-separated fields: {}", a.id);
        let (path, line, col_str, sym) = (fields[0], fields[1], fields[2], fields[3]);
        assert_eq!(path, "t.ts", "path field: {}", a.id);
        assert_eq!(line, "1", "line field is 1: {}", a.id);
        let col: usize = col_str.parse().expect("col field is numeric");
        assert!(col >= 1, "col is 1-based (>= 1): {}", a.id);
        assert_eq!(sym, a.symbol, "4th field is the symbol: {}", a.id);

        // Parse the *whole* anonymous symbol shape `<arrow@L{line}C{col}>` and
        // cross-check both coordinates against the id fields — a loose suffix
        // match would accept malformed symbols (`<arrow@L1CjunkC12>`, a missing
        // `>`, or an `L{line}` that disagrees with the id).
        let inner = a
            .symbol
            .strip_prefix("<arrow@L")
            .and_then(|s| s.strip_suffix('>'))
            .expect("anonymous arrow symbol is <arrow@L{line}C{col}>");
        let (sym_line, sym_col_str) = inner
            .rsplit_once('C')
            .expect("symbol has an L{line}C{col} shape");
        assert_eq!(
            sym_line, line,
            "symbol L{{line}} matches id line field: {}",
            a.symbol
        );
        let sym_col: usize = sym_col_str.parse().expect("symbol col is numeric");
        assert_eq!(
            sym_col, col,
            "symbol C{{col}} matches id col field: {}",
            a.symbol
        );
    }
}

// ── Phase-3d Task 1: module-init unit ────────────────────────────────────────

/// Run `analyze_units` (the full two-pass) on inline TS src via `TsFrontend::analyze`.
fn analyze_module_src(src: &str) -> Vec<Hotspot> {
    let frontend = TsFrontend {
        lang: Lang::Ts,
        include_tests: false,
    };
    let out = frontend.analyze(&[make_source("module_init.ts", src)]);
    out.functions
}

/// A module with top-level effectful statements must produce a `<module>` hotspot
/// with effects. Pure functions inside are their own separate units.
#[test]
fn module_init_unit_emitted_for_effectful_module() {
    let src = "import { createClient } from './db';\n\
               export const client = createClient();\n\
               fetch('https://x');\n\
               function pure() { return 1; }\n";
    let hotspots = analyze_module_src(src);

    // <module> hotspot must exist.
    let module_hs = hotspots
        .iter()
        .find(|h| h.symbol == "<module>")
        .expect("<module> hotspot must be emitted for effectful top-level statements");

    // Must have at least one effect (the top-level fetch → net.fs.db).
    assert!(
        !module_hs.effects.is_empty(),
        "<module> must have effects; got empty effects"
    );
    assert!(
        module_hs
            .effects
            .iter()
            .any(|e| e.kind.wire() == "net.fs.db"),
        "<module> must carry a net.fs.db effect from top-level fetch; effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );

    // <module> id must be "module_init.ts:1:1:<module>".
    assert_eq!(
        module_hs.id, "module_init.ts:1:1:<module>",
        "<module> id must be path:1:1:<module>"
    );

    // pure() must be a separate unit with NO effects (own-body = top-level only).
    let pure_hs = hotspots
        .iter()
        .find(|h| h.symbol == "pure")
        .expect("pure() unit must still be emitted");
    assert!(
        pure_hs.effects.is_empty(),
        "pure() must have no effects (own-body isolation); got: {:?}",
        pure_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );

    // <module> effects must NOT include anything from inside pure().
    // (The fetch is at module scope; pure() has `return 1` which is not effectful.)
    // This is inherently satisfied by the own-body semantics, but verify explicitly:
    // the <module> effects count must equal the number of top-level call effects
    // (no double-counting from pure's body).
    // We only assert the pure() function's effects are not attributed to <module>,
    // which the above pure_hs.effects.is_empty() already covers.
}

/// A pure module (only imports + a named function) must produce NO `<module>` hotspot.
#[test]
fn module_init_unit_absent_for_pure_module() {
    let src = "import x from './y';\n\
               function f() { return 1; }\n";
    let hotspots = analyze_module_src(src);

    assert!(
        !hotspots.iter().any(|h| h.symbol == "<module>"),
        "pure module must NOT produce a <module> hotspot; got: {:?}",
        hotspots.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
}

/// The TS `<module>` unit's frontend record must have `is_root == false`.
/// The frontend is root-agnostic; the CLI sets the real value for explicit-file
/// entries (cross-file guideline §"Module-init units").
#[test]
fn module_init_record_is_root_false_at_frontend() {
    // An effectful top-level call produces a <module> hotspot + record.
    let src = "fetch('https://api.example.com/data');\n";
    let (_, records) = analyze_source("module_init.ts", src);

    let module_rec = records
        .iter()
        .find(|r| r.symbol == "<module>")
        .expect("<module> record must be emitted");
    assert!(
        !module_rec.is_root,
        "<module> frontend record.is_root must be false (CLI sets the real value)"
    );
}

// ── Finding 3 (Copilot C5): pure fn/class decls not treated as executable ────

/// A module with only an import and a named function (whose body has a `fetch`)
/// must NOT produce a `<module>` hotspot.  The function body is NOT executed at
/// import time — effects belong to `f`, not to the module-init.
#[test]
fn module_init_absent_when_only_fn_decl_has_effects() {
    let src = "import x from './y';\nfunction f() { fetch('z'); }\n";
    let hotspots = analyze_module_src(src);

    assert!(
        !hotspots.iter().any(|h| h.symbol == "<module>"),
        "module with only fn decl must NOT produce a <module> hotspot; got: {:?}",
        hotspots.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
}

// ── I1 fix: bare (non-exported) class decls with static inits ────────────────

/// `class C { static x = fetch("y"); m() { fetch("z"); } }` — a bare (non-exported)
/// class at module level.  Its static field initialiser runs at import time, so a
/// `<module>` hotspot must be emitted carrying exactly that `fetch` (class 7).
/// The method body `m()` must NOT be attributed to `<module>` (own-body isolation).
/// A bare `function f(){}` with an effectful body must NOT produce a `<module>`.
/// A bare `class Empty {}` with no static effects must NOT produce a `<module>`.
#[test]
fn module_init_bare_class_static_init_contributes_to_module() {
    // Bare (non-exported) class with a static field init and an instance method.
    let src = "class C { static x = fetch('y'); m() { fetch('z'); } }\n";
    let hotspots = analyze_module_src(src);

    let module_hs = hotspots
        .iter()
        .find(|h| h.symbol == "<module>")
        .expect("<module> must be emitted for bare class with static fetch");

    // Must carry a net.fs.db effect from the static-field fetch.
    assert!(
        module_hs
            .effects
            .iter()
            .any(|e| e.kind.wire() == "net.fs.db"),
        "<module> must carry net.fs.db from bare-class static-init fetch; effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );

    // Exactly 1 net.fs.db effect — the method fetch must NOT be attributed.
    let net_count = module_hs
        .effects
        .iter()
        .filter(|e| e.kind.wire() == "net.fs.db")
        .count();
    assert_eq!(
        net_count,
        1,
        "<module> must have exactly 1 net.fs.db (static-init only, not method body); \
         effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );
}

/// A bare `function f() { fetch("z"); }` — function body does NOT execute at
/// import time — must NOT produce a `<module>` hotspot.
#[test]
fn module_init_absent_for_bare_fn_decl_with_body_effects() {
    let src = "function f() { fetch('z'); }\n";
    let hotspots = analyze_module_src(src);

    assert!(
        !hotspots.iter().any(|h| h.symbol == "<module>"),
        "bare fn decl must NOT produce a <module> hotspot; got: {:?}",
        hotspots.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
}

/// A bare `class Empty {}` with no static effectful init must NOT produce a
/// `<module>` hotspot (the ≥1-effect guard correctly drops it).
#[test]
fn module_init_absent_for_bare_class_without_static_effects() {
    let src = "class Empty {}\n";
    let hotspots = analyze_module_src(src);

    assert!(
        !hotspots.iter().any(|h| h.symbol == "<module>"),
        "bare class with no static effects must NOT produce a <module> hotspot; got: {:?}",
        hotspots.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
}

// ── Finding 5: exported/default class declarations with static inits ─────────

/// `export class Foo { static client = fetch("y"); method() { fetch("z"); } }`
/// The `<module>` hotspot must capture the `fetch` in the static initialiser but
/// NOT the `fetch` inside `method()` (own-body isolation).
#[test]
fn module_init_includes_exported_class_static_init() {
    let src = "export class Foo {\n\
                 static client = fetch('y');\n\
                 method() { fetch('z'); }\n\
               }\n";
    let hotspots = analyze_module_src(src);

    // A <module> hotspot must be emitted (the static initialiser has a fetch).
    let module_hs = hotspots
        .iter()
        .find(|h| h.symbol == "<module>")
        .expect("<module> hotspot must be emitted for exported class with static fetch");

    // Must carry a net.fs.db effect from the static-field fetch.
    assert!(
        module_hs
            .effects
            .iter()
            .any(|e| e.kind.wire() == "net.fs.db"),
        "<module> must carry a net.fs.db effect from static-field fetch; effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );

    // The effect count must equal 1 (only the static-field fetch).
    // method()'s fetch must NOT be attributed to <module> (own-body isolation).
    let net_fs_db_count = module_hs
        .effects
        .iter()
        .filter(|e| e.kind.wire() == "net.fs.db")
        .count();
    assert_eq!(
        net_fs_db_count,
        1,
        "<module> must have exactly 1 net.fs.db effect (static-init only, not method body); \
         effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );
}

/// `export class Foo { method() {} }` — no static effectful init → NO `<module>`.
#[test]
fn module_init_absent_for_exported_class_without_static_effects() {
    let src = "export class Foo { method() { fetch('y'); } }\n";
    let hotspots = analyze_module_src(src);

    assert!(
        !hotspots.iter().any(|h| h.symbol == "<module>"),
        "exported class with NO static effects must NOT produce a <module> hotspot; got: {:?}",
        hotspots.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
}

/// `export default class Bar { static x = fetch("y"); method() { fetch("z"); } }`
/// Same isolation guarantee for the default-exported form.
#[test]
fn module_init_includes_export_default_class_static_init() {
    let src = "export default class Bar {\n\
                 static x = fetch('y');\n\
                 method() { fetch('z'); }\n\
               }\n";
    let hotspots = analyze_module_src(src);

    let module_hs = hotspots
        .iter()
        .find(|h| h.symbol == "<module>")
        .expect("<module> must be emitted for export default class with static fetch");

    assert!(
        module_hs
            .effects
            .iter()
            .any(|e| e.kind.wire() == "net.fs.db"),
        "<module> must carry net.fs.db from static-field fetch; effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );

    let net_fs_db_count = module_hs
        .effects
        .iter()
        .filter(|e| e.kind.wire() == "net.fs.db")
        .count();
    assert_eq!(
        net_fs_db_count,
        1,
        "<module> must have exactly 1 net.fs.db effect (static-init only, not method body); \
         effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );
}

// ── Finding 2 (Codex-A): module-init captures export default <expr> effects ──

/// `export default fetch("y")` — a call expression evaluated at import time —
/// must produce a `<module>` hotspot with a `net.fs.db` effect.
#[test]
fn module_init_captures_export_default_effectful_expr() {
    let src = "export default fetch('https://api.example.com/data');\n";
    let hotspots = analyze_module_src(src);

    let module_hs = hotspots
        .iter()
        .find(|h| h.symbol == "<module>")
        .expect("<module> must be emitted for export default <call expr>");

    assert!(
        module_hs
            .effects
            .iter()
            .any(|e| e.kind.wire() == "net.fs.db"),
        "<module> must carry a net.fs.db effect from `export default fetch(...)`; \
         effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );
}

/// `export default 42` — a pure literal expression — must NOT produce a
/// `<module>` hotspot (the ≥1-effect guard drops it).
#[test]
fn module_init_absent_for_export_default_pure_literal() {
    let src = "export default 42;\n";
    let hotspots = analyze_module_src(src);

    assert!(
        !hotspots.iter().any(|h| h.symbol == "<module>"),
        "pure `export default 42` must NOT produce a <module> hotspot; got: {:?}",
        hotspots.iter().map(|h| &h.symbol).collect::<Vec<_>>()
    );
}
