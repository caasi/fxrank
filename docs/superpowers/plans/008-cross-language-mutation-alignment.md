# Cross-language mutation-classification alignment — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align the three FxRank language frontends (Rust/syn, TS/swc, Python/libcst) to behavioral parity on five drifted mutation-classification cases (F1–F5), and ship a descriptive guideline documenting the shared model + the honest language differences.

**Architecture:** No shared code layer and no scope rewrite. Each frontend keeps its own `detect/mutation.rs` native walk; we patch each one's classification cascade so the *same concept* produces the *same effect* across languages, while preserving the honest per-language differences. Source of truth: `docs/superpowers/specs/008-cross-language-mutation-alignment.md`.

**Tech Stack:** Rust (Cargo workspace), `syn`/`swc`/`libcst` parsers, `insta` snapshot testing, `fxrank-core` shared effect/score vocabulary.

## Global Constraints

- **Implementation goes on a feature branch, never `main`** (per CLAUDE.md). Prefer a git worktree on `/dev/shm` (see `superpowers:using-git-worktrees`). Branch suggestion: `feat/008-mutation-alignment`.
- **CI gates (run before every push, verbatim):** `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.
- **Snapshots** use `insta`: review pending with `cargo insta review`, accept intentional deltas with `cargo insta accept`. **Never blind-accept** — inspect every diff.
- **One fix per commit** (conventional commits). Each F-number is its own reviewable commit so behavioral deltas are isolated.
- **The anti-Goodhart canary is an invariant** — it must stay green throughout: a `&self` interior-mutability write (`hidden.mutation`/3, **no discount**) must out-score a `&mut self` writer (`param.mutation`/3 discounted to 2). Guard tests: `hidden_mutation_scores_higher_than_declared_mut_self` (`crates/fxrank-lang-rust/tests/rust_frontend.rs`), `snapshot_inversion_pair` (`crates/fxrank-lang-rust/tests/snapshots.rs`).
- **KEEP behaviors must stay byte-identical** (spec §2 honest differences): Rust mut-channel discount + unsafe-cancel; TS/Python typed-boundary discount; Python `global`→`global.mutation`/Exact and `nonlocal`→`this.mutation`/Exact/not-hidden; `self.attr=` in `__init__`→`local.mutation`/contained vs `self.x.append()`→`this.mutation`; plain bare-name rebind (Python no-emit / TS+Rust `local.mutation`); per-language mutating-method allowlists; destructuring-target drops.
- **`hidden.mutation` subreason vocabulary (consistent across frontends):** `"interior-mut"` (Rust interior-mutability), `"captured-binding"` (the captured/unresolved fallback — all three frontends), `"ref-cell-write"` (TS `useRef().current` — unchanged).
- **Canonical effect facts (the alignment targets):** `local.mutation`=class 1/contained; `param.mutation`=class 3; `this.mutation`=class 3; `global.mutation`=class 6; `hidden.mutation`=class 3/hidden. (From `fxrank-core/src/effect.rs`.)
- **Running specific tests:** `cargo test` takes a **single** filter positional. Where a step lists several test names, run them via a shared-substring filter when one exists (e.g. `cargo test -p fxrank-lang-ts ctor_`) or run the whole package (`cargo test -p <pkg>`) — the listed names are the tests that must **fail** (RED) or **pass** (GREEN); confirm each in the output. Do not pass multiple bare filters in one `cargo test` invocation.

**Task order:** Rust (R1–R5) → Python (P1–P4) → TS (T1–T2) → Conformance (C) → Dogfood (D) → Guideline (G). The three frontend sections are independent (separate crates/commits) and may be done in any order; the guideline is written last so it documents the *realized* behavior (this reorders spec §8, which listed the guideline first — intentional: a descriptive doc should reflect what shipped).

---

# Rust frontend (Tasks R1–R5)

**Intentional deltas:** retires the SCREAMING_SNAKE proxy (`is_screaming_snake`) and rewires `mutation::detect` to consume the real static-name set + the `ImportTable`. The committed insta snapshots are driven by `tests/fixtures/worked_cases.rs`, which has **no `static` items** and **no captured/free/import-base writes**, and `summarize()` in `tests/snapshots.rs` does **not** serialize `subreason` — so **none of the eight `.snap` files change**. New behavior is asserted by direct-assertion tests in `tests/rust_frontend.rs` over new fixtures in `tests/fixtures/mutation.rs`. The old test `screaming_snake_write_is_global_mutation_class_6` is replaced (R2). If any `.snap` reports pending, **stop** — a fixture has an unexpected static/free write.

### Task R1: Thread the static-name set and `ImportTable` into `mutation::detect`

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/mod.rs` (`gather`, line 93)
- Modify: `crates/fxrank-lang-rust/src/detect/mutation.rs` (`detect` lines 34–39; `MutationWalker` struct lines 41–57; `seed` lines 60–103)
- Test: `crates/fxrank-lang-rust/tests/rust_frontend.rs`

**Interfaces:**
- Consumes (in `gather`): `imports: &ImportTable`, `statics: &HashSet<String>` — both already in scope (passed to `calls::detect` on line 91).
- Produces: `pub fn detect<'a>(block: &syn::Block, sig: &syn::Signature, statics: &'a HashSet<String>, imports: &'a ImportTable) -> Vec<Effect>` (was `detect(block, sig)`).

Pure plumbing: thread the two new params to where the `record_write` cascade will consume them (R2–R5). No behavioral change — all existing tests stay green.

- [ ] **Step 1: Write the failing test** — pin the new signature. Add to `tests/rust_frontend.rs`:

```rust
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
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-rust mutation_detect_accepts_statics_and_imports`. Expected: **compile error** `this function takes 2 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Implement** — three edits.

In `detect/mutation.rs`, add the import near the top (alongside `use std::collections::HashSet;`):

```rust
use crate::imports::ImportTable;
```

Replace `detect` (lines 35–39):

```rust
/// Detect mutation effects in `block`, seeding binding sets from `sig`.
///
/// `statics` is the file-level set of real `static` item names; `imports` is the
/// `use`-table. Both feed `record_write`'s base-resolution cascade.
pub fn detect<'a>(
    block: &syn::Block,
    sig: &syn::Signature,
    statics: &'a HashSet<String>,
    imports: &'a ImportTable,
) -> Vec<Effect> {
    let mut walker = MutationWalker::seed(sig, statics, imports);
    walker.visit_block(block);
    walker.effects
}
```

Give `MutationWalker` a lifetime and two borrowed fields. Change `struct MutationWalker {` (line 41) to `struct MutationWalker<'a> {`, add (after line 56, before `effects: Vec<Effect>,`):

```rust
    /// File-level real `static` item names (`static`/`static mut`/atomics/…).
    statics: &'a HashSet<String>,
    /// The `use`-table, for resolving a write base through an import.
    imports: &'a ImportTable,
```

Change the impl headers: `impl MutationWalker {` → `impl<'a> MutationWalker<'a> {`, and `impl<'ast> Visit<'ast> for MutationWalker {` → `impl<'a, 'ast> Visit<'ast> for MutationWalker<'a> {`.

Change `seed` (line 60) to accept and store them:

```rust
    fn seed(
        sig: &syn::Signature,
        statics: &'a HashSet<String>,
        imports: &'a ImportTable,
    ) -> Self {
        let mut w = MutationWalker {
            mut_params: HashSet::new(),
            mut_self: false,
            shared_refs: HashSet::new(),
            let_mut: HashSet::new(),
            locals: HashSet::new(),
            unsafe_depth: 0,
            unsafe_fn: sig.unsafety.is_some(),
            statics,
            imports,
            effects: Vec::new(),
        };
```

In `detect/mod.rs`, change `gather` line 93 from `effects.extend(mutation::detect(&unit.block, &unit.sig));` to:

```rust
    effects.extend(mutation::detect(&unit.block, &unit.sig, statics, imports));
```

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-rust mutation_detect_accepts_statics_and_imports`, then `cargo test -p fxrank-lang-rust` (existing tests still green; new fields stored but unused).
- [ ] **Step 5: Snapshot review** — `cargo insta test -p fxrank-lang-rust --review`. Expected: **no pending snapshots**.
- [ ] **Step 6: Commit** — `git commit -m "refactor(rust): thread static-name set and ImportTable into mutation::detect"`

### Task R2: F2 — real-static write → `global.mutation`/6; retire `is_screaming_snake`

> **Replaces** the existing test `screaming_snake_write_is_global_mutation_class_6` (in
> `tests/rust_frontend.rs`), which asserted the casing proxy. **`collect_static_names` (lib.rs)
> collects all *top-level/file-level* `static` items** — it matches `syn::Item::Static(_)`, so
> both `static mut X` and a plain `static X: AtomicU32` are in the set; no extension needed.
> (It does **not** recurse into inline `mod { … }` blocks, so a `static` declared inside a nested
> inline module is not collected — an accepted edge-case gap, same as the `statics` set already
> used by `calls::detect`.)
> **F2 has two emission sites:** (1) `record_write`'s cascade for *assignment / mutating-method*
> writes to a static, and (2) the **interior-mutator branch** of `visit_expr_method_call` for an
> *interior-mutable* static written via an `is_interior_mutator` method — which never reaches
> `record_write`. That method set is `{borrow_mut, set, replace, store, swap, fetch_*}`, so it
> catches **atomics** (`.store()`/`.swap()`/`.fetch_*`), **`Cell`/`OnceLock`** (`.set()`),
> **`RefCell`** (`.borrow_mut()`). It does **NOT** catch `Mutex`/`RwLock` via `.lock()`/`.read()`/
> `.write()` (`lock`/`read`/`write` are not in `is_interior_mutator`) — those statics remain
> uncaught; **expanding `is_interior_mutator` is out of scope here** because it would also change
> the `&self` hidden-mutation behavior. `shared_refs` is checked **before** `statics` so the
> anti-Goodhart `&self` interior-mut case stays `hidden.mutation`. No committed `.snap` changes
> (snapshots analyze only `worked_cases.rs`, which has no static writes).

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/mutation.rs` (`record_write` static arm; the `is_interior_mutator` branch of `visit_expr_method_call`; delete `is_screaming_snake`; module doc)
- Modify: `crates/fxrank-lang-rust/tests/fixtures/mutation.rs` (append three fixtures)
- Modify: `crates/fxrank-lang-rust/tests/rust_frontend.rs` (delete `screaming_snake_write_is_global_mutation_class_6`; add four tests)

**Interfaces:**
- Consumes: `self.statics: &HashSet<String>` (from R1); `self.shared_refs`; `base_ident(&node.receiver)`.
- Produces: `global.mutation`/6 from both the `record_write` cascade and the interior-mutator branch when the base is a real static.

- [ ] **Step 1: Write the failing fixtures** — append to `tests/fixtures/mutation.rs`:

```rust
// R2 (F2): a *lowercase* `static mut` written by direct assignment. Proves the
// real-static detection is casing-INDEPENDENT (pre-fix `is_screaming_snake`
// rejects the lowercase base → dropped → no global.mutation).
static mut counter_cell: u32 = 0;
fn write_lower_static_mut() {
    unsafe {
        counter_cell = 1;
    }
}

// R2 (F2): a plain `static` of interior-mutable type written via `.store()`. The
// write routes through the interior-mutator branch of visit_expr_method_call, NOT
// record_write. Pre-fix that branch only fires for `shared_refs` bases, so an
// atomic static base is dropped → no global.mutation.
use std::sync::atomic::{AtomicU32, Ordering};
static HITS: AtomicU32 = AtomicU32::new(0);
fn store_atomic_static() {
    HITS.store(1, Ordering::Relaxed);
}

// R2 (F2): an UPPERCASE base bound NOWHERE and NOT a file-level static — the real
// proxy-retirement discriminator. Pre-fix `is_screaming_snake("UNBOUND_THING")` is
// true → wrongly emits global.mutation. Post-fix it is not in `statics` → dropped.
fn write_unbound_upper() {
    UNBOUND_THING.field = 1;
}
```

Delete the `screaming_snake_write_is_global_mutation_class_6` test in `tests/rust_frontend.rs` and add:

```rust
// ── Task R2 (F2): real-static write → global.mutation (class 6) ───────────────
/// A *lowercase* `static mut` written by direct assignment is global.mutation/6
/// (casing-independent; the old proxy rejected the lowercase base).
#[test]
fn lowercase_static_mut_assign_is_global_mutation_class_6() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "write_lower_static_mut");
    let e = one_kind(&effects, "global.mutation");
    assert_eq!(e.class, 6, "global.mutation is class 6 (no class-4 downgrade)");
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
```

> Adjust `"User::set"` to whatever symbol the existing `&self` interior-mutability fixture uses
> in `tests/fixtures/mutation.rs` (the one the canary `hidden_mutation_scores_higher_than_declared_mut_self` exercises).

- [ ] **Step 2: Run, verify RED** — `cargo test -p fxrank-lang-rust` (whole package). Expected: the two `*_is_global_mutation_class_6` tests fail with `expected exactly one global.mutation effect, got []` (lowercase rejected by the proxy; atomic base not in `shared_refs`); `unbound_uppercase_non_static_is_not_global_mutation` fails because `is_screaming_snake("UNBOUND_THING")` is true → a spurious `global.mutation` IS present. (`self_interior_mutation_stays_hidden_not_global` passes already — canary baseline.)

- [ ] **Step 3: Implement — site 1 (`record_write` cascade)** — replace the `is_screaming_snake` arm:

```rust
        } else if !self.locals.contains(&base) && self.statics.contains(&base) {
            // F2: base is bound in no local/param/let-mut set but IS a file-level
            // `static` — a real static write (direct/compound assignment, or a
            // mutating method like `STATIC_VEC.push`). Class-4 module-private
            // downgrade DEFERRED per spec — always class 6.
            self.push_plain(
                EffectKind::GlobalMutation,
                Tier::Heuristic,
                false,
                line,
                format!("write to global {base}"),
            );
        }
```

- [ ] **Step 4: Implement — site 2 (interior-mutator branch)** — extend the `is_interior_mutator` branch of `visit_expr_method_call`. **`shared_refs` first** (preserves the anti-Goodhart `&self` → hidden case), **then `statics`**:

```rust
        if is_interior_mutator(&method) {
            let base = base_ident(&node.receiver);
            if base.as_deref().is_some_and(|b| self.shared_refs.contains(b)) {
                // Hidden mutation through a shared `&` base (`&T` param, or `self`
                // when the receiver is `&self`). Checked FIRST so the anti-Goodhart
                // `&self` interior-mut case stays `hidden`.
                let base = base.expect("checked Some above");
                self.push_plain(
                    EffectKind::HiddenMutation,
                    Tier::Heuristic,
                    true,
                    line,
                    format!(".{method} on shared &{base}"),
                );
            } else if base.as_deref().is_some_and(|b| self.statics.contains(b)) {
                // F2: the receiver base is a file-level static written via an
                // interior-mutability mutator (`.store()`/`.swap()`/`.fetch_*` on an
                // atomic, `.set()` on a Cell/OnceLock, `.borrow_mut()` on a RefCell)
                // → global.mutation, class 6. (Mutex/RwLock `.lock()` is NOT in
                // is_interior_mutator and is not caught — see the task note.)
                let base = base.expect("checked Some above");
                self.push_plain(
                    EffectKind::GlobalMutation,
                    Tier::Heuristic,
                    false,
                    line,
                    format!("interior write to global {base} via .{method}"),
                );
            }
        } else if is_mutating_method(&method) {
            self.record_write(&node.receiver, line);
        }
```

- [ ] **Step 5: Implement — delete `is_screaming_snake` and fix the module doc.** Remove the whole `fn is_screaming_snake(...) -> bool { ... }`. Change the module-doc bullet:

```rust
//! - base is a file-level `static` item (not a local/param) → `global.mutation`
//!   (class 6, heuristic — written by assignment, a mutating method, or an
//!   interior-mutability mutator like `.store()` on an atomic static).
```

(R3 replaces the cascade fall-through; for now a non-local non-static base is still dropped.)

- [ ] **Step 6: Run, verify GREEN** — `cargo test -p fxrank-lang-rust` (whole crate). Expected: all four new tests pass (`lowercase_static_mut_assign_…`, `atomic_static_store_…`, `unbound_uppercase_non_static_…`, `self_interior_mutation_stays_hidden_not_global`) **and** the canary `hidden_mutation_scores_higher_than_declared_mut_self` still passes.
- [ ] **Step 7: Snapshot review** — `cargo insta test -p fxrank-lang-rust --review`. Expected: **no pending** (snapshots use `worked_cases.rs`, untouched).
- [ ] **Step 8: Clippy + fmt** — `cargo clippy -p fxrank-lang-rust --all-targets -- -D warnings && cargo fmt --check` (confirms `is_screaming_snake` is fully removed, not orphaned).
- [ ] **Step 9: Commit** — `git commit -m "feat(rust): F2 — real-static (incl. atomic) writes → global.mutation/6, retire SCREAMING_SNAKE proxy"`

### Task R3: F1 — cascade-tail fallback → `hidden.mutation`/3/hidden

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/mutation.rs` (`record_write` cascade tail, after the static arm)
- Modify: `crates/fxrank-lang-rust/tests/fixtures/mutation.rs` (append fixture)
- Modify: `crates/fxrank-lang-rust/tests/rust_frontend.rs` (new test)

**Interfaces:**
- Produces: an `else` tail emitting `hidden.mutation` (class 3, hidden=true, Tier::Heuristic) for a base resolving to none of {`let_mut`/local, `mut_params`, `self`, real-static}. TS reference: `hidden.mutation` class 3, hidden=true.

- [ ] **Step 1: Write the failing test** — the base must be one the walker never sees declared (so it stays unresolved). Append to `tests/fixtures/mutation.rs`:

```rust
// 008-F1: `external_thing` is bound nowhere in this fn (no let/param/self) and is
// NOT a file-level static. A write to it is an unresolved free binding → the
// cascade tail emits hidden.mutation (class 3, hidden).
fn writes_unresolved_free_binding() {
    external_thing.field = 1;
}
```

Add to `tests/rust_frontend.rs`:

```rust
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
}
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-rust unresolved_free_binding_write_is_hidden_mutation_class_3`. Expected: **FAIL** — `got []` (the R2 cascade has no `else` tail; `external_thing` is not local, not a static → dropped).

- [ ] **Step 3: Implement** — add a final `else` to the `record_write` cascade (after the F2 static arm):

```rust
        } else if !self.locals.contains(&base) && self.statics.contains(&base) {
            // 008-F2: a real file-level `static` write → global.mutation.
            self.push_plain(
                EffectKind::GlobalMutation, Tier::Heuristic, false, line,
                format!("write to static {base}"),
            );
        } else if !self.locals.contains(&base) {
            // 008-F1: the base resolves to no local/param/self/static binding —
            // a write to a captured/unresolved outer binding, hidden from this
            // signature → hidden.mutation (class 3, hidden). TS parity.
            self.push_plain(
                EffectKind::HiddenMutation, Tier::Heuristic, true, line,
                format!("write to captured binding {base}"),
            );
        }
```

(R4 inserts an import arm *before* this tail; R5 attaches the `captured-binding` subreason.)

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-rust unresolved_free_binding_write_is_hidden_mutation_class_3`, then `cargo test -p fxrank-lang-rust`. **Re-run the canary:** `cargo test -p fxrank-lang-rust hidden_mutation_scores_higher_than_declared_mut_self` — still green (the tail does not touch `&self`/`&mut self`).
- [ ] **Step 5: Snapshot review** — `cargo insta test -p fxrank-lang-rust --review`. Expected: **no pending**.
- [ ] **Step 6: Commit** — `git commit -m "feat(rust): F1 — unresolved free-binding write scores as hidden.mutation (TS parity)"`

### Task R4: F5 — import-resolved write base → `global.mutation`/6

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/mutation.rs` (`record_write` — insert import arm before the F1 tail)
- Modify: `crates/fxrank-lang-rust/tests/fixtures/mutation.rs` (append fixture)
- Modify: `crates/fxrank-lang-rust/tests/rust_frontend.rs` (new test)

**Interfaces:**
- Consumes: `self.imports.resolve(&base)` → `Option<&str>` (imports.rs:71).
- Produces: a cascade arm that emits `global.mutation`/6 when `self.imports.resolve(&base).is_some()`. Near-vacuous for Rust (a `use` resolves a type/fn path, not a writable binding) — implemented for symmetry; the fixture is contrived to force the path.

- [ ] **Step 1: Write the failing test** — append to `tests/fixtures/mutation.rs`:

```rust
// 008-F5: `imported_cell` is brought in by a `use`. A write whose base resolves
// through the ImportTable is module-external ambient state → global.mutation/6.
// Contrived: in real Rust a `use` names a type/fn, so this path is near-vacuous.
use some_crate::imported_cell;
fn writes_imported_base() {
    imported_cell.field = 1;
}
```

Add to `tests/rust_frontend.rs`:

```rust
// ── Spec 008 F5: import-resolved write base → global.mutation ────────────────
#[test]
fn import_resolved_write_base_is_global_mutation_class_6() {
    let out = analyze_fixture("mutation.rs");
    let effects = effects_of(&out, "writes_imported_base");
    let e = one_kind(&effects, "global.mutation");
    assert_eq!(e.class, 6, "import-resolved write is global.mutation class 6");
    assert_eq!(e.tier, Tier::Heuristic);
    assert!(e.evidence.contains("imported_cell"), "evidence names the imported base, got: {}", e.evidence);
}
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-rust import_resolved_write_base_is_global_mutation_class_6`. Expected: **FAIL** — after R3, `imported_cell` falls into the F1 `hidden.mutation` tail → `got [hidden.mutation]`, not `global.mutation`.

- [ ] **Step 3: Implement** — insert an import arm **between** the F2 static arm and the F1 tail:

```rust
        } else if !self.locals.contains(&base) && self.imports.resolve(&base).is_some() {
            // 008-F5: the base resolves through the `use`-table — module-external
            // ambient state → global.mutation. Near-vacuous for Rust; implemented
            // for symmetry with the TS/Python frontends.
            self.push_plain(
                EffectKind::GlobalMutation, Tier::Heuristic, false, line,
                format!("write to imported {base}"),
            );
        } else if !self.locals.contains(&base) {
            // 008-F1: unresolved captured binding → hidden.mutation.
            self.push_plain(
                EffectKind::HiddenMutation, Tier::Heuristic, true, line,
                format!("write to captured binding {base}"),
            );
        }
```

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-rust import_resolved_write_base_is_global_mutation_class_6`, then `cargo test -p fxrank-lang-rust` (the R3 free-binding test still passes — `external_thing` resolves through neither statics nor imports).
- [ ] **Step 5: Snapshot review** — `cargo insta test -p fxrank-lang-rust --review`. Expected: **no pending**.
- [ ] **Step 6: Commit** — `git commit -m "feat(rust): F5 — import-resolved write base scores as global.mutation (cross-language symmetry)"`

### Task R5: F3 — consistent `subreason` on `hidden.mutation`

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/mutation.rs` (`push_plain` lines 135–157; interior-mut call site lines 301–307; the F1 tail)
- Modify: `crates/fxrank-lang-rust/tests/rust_frontend.rs` (extend two tests)

**Interfaces:**
- Produces: `push_plain` gains a `subreason: Option<&str>` parameter; the two `hidden.mutation` sites pass `Some("interior-mut")` and `Some("captured-binding")`; all non-hidden callers pass `None`. Reporting only — no class change.

- [ ] **Step 1: Write the failing test** — extend the interior-mut test (`self_interior_mutation_is_hidden_mutation_no_discount`, ~line 380) and the R3 F1 test:

```rust
// (in self_interior_mutation_is_hidden_mutation_no_discount, after the tier assertion)
    assert_eq!(e.subreason.as_deref(), Some("interior-mut"),
        "interior-mutability hidden write carries subreason interior-mut");
```

```rust
// (in unresolved_free_binding_write_is_hidden_mutation_class_3, after the tier assertion)
    assert_eq!(e.subreason.as_deref(), Some("captured-binding"),
        "captured-binding hidden write carries subreason captured-binding");
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-rust self_interior_mutation_is_hidden_mutation_no_discount unresolved_free_binding_write_is_hidden_mutation_class_3`. Expected: **FAIL** — `push_plain` hard-codes `subreason: None` (line 153), so both are `None`.

- [ ] **Step 3: Implement** — add the parameter to `push_plain` (lines 135–157):

```rust
    /// Emit a plain class-N mutation effect (hidden/local/global): no discount.
    fn push_plain(
        &mut self,
        kind: EffectKind,
        tier: Tier,
        hidden: bool,
        line: usize,
        evidence: String,
        subreason: Option<&str>,
    ) {
        let class = kind.base_class();
        self.effects.push(Effect {
            kind,
            class,
            discounted_to: None,
            weight: weight_for_class(class),
            line,
            tier,
            hidden,
            evidence,
            discount: None,
            subreason: subreason.map(str::to_string),
            confidence: detection_confidence(tier, false, false),
        });
    }
```

Update **every** `push_plain` call site (the compiler will list them all once the signature changes): `local.mutation` (record_write) → trailing `None`; `global.mutation` F2 `record_write` static arm → `None`; **`global.mutation` F2 interior-mutator-static arm** (added in R2 site 2) → `None`; `global.mutation` F5 import arm → `None`; `hidden.mutation` F1 tail → `Some("captured-binding")`; `hidden.mutation` interior-mut shared-ref site:

```rust
                self.push_plain(
                    EffectKind::HiddenMutation,
                    Tier::Heuristic,
                    true,
                    line,
                    format!(".{method} on shared &{base}"),
                    Some("interior-mut"),
                );
```

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-rust self_interior_mutation_is_hidden_mutation_no_discount unresolved_free_binding_write_is_hidden_mutation_class_3`, then `cargo test -p fxrank-lang-rust`.
- [ ] **Step 5: Snapshot review** — `cargo insta test -p fxrank-lang-rust --review`. Expected: **no pending** (`summarize()` omits `subreason`).
- [ ] **Step 6: Final gates** — `cargo fmt --check && cargo clippy -p fxrank-lang-rust --all-targets -- -D warnings && cargo test -p fxrank-lang-rust`.
- [ ] **Step 7: Commit** — `git commit -m "feat(rust): F3 — tag hidden.mutation with interior-mut / captured-binding subreason"`

---

# Python frontend (Tasks P1–P4)

**Intentional deltas:** Python emits `hidden.mutation` for the **first time**. The committed dogfood snapshot (`tests/snapshots/snapshots__dogfood_report.snap`, from `tests/fixtures/dogfood.py`) has **no** captured-binding or import-rooted write, so it is **unchanged** — verify clean after P4. New behavior is asserted by new unit tests in `src/detect/mutation.rs::tests` + new fixture functions appended to `tests/fixtures/mutation.py`. The existing mutation tests (`classifies_mutation_by_escape`, `plain_assign_to_global_nonlocal_names_escapes`, `self_method_and_subscript_mutations_escape_even_in_init`, `mutating_method_evidence_uses_full_receiver`) and the KEEP behaviors they guard MUST keep passing untouched.

### Task P1: Hidden-aware `MutSink` push (`push_hidden` sibling)

**Files:**
- Modify: `crates/fxrank-lang-python/src/detect/mutation.rs` (add `push_hidden` after `push`, current lines 462–487)
- Test: `crates/fxrank-lang-python/src/detect/mutation.rs::tests`

**Interfaces:**
- Produces: `fn push_hidden(&mut self, line: usize, evidence: String, subreason: &str)` on `impl MutSink<'_>` — always `EffectKind::HiddenMutation`, `Tier::Heuristic`, `hidden: true`, `subreason: Some(..)`, `contained: false`. No cascade arm calls it yet (P4 does); this task proves the emission path.

- [ ] **Step 1: Write the failing test** — add to `mod tests` (drives `push_hidden` directly):

```rust
/// PREREQ 1: MutSink can emit a HiddenMutation (hidden:true + subreason) —
/// the channel Python has never used. `push` stays the honest hidden:false path.
#[test]
fn push_hidden_emits_hidden_mutation_with_subreason() {
    let params = std::collections::HashSet::new();
    let globals = std::collections::HashSet::new();
    let nonlocals = std::collections::HashSet::new();
    let locals = std::collections::HashSet::new();
    let src = "x\n";
    let span = crate::source::SpanIndex::new(src);
    let mut sink = MutSink {
        params: &params, globals: &globals, nonlocals: &nonlocals, locals: &locals,
        is_init: false, span: &span, effects: Vec::new(),
    };
    sink.push_hidden(1, "outer_acc.append(…)".to_string(), "captured-binding");

    assert_eq!(sink.effects.len(), 1);
    let (effect, contained) = &sink.effects[0];
    assert_eq!(effect.kind, EffectKind::HiddenMutation);
    assert_eq!(effect.class, 3);
    assert!(effect.hidden, "push_hidden must set hidden:true");
    assert_eq!(effect.subreason.as_deref(), Some("captured-binding"));
    assert!(!contained, "hidden writes escape — contained=false");
}
```

> Note: this test constructs `MutSink` with its **current** fields. After P2 adds the `imports` field, update this literal to include `imports: &imports` (build an empty `Imports`). The TDD runner will catch the missing field as a compile error in P2.

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-python push_hidden_emits_hidden_mutation_with_subreason`. Expected: **compile error** `no method named \`push_hidden\``.

- [ ] **Step 3: Implement** — add `push_hidden` right after `push` (after line 487):

```rust
    /// Push a `HiddenMutation` (`hidden:true` + a `subreason`), always escaping
    /// (`contained:false`). Used for writes whose root is an opaque captured /
    /// imported binding — the analog of the TS frontend's `captured` hidden case.
    fn push_hidden(&mut self, line: usize, evidence: String, subreason: &str) {
        let kind = EffectKind::HiddenMutation;
        let tier = Tier::Heuristic;
        let class = kind.base_class();
        self.effects.push((
            Effect {
                kind,
                class,
                discounted_to: None,
                weight: weight_for_class(class),
                line,
                tier,
                hidden: true,
                evidence,
                discount: None,
                subreason: Some(subreason.to_owned()),
                confidence: detection_confidence(tier, false, false),
            },
            false,
        ));
    }
```

Add `#[allow(dead_code)]` on the method for this commit only (P4 removes the allow when it wires the call).

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-python push_hidden_emits_hidden_mutation_with_subreason`, then `cargo clippy -p fxrank-lang-python --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `git commit -m "feat(python): add hidden-aware push_hidden to MutSink"`

### Task P2: Thread the `ImportTable` into `mutation::detect`

**Files:**
- Modify: `crates/fxrank-lang-python/src/detect/mutation.rs` (`detect` lines 56–79; `MutSink` struct lines 285–296; test helpers `mutation_effects`/`mutation_evidence` lines 555–591; the P1 test literal)
- Modify: `crates/fxrank-lang-python/src/detect/mod.rs` (call site, line 922)

**Interfaces:**
- Consumes: `&Imports` (`crate::imports::Imports`, already built in `analyze_unit` as `imports`).
- Produces: `pub fn detect(unit: &FnUnit, imports: &Imports, span: &SpanIndex) -> Vec<(Effect, bool)>`; `MutSink` gains `imports: &'a Imports`.

- [ ] **Step 1: Write the failing test** — add to `mod tests`:

```rust
/// PREREQ 2: mutation::detect accepts the ImportTable so F5 + F1 can resolve roots.
#[test]
fn detect_accepts_imports_param() {
    let src = "def f(lst):\n    lst.append(1)\n";
    let module = libcst_native::parse_module(src, None).unwrap();
    let imports = crate::imports::Imports::build(&module);
    let span = crate::source::SpanIndex::new(src);
    let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
    let (units, _) = functions::collect(&module, src, &span, &anchors);
    let f = units.iter().find(|u| u.symbol == "f").unwrap();
    let pairs = detect(f, &imports, &span);
    assert!(
        pairs.iter().any(|(e, _)| e.kind == ParamMutation),
        "lst.append where lst is a param → ParamMutation, got: {:?}",
        pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
    );
}
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-python detect_accepts_imports_param`. Expected: **compile error** `this function takes 2 arguments but 3 arguments were supplied`.

- [ ] **Step 3: Implement** — add `use crate::imports::Imports;`. Rewrite the head of `detect` (lines 56–79):

```rust
pub fn detect(unit: &FnUnit, imports: &Imports, span: &SpanIndex) -> Vec<(Effect, bool)> {
    let params = collect_param_names(unit.params);
    let mut globals: HashSet<String> = HashSet::new();
    let mut nonlocals: HashSet<String> = HashSet::new();
    let mut locals: HashSet<String> = HashSet::new();
    prescan_body(&unit.body, &mut globals, &mut nonlocals, &mut locals);

    let is_init = unit.symbol == "__init__";
    let mut sink = MutSink {
        params: &params,
        globals: &globals,
        nonlocals: &nonlocals,
        locals: &locals,
        imports,
        is_init,
        span,
        effects: Vec::new(),
    };
    walk_own_body(unit, &mut sink);
    sink.effects
}
```

Add the field to `MutSink` (lines 285–296), after `locals`:

```rust
    /// File-wide import table — lets the cascade resolve an import-rooted write (F5)
    /// and distinguish a captured opaque binding (F1) from a known module.
    imports: &'a Imports,
```

Add `#[allow(dead_code)]` on the `imports` field for this commit (P3 reads it). Update the call site in `mod.rs:922`:

```rust
    let mut_pairs = mutation::detect(unit, imports, span);
```

In `mutation_effects`/`mutation_evidence` (lines 555–591) and the P1 test, after each `let module = …`/before constructing the sink, add `let imports = crate::imports::Imports::build(&module);` and pass `&imports` to `detect(...)`/the `MutSink { …, imports: &imports, … }` literal.

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-python` (whole crate — confirm the four existing mutation tests + the P1 test still pass), then `cargo clippy -p fxrank-lang-python --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `git commit -m "feat(python): thread ImportTable into mutation::detect"`

### Task P3: F5 — import-rooted write → `global.mutation`/6

**Files:**
- Modify: `crates/fxrank-lang-python/src/detect/mutation.rs` (`classify_and_push` lines 396–460 — insert after the `locals` arm, before the fall-off; remove the P2 `#[allow(dead_code)]` on `imports`)
- Modify: `crates/fxrank-lang-python/tests/fixtures/mutation.py` (append fixture)
- Test: `crates/fxrank-lang-python/src/detect/mutation.rs::tests`

**Interfaces:**
- Consumes: `self.imports.resolve(&root)` → `Option<&str>` (imports.rs:68).
- Produces: a `classify_and_push` arm emitting `EffectKind::GlobalMutation`/6/Heuristic/`contained:false` when the root resolves through imports. Order: after `locals` (a same-named local shadows the import), before the F1 fallback.

- [ ] **Step 1: Write the failing test** — append to `tests/fixtures/mutation.py`:

```python
import config  # module-level import — `config` resolves through the ImportTable

def mutates_imported_module():
    config.settings.append(1)  # F5: root `config` → import → global.mutation/6, contained=false
```

Add to `mod tests`:

```rust
/// F5: a write whose root resolves through the ImportTable is module-level state
/// escaping the function → global.mutation/6, contained=false. Inserted AFTER the
/// `locals` arm (a same-named local shadows the import) and BEFORE the F1 fallback.
#[test]
fn import_rooted_write_is_global_mutation() {
    let m = mutation_effects("mutation");
    assert!(
        m["mutates_imported_module"].contains(&(GlobalMutation, false)),
        "config.settings.append(…) where `config` is imported must be GlobalMutation(false), got: {:?}",
        m["mutates_imported_module"]
    );
}
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-python import_rooted_write_is_global_mutation`. Expected: **FAIL** — pre-fix the write falls off with no emission → `got: []`.

- [ ] **Step 3: Implement** — in `classify_and_push`, change the `locals` arm (lines 453–455) to `return` after pushing, then insert the F5 arm:

```rust
        // Local binding mutation: root is locally assigned (and not global/nonlocal).
        if self.locals.contains(&root) {
            self.push(EffectKind::LocalMutation, Tier::Exact, line, evidence, true);
            return;
        }

        // F5: root resolves through the ImportTable → module-level state (the imported
        // module/name) escaping the function. global.mutation (class 6, Heuristic),
        // contained=false. A same-named LOCAL already won above.
        if self.imports.resolve(&root).is_some() {
            self.push(
                EffectKind::GlobalMutation,
                Tier::Heuristic,
                line,
                format!("{evidence} (imported `{root}`)"),
                false,
            );
            return;
        }
```

Remove the `#[allow(dead_code)]` from `MutSink.imports`. (Tier is `Heuristic`, not `Exact`: import name-resolution is a syntactic guess, unlike the `Exact` `global`/`nonlocal` declaration arms.)

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-python import_rooted_write_is_global_mutation`, then `cargo test -p fxrank-lang-python` (the four existing tests unaffected — F5 sits after `params`/`locals`), then `cargo clippy -p fxrank-lang-python --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `git commit -m "feat(python): F5 import-rooted write → global.mutation/6"`

### Task P4: F1/F3 — cascade-tail fallback → `hidden.mutation`/3 with `subreason: "captured-binding"`

**Files:**
- Modify: `crates/fxrank-lang-python/src/detect/mutation.rs` (replace the no-emit fall-off, lines 457–459, now after the F5 arm; remove the P1 `#[allow(dead_code)]` on `push_hidden`)
- Modify: `crates/fxrank-lang-python/tests/fixtures/mutation.py` (append fixture)
- Test: `crates/fxrank-lang-python/src/detect/mutation.rs::tests`

**Interfaces:**
- Consumes: `self.push_hidden(line, evidence, "captured-binding")` (P1).
- Produces: the cascade tail emits `HiddenMutation`/3/`hidden:true`/`subreason:"captured-binding"`/`contained:false` instead of dropping. F3 satisfied by the same arm.

- [ ] **Step 1: Write the failing test** — append to `tests/fixtures/mutation.py`:

```python
def captures_outer_binding():
    outer = []
    def inner():
        outer.append(1)  # F1: `outer` is none of self/global/nonlocal/param/local-here
                         # nor an import → captured opaque binding → hidden.mutation/3
    return inner
```

Add to `mod tests`:

```rust
/// F1/F3: a write whose root resolves to NONE of {self, globals, nonlocals, params,
/// locals, import} is a captured outer/opaque binding. Pre-fix the cascade fell off
/// silently; now it emits hidden.mutation/3, hidden=true, contained=false, subreason
/// "captured-binding" (the Python analog of the TS `captured` hidden case).
#[test]
fn captured_binding_subreason_is_set() {
    let src = std::fs::read_to_string("tests/fixtures/mutation.py").unwrap();
    let module = libcst_native::parse_module(&src, None).unwrap();
    let imports = crate::imports::Imports::build(&module);
    let span = crate::source::SpanIndex::new(&src);
    let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
    let (units, _) = functions::collect(&module, &src, &span, &anchors);
    let inner = units.iter().find(|u| u.symbol == "inner").unwrap();
    let pairs = detect(inner, &imports, &span);
    let hidden = pairs
        .iter()
        .find(|(e, _)| e.kind == HiddenMutation)
        .map(|(e, _)| e)
        .expect("inner must emit a HiddenMutation");
    assert_eq!(hidden.class, 3);
    assert!(hidden.hidden, "captured-binding HiddenMutation must be hidden:true");
    assert_eq!(hidden.subreason.as_deref(), Some("captured-binding"));
    assert!(
        pairs.iter().any(|(e, c)| e.kind == HiddenMutation && !*c),
        "captured-binding write escapes — contained=false"
    );
}
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-python captured_binding_subreason_is_set`. Expected: **FAIL** — panics at `.expect("inner must emit a HiddenMutation")` (pre-fix the cascade falls off → no emit).

- [ ] **Step 3: Implement** — replace the no-emit fall-off comment (lines 457–459, the tail of `classify_and_push`, after the F5 arm) with:

```rust
        // F1: the root resolves to NONE of self/global/nonlocal/param/local/import —
        // a captured outer binding (a closed-over variable, or a module-level name
        // not declared `global`). The write escapes through an opaque channel we
        // cannot bound syntactically → hidden.mutation (class 3, hidden:true,
        // contained:false), subreason "captured-binding". Mirrors the TS `captured`
        // hidden case (Milestone A left it un-emitted).
        self.push_hidden(line, evidence, "captured-binding");
```

Remove the `#[allow(dead_code)]` from `push_hidden` (P1).

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-python captured_binding_subreason_is_set`, then `cargo test -p fxrank-lang-python`. The four existing tests must still pass — notably `plain_local_binding` stays empty because a plain bare-name binding `y = 1` is dropped earlier in `on_assign_target` (line 379) and never reaches `classify_and_push`; F1 fires only for method/subscript/aug writes on an unresolved root. Then `cargo clippy -p fxrank-lang-python --all-targets -- -D warnings`.
- [ ] **Step 5: Snapshot review** — `cargo insta test -p fxrank-lang-python`. The dogfood fixture has no captured/import write → expect **no pending**. If a pending diff appears, **stop and inspect** — only accept an intended, explained change.
- [ ] **Step 6: Commit** — `git commit -m "feat(python): F1/F3 captured-binding write → hidden.mutation/3"`

> **Boundary-discount note (no separate task):** both new effects (F5 `global.mutation`, F1 `hidden.mutation`) are `contained:false`, and `analyze_unit`'s discount loop (mod.rs:923–941) only fires for `contained && coverage != None` — so neither is ever boundary-discounted. Correct by construction; guarded by the existing `boundary_discount_zeros_contained_local_when_typed` test (which exercises only the `contained:true` `local.mutation` path).

---

# TS frontend (Tasks T1–T2)

**Intentional deltas:** TS is the *reference* for F1 (captured→hidden already correct) and F5 (imports→global already correct) — no change there. Only F4 (constructor breadth) and F3 (captured subreason) apply. **No committed TS snapshot changes:** `worked.ts` has no constructor; the React fixtures have no constructor method/subscript-on-`this` write (F4 no churn); and **no snapshotted fixture contains a captured-binding `hidden.mutation`** — the only `hidden.mutation` in any TS snapshot is the `uncontrolled_cell` *ref-cell* write (`inputRef.current = …`, already `subreason: "ref-cell-write"`), which T2 leaves untouched. So both F4 and F3 are proven by **direct-assertion unit tests**, not snapshots.

### Task T1: F4 — constructor breadth parity (only a direct `this.<ident>` field-init stays contained)

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/mutation.rs` (`record_write` lines 136–184; `classify` lines 198–217; add `direct_this_field`/`strip_place_wrappers` helpers near `base_ident` ~line 357)
- Test: `crates/fxrank-lang-ts/src/detect/mutation.rs::tests`

**Interfaces:**
- Consumes: the `place: &Expr` already in `record_write` (line 136), `self.is_constructor`, and the write `verb`.
- Produces: `classify` gains two params — `fn classify(&self, base: &str, place: &Expr, assign_like: bool) -> Classification`. The sole caller is `record_write`. `assign_like` is `!verb.starts_with('.')` (a method-call receiver is never a direct field-init). A free fn `direct_this_field(place)` decides "is the assignment target exactly `this.<ident>`".

- [ ] **Step 1: Write the failing test** — add to `mod tests` (uses the existing `detect_in_fn` helper; constructor unit symbol is `<class>.constructor`):

```rust
    #[test]
    fn ctor_direct_field_init_stays_local_mutation() {
        // F4: a DIRECT field-init `this.x = 1` in a constructor stays
        // local.mutation/1/contained (MUST NOT regress).
        let effects = detect_in_fn("class C { x = 0; constructor(){ this.x = 1; } }", "C.constructor");
        let e = effects.iter().find(|(e, _)| e.kind == EffectKind::LocalMutation)
            .expect("direct this.x = 1 must be local.mutation");
        assert_eq!(e.0.effective_class(), 1);
        assert!(e.1, "direct field-init must be contained == true");
        assert!(!effects.iter().any(|(e, _)| e.kind == EffectKind::ThisMutation),
            "direct field-init must not escape to this.mutation");
    }

    #[test]
    fn ctor_method_call_on_this_escapes_to_this_mutation() {
        // F4: a mutating-method receiver on `this` (`this.items.push(1)`) is NOT a
        // direct field-init — escapes to this.mutation/3/not-contained.
        let effects = detect_in_fn("class C { items = []; constructor(){ this.items.push(1); } }", "C.constructor");
        let e = effects.iter().find(|(e, _)| e.kind == EffectKind::ThisMutation)
            .expect("this.items.push(1) must escape to this.mutation");
        assert_eq!(e.0.effective_class(), 3);
        assert!(!e.1, "method-call receiver on this must be contained == false");
        assert!(!effects.iter().any(|(e, c)| *c && e.kind == EffectKind::LocalMutation),
            "method-call receiver on this must not collapse to contained local.mutation");
    }

    #[test]
    fn ctor_subscript_write_on_this_escapes_to_this_mutation() {
        // F4: a subscript write on `this` (`this[i] = 1`) is a member-chain write,
        // not a direct `this.<ident>` field-init — escapes to this.mutation/3.
        let effects = detect_in_fn("class C { constructor(i: number){ this[i] = 1; } }", "C.constructor");
        let e = effects.iter().find(|(e, _)| e.kind == EffectKind::ThisMutation)
            .expect("this[i] = 1 must escape to this.mutation");
        assert_eq!(e.0.effective_class(), 3);
        assert!(!e.1, "subscript write on this must be contained == false");
    }
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-ts ctor_direct_field_init_stays_local_mutation ctor_method_call_on_this_escapes_to_this_mutation ctor_subscript_write_on_this_escapes_to_this_mutation`. Expected: the direct-init test **passes** (already correct), the method-call and subscript tests **fail** — today `classify("this")` returns `local.mutation`/1/contained for all `this` writes in a constructor, so `find(ThisMutation)` panics.

- [ ] **Step 3: Implement** — thread the place shape + `assign_like` into the constructor decision.

Change `record_write` (around line 168) to compute `assign_like` and pass both to `classify`:

```rust
        let c = if is_ref_cell {
            // … ref-cell branch unchanged …
        } else {
            // A mutating-method receiver (verb begins with '.') is never a direct
            // field-init even when its place is `this.<ident>` — pushing onto
            // `this.items` mutates field contents, so it escapes. Only a true
            // assignment target can be a contained ctor field-init.
            let assign_like = !verb.starts_with('.');
            self.classify(&base, place, assign_like)
        };
```

Change `classify` (lines 198–206):

```rust
    fn classify(&self, base: &str, place: &Expr, assign_like: bool) -> Classification {
        use EffectKind::*;
        if base == "this" {
            if self.is_constructor && assign_like && direct_this_field(place) {
                // A DIRECT field-init `this.<ident> = …` — honest, bounded local init.
                Classification::new(LocalMutation, 1, true, false, Tier::Heuristic, "ctor this")
            } else {
                // Normal method, OR a constructor write that is NOT a direct field-init
                // (method receiver `this.xs.push`, subscript `this[i]`, deeper chain
                // `this.a.b`) — escapes, not contained.
                Classification::new(ThisMutation, 3, false, false, Tier::Heuristic, "this field")
            }
        } else if self.locals.contains(base) {
            Classification::new(LocalMutation, 1, true, false, Tier::Exact, "local")
        } else if self.params.contains(base) {
            Classification::new(ParamMutation, 3, false, false, Tier::Heuristic, "param")
        } else if base == "globalThis" || base == "window" || self.imports.resolve(base).is_some() {
            Classification::new(GlobalMutation, 6, false, false, Tier::Heuristic, "global")
        } else {
            Classification::new(HiddenMutation, 3, false, true, Tier::Heuristic, "captured")
        }
    }
```

Add the helpers near `base_ident` (~line 357):

```rust
/// `true` iff `place` is a DIRECT named-field place off bare `this` — `this.x`
/// or `this.#x` (a private field init is still a direct field init). The only
/// constructor write shape that stays a contained `local.mutation` (F4). A
/// computed `this[i]`, a method-call receiver (`this.xs.push`), or a deeper
/// chain (`this.a.b`) all return `false` and escape to `this.mutation`.
fn direct_this_field(place: &Expr) -> bool {
    match place {
        Expr::Member(m) => {
            // A *named* field place — `this.x` (Ident) or `this.#x` (PrivateName) —
            // is a direct field-init. A computed `this[i]` (Computed) is a member-chain
            // write, not a field-init, and escapes. (Match `&m.prop` by reference.)
            if !matches!(&m.prop, MemberProp::Ident(_) | MemberProp::PrivateName(_)) {
                return false;
            }
            matches!(strip_place_wrappers(&m.obj), Expr::This(_))
        }
        Expr::Paren(p) => direct_this_field(&p.expr),
        Expr::TsAs(e) => direct_this_field(&e.expr),
        Expr::TsNonNull(e) => direct_this_field(&e.expr),
        Expr::TsTypeAssertion(e) => direct_this_field(&e.expr),
        Expr::TsSatisfies(e) => direct_this_field(&e.expr),
        _ => false,
    }
}

/// Strip `Paren` / TS-only wrappers to reach the underlying receiver, mirroring
/// what `base_ident` sees through.
fn strip_place_wrappers(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(p) => strip_place_wrappers(&p.expr),
        Expr::TsAs(e) => strip_place_wrappers(&e.expr),
        Expr::TsNonNull(e) => strip_place_wrappers(&e.expr),
        Expr::TsTypeAssertion(e) => strip_place_wrappers(&e.expr),
        Expr::TsSatisfies(e) => strip_place_wrappers(&e.expr),
        other => other,
    }
}
```

(Ensure `MemberProp` is imported in the `use swc_*` block — check the existing imports; add it if missing.)

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-ts ctor_direct_field_init_stays_local_mutation ctor_method_call_on_this_escapes_to_this_mutation ctor_subscript_write_on_this_escapes_to_this_mutation`. Then regression-guard: `cargo test -p fxrank-lang-ts contained_flag_tracks_escape classifies_mutation_by_escape useref local_write_stays_local` (the `WithCtor.constructor` direct-init, normal-method `this.mutation`, param-shadow, ref-cell paths must still pass).
- [ ] **Step 5: Snapshot guard** — `cargo test -p fxrank-lang-ts --test snapshots --test react`. Expected: clean, **no** pending `.snap.new` (no committed fixture has a constructor method/subscript-on-`this` write). If pending, investigate before continuing.
- [ ] **Step 6: Commit** — `git commit -m "fix(ts): F4 constructor breadth parity — only direct this.<ident> init stays contained"`

### Task T2: F3 — captured fallback gains subreason `"captured-binding"`

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/mutation.rs` (the `classify` "captured" fallback, lines 213–216)
- Test: `crates/fxrank-lang-ts/src/detect/mutation.rs::tests`

**Interfaces:** No signature change. The captured fallback `Classification` gains `.with_subreason("captured-binding")` (the builder the ref-cell path already uses at line 164). Kind/class/hidden/contained unchanged.

- [ ] **Step 1: Write the failing test** — add to `mod tests` (a module-level `let` write is the `"captured"` fallback):

```rust
    #[test]
    fn captured_binding_write_has_captured_subreason() {
        // F3: a captured outer-binding write (module-level `counter`, not local/param)
        // is hidden.mutation/3 — and now carries subreason "captured-binding"
        // (reporting only; class/kind unchanged).
        let effects = detect_in("let counter = 0; function C(){ counter += 1; }");
        let e = effects.iter().find(|(e, _)| e.kind == EffectKind::HiddenMutation)
            .expect("captured write must be hidden.mutation");
        assert_eq!(e.0.effective_class(), 3);
        assert!(e.0.hidden, "captured write stays hidden == true");
        assert!(!e.1, "captured write stays contained == false");
        assert_eq!(e.0.subreason.as_deref(), Some("captured-binding"));
        assert_ne!(e.0.subreason.as_deref(), Some("ref-cell-write"));
    }
```

- [ ] **Step 2: Run test, verify it fails** — `cargo test -p fxrank-lang-ts captured_binding_write_has_captured_subreason`. Expected: fails on the subreason assert — today the captured fallback builds with no subreason (`left: None`).

- [ ] **Step 3: Implement** — add the subreason to the captured fallback only (lines 213–216); leave the ref-cell path (line 164) untouched:

```rust
        } else {
            // Captured outer/module binding — hidden from the signature.
            Classification::new(HiddenMutation, 3, false, true, Tier::Heuristic, "captured")
                .with_subreason("captured-binding")
        }
```

- [ ] **Step 4: Run test, verify pass** — `cargo test -p fxrank-lang-ts captured_binding_write_has_captured_subreason`. Regression-guard: `cargo test -p fxrank-lang-ts useref_current_write_is_hidden_mutation classifies_mutation_by_escape` (ref-cell subreason `"ref-cell-write"` and `viaClosure`→`hidden.mutation` still hold).
- [ ] **Step 5: Snapshot guard (no churn expected)** — `cargo test -p fxrank-lang-ts --test snapshots --test react`. Expected: **clean, no pending `.snap.new`** — no snapshotted TS fixture contains a captured-binding `hidden.mutation` (the only snapshotted `hidden.mutation` is the `uncontrolled_cell` ref-cell write, which T2 does not touch). The behavior is proven by the Step-1 unit test. If insta reports a pending diff, **stop and investigate** — it means a fixture unexpectedly exercises the captured fallback.
- [ ] **Step 6: Commit** — `git commit -m "feat(ts): captured-binding mutation gains \"captured-binding\" subreason (F3)"`

---

# Cross-cutting tasks

### Task C: Cross-frontend conformance + full workspace gates

**Files:** Test-only — `crates/fxrank-lang-{rust,ts,python}/tests/` (the per-fix tests already added in R/P/T); this task adds the explicit cross-frontend parity assertions §6 calls for and runs the full gates.

**Interfaces:** none (verification task).

- [ ] **Step 1: Confirm the per-frontend parity tests exist and name the same canonical facts.** Verify these tests are present and green (added in R/P/T):
  - F1 captured→`hidden.mutation`/3/hidden: Rust `unresolved_free_binding_write_is_hidden_mutation_class_3`, TS `captured_binding_write_has_captured_subreason`, Python `captured_binding_subreason_is_set`.
  - F2 real-static→`global.mutation`/6 + UPPERCASE-non-static NOT global: Rust `lowercase_static_mut_assign_is_global_mutation_class_6`, `atomic_static_store_is_global_mutation_class_6`, `unbound_uppercase_non_static_is_not_global_mutation`, `self_interior_mutation_stays_hidden_not_global`.
  - F4 ctor direct vs method/subscript: TS `ctor_direct_field_init_stays_local_mutation`, `ctor_method_call_on_this_escapes_to_this_mutation`, `ctor_subscript_write_on_this_escapes_to_this_mutation` (Python reference: `self_method_and_subscript_mutations_escape_even_in_init`).
  - F5 import→`global.mutation`/6: Rust `import_resolved_write_base_is_global_mutation_class_6`, Python `import_rooted_write_is_global_mutation`.
  - Run them per package (cargo accepts only **one** filter substring per invocation):
    ```bash
    cargo test -p fxrank-lang-rust   # all rust-frontend tests incl. the new F1/F2/F5/F3
    cargo test -p fxrank-lang-python # incl. captured_binding / import_rooted
    cargo test -p fxrank-lang-ts     # incl. ctor_* / captured_binding_write_has_captured_subreason
    ```
    Expected: all pass.

- [ ] **Step 2: Re-assert the anti-Goodhart canary across the suite.** Run: `cargo test --workspace hidden_mutation_scores_higher_than_declared_mut_self snapshot_inversion_pair`. Expected: pass (the alignment must not have perturbed the inversion).

- [ ] **Step 3: Confirm React internals un-regressed.** Run: `cargo test -p fxrank-lang-ts --test react`. Expected: pass with **no** snapshot churn — score-inheritance, `EffectInRender`, and ref-cell-inheritance are all unchanged (the alignment touches none of them).

- [ ] **Step 4: Full CI gates.** Run, in order:
  ```bash
  cargo fmt --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
  Expected: all green. Also confirm the slim builds still compile: `cargo build -p fxrank --no-default-features --features rust` / `--features ts` / `--features python`.

- [ ] **Step 5: Commit** (if Steps 1–4 added any cross-frontend test) — `git commit -m "test: cross-frontend mutation-alignment conformance (spec 008 §6)"`. If no new test was needed (the per-fix tests already cover §6), skip the commit and note it.

### Task D: Dogfood pass + delta note

**Files:** Create `docs/008-dogfood-deltas.md` (a short note recording observed ranking changes).

**Interfaces:** none.

- [ ] **Step 1: Capture the baseline.** From the merge-base (pre-008) or via `git stash`, build and scan a representative corpus of each language. Run and save output:
  ```bash
  cargo run -p fxrank -- scan crates/ | jq '.hotspots[] | {id, own_score}' > /tmp/008-rust-before.json
  # plus a TS repo and a Python repo from the usual dogfood corpora (local React/TS, Python, Rust)
  ```
  (Use the dogfood repos recorded in project memory; do not commit their output.)

- [ ] **Step 2: Scan with the aligned build** and diff:
  ```bash
  cargo run -p fxrank -- scan crates/ | jq '.hotspots[] | {id, own_score}' > /tmp/008-rust-after.json
  diff <(jq -S . /tmp/008-rust-before.json) <(jq -S . /tmp/008-rust-after.json) || true
  ```
  Expected: the only deltas are the intended ones — Python functions gaining `hidden`/`global` effects (were pure); Rust **unresolved UPPERCASE non-static bases** moving `global`→`hidden` (uppercase *locals* stay `local.mutation`) and real statics now caught as `global` (incl. atomics via `.store()` etc.; **not** `Mutex`/`RwLock` `.lock()`, which `is_interior_mutator` does not cover); TS constructor method/subscript writes moving `local`→`this.mutation`. **No unexplained movement.**

- [ ] **Step 3: Write `docs/008-dogfood-deltas.md`** — a short bullet list: for each frontend, the functions whose ranking moved and which F-number caused it. If any movement is unexplained, **stop** — it is a bug, not a delta; fix the responsible task before proceeding.

- [ ] **Step 4: Commit** — `git commit -m "docs: record spec-008 dogfood ranking deltas"`

### Task G: Write the descriptive guideline

**Files:** Create `docs/mutation-classification-guideline.md`.

**Interfaces:** none (documentation; written last so it reflects realized behavior).

- [ ] **Step 1: Write the guideline** with this exact content (descriptive, advisory — NOT a normative contract; a future frontend MAY read it, nothing MUST conform):

````markdown
# Mutation-classification guideline (descriptive)

How FxRank's language frontends classify a write site into a mutation effect. This
is a **descriptive reference** — it documents the shared model and the honest
per-language differences. It is not a contract; nothing is required to conform.

## Shared model

Each frontend (`crates/fxrank-lang-{rust,ts,python}/src/detect/mutation.rs`) reduces
a write to a **base name**, classifies it against **flat per-unit binding sets** via a
**fixed priority cascade**, and emits an `EffectKind` (`fxrank-core`). No lexical scope
stack; an *unresolved base* is the proxy for "captured/module". TS/Python stop at nested
functions; Rust's mutation walker descends into closures and block-local `fn` items.

## Canonical mapping

| write case | EffectKind | class | contained | hidden | tier |
|---|---|:--:|:--:|:--:|:--:|
| body-local place mutation | local.mutation | 1 | yes | no | Exact |
| param place mutation (no declared channel) | param.mutation | 3 | no | no | Heuristic |
| `&mut` param / `&mut self` (Rust) | param.mutation | 3 (discounted_to 1/2 when safe) | — | no | Heuristic |
| receiver field, normal method | this.mutation | 3 | no | no | Heuristic |
| constructor *direct* field-init | local.mutation | 1 | yes | no | Heuristic |
| explicit declared capture (`nonlocal`, Python) | this.mutation | 3 | no | no | Exact |
| real global / static | global.mutation | 6 | no | no | Exact/Heuristic |
| write to imported binding | global.mutation | 6 | no | no | Heuristic |
| interior-mutability / ref-cell write | hidden.mutation | 3 | no | yes | Heuristic |
| captured-enclosing / unresolved base | hidden.mutation | 3 | no | yes | Heuristic |

`hidden.mutation` subreasons: `"interior-mut"` (Rust interior-mutability), `"ref-cell-write"`
(TS `useRef().current`), `"captured-binding"` (the unresolved fallback, all three).

## Honest per-language differences (intentional — not aligned)

- **Rust mut-channel discount** (`apply_discount`, `&mut`→−2/`&mut self`→−1, unsafe-cancel) —
  Rust ownership; TS/Python have no `&mut`.
- **TS/Python typed-boundary discount** (`apply_boundary_discount`, floor 0) — gradual typing;
  Rust is fully typed and does not apply it. Rust's `Effect` has no `contained` field.
- **Python `nonlocal`→this.mutation/Exact/not-hidden** — an explicitly declared capture is
  visible (not hidden); distinct kind from the implicit-capture `hidden.mutation` (same class 3).
- **Plain rebind `x = …`** — Python no-emits (name-rebinding ≠ mutation); TS/Rust emit
  local.mutation/1 (variable-slot reassignment).
- **Per-language mutating-method allowlists** — language-appropriate vocabularies.
- **Destructuring-target writes** — dropped in all three (accepted limitation).
- **Constructor breadth** — only a *direct* `this.x=`/`self.attr=` field-init is contained-local;
  a method/subscript write on the receiver escapes (TS aligned to Python's rule).

## The differentiator (must hold)

The anti-Goodhart inversion: a hidden interior-mutability write (`hidden.mutation`/3, no
discount) scores *higher* than an honest declared `&mut self` write (`param.mutation`/3
discounted to 2). Hidden state scores above declared state.

## Per-frontend realization

- **Rust** — `static`/`static mut`/atomic statics resolve via the real static-name set
  (no casing proxy); interior-mut on shared `&` receivers → hidden; closures share the parent
  unit's sets.
- **TS** — `var` hoist vs `let`/`const` TDZ unmodeled; `useRef().current` → ref-cell hidden;
  imports + `globalThis`/`window` → global.
- **Python** — `global`/`nonlocal` pre-scanned; comprehension scopes unmodeled; captured/module
  fallback → hidden.
````

- [ ] **Step 2: Commit** — `git commit -m "docs: add descriptive mutation-classification guideline (spec 008)"`

---

## Plan self-review (author checklist — done before handoff)

- **Spec coverage:** §1/§1.1 → Guideline (G) + the canonical facts in Global Constraints; §2 KEEP → Guideline + regression-guard tests in every section; §3 F1–F5 → R3/R1+R2/R5/T1+P4/R4+P3; §4 per-frontend plan → R/P/T sections; §5 outputs → Guideline (G) + per-fix commits; §6 validation → Task C + per-fix snapshot steps + Task D; §7 out-of-scope → honored (no shared layer/contract/scope rewrite); §8 tasks → all present. No gaps.
- **Placeholder scan:** none — every code step shows real code; every command shows the exact invocation + expected result.
- **Type consistency:** `mutation::detect` new signatures consistent (Rust gains `statics, imports`; Python gains `imports`); subreason strings consistent (`"interior-mut"`/`"captured-binding"`/`"ref-cell-write"`); canonical classes consistent (1/3/6) across all sections and the guideline.
