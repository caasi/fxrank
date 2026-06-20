// Task 11: whole-report insta snapshot over worked.ts.
//
// Scans `tests/fixtures/worked.ts` end-to-end through TsFrontend → Report::build,
// then snapshots the ranked report. Hard assertions encode the spec's guarantees
// independently of the snapshot text so that any regression breaks loudly.

use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_core::model::{Report, Scope};
use fxrank_lang_ts::TsFrontend;

fn analyze_worked() -> Report {
    let path = format!("{}/tests/fixtures/worked.ts", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect("worked.ts fixture exists");
    let output = TsFrontend::default().analyze(&[SourceFile {
        path: "worked.ts".into(),
        text,
    }]);
    let scope = Scope {
        input: "worked.ts".into(),
        files: 1,
        parsed: 1,
        functions: output.functions.len(),
        skipped_tests: output.skipped_tests,
        risk_features: output.module_risks,
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

// ── Snapshot: whole-report (ranked) ─────────────────────────────────────────

#[test]
fn snapshot_worked_report() {
    let report = analyze_worked();

    // ── Hard assertions (spec guarantees) ──────────────────────────────────

    // 1. loadUser → net.fs.db class 7, async_boundary true, must be ranked first.
    let load_user = find_fn(&report, "loadUser");
    assert_eq!(
        load_user.max_class, 7,
        "loadUser must have max_class 7 (net.fs.db from fetch)"
    );
    assert!(
        load_user.async_boundary,
        "loadUser must have async_boundary true"
    );
    assert!(
        load_user.await_count >= 1,
        "loadUser must have await_count >= 1"
    );
    // loadUser must be first in the ranked list.
    assert_eq!(
        report.hotspots[0].symbol, "loadUser",
        "loadUser must be the top-ranked hotspot"
    );

    // 2. buildTyped → local.mutation discounted to class 0, own_score 0.0.
    let build_typed = find_fn(&report, "buildTyped");
    let bt_lm = build_typed
        .effects
        .iter()
        .find(|e| e.kind.wire() == "local.mutation")
        .expect("buildTyped must have a local.mutation effect");
    assert_eq!(
        bt_lm.discounted_to,
        Some(0),
        "buildTyped: Full coverage must discount local.mutation to class 0"
    );
    assert_eq!(
        bt_lm.effective_class(),
        0,
        "buildTyped: effective_class must be 0"
    );
    assert_eq!(
        bt_lm.weight, 0,
        "buildTyped: local.mutation weight must be 0 (class 0)"
    );
    assert_eq!(
        build_typed.own_score, 0.0,
        "buildTyped: own_score must be 0.0 (fully discounted)"
    );

    // 3. buildUntyped → local.mutation class 1, own_score 1.0 (no discount).
    let build_untyped = find_fn(&report, "buildUntyped");
    let bu_lm = build_untyped
        .effects
        .iter()
        .find(|e| e.kind.wire() == "local.mutation")
        .expect("buildUntyped must have a local.mutation effect");
    assert_eq!(
        bu_lm.discounted_to, None,
        "buildUntyped: no typing → no discount"
    );
    assert_eq!(
        bu_lm.effective_class(),
        1,
        "buildUntyped: local.mutation effective_class must be 1"
    );
    assert_eq!(
        build_untyped.own_score, 1.0,
        "buildUntyped: own_score must be 1.0 (uncontained local.mutation)"
    );

    // 4. risky → type.escape risk, discount voided.
    let risky = find_fn(&report, "risky");
    assert!(
        risky
            .risk_features
            .iter()
            .any(|r| r.kind.wire() == "type.escape"),
        "risky must carry a type.escape risk (from `as any`)"
    );
    // `as any` poisons coverage → discount voided on any local mutation.
    // (risky has no mutation, but type.escape must be present.)

    // ── Snapshot the ranked report ──────────────────────────────────────────
    let snapshot = serde_json::json!({
        "hotspots": report.hotspots.iter().map(summarize).collect::<Vec<_>>(),
        "summary": {
            "max_class": report.summary.max_class,
            "own_score": report.summary.own_score,
            "risk_weight": report.summary.risk_weight,
            "confidence": report.summary.confidence,
        },
    });
    insta::assert_json_snapshot!("worked_report", snapshot);
}
