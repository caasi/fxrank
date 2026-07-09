//! Load-bearing invariant for plan 030: binding/scope resolution is entirely
//! frontend-internal (`crate::scope`) and must never cross the `UnitRecord`
//! boundary into `fxrank-core`. This is a mechanical guard, not red-first
//! TDD — it is expected to pass immediately, since the invariant held
//! throughout the #78 migration (Tasks 1-8 never touched `fxrank-core`).
//!
//! The true enforcement is `scope::BindingOrigin`/`ScopeTree`'s `pub(crate)`
//! visibility (the compiler refuses to let `fxrank-core` even name them) —
//! this grep is belt-and-suspenders, checking the WHOLE crate (not just
//! `record.rs`, the one file most likely to grow a leak) so a future
//! `fxrank-core` file can't quietly reintroduce these names some other way
//! (e.g. a local re-declaration, a doc comment promising a future field).

use std::path::Path;

/// Recursively collect every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir({dir:?}): {e}")) {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn core_has_no_binding_or_scope_types() {
    let core_src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../fxrank-core/src"));
    let mut files = Vec::new();
    rust_files(core_src, &mut files);
    assert!(
        !files.is_empty(),
        "expected to find .rs files under {core_src:?} — did the crate layout move?"
    );
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        for needle in ["BindingOrigin", "ScopeTree", "scope_segments"] {
            assert!(
                !text.contains(needle),
                "fxrank-core must not reference `{needle}` (#78 load-bearing invariant) — found in {path:?}"
            );
        }
    }
}
