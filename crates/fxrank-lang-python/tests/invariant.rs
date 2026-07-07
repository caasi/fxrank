//! Load-bearing invariant for spec 078: binding/scope resolution is entirely
//! frontend-internal (`crate::scope`) and must never cross the `UnitRecord`
//! boundary into `fxrank-core`. This is a mechanical guard, not red-first
//! TDD — it is expected to pass immediately, since the invariant held
//! throughout the #78 migration (Tasks 1-8 never touched `fxrank-core`).

#[test]
fn core_has_no_binding_or_scope_types() {
    let core = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fxrank-core/src/record.rs"
    ))
    .unwrap();
    for needle in ["BindingOrigin", "ScopeTree", "scope_segments"] {
        assert!(
            !core.contains(needle),
            "fxrank-core must not reference `{needle}` (#78 load-bearing invariant)"
        );
    }
}
