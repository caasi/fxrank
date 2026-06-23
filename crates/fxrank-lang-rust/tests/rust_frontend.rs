use fxrank_core::effect::{Effect, EffectKind, RiskKind, Tier};
use fxrank_core::frontend::{Frontend, FrontendOutput, SourceFile};
use fxrank_lang_rust::RustFrontend;

fn source_of(name: &str) -> SourceFile {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    let text = std::fs::read_to_string(&path).expect("fixture exists");
    SourceFile {
        path: name.into(),
        text,
    }
}

fn analyze_fixture(name: &str) -> fxrank_core::frontend::FrontendOutput {
    RustFrontend::default().analyze(&[source_of(name)])
}

/// Pull the effects of a single fixture function by its symbol.
fn effects_of(out: &fxrank_core::frontend::FrontendOutput, symbol: &str) -> Vec<Effect> {
    out.functions
        .iter()
        .find(|f| f.symbol == symbol)
        .unwrap_or_else(|| panic!("no function `{symbol}` in output"))
        .effects
        .clone()
}

/// Find the single effect of a given wire-kind among `effects`.
fn one_kind<'a>(effects: &'a [Effect], wire: &str) -> &'a Effect {
    let matches: Vec<&Effect> = effects.iter().filter(|e| e.kind.wire() == wire).collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one `{wire}` effect, got {:?}",
        effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
    );
    matches[0]
}

#[test]
fn collects_expected_function_units() {
    let out = analyze_fixture("functions.rs");
    let syms: Vec<_> = out.functions.iter().map(|f| f.symbol.clone()).collect();
    assert!(syms.contains(&"free_fn".to_string()));
    assert!(syms.contains(&"S::method".to_string()));
    assert!(syms.contains(&"T::defaulted".to_string())); // trait default BODY is a unit
    assert!(syms.contains(&"<S as T>::required".to_string())); // impl method
    assert!(!syms.contains(&"T::required".to_string())); // bodyless sig is NOT a unit
}

#[test]
fn fs_write_is_net_fs_db_class_7_path() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "fs_write");
    let e = one_kind(&effects, "net.fs.db");
    assert_eq!(e.class, 7);
    assert_eq!(e.tier, Tier::Path);
    assert!(e.evidence.contains("write"), "evidence: {}", e.evidence);
}

#[test]
fn instant_now_is_time_read_class_5_path() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "time_read");
    // Both `Instant::now()` and `SystemTime::now()` resolve to time.read.
    let times: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "time.read")
        .collect();
    assert_eq!(times.len(), 2, "expected two time.read effects");
    for e in times {
        assert_eq!(e.class, 5);
        assert_eq!(e.tier, Tier::Path);
        assert!(e.evidence.contains("now"), "evidence: {}", e.evidence);
    }
}

#[test]
fn env_read_class_4_and_env_write_class_6() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "env_calls");
    let read = one_kind(&effects, "env.read");
    assert_eq!(read.class, 4);
    assert_eq!(read.tier, Tier::Path);
    let write = one_kind(&effects, "env.write");
    assert_eq!(write.class, 6);
    assert_eq!(write.tier, Tier::Path);
}

#[test]
fn process_exit_is_process_control() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "process_exit");
    let e = one_kind(&effects, "process.control");
    assert_eq!(e.tier, Tier::Path);
    assert!(e.evidence.contains("exit"), "evidence: {}", e.evidence);
}

#[test]
fn command_spawn_emits_one_process_control_no_constructor_effect() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "command_spawn");
    // Exactly one process.control (from `.spawn`); `Command::new` yields nothing.
    let e = one_kind(&effects, "process.control");
    assert_eq!(e.tier, Tier::Heuristic);
    assert_eq!(e.evidence, ".spawn");
    assert_eq!(effects.len(), 1, "Command::new must not add an effect");
}

#[test]
fn channel_send_is_concurrency_heuristic() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "channel_send");
    let e = one_kind(&effects, "concurrency");
    assert_eq!(e.tier, Tier::Heuristic);
    assert_eq!(e.evidence, ".send");
}

#[test]
fn unwrap_is_panic_class_4_heuristic() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "unwraps");
    let e = one_kind(&effects, "panic");
    assert_eq!(e.class, 4);
    assert_eq!(e.tier, Tier::Heuristic);
    assert_eq!(e.evidence, ".unwrap");
}

#[test]
fn thread_spawn_concurrency_and_random_path() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "spawn_and_random");
    let conc = one_kind(&effects, "concurrency");
    assert_eq!(conc.tier, Tier::Path);
    let rng = one_kind(&effects, "random");
    assert_eq!(rng.class, 5);
    assert_eq!(rng.tier, Tier::Path);
}

#[test]
fn write_all_net_fs_db_and_load_ambient_read_heuristic() {
    let out = analyze_fixture("calls.rs");
    let effects = effects_of(&out, "io_and_atomic");
    let io = one_kind(&effects, "net.fs.db");
    assert_eq!(io.tier, Tier::Heuristic);
    assert_eq!(io.evidence, ".write_all");
    let amb = one_kind(&effects, "ambient.read");
    assert_eq!(amb.tier, Tier::Heuristic);
    assert_eq!(amb.evidence, ".load");
}

#[test]
fn pure_function_has_no_effects() {
    let out = analyze_fixture("calls.rs");
    assert!(effects_of(&out, "pure").is_empty());
}

// ── Task 12: macro effect detection ─────────────────────────────────────────

#[test]
fn println_is_logging_class_4_exact() {
    let out = analyze_fixture("macros.rs");
    // logging_exact uses println!, eprintln!, print!, eprint!, dbg! — all logging.
    let effects = effects_of(&out, "logging_exact");
    let logging: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "logging")
        .collect();
    assert!(
        !logging.is_empty(),
        "expected at least one logging effect in logging_exact"
    );
    for e in &logging {
        assert_eq!(e.class, 4, "logging class must be 4");
        assert_eq!(
            e.tier,
            Tier::Exact,
            "exact-ident logging must be Tier::Exact"
        );
    }
    // No unknown.macro for whitelisted/known macros.
    assert!(
        effects.iter().all(|e| e.kind != EffectKind::UnknownMacro),
        "known logging macros must not produce unknown.macro"
    );
}

#[test]
fn log_and_tracing_are_logging_path_tier_not_unknown() {
    let out = analyze_fixture("macros.rs");
    let effects = effects_of(&out, "logging_qualified");
    // log::info! and tracing::warn! → two logging effects, Tier::Path.
    let logging: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "logging")
        .collect();
    assert_eq!(
        logging.len(),
        2,
        "expected two logging effects (log::info + tracing::warn)"
    );
    for e in &logging {
        assert_eq!(e.class, 4);
        assert_eq!(
            e.tier,
            Tier::Path,
            "qualified log/tracing paths must be Tier::Path"
        );
    }
    // Critical: must NOT also emit unknown.macro.
    assert!(
        effects.iter().all(|e| e.kind != EffectKind::UnknownMacro),
        "log:: and tracing:: macros must not produce unknown.macro"
    );
}

#[test]
fn panic_and_assert_are_panic_class_4_exact() {
    let out = analyze_fixture("macros.rs");
    let effects = effects_of(&out, "panic_macros");
    let panics: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "panic")
        .collect();
    assert!(!panics.is_empty(), "expected panic effects in panic_macros");
    for e in &panics {
        assert_eq!(e.class, 4, "panic class must be 4");
        assert_eq!(e.tier, Tier::Exact);
    }
    assert!(
        effects.iter().all(|e| e.kind != EffectKind::UnknownMacro),
        "panic macros must not produce unknown.macro"
    );
}

#[test]
fn whitelisted_macros_emit_no_effects() {
    let out = analyze_fixture("macros.rs");
    // whitelisted() uses vec!, format!, matches!, concat!, stringify! — no effects.
    // It also calls .unwrap() via write_macros indirectly... but whitelisted() is
    // its own function and should be clean.
    let effects = effects_of(&out, "whitelisted");
    let macro_effects: Vec<&Effect> = effects
        .iter()
        .filter(|e| {
            matches!(
                e.kind.wire(),
                "logging" | "panic" | "net.fs.db" | "unknown.macro"
            )
        })
        .collect();
    assert!(
        macro_effects.is_empty(),
        "whitelisted macros must emit no macro effects, got: {:?}",
        macro_effects
            .iter()
            .map(|e| e.kind.wire())
            .collect::<Vec<_>>()
    );
}

#[test]
fn write_and_writeln_are_net_fs_db_class_7_heuristic() {
    let out = analyze_fixture("macros.rs");
    let effects = effects_of(&out, "write_macros");
    let io: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "net.fs.db")
        .collect();
    assert!(
        io.len() >= 2,
        "expected at least two net.fs.db effects (write! + writeln!), got: {}",
        io.len()
    );
    for e in &io {
        assert_eq!(e.class, 7, "write!/writeln! must map to net.fs.db class 7");
        assert_eq!(
            e.tier,
            Tier::Heuristic,
            "write!/writeln! must be Tier::Heuristic"
        );
    }
    assert!(
        effects.iter().all(|e| e.kind != EffectKind::UnknownMacro),
        "write!/writeln! must not produce unknown.macro"
    );
}

#[test]
fn unknown_macro_is_class_2_with_confidence_0_4() {
    let out = analyze_fixture("macros.rs");
    // unknown_macro_only() contains exactly one invocation: my_macro!()
    let effects = effects_of(&out, "unknown_macro_only");
    let unknowns: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "unknown.macro")
        .collect();
    assert_eq!(
        unknowns.len(),
        1,
        "expected exactly one unknown.macro effect, got {:?}",
        effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
    );
    let e = unknowns[0];
    assert_eq!(e.class, 2, "unknown.macro must be class 2");
    assert!(
        e.evidence.contains("my_macro"),
        "evidence must contain the macro name, got: {}",
        e.evidence
    );
    assert!(
        e.evidence.ends_with('!'),
        "evidence must end with '!', got: {}",
        e.evidence
    );
    // The hotspot's function confidence must be 0.4 (min of [0.4]).
    let hotspot = out
        .functions
        .iter()
        .find(|f| f.symbol == "unknown_macro_only")
        .expect("unknown_macro_only not found");
    assert!(
        (hotspot.confidence - 0.4).abs() < 1e-9,
        "function confidence must be 0.4 for a single unknown.macro, got: {}",
        hotspot.confidence
    );
}

// ── Task 13a: param.mutation + containment discount ─────────────────────────

#[test]
fn mut_param_method_write_is_param_mutation_discounted_to_1() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "fill");
    let e = one_kind(&effects, "param.mutation");
    assert_eq!(e.class, 3, "param.mutation base class is 3");
    assert_eq!(
        e.discounted_to,
        Some(1),
        "MutParam discounts class 3 down by 2 → 1"
    );
    assert_eq!(e.tier, Tier::Heuristic, "write-through is heuristic");
    assert!(!e.hidden, "declared &mut is not hidden");
    assert!(e.discount.is_some(), "must record a discount reason");
}

#[test]
fn mut_self_field_assign_is_param_mutation_discounted_to_2() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "User::set_name");
    let e = one_kind(&effects, "param.mutation");
    assert_eq!(e.class, 3);
    assert_eq!(
        e.discounted_to,
        Some(2),
        "MutSelf discounts class 3 down by 1 → 2"
    );
    assert_eq!(e.tier, Tier::Heuristic);
}

#[test]
fn save_discounts_mutation_but_keeps_net_fs_db_class_7() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "save");
    let mutation = one_kind(&effects, "param.mutation");
    assert_eq!(mutation.class, 3);
    assert_eq!(
        mutation.discounted_to,
        Some(1),
        "the &mut param mutation discounts to 1"
    );
    let io = one_kind(&effects, "net.fs.db");
    assert_eq!(io.class, 7, "the IO effect is untouched by the discount");
    assert_eq!(io.discounted_to, None);
}

// ── Task 13b: hidden.mutation via shared-ref interior mutation ───────────────

#[test]
fn self_interior_mutation_is_hidden_mutation_no_discount() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "User::set");
    let e = one_kind(&effects, "hidden.mutation");
    assert_eq!(e.class, 3, "hidden.mutation is class 3");
    assert_eq!(e.discounted_to, None, "hidden mutation is never discounted");
    assert!(e.hidden, "interior mutation through &self is hidden");
    assert_eq!(e.tier, Tier::Heuristic);
    assert_eq!(
        e.subreason.as_deref(),
        Some("interior-mut"),
        "interior-mutability hidden write carries subreason interior-mut"
    );
}

#[test]
fn shared_ref_interior_mutation_is_hidden_mutation() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "bump");
    let e = one_kind(&effects, "hidden.mutation");
    assert_eq!(e.class, 3);
    assert!(e.hidden);
}

/// THE anti-Goodhart inversion: hidden &self interior mutation must score
/// strictly higher than the equivalent declared &mut self mutation.
#[test]
fn hidden_mutation_scores_higher_than_declared_mut_self() {
    use fxrank_core::score::own_score;
    let out = analyze_fixture("mutation.rs");
    let hidden = effects_of(&out, "User::set"); // &self + borrow_mut
    let declared = effects_of(&out, "User::set_name"); // &mut self

    let hidden_score = own_score(&hidden.iter().map(|e| e.weight).collect::<Vec<_>>());
    let declared_score = own_score(&declared.iter().map(|e| e.weight).collect::<Vec<_>>());
    assert!(
        hidden_score > declared_score,
        "hidden ({hidden_score}) must outrank declared ({declared_score})"
    );
}

// ── Task 13c: local.mutation write-site detection ───────────────────────────

#[test]
fn let_mut_writes_are_two_local_mutations_class_1_exact() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "locals");
    let locals: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "local.mutation")
        .collect();
    assert_eq!(
        locals.len(),
        2,
        "two write sites (`x += 1` and `x = 2`), declaration is not a write"
    );
    for e in &locals {
        assert_eq!(e.class, 1, "local.mutation is class 1");
        assert_eq!(e.tier, Tier::Exact, "local writes are exact");
        assert_eq!(e.discounted_to, None);
        assert!(!e.hidden);
    }
}

// ── Task 13d: global.mutation detection (class 6) ───────────────────────────

// ── Task R2 (F2): real-static write → global.mutation (class 6) ───────────────
/// A *lowercase* `static mut` written by direct assignment is global.mutation/6
/// (casing-independent; the old proxy rejected the lowercase base).
#[test]
fn lowercase_static_mut_assign_is_global_mutation_class_6() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "write_lower_static_mut");
    let e = one_kind(&effects, "global.mutation");
    assert_eq!(
        e.class, 6,
        "global.mutation is class 6 (no class-4 downgrade)"
    );
    assert_eq!(e.tier, Tier::Heuristic, "static write-through is heuristic");
    assert_eq!(e.discounted_to, None, "global.mutation is never discounted");
    assert!(!e.hidden, "a global static write is not hidden");
}

/// An interior-mutable plain `static` written via `.store()` is global.mutation/6
/// (the interior-mutator emission site: a static base, not a shared_refs member).
#[test]
fn atomic_static_store_is_global_mutation_class_6() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "store_atomic_static");
    let e = one_kind(&effects, "global.mutation");
    assert_eq!(e.class, 6);
    assert_eq!(e.tier, Tier::Heuristic);
    assert_eq!(e.discounted_to, None);
    assert!(
        effects.iter().all(|e| e.kind.wire() != "hidden.mutation"),
        "atomic static .store() is global, not hidden"
    );
}

/// An UPPERCASE ident bound nowhere and NOT a static must NOT be global.mutation
/// (the proxy-retirement discriminator: the old casing heuristic flagged it).
#[test]
fn unbound_uppercase_non_static_is_not_global_mutation() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "write_unbound_upper");
    assert!(
        effects.iter().all(|e| e.kind.wire() != "global.mutation"),
        "an UPPERCASE non-static base must not be global.mutation, got: {:?}",
        effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
    );
}

/// Regression (anti-Goodhart): a `&self` interior mutation stays hidden.mutation,
/// NOT global.mutation, after the static rewiring (`self` is in shared_refs,
/// checked first). Uses the existing interior-mut fixture/symbol.
#[test]
fn self_interior_mutation_stays_hidden_not_global() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "User::set");
    assert!(
        effects.iter().any(|e| e.kind.wire() == "hidden.mutation"),
        "User::set must still emit hidden.mutation"
    );
    assert!(
        effects.iter().all(|e| e.kind.wire() != "global.mutation"),
        "User::set must NOT emit global.mutation (shared_refs checked before statics)"
    );
}

// ── Task 13e: lexical unsafe-cancel ─────────────────────────────────────────

#[test]
fn mut_write_inside_unsafe_cancels_discount_stays_class_3() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "w_unsafe");
    let e = one_kind(&effects, "param.mutation");
    assert_eq!(e.class, 3);
    assert_eq!(
        e.effective_class(),
        3,
        "discount cancelled under unsafe → effective class 3"
    );
    assert_eq!(
        e.discounted_to,
        Some(3),
        "discounted_to set consistently to the (cancelled) result"
    );
}

#[test]
fn mut_write_outside_unsafe_keeps_discount_to_1() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "w_outside");
    let e = one_kind(&effects, "param.mutation");
    assert_eq!(e.class, 3);
    assert_eq!(
        e.discounted_to,
        Some(1),
        "write is outside the unsafe block → discount still applies"
    );
}

// ── Task 14: risk_features detection ─────────────────────────────────────────

/// Helper: pull risk_features for a single fixture function by symbol.
fn risks_of(out: &FrontendOutput, symbol: &str) -> Vec<fxrank_core::effect::RiskFeature> {
    out.functions
        .iter()
        .find(|f| f.symbol == symbol)
        .unwrap_or_else(|| panic!("no function `{symbol}` in output"))
        .risk_features
        .clone()
}

/// Helper: find the single RiskFeature of a given RiskKind among `features`.
fn one_risk(
    features: &[fxrank_core::effect::RiskFeature],
    kind: RiskKind,
) -> &fxrank_core::effect::RiskFeature {
    let matches: Vec<_> = features.iter().filter(|r| r.kind == kind).collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one {:?} risk feature, got kinds: {:?}",
        kind,
        features.iter().map(|r| r.kind).collect::<Vec<_>>()
    );
    matches[0]
}

/// Helper: find the hotspot for a symbol.
fn hotspot_of(out: &FrontendOutput, symbol: &str) -> fxrank_core::model::Hotspot {
    out.functions
        .iter()
        .find(|f| f.symbol == symbol)
        .unwrap_or_else(|| panic!("no function `{symbol}` in output"))
        .clone()
}

#[test]
fn unsafe_block_is_risk_class_5() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "unsafe_block_example");
    let r = one_risk(&risks, RiskKind::UnsafeBlock);
    assert_eq!(r.class, 5, "UnsafeBlock must be class 5");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn unsafe_fn_is_risk_class_5() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "unsafe_fn_example");
    let r = one_risk(&risks, RiskKind::UnsafeFn);
    assert_eq!(r.class, 5, "UnsafeFn must be class 5");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn transmute_is_risk_class_7() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "transmute_example");
    let r = one_risk(&risks, RiskKind::Transmute);
    assert_eq!(r.class, 7, "Transmute must be class 7");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn raw_ptr_deref_inside_unsafe_is_class_7_heuristic() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "raw_ptr_deref_example");
    let r = one_risk(&risks, RiskKind::RawPtrDeref);
    assert_eq!(r.class, 7, "RawPtrDeref must be class 7");
    assert_eq!(
        r.tier,
        Tier::Heuristic,
        "raw deref approximation is Heuristic"
    );
}

#[test]
fn get_unchecked_is_class_7_heuristic() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "get_unchecked_example");
    let r = one_risk(&risks, RiskKind::GetUnchecked);
    assert_eq!(r.class, 7, "GetUnchecked must be class 7");
    assert_eq!(r.tier, Tier::Heuristic);
}

#[test]
fn maybe_uninit_is_class_7() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "maybe_uninit_example");
    let r = one_risk(&risks, RiskKind::MaybeUninit);
    assert_eq!(r.class, 7, "MaybeUninit must be class 7");
}

#[test]
fn box_leak_is_class_4() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "box_leak_example");
    let r = one_risk(&risks, RiskKind::BoxLeak);
    assert_eq!(r.class, 4, "BoxLeak must be class 4");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn mem_forget_is_class_4() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "mem_forget_example");
    let r = one_risk(&risks, RiskKind::MemForget);
    assert_eq!(r.class, 4, "MemForget must be class 4");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn manually_drop_is_class_4() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "manually_drop_example");
    let r = one_risk(&risks, RiskKind::ManuallyDrop);
    assert_eq!(r.class, 4, "ManuallyDrop must be class 4");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn asm_macro_is_class_7_exact() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "asm_example");
    let r = one_risk(&risks, RiskKind::Asm);
    assert_eq!(r.class, 7, "Asm must be class 7");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn write_volatile_is_raw_ptr_op_class_7_exact() {
    // std::ptr::write_volatile is a genuine volatile raw-memory write.
    // It is classified as RawPtrOp (class 7, Tier::Exact) — the same kind
    // as ptr::read/write/copy_nonoverlapping.
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "write_volatile_example");
    // The unsafe block in write_volatile_example also produces UnsafeBlock — we want
    // exactly the RawPtrOp entry.
    let raw_ptr_ops: Vec<_> = risks
        .iter()
        .filter(|r| r.kind == RiskKind::RawPtrOp)
        .collect();
    assert_eq!(
        raw_ptr_ops.len(),
        1,
        "expected exactly one RawPtrOp risk from std::ptr::write_volatile, got kinds: {:?}",
        risks.iter().map(|r| r.kind).collect::<Vec<_>>()
    );
    let r = raw_ptr_ops[0];
    assert_eq!(r.class, 7, "RawPtrOp must be class 7");
    assert_eq!(
        r.tier,
        Tier::Exact,
        "std::ptr::write_volatile is Tier::Exact"
    );
    assert!(
        r.evidence.contains("write_volatile"),
        "evidence must name the call, got: {}",
        r.evidence
    );
}

#[test]
fn module_risks_contain_impl_drop_and_extern_block() {
    let out = analyze_fixture("risk.rs");

    let impl_drops: Vec<_> = out
        .module_risks
        .iter()
        .filter(|r| r.kind == RiskKind::ImplDrop)
        .collect();
    assert_eq!(impl_drops.len(), 1, "expected one ImplDrop module risk");
    let drop_risk = impl_drops[0];
    assert_eq!(drop_risk.class, 2, "ImplDrop must be class 2");
    assert_eq!(drop_risk.tier, Tier::Exact);
    assert!(!drop_risk.path.is_empty(), "path must be set");

    let extern_blocks: Vec<_> = out
        .module_risks
        .iter()
        .filter(|r| r.kind == RiskKind::ExternBlock)
        .collect();
    assert_eq!(
        extern_blocks.len(),
        1,
        "expected one ExternBlock module risk"
    );
    let ext_risk = extern_blocks[0];
    assert_eq!(ext_risk.class, 2, "ExternBlock must be class 2");
    assert_eq!(ext_risk.tier, Tier::Exact);
    assert!(!ext_risk.path.is_empty(), "path must be set");
}

#[test]
fn module_risks_are_not_attached_to_any_function() {
    // ImplDrop and ExternBlock must NOT appear in any function's risk_features.
    let out = analyze_fixture("risk.rs");
    for hotspot in &out.functions {
        let bad: Vec<_> = hotspot
            .risk_features
            .iter()
            .filter(|r| matches!(r.kind, RiskKind::ImplDrop | RiskKind::ExternBlock))
            .collect();
        assert!(
            bad.is_empty(),
            "function `{}` must not carry ImplDrop/ExternBlock risk features: {:?}",
            hotspot.symbol,
            bad.iter().map(|r| r.kind).collect::<Vec<_>>()
        );
    }
}

#[test]
fn risk_feeds_ranking_forget_only_max_class_4_weight_5() {
    // A function whose only signal is std::mem::forget → MemForget (class 4).
    // max_class must be 4 (from risk_class, no effects), risk_weight must be
    // weight_for_class(4) == 5.
    let out = analyze_fixture("risk.rs");
    let hotspot = hotspot_of(&out, "forget_only");
    assert!(
        hotspot.effects.is_empty(),
        "forget_only must have no Effect entries"
    );
    assert_eq!(hotspot.max_class, 4, "risk_class 4 must feed max_class");
    assert_eq!(
        hotspot.risk_weight, 5,
        "weight_for_class(4) == 5 must be risk_weight"
    );
}

#[test]
fn env_write_unsafe_combo_has_both_effect_and_risk() {
    // unsafe { std::env::set_var("K","v"); }
    // → one env.write effect (class 6) AND one UnsafeBlock risk feature (class 5).
    let out = analyze_fixture("risk.rs");
    let effects = effects_of(&out, "env_write_unsafe_combo");
    let risks = risks_of(&out, "env_write_unsafe_combo");

    let env_write = one_kind(&effects, "env.write");
    assert_eq!(env_write.class, 6, "env.write must be class 6");

    let unsafe_block = one_risk(&risks, RiskKind::UnsafeBlock);
    assert_eq!(unsafe_block.class, 5, "UnsafeBlock must be class 5");
}

// ── Task 15a: async_boundary + await_count ───────────────────────────────────

#[test]
fn async_fn_with_two_awaits_has_boundary_true_and_count_two() {
    let out = analyze_fixture("async.rs");
    let hs = hotspot_of(&out, "two_awaits");
    assert!(hs.async_boundary, "async fn must set async_boundary = true");
    assert_eq!(hs.await_count, 2, "two .await sites → await_count 2");
}

#[test]
fn async_fn_no_await_has_boundary_true_count_zero() {
    let out = analyze_fixture("async.rs");
    let hs = hotspot_of(&out, "async_no_await");
    assert!(
        hs.async_boundary,
        "async fn with no await: async_boundary still true"
    );
    assert_eq!(hs.await_count, 0);
}

#[test]
fn sync_fn_no_await_has_boundary_false_count_zero() {
    let out = analyze_fixture("async.rs");
    let hs = hotspot_of(&out, "sync_no_await");
    assert!(!hs.async_boundary, "sync fn: async_boundary false");
    assert_eq!(hs.await_count, 0);
}

// ── Task 15b: unresolved-await confidence penalty ────────────────────────────

#[test]
fn async_fn_with_one_await_confidence_is_0_8() {
    // No other effects — only the await penalty (0.8) folds in.
    let out = analyze_fixture("async.rs");
    let hs = hotspot_of(&out, "async_with_await");
    assert!(
        (hs.confidence - 0.8).abs() < 1e-9,
        "await-only async fn: confidence must be 0.8, got {}",
        hs.confidence
    );
}

#[test]
fn async_fn_with_heuristic_and_await_confidence_is_0_6() {
    // min(0.6 heuristic, 0.8 await-penalty) = 0.6
    let out = analyze_fixture("async.rs");
    let hs = hotspot_of(&out, "async_with_heuristic_and_await");
    assert!(
        (hs.confidence - 0.6).abs() < 1e-9,
        "heuristic + await: confidence must be 0.6, got {}",
        hs.confidence
    );
}

#[test]
fn sync_fn_with_heuristic_confidence_is_0_6_no_await_penalty() {
    // Sync fn: no await penalty injected; only heuristic confidence 0.6.
    let out = analyze_fixture("async.rs");
    let hs = hotspot_of(&out, "sync_heuristic_only");
    assert!(
        (hs.confidence - 0.6).abs() < 1e-9,
        "sync fn heuristic: confidence must be 0.6, got {}",
        hs.confidence
    );
}

// ── Task 15c: static-path ambient.read ──────────────────────────────────────

#[test]
fn bare_static_read_emits_ambient_read_class_2_heuristic() {
    let out = analyze_fixture("statics.rs");
    let effects = effects_of(&out, "read_cfg");
    let e = one_kind(&effects, "ambient.read");
    assert_eq!(e.class, 2, "ambient.read from static path is class 2");
    assert_eq!(
        e.tier,
        Tier::Heuristic,
        "static-path read is Heuristic tier"
    );
    assert!(
        e.evidence.contains("CONFIG"),
        "evidence must name the static, got: {}",
        e.evidence
    );
}

#[test]
fn static_in_expression_emits_ambient_read() {
    let out = analyze_fixture("statics.rs");
    let effects = effects_of(&out, "doubled_cfg");
    let ambs: Vec<_> = effects
        .iter()
        .filter(|e| e.kind.wire() == "ambient.read")
        .collect();
    assert!(
        !ambs.is_empty(),
        "CONFIG in expression must still emit ambient.read"
    );
}

#[test]
fn function_callee_path_does_not_emit_ambient_read() {
    // `some_helper()` — function calls are handled by visit_expr_call, not visit_expr_path.
    // The helper is not in `statics`, so no ambient.read should appear.
    let out = analyze_fixture("statics.rs");
    let effects = effects_of(&out, "callee_not_read");
    assert!(
        effects.iter().all(|e| e.kind.wire() != "ambient.read"),
        "function call callee must not produce ambient.read; effects: {:?}",
        effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
    );
}

#[test]
fn callable_static_callee_does_not_emit_ambient_read() {
    // `CONFIG_FN()` — `CONFIG_FN` IS in the statics set, yet because it appears
    // in the callee position it must NOT emit ambient.read.  This test would fail
    // against the buggy code (no in_callee guard) and passes only with the fix.
    let out = analyze_fixture("statics.rs");
    let effects = effects_of(&out, "calls_callable_static");
    let ambs: Vec<_> = effects
        .iter()
        .filter(|e| e.kind.wire() == "ambient.read")
        .collect();
    assert!(
        ambs.is_empty(),
        "calling a static in callee position must not produce ambient.read; got: {:?}",
        ambs.iter().map(|e| &e.evidence).collect::<Vec<_>>()
    );
}

#[test]
fn load_and_static_emits_two_ambient_reads() {
    // .load() gives one ambient.read (heuristic existing); CONFIG gives another.
    let out = analyze_fixture("statics.rs");
    let effects = effects_of(&out, "load_and_static");
    let ambs: Vec<_> = effects
        .iter()
        .filter(|e| e.kind.wire() == "ambient.read")
        .collect();
    assert_eq!(
        ambs.len(),
        2,
        "expected two ambient.read (load + CONFIG), got: {:?}",
        ambs.iter().map(|e| &e.evidence).collect::<Vec<_>>()
    );
}

#[test]
fn no_static_read_no_ambient_read_from_path() {
    let out = analyze_fixture("statics.rs");
    let effects = effects_of(&out, "no_static_read");
    assert!(
        effects.iter().all(|e| e.kind.wire() != "ambient.read"),
        "function with no static read must emit no ambient.read"
    );
}

// ── Fix 2+3: destructuring patterns in mutation binding sets ─────────────────

/// `let (mut x, _y) = …; x = 1` must produce exactly one local.mutation (x
/// is tracked in locals and let_mut from the tuple pattern) and zero
/// global.mutation (x is known-local, not a SCREAMING_SNAKE unknown).
#[test]
fn destructured_let_produces_local_mutation_not_global() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "destructured_let_no_global");

    let locals: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "local.mutation")
        .collect();
    assert_eq!(
        locals.len(),
        1,
        "expected exactly one local.mutation for `x = 1` in destructured let, got {:?}",
        effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
    );
    assert_eq!(locals[0].class, 1, "local.mutation is class 1");
    assert_eq!(locals[0].tier, Tier::Exact);

    let globals: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "global.mutation")
        .collect();
    assert!(
        globals.is_empty(),
        "destructured let binding must not produce global.mutation false positive, got: {:?}",
        globals.iter().map(|e| &e.evidence).collect::<Vec<_>>()
    );
}

/// A destructured `&mut (i32, i32)` param mutated through a binding must NOT
/// produce a global.mutation false positive.
#[test]
fn destructured_mut_param_no_global_mutation_false_positive() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "destructured_mut_param");

    let globals: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.kind.wire() == "global.mutation")
        .collect();
    assert!(
        globals.is_empty(),
        "destructured &mut param must not produce global.mutation false positive, got: {:?}",
        globals.iter().map(|e| &e.evidence).collect::<Vec<_>>()
    );
}

// ── Task 2: is_test flag ─────────────────────────────────────────────────────

#[test]
fn collect_marks_test_code() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/skip_tests.rs"
    ))
    .unwrap();
    let file = syn::parse_file(&text).unwrap();
    let units = fxrank_lang_rust::functions::collect(&file, "skip_tests.rs");
    let by = |s: &str| units.iter().find(|u| u.symbol == s).map(|u| u.is_test);
    assert_eq!(by("prod"), Some(false));
    assert_eq!(by("free_test"), Some(true)); // #[test]
    assert_eq!(by("helper"), Some(true)); // inside #[cfg(test)] mod
    assert_eq!(by("S::method"), Some(true)); // method inside #[cfg(test)] mod
}

// ── Task 16: parse diagnostics ───────────────────────────────────────────────

// ── Gap 1: raw-ptr-deref in unsafe fn body (no inner block) ─────────────────

/// An `unsafe fn` that dereferences a raw pointer in its own body (no nested
/// `unsafe {}` block) must produce BOTH `UnsafeFn` (class 5) AND `RawPtrDeref`
/// (class 7, heuristic).  Before the fix only `UnsafeFn` was reported.
#[test]
fn raw_ptr_deref_in_unsafe_fn_body_emits_both_unsafe_fn_and_raw_ptr_deref() {
    let out = analyze_fixture("risk_gaps.rs");
    let risks = risks_of(&out, "deref_in_unsafe_fn");

    // Must have UnsafeFn (class 5).
    let unsafe_fn = one_risk(&risks, RiskKind::UnsafeFn);
    assert_eq!(unsafe_fn.class, 5, "UnsafeFn must be class 5");
    assert_eq!(unsafe_fn.tier, Tier::Exact);

    // Must also have RawPtrDeref (class 7, heuristic) — the deref is covered
    // by `fn_is_unsafe` even though there is no nested `unsafe {}` block.
    let raw_deref = one_risk(&risks, RiskKind::RawPtrDeref);
    assert_eq!(raw_deref.class, 7, "RawPtrDeref must be class 7");
    assert_eq!(
        raw_deref.tier,
        Tier::Heuristic,
        "raw deref approximation is Heuristic"
    );
}

// ── Gap 2: unsafe impl emits UnsafeImpl (class 5) ───────────────────────────

/// A file containing `unsafe impl Send for T {}` must produce at least one
/// `UnsafeImpl` entry (class 5) in `module_risks`.  The fixture also has an
/// `unsafe impl Drop`, so there are 2 in total — the test verifies the kind,
/// class, tier, path, and evidence are all correct for each entry.
#[test]
fn unsafe_impl_send_emits_module_risk_unsafe_impl_class_5() {
    let out = analyze_fixture("risk_gaps.rs");

    let unsafe_impls: Vec<_> = out
        .module_risks
        .iter()
        .filter(|r| r.kind == RiskKind::UnsafeImpl)
        .collect();
    // The fixture has both `unsafe impl Send` and `unsafe impl Drop` → 2 entries.
    assert!(
        !unsafe_impls.is_empty(),
        "expected at least one UnsafeImpl module risk"
    );
    for r in &unsafe_impls {
        assert_eq!(r.class, 5, "UnsafeImpl must be class 5");
        assert_eq!(r.tier, Tier::Exact);
        assert!(!r.path.is_empty(), "path must be set");
        assert!(
            r.evidence.contains("unsafe impl"),
            "evidence must mention `unsafe impl`, got: {}",
            r.evidence
        );
    }
}

/// An `unsafe impl Drop` must produce BOTH `ImplDrop` AND `UnsafeImpl`
/// independently — neither should else-out the other.
#[test]
fn unsafe_impl_drop_produces_both_impl_drop_and_unsafe_impl() {
    let out = analyze_fixture("risk_gaps.rs");

    let impl_drops: Vec<_> = out
        .module_risks
        .iter()
        .filter(|r| r.kind == RiskKind::ImplDrop)
        .collect();
    assert_eq!(
        impl_drops.len(),
        1,
        "unsafe impl Drop must still emit ImplDrop"
    );

    // The unsafe impl Drop is ALSO unsafe → must emit an additional UnsafeImpl.
    // Together with the plain `unsafe impl Send` above, total UnsafeImpl count
    // must be 2.
    let unsafe_impls: Vec<_> = out
        .module_risks
        .iter()
        .filter(|r| r.kind == RiskKind::UnsafeImpl)
        .collect();
    assert_eq!(
        unsafe_impls.len(),
        2,
        "both `unsafe impl Send` and `unsafe impl Drop` must each contribute a UnsafeImpl"
    );
}

// ── Fix 1+2: exact-segment matching for MaybeUninit / ManuallyDrop ───────────

/// `MaybeUninitWrapper::new()` — the segment `MaybeUninitWrapper` is NOT equal
/// to `MaybeUninit`, so it must produce no MaybeUninit risk feature.
#[test]
fn maybe_uninit_wrapper_does_not_produce_maybe_uninit_risk() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "maybe_uninit_wrapper_no_risk");
    let false_positives: Vec<_> = risks
        .iter()
        .filter(|r| r.kind == RiskKind::MaybeUninit)
        .collect();
    assert!(
        false_positives.is_empty(),
        "MaybeUninitWrapper::new() must not produce a MaybeUninit risk; got: {:?}",
        false_positives
    );
}

/// `ManuallyDropGuard::new()` — the segment `ManuallyDropGuard` is NOT equal
/// to `ManuallyDrop`, so it must produce no ManuallyDrop risk feature.
#[test]
fn manually_drop_guard_does_not_produce_manually_drop_risk() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "manually_drop_guard_no_risk");
    let false_positives: Vec<_> = risks
        .iter()
        .filter(|r| r.kind == RiskKind::ManuallyDrop)
        .collect();
    assert!(
        false_positives.is_empty(),
        "ManuallyDropGuard::new() must not produce a ManuallyDrop risk; got: {:?}",
        false_positives
    );
}

/// Sanity: `std::mem::MaybeUninit::uninit()` still produces a MaybeUninit risk
/// after switching to exact-segment matching.
#[test]
fn std_mem_maybe_uninit_uninit_still_produces_maybe_uninit_risk() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "maybe_uninit_example");
    let r = one_risk(&risks, RiskKind::MaybeUninit);
    assert_eq!(r.class, 7, "MaybeUninit must be class 7");
    assert_eq!(r.tier, Tier::Exact);
}

/// Sanity: `std::mem::ManuallyDrop::new(...)` still produces a ManuallyDrop risk
/// after switching to exact-segment matching.
#[test]
fn std_mem_manually_drop_new_still_produces_manually_drop_risk() {
    let out = analyze_fixture("risk.rs");
    let risks = risks_of(&out, "manually_drop_example");
    let r = one_risk(&risks, RiskKind::ManuallyDrop);
    assert_eq!(r.class, 4, "ManuallyDrop must be class 4");
    assert_eq!(r.tier, Tier::Exact);
}

#[test]
fn unparseable_file_becomes_a_diagnostic_not_a_panic() {
    let out = RustFrontend::default().analyze(&[
        SourceFile {
            path: "good.rs".into(),
            text: "fn f() { println!(\"x\"); }".into(),
        },
        SourceFile {
            path: "bad.rs".into(),
            text: "fn a( {".into(),
        },
    ]);
    // good file still produces a scored function
    assert!(out.functions.iter().any(|h| h.symbol == "f"));
    // bad file produces a diagnostic, run did not panic
    assert_eq!(out.diagnostics.len(), 1);
    assert_eq!(out.diagnostics[0].path, "bad.rs");
    assert!(!out.diagnostics[0].parsed);
    assert!(!out.diagnostics[0].error.is_empty());
}

// ── Task 3: skip test code by default ───────────────────────────────────────

#[test]
fn default_skips_tests_and_counts_them() {
    let out = analyze_fixture("skip_tests.rs"); // default: include_tests = false
    let syms: Vec<_> = out.functions.iter().map(|f| f.symbol.clone()).collect();
    assert!(syms.contains(&"prod".to_string()));
    assert!(
        !syms
            .iter()
            .any(|s| s == "free_test" || s.contains("helper") || s == "S::method")
    );
    assert_eq!(out.skipped_tests, 3); // free_test + helper + S::method
}

#[test]
fn include_tests_keeps_everything() {
    let out = RustFrontend {
        include_tests: true,
    }
    .analyze(&[source_of("skip_tests.rs")]);
    assert_eq!(out.skipped_tests, 0);
    assert!(out.functions.iter().any(|f| f.symbol == "free_test"));
}

// ── Task 3 (spec 005): hotspot id includes 1-based column ───────────────────

#[test]
fn id_includes_one_based_column() {
    let src = "fn foo() {}\n";
    let file = syn::parse_file(src).expect("parse");
    let units = fxrank_lang_rust::functions::collect(&file, "t.rs");
    let foo = units.iter().find(|u| u.symbol == "foo").expect("foo unit");
    // `fn foo` — the ident `foo` starts at column 4 (1-based) on line 1
    // (proc-macro2 column is 0-based: `f`=0,`n`=1,` `=2,`foo`@3 -> +1 = 4).
    assert_eq!(foo.id, "t.rs:1:4:foo");
}

#[test]
fn cfg_test_module_risks_skipped_by_default() {
    let src = "#[cfg(test)] impl Drop for T {}\n#[cfg(test)] unsafe impl Send for T {}\n#[cfg(test)] extern \"C\" { fn x(); }";
    let def = RustFrontend::default().analyze(&[SourceFile {
        path: "m.rs".into(),
        text: src.into(),
    }]);
    assert!(def.module_risks.is_empty());
    let inc = RustFrontend {
        include_tests: true,
    }
    .analyze(&[SourceFile {
        path: "m.rs".into(),
        text: src.into(),
    }]);
    assert_eq!(inc.module_risks.len(), 3); // ImplDrop + UnsafeImpl + ExternBlock
}

// ── Spec 008 R1: detect signature carries statics + imports ──────────────────
#[test]
fn mutation_detect_accepts_statics_and_imports() {
    use fxrank_lang_rust::detect::mutation;
    use fxrank_lang_rust::imports::ImportTable;
    use std::collections::HashSet;

    let file = syn::parse_file("static FOO: u32 = 0; fn f() { let mut x = 0; x = 1; }").unwrap();
    let imports = ImportTable::from_file(&file);
    let statics: HashSet<String> = ["FOO".to_string()].into_iter().collect();

    let item_fn = file
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Fn(f) if f.sig.ident == "f" => Some(f),
            _ => None,
        })
        .expect("fn f");

    let effects = mutation::detect(&item_fn.block, &item_fn.sig, &statics, &imports);
    assert!(
        effects.iter().any(|e| e.kind.wire() == "local.mutation"),
        "local write still detected after signature change"
    );
}

// ── Spec 008 F1: unresolved free-binding write → hidden.mutation ─────────────
#[test]
fn unresolved_free_binding_write_is_hidden_mutation_class_3() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "writes_unresolved_free_binding");
    let e = one_kind(&effects, "hidden.mutation");
    assert_eq!(e.class, 3, "hidden.mutation is class 3");
    assert_eq!(e.discounted_to, None, "hidden mutation is never discounted");
    assert!(e.hidden, "an unresolved free-binding write is hidden");
    assert_eq!(e.tier, Tier::Heuristic);
    assert_eq!(
        e.subreason.as_deref(),
        Some("captured-binding"),
        "captured-binding hidden write carries subreason captured-binding"
    );
}

// ── Spec 008 F5: import-resolved write base → global.mutation ────────────────
#[test]
fn import_resolved_write_base_is_global_mutation_class_6() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "writes_imported_base");
    let e = one_kind(&effects, "global.mutation");
    assert_eq!(
        e.class, 6,
        "import-resolved write is global.mutation class 6"
    );
    assert_eq!(e.tier, Tier::Heuristic);
    assert!(
        e.evidence.contains("imported_cell"),
        "evidence names the imported base, got: {}",
        e.evidence
    );
}

// ── Spec 008 FP-self: misattributed nested-receiver `self` must not emit ─────
// global.mutation or hidden.mutation on the enclosing free fn when "self" is
// in the ImportTable (e.g. via `use some_module::{self, …}`). The nested impl
// method's `self.0 += 1` must be silently dropped, NOT routed through F5/F1.
#[test]
fn nested_impl_self_write_not_attributed_to_free_fn() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "count_awaits_fp_self");
    assert!(
        effects.iter().all(|e| e.kind.wire() != "global.mutation"),
        "misattributed nested-receiver `self` write must NOT emit global.mutation, got: {:?}",
        effects
            .iter()
            .map(|e| (e.kind.wire(), e.evidence.as_str()))
            .collect::<Vec<_>>()
    );
    assert!(
        effects.iter().all(|e| e.kind.wire() != "hidden.mutation"),
        "misattributed nested-receiver `self` write must NOT emit hidden.mutation, got: {:?}",
        effects
            .iter()
            .map(|e| (e.kind.wire(), e.evidence.as_str()))
            .collect::<Vec<_>>()
    );
}
