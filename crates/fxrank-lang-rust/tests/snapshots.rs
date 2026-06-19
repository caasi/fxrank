// Task 18: insta snapshot tests over the spec's worked cases.
// Each test snapshots a key field summary for one or more flagship functions
// from tests/fixtures/worked_cases.rs, plus plain assertions that encode
// the spec's headline guarantees independently of the snapshot text.

use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_core::model::Hotspot;
use fxrank_lang_rust::RustFrontend;

fn analyze_worked_cases() -> fxrank_core::frontend::FrontendOutput {
    let path = format!(
        "{}/tests/fixtures/worked_cases.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("worked_cases.rs fixture exists");
    RustFrontend::default().analyze(&[SourceFile {
        path: "worked_cases.rs".into(),
        text,
    }])
}

/// Build a JSON summary of a hotspot — stable fields only (omit `line`/`id`
/// which depend on fixture line numbers and break on unrelated edits).
fn summarize(hs: &Hotspot) -> serde_json::Value {
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

fn find_fn<'a>(out: &'a fxrank_core::frontend::FrontendOutput, symbol: &str) -> &'a Hotspot {
    out.functions
        .iter()
        .find(|f| f.symbol == symbol)
        .unwrap_or_else(|| {
            let syms: Vec<_> = out.functions.iter().map(|f| f.symbol.as_str()).collect();
            panic!("no function `{symbol}` in output; found: {syms:?}")
        })
}

// ── Snapshot: save_user ──────────────────────────────────────────────────────

#[test]
fn snapshot_save_user() {
    let out = analyze_worked_cases();
    let hs = find_fn(&out, "save_user");

    // Spec guarantee: save_user must have a param.mutation effect discounted to class 1.
    // save_user(user: &mut User, …) mutates an explicit &mut parameter, so the MutParam
    // discount applies: class 3 down by 2 → discounted_to 1.  (MutSelf would be down 1 → 2.)
    let param_mut = hs
        .effects
        .iter()
        .find(|e| e.kind.wire() == "param.mutation");
    let e = param_mut.expect("save_user must have a param.mutation effect");
    assert_eq!(
        e.discounted_to,
        Some(1),
        "save_user param.mutation must be discounted to class 1, got: {:?}",
        e.discounted_to
    );
    assert_eq!(
        e.class, 3,
        "save_user param.mutation base class must be 3, got: {}",
        e.class
    );

    let summary = summarize(hs);
    insta::assert_json_snapshot!("save_user", summary);
}

// ── Snapshot: logging_soup vs one_io ────────────────────────────────────────

#[test]
fn snapshot_logging_soup_and_one_io() {
    let out = analyze_worked_cases();
    let soup = find_fn(&out, "logging_soup");
    let io = find_fn(&out, "one_io");

    // Spec guarantee: one_io (single fs.write, max_class 7) outranks logging_soup
    // (log::info!/println!, max_class 4).
    assert!(
        io.max_class > soup.max_class,
        "one_io.max_class ({}) must be > logging_soup.max_class ({})",
        io.max_class,
        soup.max_class
    );

    let summary = serde_json::json!({
        "logging_soup": summarize(soup),
        "one_io": summarize(io),
    });
    insta::assert_json_snapshot!("logging_soup_and_one_io", summary);
}

// ── Snapshot: inversion pair (Store::set_name vs Store::set) ────────────────

#[test]
fn snapshot_inversion_pair() {
    let out = analyze_worked_cases();
    let declared = find_fn(&out, "Store::set_name");
    let hidden = find_fn(&out, "Store::set");

    // Spec guarantee: hidden interior-mutation must score strictly higher than
    // declared &mut self mutation (the anti-Goodhart inversion).
    assert!(
        hidden.own_score > declared.own_score,
        "Store::set (hidden, own_score={}) must outrank Store::set_name (declared, own_score={})",
        hidden.own_score,
        declared.own_score
    );

    let summary = serde_json::json!({
        "declared_set_name": summarize(declared),
        "hidden_set": summarize(hidden),
    });
    insta::assert_json_snapshot!("inversion_pair", summary);
}

// ── Snapshot: pure_total ─────────────────────────────────────────────────────

#[test]
fn snapshot_pure_total() {
    let out = analyze_worked_cases();
    let hs = find_fn(&out, "pure_total");

    // Spec guarantee: no effects, own_score == 0.0.
    assert!(
        hs.effects.is_empty(),
        "pure_total must have no effects, got: {:?}",
        hs.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
    );
    assert!(
        (hs.own_score - 0.0).abs() < 1e-9,
        "pure_total own_score must be 0.0, got: {}",
        hs.own_score
    );

    insta::assert_json_snapshot!("pure_total", summarize(hs));
}

// ── Snapshot: risk_only ──────────────────────────────────────────────────────

#[test]
fn snapshot_risk_only() {
    let out = analyze_worked_cases();
    let hs = find_fn(&out, "risk_only");

    // Spec guarantee: risk_only has max_class == 4 (MemForget risk class), not 0.
    assert_eq!(
        hs.max_class, 4,
        "risk_only must have max_class 4 from MemForget risk, got: {}",
        hs.max_class
    );

    insta::assert_json_snapshot!("risk_only", summarize(hs));
}

// ── Snapshot: fallible ───────────────────────────────────────────────────────

#[test]
fn snapshot_fallible() {
    let out = analyze_worked_cases();
    let hs = find_fn(&out, "fallible");

    // Spec guarantee: `?` operator must NOT be scored as a panic/effect.
    let panic_effects: Vec<_> = hs
        .effects
        .iter()
        .filter(|e| e.kind.wire() == "panic")
        .collect();
    assert!(
        panic_effects.is_empty(),
        "fallible() must emit no panic effects from `?`, got: {:?}",
        panic_effects
            .iter()
            .map(|e| e.evidence.as_str())
            .collect::<Vec<_>>()
    );

    insta::assert_json_snapshot!("fallible", summarize(hs));
}

// ── Snapshot: async shell ────────────────────────────────────────────────────

#[test]
fn snapshot_async_shell() {
    let out = analyze_worked_cases();
    let hs = find_fn(&out, "shell");

    // Spec: async fn with .await → async_boundary == true, await_count >= 1.
    assert!(
        hs.async_boundary,
        "shell() must have async_boundary == true"
    );
    assert!(
        hs.await_count >= 1,
        "shell() must have await_count >= 1, got: {}",
        hs.await_count
    );

    insta::assert_json_snapshot!("async_shell", summarize(hs));
}

// ── Snapshot: unsafe_cancel ──────────────────────────────────────────────────

#[test]
fn snapshot_unsafe_cancel() {
    let out = analyze_worked_cases();
    let hs = find_fn(&out, "unsafe_cancel");

    // Spec: write inside unsafe{} cancels the containment discount — effective
    // class stays at 3 (not discounted to 1).
    let param_mut = hs
        .effects
        .iter()
        .find(|e| e.kind.wire() == "param.mutation");
    let e = param_mut.expect("unsafe_cancel must have a param.mutation effect");
    assert_eq!(
        e.effective_class(),
        3,
        "param.mutation inside unsafe must have effective_class 3 (discount cancelled), got: {}",
        e.effective_class()
    );

    insta::assert_json_snapshot!("unsafe_cancel", summarize(hs));
}
