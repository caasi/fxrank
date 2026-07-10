# Rust nested-unit collection alignment (032) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Issue:** [#83](../../../) — *Rust frontend: collect nested `fn`s & closures as their own `UnitRecord`s (cross-language alignment)*. **Dependent spec:** `031-scope-aware-binding-resolution-rust.md` (the Rust scope resolver) — **it lives only on branch `feat/078-rust-scope-resolution`, NOT on this branch and NOT the similarly-named `feat/078-scope-resolution`**. 031 diffs its compat baseline against the **post-#83** corpus and depends on nested units + captures existing (§1.1).

**Branch / rebase mechanics.** This plan (#83) lands on `main` first (feature branch `feat/083-nested-unit-collection` → PR → merge). Spec 031's branch `feat/078-rust-scope-resolution` then **rebases onto post-#83 `main`** so it sees nested units + captures already collected; 031 does not merge before #83.

**Settled inputs to encode, not re-litigate:** Q5 = **option (a)** — a mutably-capturing closure emits `hidden.mutation "captured-binding"` on the **closure's own unit** (cross-language-consistent with Python/TS, zero extra machinery); the **merge gate is a dogfood of the closure FP rate** (Task 6 produces it as a `docs/008-dogfood-deltas.md`-style doc). Re-containment (option b) is explicitly **not built here**.

**Two design decisions settled in review — final, encoded below, not reopened:**
- **D-Boundary = Option A (per-walker no-ops).** The Task-4 boundary is: each of the SIX own-body walkers overrides `visit_item` → no-op AND `visit_expr_closure` → no-op, so the enclosing walk stops at every nested-unit boundary. **Option B (collection-time body pruning) is dropped** — it leaked two ways (closure params still seeded into the parent; an in-body `Item::Mod` double-counted). Option A dissolves both: a `visit_item` no-op also stops at an in-body `Item::Mod`, and a `visit_expr_closure` no-op means closure params are never seeded into the parent.
- **D-OQ2 = accept + measure (option a).** Nested units stay in the core `SymbolIndex` name index; a bare `helper()` to a same-named module-level fn ambiguous-drops (symmetric — intra-tree and external edges both drop when same-named). This is the **existing language-neutral core behavior** (Python/TS nested units already do this), so it adds **zero** new cross-language obligation. Option (b) (exclude nested from `name_idx`) is **rejected** — it is a false-resolve trap (an intra-tree `helper()` would then wrongly resolve to the module-level unit). The "correct" fix (c) — scope-aware CALL resolution (call→callee-unit by lexical/nesting scope) — is **documented-only, NOT opened as an issue**: it's a core `resolve.rs` concern (the call-graph twin of #78's binding shadowing), a *possible future cross-language enhancement* gated by the measured edge-loss magnitude (Task-6 metric 5), and it does **not** block closing #78.

**Goal:** Bring `fxrank-lang-rust`'s function-unit collection in line with the cross-language invariant recorded in `docs/adding-a-language-frontend.md` §*Function-unit granularity*: **every nested `fn`, nested-`impl`/`trait` method, and closure becomes its own `Hotspot` + `UnitRecord`**, and the enclosing unit's own-body walk **stops at each nested-unit boundary** (no double-counting). Today `functions::collect_items` recurses only into `Item::Mod`, so nested `fn`s/closures fold into the enclosing unit — Rust is the cross-language outlier. This plan closes only **collection + boundary + identity + propagation**; it does **no** scope resolution (that is 031's `EnclosingCaptured` cascade — Q5(a) deliberately keeps today's flat detector cascade).

**Architecture:** Mirror the two proven siblings, not invent a third shape.
- **Collection** becomes a `syn::visit::Visit` walker (`Collector`, the swc `functions::collect` analog — `crates/fxrank-lang-ts/src/functions.rs`) that descends into every `fn`/method body **and** every expression position for `ExprClosure`, emitting one unit each; it threads `mod_path`, an `in_test` flag (inheritance), an impl/trait **type context** (like TS's `class_name`), a **nesting flag** (the Python `is_module_level` analog — `crates/fxrank-lang-python/src/functions.rs`), and a `pending_name` binding-name cursor (the TS `visit_var_declarator` → `pending_name` analog).
- **The unit container** is normalized to a TS-shaped `FnSig` + `FnBodyOwned::{Block, Expr}` so a `syn::ItemFn`/method (`Signature` + `Block`) and an `ExprClosure` (`Punctuated<Pat>` + `Box<Expr>`, no `Signature`, often no `Block`) both fit one type. Detectors walk the body via `FnBodyOwned::walk_with(&mut visitor)` (the TS `walk_with` analog).
- **The boundary** — the enclosing own-body walk must not descend into a nested unit — is the load-bearing change (Task 4), implemented as **Option A (per-walker no-ops)**: each of the six own-body walkers no-ops `visit_item` and `visit_expr_closure`. There is **no** Rust absorption mechanism: TS's React two-pass (`lib.rs::analyze_units`) is the **one** cross-language exception (spec 027, hook-callback absorption) and has **no Rust analogue** — do **not** replicate it.
- **Identity:** nested units get an **empty `canonical_path`** (the exact pattern Python uses for methods/nested defs/lambdas — `detect/mod.rs` `is_module_level` → empty), so they never populate `CanonicalIndex.primary` and can never false-resolve a `use crate::m::helper` or collide with a real module-level `helper`'s qualified/`self::` resolution.

**Tech Stack:** Rust, `syn` (with `visit`), `proc-macro2` (`span-locations`), `insta` (snapshots). Crate `crates/fxrank-lang-rust` only — **no `fxrank-core` change** (D-OQ2 accepts the existing name-index behavior; the exclusion alternative that would have touched `resolve.rs` is rejected). `resolve.rs` is read-only reference.

## Global Constraints

- **Commands:** `cargo test -p fxrank-lang-rust`; single test `cargo test -p fxrank-lang-rust <name>`; snapshots `cargo insta test -p fxrank-lang-rust` (accept only for labelled deltas, each diff eyeballed); gates `cargo fmt --check`, `cargo clippy -p fxrank-lang-rust --all-targets -- -D warnings`, and finally `cargo test --workspace`.
- **Load-bearing invariant (unchanged, mechanically guarded):** collection/identity stay **frontend-internal**. `fxrank-core` and `UnitRecord` gain **no** new scope/binding types. `UnitRecord` already carries the plain `bool` `is_root`; if Task 3 needs a name-index signal it reuses **existing** plumbing or an empty `canonical_path`, never a `BindingOrigin`/`ScopeTree` type (the 031 invariant guard, `fxrank-lang-python/tests/invariant.rs`, must stay green).
- **`analyze_unit` does NO post-`gather` mutation of effects** — so `build_record` may keep recomputing own-body via the same `gather` (the INVARIANT documented at `detect/mod.rs:107`). This plan preserves it: it adds **no** absorption step. (If a future task ever adds one, switch `build_record` to copy from the `Hotspot`, per that comment — not in scope here.)
- **Snapshot discipline (compat-then-delta, per 030/031) — sequencing matters (F-sequencing).** Task 1 (container reshape) is a **pure refactor** → snapshots **byte-identical**. Task 2 (collection) makes attribution *transiently wrong* — because the Option-A boundary is a separate task (Task 4), between Task 2 and Task 4 the parent **still double-counts** re-homed effects. Therefore **Task 2 accepts NO snapshots** (unit-level tests only); **snapshot acceptance happens ONLY at Task 4**, once the boundary makes attribution correct. The branch must **never record an intermediate wrong-attribution snapshot state**. Task 4 is the one **labelled snapshot delta** commit (parent no longer carries re-homed effects), each accepted diff eyeballed. Task 3/5 add fixtures but should not move existing snapshots except where a re-homed nested effect is the intended change (which already landed at Task 4).

### Non-goals (named, not silent)

- **N1. No scope resolution.** No `scope.rs`, no `BindingOrigin`, no `EnclosingCaptured` construction — that is **spec 031**, which rebases on this. #83 changes *which unit an effect lands on* and *whether the parent double-counts*, never *which binding a name resolves to*. A captured mutable local inside a closure keeps resolving through **today's flat cascade** (`mutation.rs::record_write`): it misses every flat set → `Unresolved` fallback → `hidden.mutation "captured-binding"` on the closure's own unit. That is exactly Q5(a); 031 later only *renames* the internal origin `Unresolved` → `EnclosingCaptured` (same effect, no diff — spec 031 AC-D3d).
- **N2. No Rust absorption.** Inline higher-order-function closure effects do **not** reappear in the parent (Task 5 documents why). The two non-reappearance mechanisms are **distinct** (F-prop-claims): `.map(|x| io(x))` does not reappear because the `.map` site is a `RefKind::Method`, hard-dropped in `resolve_ref` (the method-drop case); `spawn(cl)` does not reappear because **there is no edge from the argument *value* `cl` to the closure unit** — passing a closure by value is not a call to it — NOT because `spawn` is method-dropped (`spawn(cl)` is a `RefKind::Free` call to `spawn`). No React-two-pass analogue.
- **N3. No propagation-change beyond wiring.** The 025 cross-file fold already re-surfaces escaping effects along resolved edges; Task 5 only ensures binding-named nested-unit calls produce edges. It does not add re-containment (Q5 option b).
- **N4. `async {}` blocks are NOT units.** A bare `async { … }` / `tokio::spawn(async move { … })` is an expression, not a closure — it stays **parent-attributed** (its awaits count into the enclosing unit's `await_count`). Only `ExprClosure` (incl. `async || …`) is a unit. Task 4 scope-notes and fixtures this explicitly.
- **N5. Inline `mod` blocks** keep today's behavior (their items need `m::` qualification; `collect_items` already recurses `Item::Mod` for the top-level tree). #83 adds nested `fn`/closure descent **inside fn bodies**, not new bare-name visibility for inline-mod items (spec 031 N8).

## File Structure

- **Modify `crates/fxrank-lang-rust/src/functions.rs`** — the big one. Add `FnSig` + `FnBodyOwned` (Task 1); replace the manual `collect_items` loop with a `syn::visit::Visit` `Collector` that descends into fn bodies and closures (Task 2); add the `is_nested` flag + empty-canonical wiring hook (Task 3). (No collection-time prune — the boundary is Option A, living in the detector walkers, not here.)
- **Modify `crates/fxrank-lang-rust/src/detect/mod.rs`** — re-point `analyze_unit`, `gather`, `build_record`, and `count_awaits` from `&syn::Block`/`&syn::Signature` to `&FnBodyOwned`/`&FnSig` (Task 1); add the two boundary no-ops to the `AwaitCounter` walker (Task 4); pass `is_nested` into `canonical_path_of` and derive `referencing_mod` from a new `module_context_of(unit, module_tree)` independent of the emitted canonical path (Task 3).
- **Modify `crates/fxrank-lang-rust/src/detect/{calls,macros,mutation,risk,refs}.rs`** — each `detect*`/`extract` entry takes `&FnBodyOwned` and drives its walker via `walk_with` (Task 1); each of the five walkers here (`CallWalker`, `MacroWalker`, `MutationWalker`, `RiskWalker`, `RefsWalker`) gets the **two Option-A boundary no-ops** (`visit_item` + `visit_expr_closure`) (Task 4). `MutationWalker::seed` also grows the `is_closure` by-value-`mut` routing (Task 1 / OQ1).
- **Modify `crates/fxrank-lang-rust/src/lib.rs`** — the frontend `analyze` loop already calls `functions::collect` + `analyze_unit` + `build_record`; update only if a call signature changes (it should not — `collect` keeps returning `Vec<FnUnit>`).
- **Modify `crates/fxrank-lang-rust/tests/rust_frontend.rs`** — the integration test `mutation_detect_accepts_statics_and_imports` calls `mutation::detect(&item_fn.block, &item_fn.sig, …)` directly and stops compiling under the new `&FnBodyOwned`/`&FnSig` signature; fix it in Task 1 (F-test1).
- **Modify `docs/mutation-classification-guideline.md`** — its per-language line (~13) claims "Rust's mutation walker descends into closures and block-local `fn` items"; this plan makes that **false**. Update it (Task 4 / F-guideline) — CI cannot catch stale prose.
- **Read-only reference, do NOT edit:** `fxrank-lang-ts/src/functions.rs` (FnSig `Vec<Pat>`/FnBodyOwned/pending_name/`<arrow@LxCy>`), `fxrank-lang-ts/src/detect/refs.rs` (`visit_arrow_expr` no-op), `fxrank-lang-python/src/functions.rs` (nested-def/lambda units; own-body non-descent; `is_module_level`), `fxrank-core/src/resolve.rs` (`resolve_ref` method-drop; `SymbolIndex`/`CanonicalIndex`).
- **Create `docs/032-nested-unit-dogfood.md`** — Task 6's pre-registered dogfood-gate doc (the 031 compat-baseline artifact).
- **Tests:** unit tests inline in each modified `src/*.rs`; snapshot baseline is `crates/fxrank-lang-rust/tests/snapshots/*.snap` (driven by `tests/snapshots.rs`).

---

## Phase 0 — Container reshape (pure refactor, byte-identical)

### Task 1: `FnSig` + `FnBodyOwned` normalize the unit container

Foundational and first: a closure has no `syn::Signature` and often no `syn::Block`, so before collection can emit closures as units, every detector entry must accept the normalized container. This task changes the container, the detector signatures, and adds the `MutationWalker::seed` `is_closure` branch (OQ1). But collection still emits exactly the units it does today (no nested fns, no closures) and `is_closure` is `false` on every unit emitted here, so the new seed branch is **inert** and **snapshots stay byte-identical**.

**Files:** `src/functions.rs`, `src/detect/{mod,calls,macros,mutation,risk,refs}.rs`, `src/lib.rs`.

**Interfaces:**
- ```rust
  pub struct FnSig {
      /// Method receiver (`self`/`&self`/`&mut self`), else None. Free fns and
      /// closures have none — handled SEPARATELY from `inputs` (Blocker 1).
      pub receiver: Option<syn::Receiver>,
      /// Parameter *patterns* (mirror TS `FnSig.params: Vec<syn::Pat>`), NOT
      /// `Vec<syn::FnArg>`: a named fn/method's typed param becomes
      /// `Pat::Type(PatType{ pat, ty })` so the `&mut`/`&` type stays available
      /// for mutation seeding; a closure's params are its `Punctuated<Pat>`
      /// as-is. An untyped closure param `|v|` is a bare `Pat::Ident` with NO
      /// type — it CANNOT be an `FnArg::Typed` without fabricating a
      /// `Type::Infer`, which is exactly why `inputs` is `Vec<Pat>` not `Vec<FnArg>`.
      pub inputs: Vec<syn::Pat>,
      pub unsafety: bool,
      pub asyncness: bool,
      /// True ONLY for a closure unit. The mutation seed uses it to route a
      /// closure's by-value `mut` param into `let_mut` (reproducing today's
      /// `seed_pat_locals` behavior byte-for-byte); a named fn's by-value `mut`
      /// param stays silent (spec 031 Q1). Interface-level, not a Task-2
      /// footnote (Blocker 2 / OQ1). Distinct from `FnUnit.is_nested` (Task 3,
      /// identity): `is_closure` drives mutation seeding, `is_nested` drives
      /// canonical-path emptiness.
      pub is_closure: bool,
      /// `sig.span().start()` mapped to `(line, col+1)` for the `UnsafeFn` risk
      /// anchor (preserves `unsafe_cancel.snap` byte-for-byte). Closures: `None`.
      pub unsafe_anchor: Option<(usize, usize)>,
  }
  ```
  A named fn/method builds `receiver` from any `FnArg::Receiver` and `inputs` by mapping each `FnArg::Typed(pt)` → `Pat::Type(pt.clone())`; `unsafety`/`asyncness` mirror `sig.unsafety.is_some()`/`sig.asyncness.is_some()`; `is_closure = false`. Closures (Task 2) supply `receiver = None`, `inputs = node.inputs.iter().cloned().collect()`, `unsafety = false`, `unsafe_anchor = None`, `asyncness = node.asyncness.is_some()` (**F-async** — an `async ||` is an async boundary regardless of awaits), `is_closure = true`.
- `pub enum FnBodyOwned { Block(syn::Block), Expr(Box<syn::Expr>) }` with `pub fn walk_with<'ast, V: syn::visit::Visit<'ast>>(&'ast self, v: &mut V)` dispatching `v.visit_block(b)` / `v.visit_expr(e)` (the TS `walk_with` analog).
- `FnUnit` swaps `sig: syn::Signature` → `sig: FnSig` and `block: syn::Block` → `body: FnBodyOwned`. Keep `symbol`/`id`/`path`/`line`/`col`/`is_test`/`mod_path`.
- Detector entries re-point: `mutation::detect(body: &FnBodyOwned, sig: &FnSig, statics, imports)`; `calls::detect(body, imports, path, statics)`; `macros::detect(body)`; `risk::detect_fn_risks(body, sig, path)`; `refs::extract(body, imports, referencing_mod)`; `count_awaits(body)`. Each builds its walker and calls `body.walk_with(&mut walker)` instead of `walker.visit_block(block)`.

**Seed note (load-bearing) — the `FnArg` → `Pat`/`receiver` split changes the seed loop:** `mutation.rs::MutationWalker::seed` today matches `sig.inputs` as `FnArg::Receiver`/`FnArg::Typed`. With `FnSig { receiver, inputs: Vec<Pat>, is_closure }` it splits:
  - **Receiver** — seed `mut_self`/`shared_refs`/`locals("self")` from `sig.receiver` exactly as the old `FnArg::Receiver` arm did (`recv.reference.is_some()` / `recv.mutability.is_some()`).
  - **Named fn / method params** (`is_closure == false`) — for each `Pat::Type(pt)` in `inputs`, read `pt.ty` for `&mut`/`&` (populating `mut_params`/`shared_refs`) and `collect_pat_bindings(&pt.pat)` into `locals`, **discarding `is_mut`** for by-value typed params (today's behavior at `mutation.rs:107` — spec 031 Q1 keeps named-fn by-value `mut` silent; changing it would move unrelated snapshots).
  - **Closure params** (`is_closure == true`) — route each input pat through the existing `seed_pat_locals` path, which **honors `is_mut → let_mut`**, reproducing today's interim `visit_expr_closure`→`seed_pat_locals` seeding **byte-for-byte**. This is the OQ1/Blocker-2 fix; it lands here in Task 1 (the one place the container reshape crosses into detector behavior), guarded by `cargo insta test` byte-identity outside the closure fixtures.
- `risk::detect_fn_risks` reads `sig.unsafety` + `sig.span()` today — replace with `sig.unsafety` + `sig.unsafe_anchor` (only ever `Some` for an `unsafe fn`; closures are never unsafe). `analyze_unit` reads `sig.asyncness` — replace with `FnSig.asyncness`.

**Cross-artifact coordination (Blocker 2, cross-branch — 031 lives only on `feat/078-rust-scope-resolution`).** Spec 031 §4.2/§4.3 currently describes AC-C2 (`|mut x| → local.mutation`) as working "via the interim `seed_pat_locals` path" on the **enclosing** unit's `visit_expr_closure`. Once the closure is its own unit, that mechanism moves to the **closure's own `seed`** (the `is_closure` route above). 031's text needs a matching sync when it rebases onto post-#83 main — **flag this as a coordination item on that branch; do not edit 031 here.**

- [ ] **Step 1: Write the failing test** (in `src/functions.rs`, added to the existing `mod tests`)

```rust
#[test]
fn fnsig_and_body_normalize_named_fn() {
    let file = syn::parse_file("unsafe fn foo(x: &mut i32) { *x += 1; }").unwrap();
    let units = collect(&file, "a.rs");
    let u = &units[0];
    assert_eq!(u.sig.inputs.len(), 1); // one typed param `x` (receiver is separate)
    assert!(u.sig.receiver.is_none());
    assert!(!u.sig.is_closure); // a named fn is not a closure
    assert!(u.sig.unsafety);
    assert!(!u.sig.asyncness);
    assert!(matches!(u.body, FnBodyOwned::Block(_)));
    // walk_with drives a syn visitor over the block.
    struct Counter(usize);
    impl<'ast> syn::visit::Visit<'ast> for Counter {
        fn visit_expr_binary(&mut self, n: &'ast syn::ExprBinary) {
            self.0 += 1;
            syn::visit::visit_expr_binary(self, n);
        }
    }
    let mut c = Counter(0);
    u.body.walk_with(&mut c);
    assert_eq!(c.0, 1, "walk_with must reach the compound-assign in the body");
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p fxrank-lang-rust fnsig_and_body_normalize_named_fn` → FAIL (types/fields absent).
- [ ] **Step 3: Minimal implementation** — add `FnSig`/`FnBodyOwned`/`walk_with`; change `FnUnit` fields; in `collect_items`/`collect_from_impl`/`collect_from_trait` build `FnSig` from the `Signature` and `FnBodyOwned::Block(block)` from the body (no collection-shape change). Re-point every detector entry to `&FnBodyOwned`/`&FnSig` and drive via `walk_with`. Update `detect/mod.rs` (`analyze_unit`/`gather`/`build_record`/`count_awaits`) and `lib.rs` call sites. Fix every in-crate `detect(...)`/`extract(...)` test helper that constructs `&block`/`&sig` (grep `\.block`, `\.sig`, `detect(&`, `extract(&`, `visit_block(` across the crate — the `mutation.rs`, `calls.rs`, `refs.rs` test modules all build units and call detectors directly). **F-test1 — do NOT forget the integration test:** `crates/fxrank-lang-rust/tests/rust_frontend.rs::mutation_detect_accepts_statics_and_imports` calls `mutation::detect(&item_fn.block, &item_fn.sig, …)` directly and will **stop compiling** under the new signature. Fix it by building the unit via `functions::collect(&file, path)` and passing `&unit.body`/`&unit.sig` (preferred — exercises the real container), or by wrapping `item_fn.block`/`item_fn.sig` into `FnBodyOwned`/`FnSig` inline.
- [ ] **Step 4: Verify passes** — new test green; **`cargo insta test -p fxrank-lang-rust` shows zero pending snapshots** (pure refactor); `cargo test -p fxrank-lang-rust` green; `cargo clippy` clean.
- [ ] **Step 5: Commit** — `refactor(rust): normalize FnUnit to FnSig + FnBodyOwned (no-op) (#83)`

---

## Phase 1 — Exhaustive collection + identity

### Task 2: Collect nested `fn`s and closures as their own units

Replace the manual `collect_items` item-loop with a `syn::visit::Visit` `Collector` (the swc-collector shape) that descends into fn/method **bodies** and every expression position holding an `ExprClosure`, emitting one unit each. This re-homes effects from the parent onto the new unit — but the Option-A boundary (Task 4) has **not landed yet**, so the enclosing unit **still descends and transiently double-counts**. That is a deliberate intermediate on the feature branch, removed by Task 4. **Per F-sequencing, this task accepts NO snapshots** — verify with **unit-level tests only** (`collect` emits the right symbols/flags); snapshot acceptance is deferred to Task 4, when attribution is correct. The branch must not record a wrong-attribution snapshot state. (Option A is its own task, so folding a prune in here is not an option — Option B is dropped.)

**Interfaces / behavior to reproduce and extend:**
- **Keep every unit collected today**: free `fn`, inherent/trait-impl methods, default-bodied trait methods, inline-`mod` nesting in `mod_path`, `is_test` from `#[test]`/`#[bench]`/`#[tokio::test]`/bare `#[cfg(test)]` (the existing `fnunit_exposes_col`, `inline_module_nesting_recorded_in_mod_path`, `qualified_test_runner_attrs_mark_unit_as_test` tests must stay green).
- **Add nested `fn`s** (a `fn` in a block / fn body) and **nested-`impl`/`trait` methods** (an `impl`/`trait` inside a fn body) as units. Thread an impl/trait **type context** like TS's `class_name` (override `visit_item_impl`/`visit_item_trait` to set the self-type/trait name, compute the same `S::m` / `<S as T>::m` / `T::defaulted` symbols the current `collect_from_impl`/`collect_from_trait` produce).
- **Add closures** (`ExprClosure`, anywhere an expression appears) as units.
- **`is_test` inheritance:** thread an `in_test: bool`; a nested `fn`/method/closure inside a `#[test]`/`#[tokio::test]`/`#[cfg(test)]` unit inherits `is_test = true` (else the dogfood drowns in test-closures). Set `in_test` on entering a test unit's body; children read it.
- **Binding-name adoption (`pending_name`, the TS analog):** on `visit_local` whose `init.expr` is `Expr::Closure`, set `pending_name = Some(<let-binding ident>)`; the next `visit_expr_closure` consumes it via `.take()` as its symbol so a later `f()` edge resolves (Task 5). Cover the `let f = |…|` form (the dominant idiom); a `const`/`static` closure value is not expressible in Rust and needs no handling. Non-adopted closures fall back to **`<closure@L{line}C{col}>`** with `col` from the **closure span** (`node.span().start().column + 1`) — collision-proof for two closures on one line (mirrors TS `<arrow@L{line}C{col}>`; the crate's `id` = `path:line:col:symbol` invariant, CLAUDE.md, requires the col).

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn nested_fn_is_its_own_unit() {
    let file = syn::parse_file("fn outer() { fn helper(v: &mut Vec<i32>) { v.push(1); } }").unwrap();
    let units = collect(&file, "a.rs");
    let syms: Vec<&str> = units.iter().map(|u| u.symbol.as_str()).collect();
    assert!(syms.contains(&"outer"));
    assert!(syms.contains(&"helper"), "nested fn must be its own unit; got {syms:?}");
}

#[test]
fn closure_takes_binding_name_else_positional_fallback() {
    let src = "fn f() { let g = |x: i32| x + 1; let _ = (|y: i32| y, |z: i32| z); }";
    let file = syn::parse_file(src).unwrap();
    let units = collect(&file, "a.rs");
    let syms: Vec<&str> = units.iter().map(|u| u.symbol.as_str()).collect();
    assert!(syms.contains(&"g"), "let-bound closure adopts `g`; got {syms:?}");
    // two anonymous closures on the SAME line → two distinct <closure@..> symbols.
    let anon: Vec<&str> = syms.iter().copied().filter(|s| s.starts_with("<closure@L")).collect();
    assert_eq!(anon.len(), 2, "two same-line closures → two units; got {anon:?}");
    assert_ne!(anon[0], anon[1], "same-line closures must differ by col; got {anon:?}");
}

#[test]
fn nested_units_inherit_is_test() {
    let file = syn::parse_file("#[test]\nfn t() { fn helper() {} let c = || 1; }").unwrap();
    let units = collect(&file, "a.rs");
    for name in ["t", "helper", "c"] {
        assert!(units.iter().find(|u| u.symbol == name).unwrap().is_test,
            "`{name}` must inherit is_test");
    }
}

// F-istest: thicken is_test inheritance across (a) a qualified-runner attr with a
// nested closure, (b) a `#[cfg(test)] mod` composing module-level `in_cfg_test`
// with unit-level inheritance, and (c) a closure-in-closure (2+ level).
#[test]
fn tokio_test_nested_closure_inherits_is_test() {
    let file = syn::parse_file(
        "#[tokio::test]\nasync fn t() { let c = || { let d = || 1; d() }; c(); }").unwrap();
    let units = collect(&file, "a.rs");
    for u in &units {
        assert!(u.is_test, "`{}` under #[tokio::test] must inherit is_test", u.symbol);
    }
    // closure-in-closure: at least two <closure@..> (c and d) are present and test.
    assert!(units.iter().filter(|u| u.symbol.starts_with("<closure@L")).count() >= 2);
}

#[test]
fn cfg_test_mod_composes_with_unit_level_inheritance() {
    // Module-level `#[cfg(test)]` sets in_cfg_test for the whole module; the
    // fn's own nested units then inherit test-ness through the unit chain.
    let file = syn::parse_file(
        "#[cfg(test)]\nmod tests { fn helper_in_test_mod() { let c = || 1; } }").unwrap();
    let units = collect(&file, "a.rs");
    for name in ["helper_in_test_mod", "c"] {
        assert!(units.iter().find(|u| u.symbol == name).unwrap().is_test,
            "`{name}` in a #[cfg(test)] mod must be test (module-level ∘ unit-level)");
    }
}
```

- [ ] **Step 2: Verify fails** — `collect` does not yet emit nested units / closures.
- [ ] **Step 3: Implement** — rewrite `functions.rs` collection as a `Collector: syn::visit::Visit` (model on `fxrank-lang-ts/src/functions.rs`'s `Collector` + `fxrank-lang-python/src/functions.rs`'s `Ctx`): `visit_item_fn` (emit + recurse body), `visit_item_impl`/`visit_item_trait` (set type context, emit methods, recurse), `visit_item_mod` (thread `mod_path`, recurse — preserve today's inline-mod behavior), `visit_local` (set `pending_name`), `visit_expr_closure` (emit with `pending_name.take()` or `<closure@..>`, recurse). Thread `in_test`. `collect(&file, path)` keeps its signature (`Vec<FnUnit>`) so `lib.rs` is untouched. Guard the closure-param seed seam below.
- [ ] **Step 4: Verify (unit tests only — NO snapshot acceptance, F-sequencing)** — new + existing `functions.rs` tests green. You may **run** `cargo insta test -p fxrank-lang-rust` to *observe* pending deltas, but **do not accept any** — attribution is still transiently wrong (parent double-counts until Task 4). Snapshot acceptance happens only at Task 4. Do not commit an accepted `.snap` in this task.
- [ ] **Step 5: Commit** — `feat(rust): collect nested fns and closures as own units — intentional delta (#83)`

**Closure-param seed seam (must not regress `binding_forms_seed_locals_not_captured`).** The `is_mut` routing itself is **already handled in Task 1** via `FnSig.is_closure` (a closure unit seeds its params through `seed_pat_locals`, honoring `is_mut → let_mut`; a named fn stays silent — settled, OQ1/Blocker-2). This task's remaining obligation is the **test re-point**:
- The existing `binding_forms_seed_locals_not_captured` test calls `detect` on the outer `f`. Its **closure case** (`let c = |mut x: Vec<i32>| x.push(1)`) must be **re-pointed to assert on the closure's own unit** — build the units via `collect`, find the `<closure@..>` unit, and run `detect(&closure.body, &closure.sig, …)` on it, asserting `local.mutation` (not `hidden.mutation`). The 4 non-closure binding forms stay on `f`. Split or parametrize the test accordingly.
- Note the transient: until Task 4's Option-A boundary lands, the parent `f`'s `MutationWalker` still descends into the closure (interim `visit_expr_closure` seeding), so `f` *also* transiently carries the closure's `local.mutation`. That is the known double-count; assert on the **closure unit** (whose seeding is now correct), not on `f`. Task 4 deletes the interim seeding and stops the parent descent.

### Task 3: Nested-unit identity guard (empty `canonical_path`; name-index decision)

A nested `fn helper` must **not** get a bare-`helper` importable canonical path: that would (i) let a `use crate::m::helper` false-resolve to the nested fn via `CanonicalIndex.primary`, and (ii) collide with a real module-level `helper`, breaking its qualified/`self::`/`super::` resolution. Mirror **exactly** what Python does for methods/nested defs/lambdas: give nested units an **empty `canonical_path`** (`detect/mod.rs` Python: `if !unit.is_module_level { vec![] }`) — empty ⇒ not inserted into `CanonicalIndex.primary` (`resolve.rs::CanonicalIndex::from_records` skips `canonical_path.is_empty()`), verified against core.

**Interfaces:**
- Add a **nesting flag** to `FnUnit` (frontend-internal `bool`, the Python `is_module_level` inverse — e.g. `is_nested: bool`, set `true` for any closure / nested `fn` / nested-impl method; `false` for top-level fns and top-level-impl/trait methods). The `Collector` sets it from its nesting depth (Python threads `def_depth`; here: any unit emitted while inside another unit's body is nested). Distinct from `FnSig.is_closure` (Task 1, mutation seed) — `is_nested` covers nested named fns/methods too, and drives canonical-path emptiness only.
- `canonical_path_of(unit, module_tree)` returns `vec![]` when `unit.is_nested` (before the module lookup), else today's `["crate"] ++ file_mod ++ mod_path ++ symbol_segments`.
- **Blocker 3 — `referencing_mod` must NOT be derived from the (now-empty) `canonical_path`.** Today `build_record` does `referencing_mod = module_of_unit(&canonical_path, &unit.symbol)` (`detect/mod.rs:123`). For a nested unit `canonical_path` is empty ⇒ `module_of_unit` returns `vec![]` ⇒ a `self::`/`super::` ref **inside** the nested body loses its anchor (`resolve_in_crate` returns `None` on an empty referencing module). Add a **separate** `module_context_of(unit, module_tree)` that computes the unit's module directly from the file module + `mod_path`, **independent of the emitted canonical path**:
  ```rust
  fn module_context_of(unit: &FnUnit, mt: &ModuleTree) -> Vec<String> {
      let Some(file_mod) = mt.module_of(&unit.path) else { return vec![]; };
      let mut segs = vec!["crate".to_string()];
      segs.extend(file_mod);
      segs.extend(unit.mod_path.iter().cloned());
      segs // NOTE: no symbol segments — this is the module, not the unit path
  }
  ```
  A nested unit lives in the **same** module as its enclosing fn (Rust fn-local items do not open a new module path — `mod_path` tracks only inline `mod` blocks), so this restores `self::`/`super::` resolution for refs inside it. `build_record` sets `referencing_mod = module_context_of(unit, module_tree)` (was `module_of_unit(&canonical_path, …)`); the canonical-path emptiness and the module context are now decoupled.

**Name-index (`SymbolIndex`) participation — SETTLED (D-OQ2 = option a, accept + measure).** Empty `canonical_path` fixes the **precise** index. But `SymbolIndex::from_records` (core) indexes **every** record by simple name, so a nested `helper` still enters under `helper`. Consequence: a **bare, unqualified** `helper()` call to a same-named module-level fn now finds 2 entries → ambiguous → dropped (a **quieter FN**, symmetric — both intra-tree and external edges drop when the name collides). Crucially, a **qualified** call — `use crate::m::helper; helper()`, `self::helper()`, `super::helper()` — resolves via `resolved_target` through `CanonicalIndex.primary`/aliases, which the nested unit (empty path) does **not** populate, so **those do NOT regress**. This is the **existing language-neutral core behavior** — Python/TS nested units already exhibit exactly this ambiguity — so #83 adds **zero** new cross-language obligation. **Decision: mirror Python — accept the bare-name ambiguity, do NOT add a core exclusion.**

- **Option (b) — exclude nested units from `name_idx` — is REJECTED**: it is a false-resolve trap. If a nested `helper` were removed from the name index, an intra-tree bare `helper()` (in the *same* fn as the nested `helper`) would then wrongly resolve to the *module-level* `helper`. Dropping-on-ambiguity is the safe, symmetric behavior.
- **Option (c) — scope-aware CALL resolution (call → callee-unit by lexical/nesting scope) — is the "correct" long-term fix but is DOCUMENTED-ONLY, NOT opened as an issue.** It is a core `resolve.rs` concern (the call-graph twin of #78's binding shadowing), a *possible future cross-language enhancement* gated by the **measured** edge-loss magnitude (Task-6 metric 5), and it does **not** block closing #78. Task 6 records the magnitude and a deferred/gated paragraph; it is not scoped into #83.

Record the residual FN honestly in the dogfood doc: a parent's bare `inner()`/`go()`/`run()` to a nested unit resolves only when that name is corpus-wide unique.

- [ ] **Step 1: Failing / regression tests** (in `detect/mod.rs` `tests`, mirroring `method_unit_gets_empty_canonical_path` in Python)

```rust
#[test]
fn nested_unit_gets_empty_canonical_path() {
    use crate::module_tree::ModuleTree;
    use fxrank_core::frontend::SourceFile;
    let mt = ModuleTree::build(&[SourceFile { path: "src/lib.rs".into(), text: String::new() }]);
    let file = syn::parse_file("fn outer() { fn helper() {} }").unwrap();
    let imports = ImportTable::from_file(&file);
    let statics = std::collections::HashSet::new();
    let unit = crate::functions::collect(&file, "src/lib.rs")
        .into_iter().find(|u| u.symbol == "helper").unwrap();
    let rec = build_record(&unit, &imports, &statics, &mt);
    assert!(rec.canonical_path.is_empty(),
        "a nested fn must not get an importable canonical_path; got {:?}", rec.canonical_path);
}

#[test]
fn same_name_module_level_fn_still_resolves_when_qualified() {
    // A real module-level `helper` + a nested `helper` in a sibling fn.
    // A `use crate::…::helper; helper()` (qualified) MUST still resolve to the
    // module-level unit via CanonicalIndex, not drop to ambiguous.
    use fxrank_core::frontend::SourceFile;
    use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
    use fxrank_core::graph::Edge;
    let files = vec![SourceFile {
        path: "src/lib.rs".into(),
        text: "pub fn helper() {}\n\
               pub fn other() { fn helper() {} }\n\
               pub fn caller() { crate::helper(); }".into(),
    }];
    let out = crate::RustFrontend::default().analyze(&files);
    let idx = CanonicalIndex::from_records(&out.records);
    let caller = out.records.iter().find(|r| r.symbol == "caller").unwrap();
    let r = caller.refs.iter().find(|r| r.base.ends_with("helper")).unwrap();
    assert!(matches!(resolve_ref_precise(r, &idx, &caller.path), Some(Edge::Resolved(_))),
        "qualified crate::helper must resolve to the module-level unit, not drop");
}

#[test]
fn nested_unit_ref_keeps_self_super_anchor_despite_empty_canonical(/* Blocker 3 */) {
    use crate::module_tree::ModuleTree;
    use fxrank_core::frontend::SourceFile;
    // A nested fn whose body makes a `self::sibling()` call. Even though the
    // nested unit's canonical_path is empty, its refs must still anchor to the
    // file module (`crate::m`) so `self::sibling` resolves to crate::m::sibling.
    let mt = ModuleTree::build(&[SourceFile { path: "src/m.rs".into(), text: String::new() }]);
    let file = syn::parse_file("fn outer() { fn nested() { self::sibling(); } }").unwrap();
    let imports = ImportTable::from_file(&file);
    let statics = std::collections::HashSet::new();
    let nested = crate::functions::collect(&file, "src/m.rs")
        .into_iter().find(|u| u.symbol == "nested").unwrap();
    let rec = build_record(&nested, &imports, &statics, &mt);
    assert!(rec.canonical_path.is_empty(), "nested unit keeps empty canonical_path");
    let r = rec.refs.iter().find(|r| r.base == "self::sibling").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["crate".into(), "m".into(), "sibling".into()]),
        "self:: inside a nested unit must anchor to crate::m via module_context_of, not lose its anchor");
}
```

- [ ] **Step 2: Verify** — the first fails (nested unit currently gets a non-empty path or none at all depending on Task 2); the second pins the no-regression contract.
- [ ] **Step 3: Implement** — add `is_nested` to `FnUnit` (set in the `Collector`); short-circuit `canonical_path_of` to `vec![]` for nested units; add `module_context_of` and switch `build_record`'s `referencing_mod` to it (Blocker 3). **First verify** against `resolve.rs` that empty `canonical_path` is skipped by `CanonicalIndex::from_records` (it is — `if !rec.canonical_path.is_empty()`), so no core change is needed for the precise path. Do **not** add a name-index exclusion (D-OQ2 rejects option b).
- [ ] **Step 4: Verify** — both tests green; `cargo test --workspace` green (the 025/3e resolution tests still pass).
- [ ] **Step 5: Commit** — `fix(rust): nested units get empty canonical_path (identity guard) (#83)`

---

## Phase 2 — The detector boundary + records

### Task 4: Own-body walk stops at nested-unit boundaries (the load-bearing change)

The enclosing unit must **not** count effects that now belong to a nested unit. Rust has **no def-time evaluation** (no decorator/param-default headers — the Python exception does not apply), so the clean, complete boundary is: **stop at every nested item AND every closure**. `syn`'s default recursion descends into nested `Item::Fn`/`Item::Impl`/`Item::Trait`/`Item::Mod` **and** `ExprClosure` bodies, so stopping only at `visit_item_fn` would still double-count a nested `impl` method or a closure — the boundary must cover **both** item and closure positions.

**Implementation — Option A (per-walker no-ops), SETTLED (D-Boundary).** In each of the **SIX** own-body walkers, override `visit_item` → no-op AND `visit_expr_closure` → no-op (12 overrides total). The six walkers are `CallWalker` (`calls.rs`), `MacroWalker` (`macros.rs`), `MutationWalker` (`mutation.rs`), `RiskWalker` (`risk.rs`), `RefsWalker` (`refs.rs`), and `AwaitCounter` (`detect/mod.rs::count_awaits` — an async closure's / nested async fn's awaits would otherwise inflate the parent's `await_count`).

```rust
fn visit_item(&mut self, _n: &'ast syn::Item) { /* nested-unit boundary: stop */ }
fn visit_expr_closure(&mut self, _n: &'ast syn::ExprClosure) { /* boundary: stop */ }
```

**Why Option A, and why Option B was dropped.** Review showed the collection-time prune (Option B) leaked two ways: (i) a closure's params were still seeded into the parent (the prune emptied the *body* but the parent walker still visited the closure's param patterns), and (ii) an in-body `Item::Mod` double-counted (the prune targeted `Item::Fn`/`Impl`/`Trait` bodies, not `Item::Mod`). Option A dissolves **both** by construction: a `visit_item` no-op also stops at an in-body `Item::Mod`, and a `visit_expr_closure` no-op means the parent never reaches the closure's params **or** body. So Option A is not merely "the other option" — it is strictly the cleaner single semantics here.

**Scope notes (must hold under Option A):**
- **Do NOT no-op `visit_expr_async`.** A bare `async { … }` / `tokio::spawn(async move { … })` is an `ExprAsync`, **not** an `ExprClosure` and **not** an `Item` — the no-ops do not touch it, so it stays parent-attributed with its awaits counted (N4). Only `ExprClosure` (incl. `async || …`) is a unit.
- **The no-ops live only on the six detector walkers**, never on the `Collector` (Task 2), which must keep descending to emit nested units.
- **Delete the now-dead interim seeding:** `MutationWalker::visit_expr_closure` currently seeds closure param locals; replace it with the no-op (the closure's own unit seeds via `FnSig.is_closure`, Task 1). `MutationWalker` keeps its other seeding overrides (`visit_local`/`visit_expr_for_loop`/`visit_expr_let`/`visit_arm`) — those are the parent's own bindings, not nested units.
- **Maintenance hazard, named honestly:** every **future** detector walker must remember both no-ops or it silently double-counts. The cross-detector fixture below guards *today's* six; a new detector is one forgotten override away from a regression. (This is the one cost of Option A over a structural prune; accepted per D-Boundary.)

- [ ] **Step 1: Write the failing fixtures** — (1) ONE canonical cross-detector boundary fixture asserting **all SIX** walkers stop (F-refs adds `refs::extract`, which the old draft omitted — a nested-body effectful call would else emit a spurious `CallSiteRef` on the parent); (2) the F-async fixture; (3) the F-3level fixture.

```rust
#[test]
fn parent_body_walk_stops_at_every_nested_unit_boundary() {
    // Effects live ONLY inside nested units; the parent `outer` must own NONE of them.
    let src = r#"
        fn outer() {
            fn nested_fn() { std::fs::write("a", b"x").unwrap(); }      // net.fs.db + panic
            let c = || { println!("hi"); };                            // logging
            struct S;
            impl S { fn m(&self) { panic!("boom"); } }                 // panic (nested-impl method)
            let _ = (nested_fn, c);
        }
        // Sibling async fixture (N4): a bare async block's await stays on the parent.
        async fn awaiter() { do_thing().await; }
    "#;
    let file = syn::parse_file(src).unwrap();
    let imports = crate::imports::ImportTable::from_file(&file);
    let statics = std::collections::HashSet::new();
    let units = crate::functions::collect(&file, "a.rs");
    let outer = units.iter().find(|u| u.symbol == "outer").unwrap();

    let calls = crate::detect::calls::detect(&outer.body, &imports, "a.rs", &statics);
    let macros = crate::detect::macros::detect(&outer.body);
    let muts = crate::detect::mutation::detect(&outer.body, &outer.sig, &statics, &imports);
    let risks = crate::detect::risk::detect_fn_risks(&outer.body, &outer.sig, "a.rs");
    // F-refs: the SIXTH walker. The parent's refs must contain NONE of the
    // nested-body calls (`std::fs::write`, `println`, `panic`); only the
    // parent's own body (here: none) may appear.
    let referencing_mod: Vec<String> = vec![];
    let refs = crate::detect::refs::extract(&outer.body, &imports, &referencing_mod);
    assert!(calls.is_empty() && macros.is_empty() && muts.is_empty() && risks.is_empty(),
        "outer must own no nested-unit effects; calls={calls:?} macros={macros:?} muts={muts:?} risks={risks:?}");
    assert!(refs.iter().all(|r| r.base != "std::fs::write"),
        "outer's refs must exclude the nested fn's call; got {refs:?}");

    // N4: the bare async block's await IS the parent's.
    let awaiter = units.iter().find(|u| u.symbol == "awaiter").unwrap();
    assert!(crate::detect::analyze_unit(awaiter, &imports, &statics).await_count >= 1,
        "a bare async fn's await stays parent-attributed");
}

// F-async: an `async ||` closure is an async boundary regardless of awaits; its
// awaits belong to the closure, an `async { … }` block's awaits to the parent.
#[test]
fn async_block_awaits_parent_async_closure_awaits_itself() {
    let src = r#"
        fn parent() {
            let _ = async { f().await };          // async BLOCK → parent-attributed
            let c = async || { g().await };       // async CLOSURE → its own unit
            let _ = c;
        }
    "#;
    let file = syn::parse_file(src).unwrap();
    let imports = crate::imports::ImportTable::from_file(&file);
    let statics = std::collections::HashSet::new();
    let units = crate::functions::collect(&file, "a.rs");
    let parent = units.iter().find(|u| u.symbol == "parent").unwrap();
    let closure = units.iter().find(|u| u.symbol == "c").unwrap();
    assert!(closure.sig.asyncness, "an `async ||` closure is async regardless of awaits");
    let ph = crate::detect::analyze_unit(parent, &imports, &statics);
    let ch = crate::detect::analyze_unit(closure, &imports, &statics);
    assert_eq!(ph.await_count, 1, "the async BLOCK's await counts on the parent");
    assert_eq!(ch.await_count, 1, "the async CLOSURE's await counts on the closure");
    // The parent's refs must exclude the closure body's `g`.
    let refs = crate::detect::refs::extract(&parent.body, &imports, &[]);
    assert!(refs.iter().all(|r| r.base != "g"), "parent refs must exclude the async closure's call `g`; got {refs:?}");
}

// F-3level: fn → impl → method → closure. The 3-level-deep closure's effect
// stays on the closure unit; none of the enclosing three units carry it.
#[test]
fn three_level_nesting_boundary() {
    let src = r#"
        fn top() {
            struct S;
            impl S { fn method(&self) { let c = || { std::fs::write("a", b"x").ok(); }; let _ = c; } }
        }
    "#;
    let file = syn::parse_file(src).unwrap();
    let imports = crate::imports::ImportTable::from_file(&file);
    let statics = std::collections::HashSet::new();
    let units = crate::functions::collect(&file, "a.rs");
    // The closure `c` owns the write; `top` and `S::method` own none.
    for enclosing in ["top", "S::method"] {
        let u = units.iter().find(|u| u.symbol == enclosing).unwrap();
        let calls = crate::detect::calls::detect(&u.body, &imports, "a.rs", &statics);
        assert!(calls.iter().all(|e| e.kind.wire() != "net.fs.db"),
            "`{enclosing}` must not carry the 3-level closure's write; got {calls:?}");
    }
}
```

- [ ] **Step 2: Verify fails** — post-Task-2, `outer`/enclosing units double-count every nested effect, and the parent's refs leak the nested call.
- [ ] **Step 3: Implement (Option A)** — add the two no-op overrides (`visit_item`, `visit_expr_closure`) to all six walkers (`CallWalker`, `MacroWalker`, `MutationWalker`, `RiskWalker`, `RefsWalker`, `AwaitCounter`). Replace `MutationWalker::visit_expr_closure`'s interim param-seeding with the no-op (the closure's own unit seeds via `FnSig.is_closure`, Task 1). Do **not** add a `visit_expr_async` override (N4). **F-guideline — also update the prose:** `docs/mutation-classification-guideline.md` (~line 13) says "Rust's mutation walker descends into closures and block-local `fn` items" — now false; change that per-language line to state Rust (like TS/Python) **stops at nested functions and closures**.
- [ ] **Step 4: Verify + accept snapshots (F-sequencing: this is where acceptance lands)** — the fixtures pass; `cargo insta test -p fxrank-lang-rust` now shows the **intended** deltas (parent no longer carries re-homed effects); **accept each, eyeballed as a de-double-count**. This is the first task that accepts snapshots. Full `cargo test -p fxrank-lang-rust` green; `cargo clippy` clean.
- [ ] **Step 5: Commit** — `feat(rust): own-body walk stops at nested-unit boundaries (Option A) — intentional delta (#83)` (plus, separately, `docs: mutation guideline — Rust stops at nested fns/closures (#83)` for the prose fix if you prefer one-fix-per-commit).

### Task 5: Records, propagation edges, and the accepted trade-offs (in writing)

Wire records so binding-named closure/nested-fn calls produce edges (the 025 fold then re-surfaces escaping effects to the caller), and **write down** the two deliberate non-reappearances so they are documented, not hand-waved.

**What to verify/wire:**
- A `let f = |…| effectful(); f();` — the closure adopts `f` (Task 2), so `refs::extract` on the parent emits a `RefKind::Free` ref with `base = "f"`; `SymbolIndex`/`resolve_ref` maps it to the closure's unit → an edge → the 025 fold re-surfaces the closure's escaping effect on the parent's `propagated_score`. **F-prop-claims — narrow the claim:** this resolves an edge **only when the closure's simple name is corpus-unique** (D-OQ2 accepts name-index ambiguity — a `let helper = || …` in a file that also has a module-level `helper` ambiguous-drops). Pin the unique case end-to-end; note the corpus-unique precondition.
- **Documented non-reappearance — TWO DISTINCT mechanisms (F-prop-claims):**
  - **`.map(|x| io(x))` — the method-drop case.** The `.map` site is a `RefKind::Method`, **hard-dropped in `resolve_ref`** (`fxrank-core/src/resolve.rs:85`), so the inline closure's IO does **not** reappear in the parent. Matches TS/Python exactly (their method refs resolve identically); we **deliberately build no Rust absorption** (N2).
  - **`spawn(cl)` — the no-edge-from-value case.** `spawn(cl)` **is** a `RefKind::Free` call to `spawn` (not method-dropped), but passing `cl` as an argument *value* is **not a call to the closure**, so there is **no edge from `cl` to the closure unit** — the closure's effect does not reappear on the parent for a completely different reason than `.map`. Do not conflate the two.
  - State both in the record doc, and pin the `.map` case with the strengthened test below.
- **Documented known-FN (untyped closure param):** an **untyped** closure param `|v| v.push(1)` (the dominant Rust idiom) seeds `locals` with `v` but the write emits nothing meaningful without a type — `record_write` sees `v` as a plain local (or, pre-any-seed, a captured base). This is a **type-inference gap** 031's resolver cannot fix either (Milestone A is syntactic). Record it as a known FN; do **not** claim "handled by the resolver."

- [ ] **Step 1: Failing test** — binding-named closure call produces a resolvable edge:

```rust
#[test]
fn binding_named_closure_call_produces_edge() {
    use fxrank_core::frontend::SourceFile;
    use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
    use fxrank_core::graph::Edge;
    let files = vec![SourceFile {
        path: "src/lib.rs".into(),
        text: "pub fn parent() { let writer = || { std::fs::write(\"a\", b\"x\").unwrap(); }; writer(); }".into(),
    }];
    let out = crate::RustFrontend::default().analyze(&files);
    let idx = CanonicalIndex::from_records(&out.records);
    let parent = out.records.iter().find(|r| r.symbol == "parent").unwrap();
    let call = parent.refs.iter().find(|r| r.base == "writer").expect("writer() ref");
    assert!(matches!(resolve_ref_precise(call, &idx, &parent.path), Some(Edge::Resolved(_))),
        "writer() must resolve to the closure's own unit");
}

#[test]
fn inline_map_closure_io_does_not_reappear_on_parent() {
    use fxrank_core::frontend::SourceFile;
    let files = vec![SourceFile {
        path: "src/lib.rs".into(),
        text: "pub fn parent(xs: Vec<i32>) { let _: Vec<_> = xs.iter().map(|x| { std::fs::write(\"a\", b\"x\").ok(); x }).collect(); }".into(),
    }];
    let out = crate::RustFrontend::default().analyze(&files);
    let parent = out.records.iter().find(|r| r.symbol == "parent").unwrap();
    // F-prop-claims — strengthen: assert the FULL non-reappearance, not just
    // "no Free ref named map".
    // (1) The `.map` ref is RefKind::Method → hard-dropped: no resolved edge.
    assert!(parent.refs.iter().all(|r| !matches!(r.kind, fxrank_core::record::RefKind::Free) || r.base != "map"),
        "the .map closure must not become a resolvable parent edge (N2)");
    // (2) After the REAL fold, no closure effect/class reaches the parent — its
    //     propagated_max_class stays == its own max_class. Build the graph with
    //     the precise resolver, fold, and materialize hotspots (test-only helper).
    use fxrank_core::{graph::CallGraph, fold::{fold, to_hotspots}, resolve::{CanonicalIndex, resolve_ref_precise}};
    let idx = CanonicalIndex::from_records(&out.records);
    let g = CallGraph::from_records(out.records.clone(), |r, owner, _nodes| {
        let owner_path = owner.split(':').next().unwrap_or(owner);
        resolve_ref_precise(r, &idx, owner_path)
    });
    let folded = fold(&g);
    let hs = to_hotspots(&g, &folded);
    let ph = hs.iter().find(|h| h.symbol == "parent").unwrap();
    assert_eq!(ph.propagated_max_class, ph.max_class,
        "no closure effect/class may propagate to the parent from an inline .map (N2); \
         own={} propagated={}", ph.max_class, ph.propagated_max_class);
}
```

> **Grounding note for the implementer:** `CallGraph::from_records(records, resolve)`, `fold(&g)`, and the test-only `to_hotspots(&g, &folded)` are the real core APIs (`fxrank-core/src/{graph,fold}.rs`); the `resolve` closure signature is `|ref, owner_id, nodes| -> Option<Edge>`. If the owner→path derivation differs in the crate's own fold tests, mirror those instead — the assertion (propagated == own) is the load-bearing part.

- [ ] **Step 2: Verify** — the first fails if Task 2's `pending_name` adoption or the boundary broke the `writer` ref/edge; the second characterizes the accepted non-reappearance (should pass once the boundary is correct — pin it).
- [ ] **Step 3: Implement** — reconcile `refs::extract` so the binding-named closure call resolves (it should already: under Option A the `RefsWalker` no-ops at the closure *definition* but still records the `writer()` *call* in the parent body, since `writer()` is a `RefKind::Free` path call, not a closure expression; verify the closure unit's `symbol`/`unit_id` land in the index). No new absorption. Ensure the closure unit's own record carries its escaping effect for the fold.
- [ ] **Step 4: Verify** — both tests green; `cargo test --workspace` green.
- [ ] **Step 5: Commit** — `feat(rust): binding-named closure/nested-fn calls produce fold edges (#83)`

---

## Phase 3 — Pre-registered dogfood gate (Q5 merge gate)

### Task 6: Closure FP-rate dogfood — pre-register the design, then run

Q5(a) ships per-closure `hidden.mutation "captured-binding"`; the **merge gate** is a dogfood of the closure FP rate — comparable to the documented **#55 ~76-site "unacceptable" baseline**. Pre-register the whole design **before** running so the number is not fitted after the fact; write results as a `docs/008-dogfood-deltas.md`-style doc that **doubles as spec 031's compat baseline artifact** (§5.5).

**Pre-registered design (fix all four before the scan):**
1. **FP definition.** A `<closure@…>` (or binding-named) unit's `captured-binding` emission is a **FP** when the captured binding is a plain **local of the immediately-enclosing fn** AND the closure is **consumed synchronously in the same expression** (the "morally-local" iterator/sort-key case — `sort_by_key`, `map`, `retain`). It is a **TP** when the closure is **stored, returned, or spawned** (escapes its enclosing frame). Decide per-site by the closure's consumption context.
2. **Corpora.** (a) the fxrank workspace itself (`cargo run -p fxrank -- scan crates/`), and (b) a concrete **iterator/closure-heavy external Rust crate**: **`ripgrep` (`BurntSushi/ripgrep`)** — iterator- and closure-dense application code, comparable in shape to the #55 dogfood. **The operator supplies the checkout** (e.g. `~/GitHub/ripgrep`); the plan does not vendor it. If ripgrep is unavailable on the host, fall back to another closure-heavy checkout the operator names, and record which crate was used.
3. **Metrics beyond FP rate (FIVE metrics — metric 5 is the D-OQ2 gate for the deferred fix (c)).** Record:
   - (i) **Ranking churn** — the share of top-20 hotspots that are `<closure@…>` units (re-homing churn).
   - (ii) **Concern-1 dilution** — the count of parent units that **drop ≥1 effect class** purely because effects re-homed to a nested unit (a parent looking "cleaner" only because its effect moved).
   - (iii) **FP count** and (iv) **FP rate** of `captured-binding` per the definition above.
   - (v) **Bare-name edge loss (D-OQ2)** — the count of resolvable bare-name call edges **lost pre-#83 vs post-#83** *specifically because a nested unit newly shares a simple name with a module-level unit* (a `helper()` that resolved before #83 now ambiguous-drops). This is the measured magnitude that gates the deferred scope-aware CALL resolution (c); it is the trigger evidence, not a reason to build (c) in #83.
4. **Thresholds fixed before running (concrete, pre-registered — F-thresholds).** Ship (a) as-is when **all** hold:
   - **FP ceiling:** closure `captured-binding` **FP count ≤ 20 absolute** across the two corpora, **AND** **FP rate ≤ 25%** of all `captured-binding` emissions (well under the #55 ~76-site "unacceptable" baseline).
   - **Ranking-churn ceiling:** metric (i) — `<closure@…>` share of the **top-20 ≤ 25%** (closures must not bury real hotspots).
   - If the FP count approaches the #55 ~76-site scale (say **> 50**) or churn exceeds the ceiling, that **data** (not a re-argued opinion) reopens Q5 for the follow-up re-containment (option b) — **do not build (b) here regardless**; record the finding and let review decide merge-vs-defer.
   - **Deferred fix (c) trigger:** if metric (v) bare-name edge loss is **material** (pre-register: **> 10 edges** across the two corpora), the dogfood doc's deferred paragraph (below) flags (c) as the recommended future enhancement — still **not built or issue-opened in #83**.

- [ ] **Step 1: Write `docs/032-nested-unit-dogfood.md`** with the pre-registered items above **filled in as the protocol** (five metrics, concrete thresholds, empty results tables), committed **before** the scan — so the design is on record. **Include a short "Deferred: scope-aware CALL resolution (option c)" paragraph** stating: (c) is the call-graph twin of #78's binding shadowing, lives in core `resolve.rs`, is a *possible future cross-language enhancement*, is **gated by metric (v)** (bare-name edge-loss magnitude) as its trigger evidence, is **not opened as an issue** and **does not block closing #78**, and D-OQ2 deliberately accepts the current bare-name ambiguity (rejecting exclusion option b as a false-resolve trap).
- [ ] **Step 2: Run** `cargo run -p fxrank -- scan crates/` and the chosen external crate; tabulate the FP/TP split, the two extra metrics, and the top-20 churn.
- [ ] **Step 3: Fill results** into the doc; classify each residual `captured-binding` as FP/TP per the definition; state the merge verdict against the fixed thresholds (ship (a) / flag for review).
- [ ] **Step 4: Verify** — `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` all clean; the doc is complete (no TBDs).
- [ ] **Step 5: Commit** — `docs(rust): pre-registered closure FP-rate dogfood gate + results (#83)`

---

## Self-Review

**Issue coverage** (#83 requirement → task):
- Descend into `fn` bodies **and** closures, one `UnitRecord` each → Task 2 (collection). ✓
- Enclosing own-body walk must not descend into a nested unit → Task 4 (boundary; **Option A** — 12 no-ops across the six walkers incl. `AwaitCounter`; F-refs adds the sixth walker to the fixture). ✓
- Nested effects reach the caller only via the 025 fold → Task 5 (binding-named edges, corpus-unique caveat; the TWO distinct inline-HOF non-reappearances — `.map` method-drop vs `spawn(cl)` no-edge-from-value — documented, F-prop-claims). ✓
- The TS-only React absorption has no Rust analogue / not in scope → N2, Task 4. ✓
- Q5(a) settled = per-closure `hidden.mutation "captured-binding"`; merge gate = dogfood → Task 6 (pre-registered, concrete thresholds), N1 (031 only renames the origin later). ✓
- 031 dependency (post-#83 baseline) → header (+ branch/rebase mechanics, F-branch) + N1 + Task 6 doubling as the 031 compat artifact. ✓

**Fable condition → task mapping** (all five, test-first):
1. `FnUnit` re-shape (foundational, first) → Task 1 (FnSig `Vec<Pat>` + separate `receiver` + `is_closure`; asyncness-from-closure; F-test1 integration-test fixup). ✓
2. Visitor-based exhaustive collection + `is_test` inheritance (thickened, F-istest) + binding-name adoption + `<closure@LxCy>` (two-same-line fixture) → Task 2. ✓
3. Nested-unit identity guard (empty `canonical_path`, D-OQ2 name-index accept, same-name regression pins, **module_context_of** for `referencing_mod`, Blocker 3) → Task 3. ✓
4. Detector boundary (item + closure no-op, **Option A settled**, single cross-detector fixture asserting all six walkers, `async {}` scope-noted + F-async fixture + F-3level fixture, F-guideline prose fix) → Task 4. ✓
5. Records + propagation + written trade-offs (corpus-unique edge; `.map` vs `spawn(cl)` distinction; strengthened inline-HOF post-fold assertion; untyped-closure-param known-FN) → Task 5. ✓
6. Pre-registered Q5 dogfood gate (FP def, named corpus `ripgrep`, FIVE metrics incl. D-OQ2 edge-loss, concrete thresholds, deferred-(c) paragraph, `008-dogfood-deltas.md`-style doc) → Task 6. ✓

**Sequencing (F-sequencing):** reshape → collection (unit-level tests only, **no snapshot acceptance**) → identity guard → detector boundary (**Option A — the only snapshot-accepting task**) → records/propagation → dogfood, each test-first (red fixture, then implement), one logical step. The **transient double-count between Task 2 and Task 4 is live** (Option A is its own task, not folded into collection); it is called out in Task 2, and the branch **never records an intermediate wrong-attribution snapshot** — acceptance is gated to Task 4.

**Placeholder scan:** no "TBD"/"handle edge cases" — each task carries its fixtures + exact commands. "Verify against the code as you write" pointers (Task 1 `unsafe_anchor` span preservation for `unsafe_cancel.snap`; Task 3 `CanonicalIndex::from_records` empty-skip; Task 5 fold-API owner→path derivation) are checks, not deferred design.

**Decisions settled in review — encoded above, NOT reopened here** (former OQ1–OQ5, now final):
- **OQ1 → SETTLED (Task 1 / Blocker 2).** The closure-param `is_mut` seam is resolved by the `FnSig.is_closure` discriminant: a closure unit seeds its by-value `mut` params via `seed_pat_locals` (honoring `is_mut → let_mut`, reproducing today's behavior byte-for-byte); a named fn stays silent (spec 031 Q1). Guarded by `cargo insta` byte-identity outside the closure fixtures. Cross-branch: 031 §4.2/§4.3 must sync when it rebases (coordination item, not edited here).
- **OQ2 → SETTLED (D-OQ2 = accept + measure).** Mirror Python: accept the bare-name name-index ambiguity (symmetric ambiguous-drop). Reject exclusion option (b) as a false-resolve trap. The correct fix (c) is documented-only, gated by Task-6 metric (v), not issue-opened, and does not block closing #78.
- **OQ3 → SETTLED (D-Boundary = Option A).** Per-walker no-ops (12 overrides), not the collection-time prune (Option B dropped — it leaked closure-param seeding and in-body `Item::Mod` double-counts; Option A dissolves both). Accepted cost: future detectors must remember both no-ops.
- **OQ4 → SETTLED (N4 + F-async).** Bare `async {}` / `tokio::spawn(async move {})` stay parent-attributed (awaits included — they are `ExprAsync`, untouched by the no-ops); an `async ||` closure is its own async-boundary unit. F-async fixture pins both await counts and the parent-refs exclusion.
- **OQ5 → SETTLED (F-thresholds).** Concrete pre-registered thresholds (FP count ≤ 20 & rate ≤ 25%; top-20 closure share ≤ 25%; reopen-Q5 trigger > 50; defer-(c) trigger metric (v) > 10). The dogfood **data**, not a re-argued opinion, decides ship-(a)-now vs defer — but (b)/(c) are not built in #83 regardless.
