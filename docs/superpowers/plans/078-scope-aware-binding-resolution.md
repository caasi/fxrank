# Scope-aware binding resolution — Python frontend (078) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Python frontend's flat per-unit binding *sets* (`globals`/`nonlocals`/`locals`/`params`) and its use of the file-wide `Imports` table for binding decisions with a **file-level scope tree** that resolves a name-use to its **nearest-enclosing binding** (a `BindingOrigin`), per `docs/scope-aware-binding-resolution-guideline.md`.

**Architecture:** A new frontend-internal `crate::scope` module builds one `ScopeTree` per file (`module → function/method → block`), collecting each scope's bindings with their `BindingOrigin`. The mutation classifier stops asking flat sets "is this name a local/param/import?" and instead calls `ScopeTree::resolve(unit, name, at)` → `BindingOrigin`, then maps origin→effect exactly as today. Migration is **compat-mode first** (byte-identical snapshots while porting), then **labelled intentional deltas** (shadowing precision, cross-function-collision close, `captured-binding` FP shrink #55).

**Tech Stack:** Rust, `libcst_native` (Python CST), `insta` (snapshots). Crate `crates/fxrank-lang-python`.

## Global Constraints

- **Load-bearing invariant (mechanically checked):** binding resolution stays **frontend-internal**. `fxrank-core` and `UnitRecord` gain **zero** scope/binding types or fields. A CI/review grep asserts `fxrank-core` references no `BindingOrigin`/`ScopeTree`/`scope`. There is **no** `fxrank-analysis` crate.
- **`BindingOrigin` is the guideline's vocabulary** (verbatim): `Param`, `Local`, `ModuleImport`, `FromImport`, `ModuleBinding`, `Global`, `EnclosingCaptured`, `HostGlobalIO`, `Unresolved`.
- **Compat gate:** every port task ends by asserting **existing snapshots are byte-identical** (`cargo insta test`; no accepted changes). Only Phase 3 tasks change snapshots, each labelled an *intentional delta*.
- **TDD:** test first, watch it fail, minimal implement, watch it pass, commit. One commit per step group.
- **Scope:** Python frontend only. Do **not** touch TS/Rust or the interim Rust `seed_pat_locals` patch (superseded in the Rust plan, not here). `#78` emits nothing for the deferred `#81` shared-global axis.
- **Commands:** `cargo test -p fxrank-lang-python`; single test `cargo test -p fxrank-lang-python <name>`; snapshots `cargo insta test -p fxrank-lang-python` then `cargo insta accept` only for labelled deltas; gates `cargo fmt --check`, `cargo clippy -p fxrank-lang-python --all-targets -- -D warnings`.

---

## File Structure

- **Create `src/scope.rs`** — the scope tree + resolver. One responsibility: name-use → `BindingOrigin` over a file scope tree. Frontend-internal (`pub(crate)`).
- **Modify `src/lib.rs`** (~line 84) — build one `ScopeTree` per file alongside `Imports`, thread it into per-unit analysis.
- **Modify `src/detect/mod.rs`** (~line 1016, `analyze_unit`) — pass the `ScopeTree` to `mutation::detect`.
- **Modify `src/detect/mutation.rs`** — `detect()` takes `&ScopeTree`; the `classify_and_push` cascade and the `on_call` #50 gate consult `resolve()` instead of the flat sets. The `prescan_*` functions and the `params`/`globals`/`nonlocals`/`locals` fields are removed once fully ported.
- **Modify `src/imports.rs`** — `Imports` stays the module-resolution source of truth for `calls.rs` (unchanged there); only its use as a *binding* oracle in `mutation.rs` moves to `scope.rs`. `is_module_import` stays (the scope builder consults it).
- **Tests:** unit tests inline in `src/scope.rs`; behavior tests extend `src/detect/mutation.rs`'s `#[cfg(test)]`; snapshot baseline is `tests/snapshots.rs` + `tests/snapshots/*.snap`.

---

## Phase 0 — Vocabulary types (frontend-internal)

### Task 1: `BindingOrigin` + `BindingId`

**Files:**
- Create: `src/scope.rs`
- Modify: `src/lib.rs` (add `mod scope;`)

**Interfaces:**
- Produces: `pub(crate) enum BindingOrigin { Param, Local, ModuleImport, FromImport, ModuleBinding, Global, EnclosingCaptured, HostGlobalIo, Unresolved }`; `pub(crate) struct BindingId { pub scope_segments: Vec<String>, pub kind_name: String }` (intra-unit id; see guideline §binding-id — `full` = unit `canonical_path` ++ these, but #78 never needs `full`).

- [ ] **Step 1: Write the failing test** (in `src/scope.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_origin_variants_exist() {
        // Compile-time coverage: every guideline origin is representable.
        let _ = [
            BindingOrigin::Param, BindingOrigin::Local, BindingOrigin::ModuleImport,
            BindingOrigin::FromImport, BindingOrigin::ModuleBinding, BindingOrigin::Global,
            BindingOrigin::EnclosingCaptured, BindingOrigin::HostGlobalIo, BindingOrigin::Unresolved,
        ];
        let id = BindingId { scope_segments: vec!["b3".into()], kind_name: "local:x".into() };
        assert_eq!(id.kind_name, "local:x");
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p fxrank-lang-python binding_origin_variants_exist` → FAIL (`scope` module / types not found).

- [ ] **Step 3: Minimal implementation** (top of `src/scope.rs`)

```rust
//! Frontend-internal scope-aware binding resolution (spec 078).
//! See docs/scope-aware-binding-resolution-guideline.md. NOTHING here crosses
//! the UnitRecord boundary; fxrank-core knows none of these types.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingOrigin {
    Param, Local, ModuleImport, FromImport, ModuleBinding, Global,
    EnclosingCaptured, HostGlobalIo, Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindingId {
    pub scope_segments: Vec<String>,
    pub kind_name: String,
}
```
Add `mod scope;` to `src/lib.rs`.

- [ ] **Step 4: Run to verify it passes** — same command → PASS.
- [ ] **Step 5: Commit** — `git add src/scope.rs src/lib.rs && git commit -m "feat(python): BindingOrigin + BindingId vocabulary (#78)"`

---

## Phase 1 — The scope tree + resolver (the engine)

### Task 2: Scope tree types + `build` skeleton (module scope only)

**Files:**
- Modify: `src/scope.rs`

**Interfaces:**
- Consumes: `libcst_native::Module`, `crate::imports::Imports`.
- Produces: `pub(crate) struct ScopeTree { … }`; `impl ScopeTree { pub(crate) fn build(module: &Module, imports: &Imports) -> Self }`; internal `Scope { kind: ScopeKind, parent: Option<ScopeId>, bindings: HashMap<String, BindingOrigin> }`, `enum ScopeKind { Module, Function, Block }`, `type ScopeId = usize`.

- [ ] **Step 1: Failing test** — a module with `import os` and `x = 0` yields a module scope whose bindings resolve `os → ModuleImport`, `x → ModuleBinding`.

```rust
#[test]
fn module_scope_collects_imports_and_module_bindings() {
    let t = build_str("import os\nx = 0\n");
    assert_eq!(t.resolve_in_module("os"), BindingOrigin::ModuleImport);
    assert_eq!(t.resolve_in_module("x"), BindingOrigin::ModuleBinding);
    assert_eq!(t.resolve_in_module("nope"), BindingOrigin::Unresolved);
}
// helper:
fn build_str(src: &str) -> ScopeTree {
    let m = libcst_native::parse_module(src, None).unwrap();
    let imports = crate::imports::Imports::build(&m);
    ScopeTree::build(&m, &imports)
}
```

- [ ] **Step 2: Verify fails** — `cargo test -p fxrank-lang-python module_scope_collects_imports` → FAIL.

- [ ] **Step 3: Implement** — `ScopeTree` holding `Vec<Scope>`, `build` creating the module scope (id 0), populating module-level bindings by walking `module.body` top-level statements: `import` → `ModuleImport` (consult `imports.is_module_import`), `from … import` → `FromImport`, assign/`def`/`class` targets → `ModuleBinding` (reuse `crate::imports::module_bindings` collection logic — extract a shared helper if needed). Add `pub(crate) fn resolve_in_module(&self, name: &str) -> BindingOrigin` looking up scope 0.

```rust
pub(crate) struct ScopeTree { scopes: Vec<Scope> }
struct Scope { kind: ScopeKind, parent: Option<usize>, bindings: std::collections::HashMap<String, BindingOrigin> }
enum ScopeKind { Module, Function, Block }

impl ScopeTree {
    pub(crate) fn build(module: &libcst_native::Module, imports: &crate::imports::Imports) -> Self {
        let mut scopes = vec![Scope { kind: ScopeKind::Module, parent: None, bindings: Default::default() }];
        collect_module_bindings(&module.body, imports, &mut scopes[0].bindings); // import/from/assign/def/class
        Self { scopes }
    }
    pub(crate) fn resolve_in_module(&self, name: &str) -> BindingOrigin {
        self.scopes[0].bindings.get(name).copied().unwrap_or(BindingOrigin::Unresolved)
    }
}
```
Note: `collect_module_bindings` mirrors `imports.rs::module_bindings` + the import-kind split; keep the Python semantics (a `from`-import name is `FromImport`, a plain `import` is `ModuleImport` via `imports.is_module_import`).

- [ ] **Step 4: Verify passes.**
- [ ] **Step 5: Commit** — `feat(python): scope tree module-scope binding collection (#78)`

### Task 3: Function scopes + params/locals + `global`/`nonlocal`

**Files:** Modify `src/scope.rs`

**Interfaces:**
- Produces: `pub(crate) fn resolve(&self, unit_scope: ScopeId, name: &str) -> (BindingOrigin, BindingId)` and a way to get a unit's `ScopeId` (`fn unit_scope_of(&self, symbol: &str) -> Option<ScopeId>` or a scope id threaded from unit collection). Function scopes carry: params → `Param`; body assigns/`for`/`with as`/`except as`/nested `def`&`class` → `Local`; `global x` names → `Global`; `nonlocal x` names → `EnclosingCaptured` (declared form).

- [ ] **Step 1: Failing tests** — reuse the guideline's cases:

```rust
#[test]
fn function_scope_params_and_locals_and_decls() {
    let t = build_str("import os\ndef f(p):\n    global g\n    nonlocal n\n    x = 1\n    p; x; g; n; os\n");
    let f = t.unit_scope_of("f").unwrap();
    assert_eq!(t.resolve(f, "p").0, BindingOrigin::Param);
    assert_eq!(t.resolve(f, "x").0, BindingOrigin::Local);
    assert_eq!(t.resolve(f, "g").0, BindingOrigin::Global);
    assert_eq!(t.resolve(f, "n").0, BindingOrigin::EnclosingCaptured);
    // shadowing: `os` here is the module import (no local os) — nearest-enclosing walks to module
    assert_eq!(t.resolve(f, "os").0, BindingOrigin::ModuleImport);
}

#[test]
fn param_shadows_import() {
    let t = build_str("import os\ndef f(os):\n    os\n");
    let f = t.unit_scope_of("f").unwrap();
    assert_eq!(t.resolve(f, "os").0, BindingOrigin::Param); // nearest-enclosing beats the module import
}
```

- [ ] **Step 2: Verify fails.**
- [ ] **Step 3: Implement** — extend `build` to create a `Function` scope per `def`/`async def`/lambda, parented to its lexical parent; populate params (`Param`), function-body binding forms (`Local`; reuse the existing prescan logic in `mutation.rs::prescan_*` — move it into `scope.rs`), `global`→`Global`, `nonlocal`→`EnclosingCaptured`. Implement `resolve(scope, name)` = walk `scope` → `parent` → … taking the first scope that binds `name`; compute `BindingId` from the walked scope path + `kind:name`. Nested `def`/`class` names are `Local` in their parent (the `seed_defs` rule, guideline; #55 Python case already covered — keep it).

- [ ] **Step 4: Verify passes** (both tests).
- [ ] **Step 5: Commit** — `feat(python): function scopes, params/locals, global/nonlocal (#78)`

### Task 4: Block scopes (`for`/`if`/`with`/`try`/comprehension) + `HostGlobalIo`

**Files:** Modify `src/scope.rs`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn for_and_with_bindings_are_local() {
    let t = build_str("def f(xs):\n    for i in xs:\n        i\n    with open('p') as fh:\n        fh\n");
    let f = t.unit_scope_of("f").unwrap();
    assert_eq!(t.resolve(f, "i").0, BindingOrigin::Local);   // loop target
    assert_eq!(t.resolve(f, "fh").0, BindingOrigin::Local);  // with-as
}
#[test]
fn ambient_storage_globals_are_hostglobalio() {
    // localStorage-style names are TS; Python has none — assert Python does NOT invent HostGlobalIo.
    let t = build_str("def f():\n    open('p')\n");
    let f = t.unit_scope_of("f").unwrap();
    assert_eq!(t.resolve(f, "open").0, BindingOrigin::Unresolved); // builtin, not a binding
}
```
(Note: `HostGlobalIo` is a TS-frontend origin — Python has no ambient storage globals, so the Python resolver never returns it. The variant exists in the shared vocabulary but Python's builder leaves those names `Unresolved`, which the mutation classifier already treats as the honest fallback. This test pins that Python does not misclassify builtins.)

- [ ] **Step 2: Verify fails.**
- [ ] **Step 3: Implement** — create `Block` scopes for suites that introduce bindings (`for`/`async for` targets, `with … as`, `except … as`), parented to the enclosing function/block; comprehensions get their own scope (Python-3 semantics — targets do not leak). Bindings there are `Local`.
- [ ] **Step 4: Verify passes.**
- [ ] **Step 5: Commit** — `feat(python): block + comprehension scopes (#78)`

---

## Phase 2 — Compat-mode port (byte-identical snapshots)

> Each task in this phase ends with `cargo insta test -p fxrank-lang-python` showing **no** pending snapshot changes. If a snapshot moves, the port changed behavior — revert and reconcile before proceeding.

### Task 5: Thread `ScopeTree` into `mutation::detect` (no behavior change yet)

**Files:** Modify `src/lib.rs`, `src/detect/mod.rs`, `src/detect/mutation.rs`

**Interfaces:**
- Consumes: `ScopeTree::build`.
- Produces: `mutation::detect(unit, imports, module_bindings, scope: &ScopeTree, span)` — new `scope` param; body still uses the old flat sets (this task only wires the value through).

- [ ] **Step 1: Failing test** — a compile-level test that `mutation::detect` accepts a `&ScopeTree`:

```rust
#[test]
fn detect_accepts_scope_tree() {
    let src = "def f(lst):\n    lst.append(1)\n";
    let m = libcst_native::parse_module(src, None).unwrap();
    let imports = crate::imports::Imports::build(&m);
    let mb = crate::imports::module_bindings(&m);
    let tree = crate::scope::ScopeTree::build(&m, &imports);
    let span = crate::source::SpanIndex::new(src);
    let anchors = crate::source::lambda_anchors(src).unwrap();
    let (units, _) = crate::functions::collect(&m, src, &span, &anchors);
    let f = units.iter().find(|u| u.symbol == "f").unwrap();
    let pairs = detect(f, &imports, &mb, &tree, &span);
    assert!(pairs.iter().any(|(e, _)| e.kind == fxrank_core::effect::EffectKind::ParamMutation));
}
```

- [ ] **Step 2: Verify fails** — signature mismatch.
- [ ] **Step 3: Implement** — add the `scope: &ScopeTree` param to `detect` and `MutSink`; build the `ScopeTree` in `lib.rs` (once per file) and thread through `analyze_unit` (`mod.rs`) to `mutation::detect`. Do not consult it yet.
- [ ] **Step 4: Verify** — the new test passes; **`cargo insta test` shows no snapshot changes**; full `cargo test -p fxrank-lang-python` green.
- [ ] **Step 5: Commit** — `refactor(python): thread ScopeTree into mutation::detect (no-op) (#78)`

### Task 6: Port the `classify_and_push` cascade onto `resolve()`

**Files:** Modify `src/detect/mutation.rs`

**Interfaces:**
- Consumes: `ScopeTree::resolve(unit_scope, name) -> (BindingOrigin, _)`.
- Produces: `classify_and_push` maps `BindingOrigin` → the *same* `(EffectKind, contained, tier)` the flat cascade produced today (per the guideline's `consuming rule` column).

- [ ] **Step 1: Failing test** — a characterization pin that the origin→effect map matches today for each origin (self/param/local/global/nonlocal/module-import/from-import/module-binding/captured), asserting identical `(EffectKind, contained)` to the current behavior. Copy 6–8 existing mutation cases and re-assert against the ported path.

- [ ] **Step 2: Verify fails** — before the port, `resolve` isn't consulted.
- [ ] **Step 3: Implement** — replace, inside `classify_and_push`, the sequence of `self.params.contains / globals / nonlocals / locals / imports.resolve (F5) / module_bindings` checks with a single `match self.scope.resolve(self.unit_scope, &root).0 { Param => …, Local => …, Global => …, EnclosingCaptured => (nonlocal→ThisMutation else captured→hidden), ModuleImport => …(see Task 7), FromImport|ModuleBinding => GlobalMutation, Unresolved => push_hidden("captured-binding") }`. Keep `self` handling (`root == "self"`) exactly as today (it precedes resolution). Delete the now-dead `prescan_*` + `params`/`globals`/`nonlocals`/`locals` fields.
- [ ] **Step 4: Verify** — new pin passes; **`cargo insta test` byte-identical**; full suite green.
- [ ] **Step 5: Commit** — `refactor(python): classify mutations via ScopeTree::resolve, compat (#78)`

### Task 7: Port the `on_call` #50 module-import gate onto `ModuleImport`

**Files:** Modify `src/detect/mutation.rs`

- [ ] **Step 1: Failing test** — reuse `method_call_suppression_respects_shadowing_and_from_imports` and `method_call_on_bare_imported_name_is_not_mutation`, re-run against the ported gate (they must still pass identically).
- [ ] **Step 2: Verify fails** — after wiring, before removing the old `is_module_import` call in `on_call`.
- [ ] **Step 3: Implement** — in `on_call`, replace the bare-`Name`-receiver `is_module_import` + shadow check with `matches!(self.scope.resolve(self.unit_scope, recv.value).0, BindingOrigin::ModuleImport)` (the resolver already encodes "not shadowed by a closer param/local, and is a plain `import`"). `FromImport`/attribute-receivers fall through as today.
- [ ] **Step 4: Verify** — those tests pass; **snapshots byte-identical**; full suite green.
- [ ] **Step 5: Commit** — `refactor(python): #50 module-import suppression via resolver, compat (#78)`

---

## Phase 3 — Intentional deltas (labelled; targeted snapshots only)

> These change behavior on purpose. Each updates only the *named* snapshot/test and its commit says **intentional delta, not a regression**.

### Task 8: Cross-function-collision close (flip the pinned test)

**Files:** Modify `src/detect/mutation.rs`

- [ ] **Step 1: Rewrite the pin** — change `cross_function_flat_import_collision_drops_signal_accepted_limitation` (currently asserts *no* `GlobalMutation`) to assert `GlobalMutation` and rename to `cross_function_import_does_not_leak_across_functions`:

```rust
#[test]
fn cross_function_import_does_not_leak_across_functions() {
    // #78: g's function-local `import os as CACHE` no longer pollutes f's CACHE.
    let src = "from cfg import CACHE\n\
               def f():\n    CACHE.append(1)\n\
               def g():\n    import os as CACHE\n";
    let pairs = detect_src(src, "f");
    assert!(
        pairs.iter().any(|(e, c)| e.kind == GlobalMutation && !*c),
        "f's CACHE (a from-imported module global) must be GlobalMutation, got: {:?}",
        pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
    );
}
```

- [ ] **Step 2: Verify fails** — if the resolver still uses a file-wide flat import set, `g`'s local import leaks; the assertion fails.
- [ ] **Step 3: Implement** — ensure `g`'s `import os as CACHE` is collected into **`g`'s function scope**, not the module scope (Task 3 already scopes it correctly; this task confirms and, if needed, removes any residual file-wide import fallback in the resolver so `f`'s `CACHE` resolves to the module `FromImport`). Confirm no other snapshot moves.
- [ ] **Step 4: Verify** — new test passes; run `cargo insta test`; **accept only** snapshots whose change is this collision fix, and eyeball each diff.
- [ ] **Step 5: Commit** — `fix(python): scope-resolve closes cross-function import collision — intentional delta (#78, closes the #55 characterization pin)`

### Task 9: Captured-binding FP shrink (#55 residuals) — labelled delta

**Files:** Modify `src/detect/mutation.rs`, tests

- [ ] **Step 1: Failing tests** — the #55 Python residuals that the flat prescan mislabeled `hidden.mutation` "captured-binding" but a real scope tree places as `Local`/`EnclosingCaptured`: e.g. a name bound by a `match`-arm / comprehension / walrus that is genuinely local. Add one focused test per residual, asserting the *correct* origin (not `captured-binding`).
- [ ] **Step 2: Verify fails** — the residuals were listed as uncollected in the guideline.
- [ ] **Step 3: Implement** — extend the scope builder (Task 4 area) to collect `match`-case captures, comprehension targets (own scope), and walrus `:=` targets. Re-run: only the targeted hotspots' snapshots move.
- [ ] **Step 4: Verify** — new tests pass; accept only the intended snapshot diffs (each an FP removed).
- [ ] **Step 5: Commit** — `fix(python): scope tree collects match/comprehension/walrus bindings — captured-binding FP shrink, intentional delta (#78, #55)`

### Task 10: Invariant guard + cleanup

**Files:** Create `tests/invariant.rs`; Modify `src/imports.rs` (remove now-unused binding-only helpers if any), `src/detect/mutation.rs` (delete dead code)

- [ ] **Step 1: Failing test** — a test asserting the load-bearing invariant is mechanically enforceable:

```rust
// tests/invariant.rs
#[test]
fn core_has_no_binding_or_scope_types() {
    let core = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../fxrank-core/src/record.rs")).unwrap();
    for needle in ["BindingOrigin", "ScopeTree", "scope_segments"] {
        assert!(!core.contains(needle), "fxrank-core must not reference `{needle}` (#78 invariant)");
    }
}
```

- [ ] **Step 2: Verify fails** — only if something leaked; expected to pass immediately if the invariant held. (If it fails, a core change slipped in — fix that first.)
- [ ] **Step 3: Implement** — delete the now-dead `prescan_*` functions and the removed `MutSink` fields; confirm `cargo clippy -D warnings` has no dead-code warnings; run `cargo fmt`.
- [ ] **Step 4: Verify** — `cargo test -p fxrank-lang-python` + `cargo test -p fxrank-lang-python --test invariant` green; `cargo clippy` clean; `cargo fmt --check` clean; full-workspace `cargo test --workspace` green.
- [ ] **Step 5: Commit** — `chore(python): remove flat binding prescan superseded by ScopeTree + invariant guard (#78)`

---

## Self-Review

**Spec coverage** (guideline → task):
- Nearest-enclosing resolution replaces flat-set membership → Tasks 3–4 (build), 6 (consume). ✓
- `BindingOrigin` vocabulary + consuming rules → Task 1 (types), Task 6 (origin→effect map). ✓
- Binding-id scheme (scope_segments below the unit) → Task 1 (`BindingId`), Task 3 (`resolve` computes it). ✓ (id is computed but frontend-internal; not persisted — matches the guideline's "recomputed each scan".)
- Shadowing subsumes #50 guards → Task 3 (`param_shadows_import`), Task 7 (gate via `ModuleImport`). ✓
- Cross-function collision → Task 8 (pin flip). ✓
- Ambient/host globals → Task 4 note (Python returns `Unresolved` for builtins; `HostGlobalIo` is the TS frontend's — not invented here). ✓
- Per-language Python specifics (`global`/`nonlocal`/comprehension/import-kind) → Tasks 3–4. ✓
- `captured-binding` FP shrink (#55) → Task 9. ✓
- Load-bearing invariant → Task 10 guard + Global Constraints. ✓
- Compat-mode-first then labelled deltas → Phase 2 (byte-identical) vs Phase 3 (labelled). ✓

**Placeholder scan:** no "TBD"/"handle edge cases"/"write tests for the above" — each task carries its test code and exact commands.

**Type consistency:** `BindingOrigin`/`BindingId`/`ScopeTree::build`/`ScopeTree::resolve`/`unit_scope_of`/`resolve_in_module` are named identically across Tasks 1–7. `HostGlobalIo` spelling is consistent. `detect(unit, imports, module_bindings, scope, span)` signature is used verbatim in Tasks 5–7.

**Open follow-through (not a gap — future frontends):** TS and Rust get their own plans (`078-ts-*`, `078-rust-*`); the Rust one supersedes the interim `seed_pat_locals` patch. `#81` (shared-global nodes) rides on this finished resolver later; #78 emits nothing for it (verified in Task 6 — origins are consumed internally, none exported).
