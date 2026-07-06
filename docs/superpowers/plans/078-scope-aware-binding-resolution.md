# Scope-aware binding resolution — Python frontend (078) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Python frontend's flat per-unit binding *sets* (`params`/`globals`/`nonlocals`/`locals`) plus its file-wide `Imports` binding lookups with a **file-level scope tree** that resolves a name-use to a `BindingOrigin` via `resolve(from_scope, name)`, per `docs/scope-aware-binding-resolution-guideline.md` — then land exactly **two** precision fixes (cross-function import collision; the #55 captured-binding FP shrink) as labelled deltas.

**Architecture:** A new frontend-internal `crate::scope` module builds one `ScopeTree` per file. **Python is function-scoped**, so the compat tree has just two scope kinds — **`Module`** and **`Function`** (one per `def`/`async def`/`lambda`); `for`/`with`/`except`/assign targets are already function-locals (the current `prescan` collects them there), so there are **no block scopes**. `resolve(from_scope, name)` walks the use's scope up to its enclosing **function** scope, then jumps to **module** scope — deliberately **not** through intermediate ancestor-function scopes (matching today's flat reach; see the non-goal note). The compat port is byte-identical **by construction**: the function scope mirrors the flat sets, the module scope mirrors `module_bindings` + the file-wide `Imports` table, and `resolve`'s order mirrors the `classify_and_push` cascade. The two deltas then replace one flat piece each: per-function import scoping (collision), and collecting the match/comprehension/walrus residuals (#55).

**Tech Stack:** Rust, `libcst_native` (Python CST), `insta` (snapshots). Crate `crates/fxrank-lang-python`.

## Global Constraints

- **Load-bearing invariant (mechanically checked):** binding resolution stays **frontend-internal**. `fxrank-core` and `UnitRecord` gain **zero** scope/binding types or fields. A test greps `fxrank-core/src/record.rs` for `BindingOrigin`/`ScopeTree`/`scope_segments`. There is **no** `fxrank-analysis` crate.
- **`BindingOrigin` is the guideline's vocabulary.** Rust identifiers (clippy `upper_case_acronyms`-clean): `Param`, `Local`, `ModuleImport`, `FromImport`, `ModuleBinding`, `Global`, `EnclosingCaptured`, `HostGlobalIo`, `Unresolved`. (`HostGlobalIo` is the Rust spelling of the guideline's prose `HostGlobalIO`; it is a **TS-only** origin — the Python builder never produces it.)
- **Compat gate (Phase 2):** every port task ends by asserting **existing snapshots are byte-identical** (`cargo insta test`; zero pending). Only Phase 3 tasks change snapshots, each labelled an *intentional delta* with the diff eyeballed.
- **Scope:** Python frontend only. Do **not** touch TS/Rust or the interim Rust `seed_pat_locals` patch (superseded in the Rust plan, not here). `#78` emits nothing for the deferred `#81` shared-global axis.
- **Commands:** `cargo test -p fxrank-lang-python`; single test `cargo test -p fxrank-lang-python <name>`; snapshots `cargo insta test -p fxrank-lang-python` (accept only for labelled deltas); gates `cargo fmt --check`, `cargo clippy -p fxrank-lang-python --all-targets -- -D warnings`, and finally `cargo test --workspace`.

### Non-goal — implicit cross-function capture (keeps #78 to two deltas)

`resolve` walks **function scope → module scope**, skipping intermediate ancestor-function scopes. So an *implicit* capture of an outer function's local (`def outer(): x=[]; def inner(): x.append(1)`) resolves to **`Unresolved` → `hidden.mutation "captured-binding"`** — **byte-identical with today**, which never walked ancestor functions either. The guideline's `EnclosingCaptured` origin is produced **only** for Python `nonlocal`-declared names (a *declared* capture, collected directly in the function scope). Full implicit-capture resolution (walking ancestor functions, promoting their `Local`/`Param` to `EnclosingCaptured`) would introduce a **third, unlisted behavior delta** (an outer local named `os` would stop being read as the module import) — out of scope for #78; a separately-justified follow-up if dogfood pressure appears.

---

## File Structure

- **Create `src/scope.rs`** — the scope tree + resolver + binding-collection helpers. One responsibility: name-use → `BindingOrigin` over a file scope tree. Frontend-internal (`pub(crate)`).
- **Modify `src/lib.rs`** (~line 84-85) — build one `ScopeTree` per file alongside `Imports`/`module_bindings`, and pass `&scope` into **all four** call sites (`analyze_unit` at ~157 and ~189; `build_record` at ~164 and ~197).
- **Modify `src/detect/mod.rs`** — thread `scope: &ScopeTree` through the three signatures that carry `imports`/`module_bindings` today: `gather` (~line 995, the actual `mutation::detect` caller at ~1016), `analyze_unit` (~1079), `build_record` (~1163).
- **Modify `src/detect/mutation.rs`** — `detect()` and `MutSink` take `&ScopeTree` + a `current_scope: ScopeId`; `classify_and_push` and the `on_call` #50 gate consult `resolve()`. The `prescan_*` collection logic **moves into `scope.rs`** (it becomes the function-scope builder); the flat `params`/`globals`/`nonlocals`/`locals` `MutSink` fields are deleted once ported.
- **Modify `src/imports.rs`** — `Imports` stays the module-resolution source of truth for `calls.rs` (unchanged there); `is_module_import`, `resolve`, `module_bindings`, and `collect_target_names` stay (the scope builder calls them). Only the *binding-oracle* use in `mutation.rs` moves to `scope.rs`.
- **Tests:** unit tests inline in `src/scope.rs`; behavior/characterization tests extend `src/detect/mutation.rs`'s `#[cfg(test)]` (reusing the existing `detect_src` helper); snapshot baseline is the existing `tests/snapshots*`.

---

## Phase 0 — Vocabulary types (frontend-internal)

### Task 1: `BindingOrigin` + `BindingId`

**Files:**
- Create: `src/scope.rs`
- Modify: `src/lib.rs` (add `mod scope;`)

**Interfaces:**
- Produces: `pub(crate) enum BindingOrigin { Param, Local, ModuleImport, FromImport, ModuleBinding, Global, EnclosingCaptured, HostGlobalIo, Unresolved }`; `pub(crate) struct BindingId { pub scope_segments: Vec<String>, pub kind_name: String }` (intra-unit id per guideline §binding-id; `full` = unit `canonical_path ++ scope_segments ++ [kind_name]`, but #78 never composes `full`).

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

## Phase 1 — The scope tree + resolver (compat-faithful engine)

> The tree built here reproduces the flat model's reach exactly (function scope = flat sets; module scope = `module_bindings` + file-wide imports). It introduces **no** behavior change on its own — Phase 2 consumes it; Phase 3 changes it.

### Task 2: `ScopeTree` + module scope

**Files:** Modify `src/scope.rs`

**Interfaces:**
- Consumes: `libcst_native::Module`, `crate::imports::Imports`, `crate::imports::module_bindings`.
- Produces: `pub(crate) struct ScopeTree { … }`; `pub(crate) type ScopeId = usize`; `impl ScopeTree { pub(crate) fn build(module: &Module, imports: &Imports, module_bindings: &HashSet<String>) -> Self; pub(crate) fn module_scope(&self) -> ScopeId /* = 0 */ }`; internal `struct Scope { kind: ScopeKind, parent: Option<ScopeId>, def_pos: Option<(usize,usize)>, bindings: HashMap<String, BindingOrigin> }`, `enum ScopeKind { Module, Function }`.

Module-scope construction (byte-identical to the flat lookups it will replace):
1. Insert every name in `module_bindings` as `ModuleBinding`.
2. Then overlay the **file-wide `Imports` table**: each imported name → `ModuleImport` if `imports.is_module_import(name)` else `FromImport`. Imports **overwrite** any same-named `ModuleBinding` — this reproduces the cascade's F5-before-F7 order (`imports.resolve` is checked before `module_bindings.contains`).

- [ ] **Step 1: Failing test**

```rust
#[test]
fn module_scope_collects_module_bindings_and_file_wide_imports() {
    let t = build_str("import os\nfrom cfg import CACHE\nx = 0\n");
    let m = t.module_scope();
    assert_eq!(t.resolve(m, "os").0, BindingOrigin::ModuleImport);
    assert_eq!(t.resolve(m, "CACHE").0, BindingOrigin::FromImport);
    assert_eq!(t.resolve(m, "x").0, BindingOrigin::ModuleBinding);
    assert_eq!(t.resolve(m, "nope").0, BindingOrigin::Unresolved);
}
// test helper (add once, reuse across scope tests):
fn build_str(src: &str) -> ScopeTree {
    let module = libcst_native::parse_module(src, None).unwrap();
    let imports = crate::imports::Imports::build(&module);
    let mb = crate::imports::module_bindings(&module);
    ScopeTree::build(&module, &imports, &mb)
}
```
(`resolve` lands in Task 3; for this task a minimal `resolve` that looks up a single scope's map then falls to `Unresolved` is enough — Task 3 generalizes it to the parent walk.)

- [ ] **Step 2: Verify fails** — types/`build`/`resolve` absent.
- [ ] **Step 3: Implement** — `ScopeTree { scopes: Vec<Scope> }`; `build` pushes the `Module` scope (id 0) and fills its `bindings` per the two-step construction above. Add a provisional `resolve(&self, scope: ScopeId, name: &str) -> (BindingOrigin, BindingId)` that checks `scopes[scope].bindings` then returns `(Unresolved, …)`.
- [ ] **Step 4: Verify passes.**
- [ ] **Step 5: Commit** — `feat(python): scope tree module scope (bindings + file-wide imports), #78`

### Task 3: Function scopes + `resolve` walk (port the prescan collection)

**Files:** Modify `src/scope.rs`

**Interfaces:**
- Produces: `pub(crate) fn resolve(&self, from_scope: ScopeId, name: &str) -> (BindingOrigin, BindingId)` — walk `from_scope` up to and including its **enclosing function scope**, then jump to **module scope**; first match wins; `Unresolved` if none. `pub(crate) fn unit_scope_of(&self, unit: &crate::functions::FnUnit) -> ScopeId` — maps a unit to its function scope by `(unit.line, unit.col)`; the synthetic `<module>` unit (its def_pos never matches a real `def`) maps to `module_scope()`.

Function-scope construction — **move the existing `prescan` logic** (`mutation.rs::prescan_body`/`prescan_suite`/`prescan_stmt`/`prescan_compound`/`prescan_orelse`/`prescan_small`) into `scope.rs` and drive it per function `def`/`async def`/`lambda`, producing the same names it produces today, mapped to origins:
- params (`collect_param_names`) → `Param`
- `prescan` `locals` (assign/annassign/augassign-Name/`for`/`with as`/`except as`/`except* as`/nested `def`&`class` names with `seed_defs=true`) → `Local`
- `prescan` `globals` (`global x`) → `Global`
- `prescan` `nonlocals` (`nonlocal x`) → `EnclosingCaptured` (declared capture)

Do **not** create scopes for `for`/`with`/`if`/`try`/`match` suites — Python function-scoping means those names already belong to the function scope (as they do in `prescan` today). Comprehensions are **not** given their own scope yet (that is the #55 delta, Task 8) — matching today, where comprehension targets are uncollected.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn function_scope_params_locals_and_decls() {
    let t = build_str("import os\ndef f(p):\n    global g\n    nonlocal n\n    x = 1\n    for i in p:\n        i\n    p; x; g; n; os\n");
    let f = t.unit_scope_by_pos(2, 1); // def f at line 2 col 1
    assert_eq!(t.resolve(f, "p").0, BindingOrigin::Param);
    assert_eq!(t.resolve(f, "x").0, BindingOrigin::Local);
    assert_eq!(t.resolve(f, "i").0, BindingOrigin::Local);   // for-target is function-local
    assert_eq!(t.resolve(f, "g").0, BindingOrigin::Global);
    assert_eq!(t.resolve(f, "n").0, BindingOrigin::EnclosingCaptured);
    assert_eq!(t.resolve(f, "os").0, BindingOrigin::ModuleImport); // walks to module
}
#[test]
fn param_shadows_module_import() {
    let t = build_str("import os\ndef f(os):\n    os\n");
    let f = t.unit_scope_by_pos(2, 1);
    assert_eq!(t.resolve(f, "os").0, BindingOrigin::Param); // nearest-enclosing beats the import
}
#[test]
fn implicit_capture_is_unresolved_not_local() {
    // Non-goal boundary: inner's use of outer's local resolves to Unresolved
    // (function->module walk skips ancestor functions) — byte-identical with the flat model.
    let t = build_str("def outer():\n    x = []\n    def inner():\n        x.append(1)\n");
    let inner = t.unit_scope_by_pos(3, 5); // nested def inner
    assert_eq!(t.resolve(inner, "x").0, BindingOrigin::Unresolved);
}
```
(`unit_scope_by_pos(line, col)` is a `#[cfg(test)]` helper on `ScopeTree` that finds the function scope whose `def_pos == Some((line, col))`.)

- [ ] **Step 2: Verify fails.**
- [ ] **Step 3: Implement** — extend `build` to create a `Function` scope (recording `def_pos = Some((line,col))` from the CST node's position) for every `def`/`async def`/`lambda`, parented to its lexical parent scope; populate it from the moved `prescan` collection. Implement `resolve` as the function→module walk: from `from_scope`, follow `parent` while the current scope is not `Module`, stopping at (and checking) the first `Function` scope, then check `module_scope()`. Compute `BindingId { scope_segments, kind_name }` from the matched scope's path-below-unit + `"kind:name"` (frontend-internal; not asserted byte-wise by consumers). Add the `unit_scope_of` position lookup and the `unit_scope_by_pos` test helper.
- [ ] **Step 4: Verify passes** (all three tests).
- [ ] **Step 5: Commit** — `feat(python): function scopes + resolve(from_scope,name) walk (#78)`

---

## Phase 2 — Compat-mode port (byte-identical snapshots)

> These are **characterization** tasks, not red-first TDD: existing tests/snapshots already pass and must **stay** green. Each task's "failing" step is the transient state after the old path is removed but before the resolver is wired. If any snapshot moves, the port changed behavior — revert and reconcile.

### Task 4: Thread `ScopeTree` + `current_scope` into `detect` (no behavior change)

**Files:** Modify `src/lib.rs`, `src/detect/mod.rs`, `src/detect/mutation.rs`

**Interfaces:**
- Produces: `mutation::detect(unit, imports, module_bindings, scope: &ScopeTree, span)`; `MutSink` gains `scope: &ScopeTree` and `current_scope: ScopeId` (initialized to `scope.unit_scope_of(unit)`); `gather`, `analyze_unit`, `build_record` each gain a `scope: &ScopeTree` param. Body still uses the flat sets — this task only wires the value through.

The `current_scope` field is the use-location cursor: in Phase 2 it never changes (Python compat has no sub-scopes), so it always equals the unit's function scope. Task 8 makes it descend into comprehension scopes.

- [ ] **Step 1: Failing test** — a compile-level test that `detect` accepts a `&ScopeTree` and still finds the param mutation:

```rust
#[test]
fn detect_accepts_scope_tree() {
    let src = "def f(lst):\n    lst.append(1)\n";
    let module = libcst_native::parse_module(src, None).unwrap();
    let imports = crate::imports::Imports::build(&module);
    let mb = crate::imports::module_bindings(&module);
    let tree = crate::scope::ScopeTree::build(&module, &imports, &mb);
    let span = crate::source::SpanIndex::new(src);
    let anchors = crate::source::lambda_anchors(src).unwrap();
    let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
    let f = units.iter().find(|u| u.symbol == "f").unwrap();
    let pairs = detect(f, &imports, &mb, &tree, &span);
    assert!(pairs.iter().any(|(e, _)| e.kind == fxrank_core::effect::EffectKind::ParamMutation));
}
```
(Confirm the exact `functions::collect` signature/return shape when writing — mirror an existing `mutation.rs` test that constructs a unit.)

- [ ] **Step 2: Verify fails** — signature mismatch.
- [ ] **Step 3: Implement** — add the `scope` param to `detect`/`gather`/`analyze_unit`/`build_record`; add `scope`/`current_scope` to `MutSink`; build the `ScopeTree` once in `lib.rs` (~after line 85) and pass `&scope` to all four call sites (`analyze_unit` ×2, `build_record` ×2). Do not consult it in classification yet.
- [ ] **Step 4: Verify** — the new test passes; **`cargo insta test` shows zero pending snapshots**; full `cargo test -p fxrank-lang-python` green.
- [ ] **Step 5: Commit** — `refactor(python): thread ScopeTree into detect/gather/build_record (no-op) (#78)`

### Task 5: Port `classify_and_push` onto `resolve()`

**Files:** Modify `src/detect/mutation.rs`

**Interfaces:**
- Consumes: `ScopeTree::resolve(self.current_scope, &root) -> (BindingOrigin, _)`.
- Produces: `classify_and_push` maps `BindingOrigin` → the **same** `(EffectKind, Tier, contained, evidence)` the flat cascade produces today.

The exact origin→effect map (verified against the current cascade `mutation.rs:510-628`; `root == "self"` is handled **before** `resolve` is consulted, unchanged):

| resolved origin | effect | tier | contained | evidence suffix |
|---|---|---|---|---|
| `Global` | `GlobalMutation` | `Exact` | false | `global {root} ({ev})` |
| `EnclosingCaptured` (nonlocal-declared) | `ThisMutation` | `Exact` | false | `nonlocal {root} ({ev})` |
| `Param` | `ParamMutation` | `Heuristic` | false | `{ev}` |
| `Local` | `LocalMutation` | `Exact` | **true** | `{ev}` |
| `ModuleImport` \| `FromImport` | `GlobalMutation` | `Heuristic` | false | `{ev} (imported `{root}`)` |
| `ModuleBinding` | `GlobalMutation` | `Heuristic` | false | `{ev} (module-level `{root}`)` |
| `Unresolved` | `HiddenMutation` (`push_hidden`) | `Heuristic` | false | subreason `"captured-binding"` |

Note: `ModuleImport` and `FromImport` **both** map to the imported-`root` arm here (today both are caught by the single F5 `imports.resolve(root).is_some()` check). Their split matters only at the `on_call` gate (Task 6). `HostGlobalIo` is unreachable in Python and needs no arm (add `HostGlobalIo => unreachable!("Python resolver never emits HostGlobalIo")` or fold it into the `Unresolved` arm — pick the clippy-clean form).

- [ ] **Step 1: Characterization pin** — add a test asserting the resolved path yields the same `(EffectKind, contained)` as today for one case per origin. Reuse `detect_src`:

```rust
#[test]
fn classify_origins_match_flat_behavior() {
    // one representative write per origin; assert exact (kind, contained)
    let cases: &[(&str, &str, EffectKind, bool)] = &[
        ("import os\ndef f():\n    global g\n    g = 1\n", "f", EffectKind::GlobalMutation, false), // Global (rebind under `global`)
        ("def f(p):\n    p.append(1)\n", "f", EffectKind::ParamMutation, false),                  // Param
        ("def f():\n    x = []\n    x.append(1)\n", "f", EffectKind::LocalMutation, true),          // Local
        ("from cfg import CACHE\ndef f():\n    CACHE.append(1)\n", "f", EffectKind::GlobalMutation, false), // FromImport
        ("STORE = {}\ndef f():\n    STORE['k'] = 1\n", "f", EffectKind::GlobalMutation, false),      // ModuleBinding
        ("def f():\n    captured.append(1)\n", "f", EffectKind::HiddenMutation, false),              // Unresolved -> captured-binding
    ];
    for (src, sym, kind, contained) in cases {
        let pairs = detect_src(src, sym);
        assert!(pairs.iter().any(|(e, c)| e.kind == *kind && *c == *contained),
            "src {src:?}: expected {kind:?} contained={contained}, got {:?}",
            pairs.iter().map(|(e, c)| (e.kind, *c)).collect::<Vec<_>>());
    }
}
```

- [ ] **Step 2: Verify** — this pin passes **on the current flat code** first (it characterizes existing behavior); confirm green before touching `classify_and_push`.
- [ ] **Step 3: Implement** — replace the `globals`/`nonlocals`/`params`/`locals`/`imports.resolve`/`module_bindings` cascade (after the `self` arm) with a single `match self.scope.resolve(self.current_scope, &root).0 { … }` per the table. Delete the now-dead `MutSink` fields `params`/`globals`/`nonlocals`/`locals` and stop populating them in `detect`.
- [ ] **Step 4: Verify** — pin still passes; **`cargo insta test` byte-identical**; full suite green.
- [ ] **Step 5: Commit** — `refactor(python): classify mutations via ScopeTree::resolve, compat (#78)`

### Task 6: Port the `on_call` #50 gate onto `ModuleImport`

**Files:** Modify `src/detect/mutation.rs`

The current gate suppresses a mutating-method call on a bare `Name` receiver when `!shadowed && imports.is_module_import(name)`, where `shadowed` = the name is in `params`/`locals`/`globals`/`nonlocals`. `resolve` already encodes exactly this: a closer param/local/global/nonlocal wins the walk, and only a plain `import` (not `from`-import) is `ModuleImport`. So `matches!(resolve(current_scope, name).0, ModuleImport)` ≡ `!shadowed && is_module_import(name)`.

- [ ] **Step 1: Characterization** — confirm `method_call_suppression_respects_shadowing_and_from_imports` and `method_call_on_bare_imported_name_is_not_mutation` pass on the current code (they already exist).
- [ ] **Step 2: Verify** — after removing the old `is_module_import` + `shadowed` block but before wiring `resolve`, those tests fail (transient).
- [ ] **Step 3: Implement** — in `on_call`, replace the bare-`Name`-receiver suppression block with `if matches!(self.scope.resolve(self.current_scope, recv.value).0, BindingOrigin::ModuleImport) { return; }`. Attribute receivers and `FromImport` names fall through exactly as today.
- [ ] **Step 4: Verify** — both tests pass; **snapshots byte-identical**; full suite green.
- [ ] **Step 5: Commit** — `refactor(python): #50 module-import suppression via resolver, compat (#78)`

---

## Phase 3 — Intentional deltas (labelled; targeted snapshots only)

> These change behavior on purpose. Each updates only its named test(s), accepts only the snapshot diffs it explains, and the commit says **intentional delta, not a regression**.

### Task 7: Close the cross-function import collision (per-function import scoping)

**Files:** Modify `src/scope.rs`, `src/detect/mutation.rs` (test)

The compat module scope holds the **file-wide** `Imports` table, so a function-local `import os as CACHE` in `g` leaks into `f`'s `CACHE` (the tracked collision). This delta makes module scope hold **module-level imports only**, and collects **function-body imports into that function's own scope**.

- [ ] **Step 1: Flip the pinned test** — rewrite `cross_function_flat_import_collision_drops_signal_accepted_limitation` (asserts *no* `GlobalMutation`) to assert `GlobalMutation`, renamed:

```rust
#[test]
fn cross_function_import_does_not_leak_across_functions() {
    // #78: g's function-local `import os as CACHE` no longer suppresses f's CACHE mutation.
    let src = "from cfg import CACHE\n\
               def f():\n    CACHE.append(1)\n\
               def g():\n    import os as CACHE\n";
    let pairs = detect_src(src, "f");
    assert!(pairs.iter().any(|(e, c)| e.kind == EffectKind::GlobalMutation && !*c),
        "f's CACHE (module-level from-import) must be GlobalMutation, got {:?}",
        pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>());
}
```

- [ ] **Step 2: Verify fails** — with the file-wide module-scope imports, `g`'s local `import … as CACHE` still shadows/suppresses; the assertion fails.
- [ ] **Step 3: Implement** — in `scope.rs`: (a) build module-scope imports from **top-level** `import`/`from` statements only (walk `module.body` directly, not the recursive file-wide `Imports` table); (b) in the function-scope builder, collect function-body `import`/`from … import` statements into **that function's** scope (`ModuleImport`/`FromImport`), so a local import binds locally and never reaches module scope. `f`'s `CACHE` now resolves via module scope to the top-level `from cfg import CACHE` (`FromImport` → `GlobalMutation`); `g`'s `CACHE` is local to `g`.
- [ ] **Step 4: Verify** — new test passes; `cargo insta test`; **accept only** snapshot diffs where a function-local import previously polluted another function (eyeball each — each is a recovered signal); full suite green.
- [ ] **Step 5: Commit** — `fix(python): scope-resolve closes cross-function import collision — intentional delta (#78)`

### Task 8: Captured-binding FP shrink — match/comprehension/walrus (#55)

**Files:** Modify `src/scope.rs`, `src/detect/mutation.rs` (tests, `current_scope` descent)

The `prescan` (now the function-scope builder) does **not** collect three binding forms, so their names fall to `Unresolved → hidden.mutation "captured-binding"` — false positives. This delta collects them:
- **`match`-case captures** (`case Point(x, y):`, `case [a, *rest]:`, `case {"k": v}:`, `case Foo() as b:`) → function-scope `Local`.
- **walrus** `:=` targets → function-scope `Local`.
- **comprehension** `for` targets → their **own** comprehension scope (Python-3 semantics: they do not leak to the function). A mutation *inside* the comprehension resolves via `current_scope` descending into that scope.

- [ ] **Step 1: Failing tests** — one focused test per residual asserting the corrected origin (not `HiddenMutation`):

```rust
#[test]
fn match_capture_is_local_not_captured() {
    let src = "def f(cmd):\n    match cmd:\n        case [name, *args]:\n            args.append(1)\n";
    let pairs = detect_src(src, "f");
    assert!(pairs.iter().any(|(e, c)| e.kind == EffectKind::LocalMutation && *c));
    assert!(!pairs.iter().any(|(e, _)| e.kind == EffectKind::HiddenMutation));
}
#[test]
fn walrus_target_is_local_not_captured() {
    let src = "def f(xs):\n    if (buf := list(xs)):\n        buf.append(1)\n";
    let pairs = detect_src(src, "f");
    assert!(pairs.iter().any(|(e, c)| e.kind == EffectKind::LocalMutation && *c));
    assert!(!pairs.iter().any(|(e, _)| e.kind == EffectKind::HiddenMutation));
}
#[test]
fn comprehension_target_mutation_is_local_not_captured() {
    let src = "def f(rows):\n    return [r.sort() or r for r in rows]\n";
    let pairs = detect_src(src, "f");
    assert!(!pairs.iter().any(|(e, _)| e.kind == EffectKind::HiddenMutation),
        "comprehension target `r` must resolve locally, got {:?}",
        pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>());
}
```
(Confirm `libcst_native`'s node names for `Match`/`MatchCase` patterns, `NamedExpr` (walrus), and comprehension `CompFor` when writing — the summary notes `Match` and `NamedExpr` are already modeled elsewhere in the frontend.)

- [ ] **Step 2: Verify fails** — these currently emit `HiddenMutation "captured-binding"`.
- [ ] **Step 3: Implement** —
  - Function-scope builder: walk `match` cases collecting pattern captures (recursing through sequence/mapping/class/`as` patterns) → `Local`; collect walrus `:=` targets anywhere in the function body → `Local`.
  - Create a `Comprehension` scope kind (child of the enclosing function/comprehension) for each list/set/dict/generator comprehension, collecting its `for` targets → `Local`. Have `MutSink` set `current_scope` to the comprehension's scope while walking that comprehension's elements, restoring it after (a save/restore around the comprehension visit). `resolve` already walks `Comprehension → … → Function → Module`.
- [ ] **Step 4: Verify** — the three tests pass; `cargo insta test`; **accept only** diffs that are captured-binding FPs turning into `local.mutation` (each a removed FP); full suite green.
- [ ] **Step 5: Commit** — `fix(python): collect match/comprehension/walrus bindings — captured-binding FP shrink, intentional delta (#78, #55)`

### Task 9: Invariant guard + cleanup

**Files:** Create `tests/invariant.rs`; Modify `src/detect/mutation.rs` (delete dead code), `src/scope.rs` (finalize)

- [ ] **Step 1: Add the invariant guard**

```rust
// tests/invariant.rs
#[test]
fn core_has_no_binding_or_scope_types() {
    let core = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../fxrank-core/src/record.rs")).unwrap();
    for needle in ["BindingOrigin", "ScopeTree", "scope_segments"] {
        assert!(!core.contains(needle),
            "fxrank-core must not reference `{needle}` (#78 load-bearing invariant)");
    }
}
```

- [ ] **Step 2: Verify** — expected to **pass immediately** (the invariant held throughout). If it fails, a core change leaked in — fix that first. (This is a guard, not red-first TDD.)
- [ ] **Step 3: Cleanup** — remove any now-dead `prescan_*` remnants left in `mutation.rs` (the collection lives in `scope.rs` now) and the removed `MutSink` fields; run `cargo fmt`; ensure `cargo clippy -p fxrank-lang-python --all-targets -- -D warnings` is clean (no dead-code/`upper_case_acronyms` warnings).
- [ ] **Step 4: Verify** — `cargo test -p fxrank-lang-python` + `cargo test -p fxrank-lang-python --test invariant` green; `cargo clippy` clean; `cargo fmt --check` clean; `cargo test --workspace` green.
- [ ] **Step 5: Commit** — `chore(python): remove flat binding prescan superseded by ScopeTree + invariant guard (#78)`

---

## Self-Review

**Spec coverage** (guideline → task):
- Nearest-enclosing resolution replaces flat-set membership → Tasks 2–3 (build), 5 (consume). ✓ (Python reach = function→module, per the non-goal note.)
- `BindingOrigin` vocabulary + consuming rules → Task 1 (types), Task 5 (origin→effect table). ✓
- Binding-id scheme (scope_segments below the unit) → Task 1 (`BindingId`), Task 3 (`resolve` computes it, frontend-internal, unpersisted). ✓
- Shadowing subsumes #50 guards → Task 3 (`param_shadows_module_import`), Task 6 (gate via `ModuleImport`). ✓
- Cross-function collision → Task 7 (per-function import scoping + pin flip). ✓
- Ambient/host globals → not a Python concern (`HostGlobalIo` is TS-only; Python builtins stay `Unresolved`) — Task 5 note. ✓
- Python specifics (`global`/`nonlocal`/comprehension-scope/import-kind/match/walrus) → Tasks 3, 8. ✓
- `captured-binding` FP shrink (#55) → Task 8. ✓
- Load-bearing invariant → Task 9 guard + Global Constraints. ✓
- Compat-mode-first then labelled deltas → Phase 2 (byte-identical characterization) vs Phase 3 (two labelled deltas). ✓
- Non-goal: implicit cross-function capture stays `Unresolved` (byte-identical, no third delta) → Global Constraints note + Task 3 `implicit_capture_is_unresolved_not_local`. ✓

**Placeholder scan:** no "TBD"/"handle edge cases"/"write tests for the above" — each task carries its test code and exact commands. Two "confirm the exact node name/signature when writing" notes (Task 4 `functions::collect`, Task 8 `libcst` pattern nodes) are pointers to verify against the crate, not deferred design.

**Type consistency:** `BindingOrigin`/`BindingId`/`ScopeTree::build(module, imports, module_bindings)`/`resolve(from_scope, name)`/`unit_scope_of`/`module_scope`/`current_scope`/`ScopeId` are named identically across all tasks. `HostGlobalIo` (Rust spelling) is used consistently; the guideline's prose `HostGlobalIO` is noted as the same variant. `detect(unit, imports, module_bindings, scope, span)` is used verbatim in Tasks 4–6.

**Open follow-through (not gaps):** TS and Rust get their own plans (`079-*`/later); the Rust one supersedes the interim `seed_pat_locals` patch. Full implicit cross-function capture and `#81` (shared-global nodes) ride on this finished resolver later; #78 emits nothing for `#81`.
