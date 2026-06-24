// Task 13: insta snapshot test over the dogfood fixture.
//
// Scans `tests/fixtures/dogfood.py` end-to-end through PythonFrontend → Report::build,
// then snapshots the ranked report. Hard assertions encode the spec's guarantees
// independently of the snapshot text so that any regression breaks loudly.
//
// The file is reported under the logical path `"dogfood.py"` (not the real
// `tests/fixtures/dogfood.py`) so the `tests/` segment in the real path does not
// trigger the path-based test-file skip.

use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_core::model::{Report, Scope};
use fxrank_lang_python::PythonFrontend;

fn analyze_dogfood(include_tests: bool) -> Report {
    let path = format!("{}/tests/fixtures/dogfood.py", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect("dogfood.py fixture exists");
    let output = PythonFrontend { include_tests }.analyze(&[SourceFile {
        // Report under a neutral logical path so the `tests/` segment of the
        // real filesystem path does not trigger the path-based skip rule.
        path: "dogfood.py".into(),
        text,
    }]);
    let scope = Scope {
        input: "dogfood.py".into(),
        files: 1,
        // Derive parsed from the frontend output so a future parse regression on
        // the fixture is reflected, rather than hard-coding success.
        parsed: 1usize.saturating_sub(output.diagnostics.iter().filter(|d| !d.parsed).count()),
        functions: output.functions.len(),
        skipped_tests: output.skipped_tests,
        skipped_excluded: 0,
        risk_features: output.module_risks,
        external_reaches: vec![],
    };
    Report::build(scope, output.functions, output.diagnostics, None)
}

/// Build a stable JSON summary of a single hotspot (omits `line`/`id` which
/// depend on fixture line numbers and break on unrelated edits).
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
        })).collect::<Vec<_>>(),
        "risk_features": hs.risk_features.iter().map(|r| serde_json::json!({
            "kind": r.kind.wire(),
            "class": r.class,
        })).collect::<Vec<_>>(),
    })
}

fn find_fn<'a>(report: &'a Report, symbol: &str) -> &'a fxrank_core::model::Hotspot {
    report
        .hotspots
        .iter()
        .find(|h| h.symbol == symbol)
        .unwrap_or_else(|| {
            let syms: Vec<_> = report.hotspots.iter().map(|h| h.symbol.as_str()).collect();
            panic!("no function `{symbol}` in report; found: {syms:?}")
        })
}

// ── Snapshot: whole dogfood report ──────────────────────────────────────────

#[test]
fn snapshot_dogfood_report() {
    // Default (include_tests=false): test_helper is skipped.
    let report = analyze_dogfood(false);

    // ── Hard assertions (spec guarantees) ──────────────────────────────────

    // 1. io_world → net.fs.db class 7, must be ranked first (world effect).
    let io_world = find_fn(&report, "io_world");
    assert_eq!(
        io_world.max_class, 7,
        "io_world must have max_class 7 (net.fs.db from open + requests.get)"
    );
    assert_eq!(
        report.hotspots[0].symbol, "io_world",
        "io_world must be the top-ranked hotspot"
    );

    // 2. typed_local → local.mutation discounted to class 0, own_score 0.0.
    let typed_local = find_fn(&report, "typed_local");
    let lm = typed_local
        .effects
        .iter()
        .find(|e| e.kind.wire() == "local.mutation")
        .expect("typed_local must have a local.mutation effect");
    assert_eq!(
        lm.discounted_to,
        Some(0),
        "typed_local: Full coverage must discount local.mutation to class 0"
    );
    assert_eq!(
        typed_local.own_score, 0.0,
        "typed_local: own_score must be 0.0 (fully discounted)"
    );

    // 3. StateHolder.update → this.mutation (self.value += delta) is NOT discounted.
    //    Python has no Rust-style &mut self ownership model — receiver state
    //    always escapes, so `this.mutation` is never discounted.
    let update = find_fn(&report, "update");
    let tm = update
        .effects
        .iter()
        .find(|e| e.kind.wire() == "this.mutation")
        .expect("update must have a this.mutation effect");
    assert_eq!(
        tm.discounted_to, None,
        "update: this.mutation must NOT be discounted (receiver state escapes)"
    );
    assert_eq!(
        update.max_class, 3,
        "update: max_class must be 3 (this.mutation class)"
    );

    // 4. dynamic → dynamic.code risk present, from eval().
    let dynamic = find_fn(&report, "dynamic");
    assert!(
        dynamic
            .risk_features
            .iter()
            .any(|r| r.kind.wire() == "dynamic.code"),
        "dynamic must carry a dynamic.code risk_feature (from eval)"
    );

    // 5. test_helper must be absent (skipped by default).
    assert!(
        !report.hotspots.iter().any(|h| h.symbol == "test_helper"),
        "test_helper must be skipped when include_tests=false"
    );
    assert!(
        report.scope.skipped_tests >= 1,
        "skipped_tests must be >= 1 when test_helper is skipped"
    );

    // 6. Lambda is present and scores 0.
    let lambda = report
        .hotspots
        .iter()
        .find(|h| h.symbol.starts_with("<lambda@"))
        .expect("transform lambda must appear in the report");
    assert_eq!(lambda.own_score, 0.0, "pure lambda must have own_score 0.0");

    // ── Snapshot the ranked report ──────────────────────────────────────────
    let snapshot = serde_json::json!({
        "hotspots": report.hotspots.iter().map(summarize).collect::<Vec<_>>(),
        "summary": {
            "max_class": report.summary.max_class,
            "own_score": report.summary.own_score,
            "risk_weight": report.summary.risk_weight,
            "confidence": report.summary.confidence,
        },
        "scope": {
            "skipped_tests": report.scope.skipped_tests,
        },
    });
    insta::assert_json_snapshot!("dogfood_report", snapshot);
}

/// The Python `<module>` unit record emitted by the frontend must have
/// `is_root == false` — the frontend is root-agnostic; the CLI sets the real
/// value for explicit-file entries (cross-file guideline §"Module-init units").
#[test]
fn module_init_record_is_root_false_at_frontend() {
    // An effectful top-level call (print) produces a <module> hotspot + record.
    let src = "print('hello')  # import-time side-effect\n";
    let output = PythonFrontend {
        include_tests: false,
    }
    .analyze(&[SourceFile {
        path: "module_init.py".into(),
        text: src.into(),
    }]);

    let module_rec = output
        .records
        .iter()
        .find(|r| r.symbol == "<module>")
        .expect("<module> record must be emitted for effectful top-level Python module");
    assert!(
        !module_rec.is_root,
        "<module> frontend record.is_root must be false (CLI sets the real value)"
    );
}

// ── Fix 2: Python module-init captures top-level class-body import-time effects ─

/// A top-level class with effectful class-level code runs at import time.
/// `class C:\n    DATA = open("y")` must produce a `<module>` hotspot that
/// captures the `open` call (net.fs.db / class 7).
#[test]
fn class_level_open_contributes_to_module_init() {
    let src = "class C:\n    DATA = open('y')\n    def m(self):\n        open('z')\n";
    let output = PythonFrontend {
        include_tests: false,
    }
    .analyze(&[SourceFile {
        path: "class_init.py".into(),
        text: src.into(),
    }]);

    // A `<module>` hotspot must be emitted for the class-level `open`.
    let module_hs = output
        .functions
        .iter()
        .find(|h| h.symbol == "<module>")
        .expect("<module> hotspot must be emitted: class C: DATA = open('y') runs at import");

    // The class-level `open` (net.fs.db, class 7) must appear in `<module>` effects.
    let has_open_effect = module_hs
        .effects
        .iter()
        .any(|e| e.kind.wire().contains("fs") || e.kind.wire().contains("net"));
    assert!(
        has_open_effect,
        "<module> must capture the class-level open('y') net.fs.db effect; \
         effects: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );

    // The METHOD body `open('z')` must NOT appear in `<module>` — it belongs to `m`.
    // Verify by checking `m` unit exists separately (own-body unaffected).
    // The Python frontend uses the bare method name as the symbol (e.g. "m" or "C.m").
    let method_hs = output
        .functions
        .iter()
        .find(|h| h.symbol == "m" || h.symbol.ends_with(".m"));
    assert!(
        method_hs.is_some(),
        "method `m` must have its own hotspot unit (own-body: open('z')); \
         got symbols: {:?}",
        output
            .functions
            .iter()
            .map(|h| &h.symbol)
            .collect::<Vec<_>>()
    );
}

/// A class with ONLY method definitions (no class-level effectful code) must NOT
/// produce a `<module>` hotspot.
#[test]
fn pure_class_no_class_level_effects_no_module_init() {
    let src = "class Pure:\n    def m(self):\n        pass\n";
    let output = PythonFrontend {
        include_tests: false,
    }
    .analyze(&[SourceFile {
        path: "pure_class.py".into(),
        text: src.into(),
    }]);

    let module_hs = output.functions.iter().find(|h| h.symbol == "<module>");
    assert!(
        module_hs.is_none(),
        "<module> must NOT be emitted for a class with no class-level effects; \
         got hotspots: {:?}",
        output
            .functions
            .iter()
            .map(|h| &h.symbol)
            .collect::<Vec<_>>()
    );
}

/// Confirm the method unit itself is still scored independently — no double-count
/// of the method body's effects on `<module>`.
#[test]
fn method_body_effect_stays_on_method_unit_not_module() {
    // Class has a class-level open AND a method with open.
    // `<module>` gets only the class-level effect; `m` gets only its own open.
    let src =
        "class C:\n    X = open('class_level')\n    def m(self):\n        open('method_level')\n";
    let output = PythonFrontend {
        include_tests: false,
    }
    .analyze(&[SourceFile {
        path: "no_double_count.py".into(),
        text: src.into(),
    }]);

    // Exactly one `<module>` hotspot.
    let module_count = output
        .functions
        .iter()
        .filter(|h| h.symbol == "<module>")
        .count();
    assert_eq!(module_count, 1, "exactly one <module> hotspot expected");

    let module_hs = output
        .functions
        .iter()
        .find(|h| h.symbol == "<module>")
        .unwrap();
    // `<module>` must have exactly ONE open-call effect (the class-level one).
    let open_effects: Vec<_> = module_hs
        .effects
        .iter()
        .filter(|e| e.kind.wire().contains("net") || e.kind.wire().contains("fs"))
        .collect();
    assert_eq!(
        open_effects.len(),
        1,
        "<module> must have exactly 1 net.fs.db effect (class-level open only, \
         not the method's open); got: {:?}",
        module_hs
            .effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );
}
