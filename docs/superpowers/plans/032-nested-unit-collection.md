# Rust nested-unit collection alignment (032) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Issue:** [#83](../../../) — *Rust frontend: collect nested `fn`s & closures as their own `UnitRecord`s (cross-language alignment)*. **Dependent spec:** [`031-scope-aware-binding-resolution-rust.md`](../specs/031-scope-aware-binding-resolution-rust.md) (the Rust scope resolver) diffs its compat baseline against the **post-#83** corpus and depends on nested units + captures existing (§1.1). **Settled inputs to encode, not re-litigate:** Q5 = **option (a)** — a mutably-capturing closure emits `hidden.mutation "captured-binding"` on the **closure's own unit** (cross-language-consistent with Python/TS, zero extra machinery); the **merge gate is a dogfood of the closure FP rate** (Task 6 produces it as a `docs/008-dogfood-deltas.md`-style doc). Re-containment (option b) is explicitly **not built here**.

**Goal:** Bring `fxrank-lang-rust`'s function-unit collection in line with the cross-language invariant recorded in `docs/adding-a-language-frontend.md` §*Function-unit granularity*: **every nested `fn`, nested-`impl`/`trait` method, and closure becomes its own `Hotspot` + `UnitRecord`**, and the enclosing unit's own-body walk **stops at each nested-unit boundary** (no double-counting). Today `functions::collect_items` recurses only into `Item::Mod`, so nested `fn`s/closures fold into the enclosing unit — Rust is the cross-language outlier. This plan closes only **collection + boundary + identity + propagation**; it does **no** scope resolution (that is 031's `EnclosingCaptured` cascade — Q5(a) deliberately keeps today's flat detector cascade).

**Architecture:** Mirror the two proven siblings, not invent a third shape.
- **Collection** becomes a `syn::visit::Visit` walker (`Collector`, the swc `functions::collect` analog — `crates/fxrank-lang-ts/src/functions.rs`) that descends into every `fn`/method body **and** every expression position for `ExprClosure`, emitting one unit each; it threads `mod_path`, an `in_test` flag (inheritance), an impl/trait **type context** (like TS's `class_name`), a **nesting flag** (the Python `is_module_level` analog — `crates/fxrank-lang-python/src/functions.rs`), and a `pending_name` binding-name cursor (the TS `visit_var_declarator` → `pending_name` analog).
- **The unit container** is normalized to a TS-shaped `FnSig` + `FnBodyOwned::{Block, Expr}` so a `syn::ItemFn`/method (`Signature` + `Block`) and an `ExprClosure` (`Punctuated<Pat>` + `Box<Expr>`, no `Signature`, often no `Block`) both fit one type. Detectors walk the body via `FnBodyOwned::walk_with(&mut visitor)` (the TS `walk_with` analog).
- **The boundary** — the enclosing own-body walk must not descend into a nested unit — is the load-bearing change (Task 4). There is **no** Rust absorption mechanism: TS's React two-pass (`lib.rs::analyze_units`) is the **one** cross-language exception (spec 027, hook-callback absorption) and has **no Rust analogue** — do **not** replicate it.
- **Identity:** nested units get an **empty `canonical_path`** (the exact pattern Python uses for methods/nested defs/lambdas — `detect/mod.rs` `is_module_level` → empty), so they never populate `CanonicalIndex.primary` and can never false-resolve a `use crate::m::helper` or collide with a real module-level `helper`'s qualified/`self::` resolution.

**Tech Stack:** Rust, `syn` (with `visit`), `proc-macro2` (`span-locations`), `insta` (snapshots). Crate `crates/fxrank-lang-rust`; one core read-only touch (`fxrank-core/src/resolve.rs`) only if Task 3 chooses name-index exclusion (see Task 3 decision).

## Global Constraints

- **Commands:** `cargo test -p fxrank-lang-rust`; single test `cargo test -p fxrank-lang-rust <name>`; snapshots `cargo insta test -p fxrank-lang-rust` (accept only for labelled deltas, each diff eyeballed); gates `cargo fmt --check`, `cargo clippy -p fxrank-lang-rust --all-targets -- -D warnings`, and finally `cargo test --workspace`.
- **Load-bearing invariant (unchanged, mechanically guarded):** collection/identity stay **frontend-internal**. `fxrank-core` and `UnitRecord` gain **no** new scope/binding types. `UnitRecord` already carries the plain `bool` `is_root`; if Task 3 needs a name-index signal it reuses **existing** plumbing or an empty `canonical_path`, never a `BindingOrigin`/`ScopeTree` type (the 031 invariant guard, `fxrank-lang-python/tests/invariant.rs`, must stay green).
- **`analyze_unit` does NO post-`gather` mutation of effects** — so `build_record` may keep recomputing own-body via the same `gather` (the INVARIANT documented at `detect/mod.rs:107`). This plan preserves it: it adds **no** absorption step. (If a future task ever adds one, switch `build_record` to copy from the `Hotspot`, per that comment — not in scope here.)
- **Snapshot discipline (compat-then-delta, per 030/031):** Task 1 (container reshape) is a **pure refactor** → snapshots **byte-identical**. Tasks 2 and 4 change attribution on purpose → **labelled snapshot deltas**, each commit says *intentional delta*, each accepted diff eyeballed. Task 3/5 add fixtures but should not move existing snapshots except where a re-homed nested effect is the intended change.

### Non-goals (named, not silent)

- **N1. No scope resolution.** No `scope.rs`, no `BindingOrigin`, no `EnclosingCaptured` construction — that is **spec 031**, which rebases on this. #83 changes *which unit an effect lands on* and *whether the parent double-counts*, never *which binding a name resolves to*. A captured mutable local inside a closure keeps resolving through **today's flat cascade** (`mutation.rs::record_write`): it misses every flat set → `Unresolved` fallback → `hidden.mutation "captured-binding"` on the closure's own unit. That is exactly Q5(a); 031 later only *renames* the internal origin `Unresolved` → `EnclosingCaptured` (same effect, no diff — spec 031 AC-D3d).
- **N2. No Rust absorption.** Inline higher-order-function closure effects (`.map(|x| io(x))`, `spawn(cl)`) do **not** reappear in the parent (Task 5 documents why — the `.map` ref is `RefKind::Method`, hard-dropped in `resolve_ref`). No React-two-pass analogue.
- **N3. No propagation-change beyond wiring.** The 025 cross-file fold already re-surfaces escaping effects along resolved edges; Task 5 only ensures binding-named nested-unit calls produce edges. It does not add re-containment (Q5 option b).
- **N4. `async {}` blocks are NOT units.** A bare `async { … }` / `tokio::spawn(async move { … })` is an expression, not a closure — it stays **parent-attributed** (its awaits count into the enclosing unit's `await_count`). Only `ExprClosure` (incl. `async || …`) is a unit. Task 4 scope-notes and fixtures this explicitly.
- **N5. Inline `mod` blocks** keep today's behavior (their items need `m::` qualification; `collect_items` already recurses `Item::Mod` for the top-level tree). #83 adds nested `fn`/closure descent **inside fn bodies**, not new bare-name visibility for inline-mod items (spec 031 N8).

## File Structure

- **Modify `crates/fxrank-lang-rust/src/functions.rs`** — the big one. Add `FnSig` + `FnBodyOwned` (Task 1); replace the manual `collect_items` loop with a `syn::visit::Visit` `Collector` that descends into fn bodies and closures (Task 2); add the nesting flag + empty-canonical wiring hook (Task 3); host the boundary prune if option B is chosen (Task 4).
- **Modify `crates/fxrank-lang-rust/src/detect/mod.rs`** — re-point `analyze_unit`, `gather`, `build_record`, and `count_awaits` from `&syn::Block`/`&syn::Signature` to `&FnBodyOwned`/`&FnSig` (Task 1); pass the nesting flag into `canonical_path_of` (Task 3).
- **Modify `crates/fxrank-lang-rust/src/detect/{calls,macros,mutation,risk,refs}.rs`** — each `detect*`/`extract` entry takes `&FnBodyOwned` and drives its walker via `walk_with` (Task 1); each walker respects the nested-unit boundary (Task 4, if option A).
- **Modify `crates/fxrank-lang-rust/src/lib.rs`** — the frontend `analyze` loop already calls `functions::collect` + `analyze_unit` + `build_record`; update only if a call signature changes (it should not — `collect` keeps returning `Vec<FnUnit>`).
- **Read-only reference, do NOT edit:** `fxrank-lang-ts/src/functions.rs` (FnSig/FnBodyOwned/pending_name/`<arrow@LxCy>`), `fxrank-lang-ts/src/detect/refs.rs` (`visit_arrow_expr` no-op), `fxrank-lang-python/src/functions.rs` (nested-def/lambda units; own-body non-descent; `is_module_level`), `fxrank-core/src/resolve.rs` (`resolve_ref` method-drop; `SymbolIndex`/`CanonicalIndex`).
- **Create `docs/032-nested-unit-dogfood.md`** — Task 6's pre-registered dogfood-gate doc (the 031 compat-baseline artifact).
- **Tests:** unit tests inline in each modified `src/*.rs`; snapshot baseline is `crates/fxrank-lang-rust/tests/snapshots/*.snap` (driven by `tests/snapshots.rs`).

---

## Phase 0 — Container reshape (pure refactor, byte-identical)

### Task 1: `FnSig` + `FnBodyOwned` normalize the unit container

Foundational and first: a closure has no `syn::Signature` and often no `syn::Block`, so before collection can emit closures as units, every detector entry must accept the normalized container. This task changes **only the container and the detector signatures** — collection still emits exactly the units it does today (no nested fns, no closures), so **snapshots stay byte-identical**.

**Files:** `src/functions.rs`, `src/detect/{mod,calls,macros,mutation,risk,refs}.rs`, `src/lib.rs`.

**Interfaces:**
- `pub struct FnSig { pub inputs: Vec<syn::FnArg>, pub unsafety: bool, pub asyncness: bool, pub unsafe_anchor: Option<(usize, usize)> }` — `inputs` are the receiver + typed params for a named fn/method (`sig.inputs.iter().cloned().collect()`); `unsafety`/`asyncness` mirror `sig.unsafety.is_some()`/`sig.asyncness.is_some()`; `unsafe_anchor` is `sig.span().start()` mapped to `(line, col+1)` for the `UnsafeFn` risk anchor (preserving `unsafe_cancel.snap` byte-for-byte). Closures (Task 2) supply `unsafety=false`, `unsafe_anchor=None`.
- `pub enum FnBodyOwned { Block(syn::Block), Expr(Box<syn::Expr>) }` with `pub fn walk_with<'ast, V: syn::visit::Visit<'ast>>(&'ast self, v: &mut V)` dispatching `v.visit_block(b)` / `v.visit_expr(e)` (the TS `walk_with` analog).
- `FnUnit` swaps `sig: syn::Signature` → `sig: FnSig` and `block: syn::Block` → `body: FnBodyOwned`. Keep `symbol`/`id`/`path`/`line`/`col`/`is_test`/`mod_path`.
- Detector entries re-point: `mutation::detect(body: &FnBodyOwned, sig: &FnSig, statics, imports)`; `calls::detect(body, imports, path, statics)`; `macros::detect(body)`; `risk::detect_fn_risks(body, sig, path)`; `refs::extract(body, imports, referencing_mod)`; `count_awaits(body)`. Each builds its walker and calls `body.walk_with(&mut walker)` instead of `walker.visit_block(block)`.

**Seed note (load-bearing):** `mutation.rs::MutationWalker::seed` iterates `sig.inputs` today as `&syn::Signature.inputs`. With `FnSig.inputs: Vec<syn::FnArg>` the loop is unchanged. `risk::detect_fn_risks` reads `sig.unsafety` + `sig.span()` today — replace with `sig.unsafety` + `sig.unsafe_anchor` (only ever `Some` for a `unsafe fn`; closures are never unsafe). `analyze_unit` reads `sig.asyncness` — replace with `FnSig.asyncness`.

- [ ] **Step 1: Write the failing test** (in `src/functions.rs`, added to the existing `mod tests`)

```rust
#[test]
fn fnsig_and_body_normalize_named_fn() {
    let file = syn::parse_file("unsafe fn foo(x: &mut i32) { *x += 1; }").unwrap();
    let units = collect(&file, "a.rs");
    let u = &units[0];
    assert_eq!(u.sig.inputs.len(), 1);
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
- [ ] **Step 3: Minimal implementation** — add `FnSig`/`FnBodyOwned`/`walk_with`; change `FnUnit` fields; in `collect_items`/`collect_from_impl`/`collect_from_trait` build `FnSig` from the `Signature` and `FnBodyOwned::Block(block)` from the body (no collection-shape change). Re-point every detector entry to `&FnBodyOwned`/`&FnSig` and drive via `walk_with`. Update `detect/mod.rs` (`analyze_unit`/`gather`/`build_record`/`count_awaits`) and `lib.rs` call sites. Fix every in-crate `detect(...)`/`extract(...)` test helper that constructs `&block`/`&sig` (grep `\.block`, `\.sig`, `detect(&`, `extract(&`, `visit_block(` across the crate — the `mutation.rs`, `calls.rs`, `refs.rs` test modules all build units and call detectors directly).
- [ ] **Step 4: Verify passes** — new test green; **`cargo insta test -p fxrank-lang-rust` shows zero pending snapshots** (pure refactor); `cargo test -p fxrank-lang-rust` green; `cargo clippy` clean.
- [ ] **Step 5: Commit** — `refactor(rust): normalize FnUnit to FnSig + FnBodyOwned (no-op) (#83)`

---

## Phase 1 — Exhaustive collection + identity

### Task 2: Collect nested `fn`s and closures as their own units

Replace the manual `collect_items` item-loop with a `syn::visit::Visit` `Collector` (the swc-collector shape) that descends into fn/method **bodies** and every expression position holding an `ExprClosure`, emitting one unit each. This is an **intentional attribution delta** — snapshots holding closures/nested fns move (the effects re-home from the parent onto the new unit). Because the boundary (Task 4) has not landed yet, the enclosing unit **still descends and transiently double-counts**; that is a known intermediate on the feature branch, removed by Task 4. (An implementer who prefers no transient may fold Task 4's option-B prune into this task — same file; see Task 4.)

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
```

- [ ] **Step 2: Verify fails** — `collect` does not yet emit nested units / closures.
- [ ] **Step 3: Implement** — rewrite `functions.rs` collection as a `Collector: syn::visit::Visit` (model on `fxrank-lang-ts/src/functions.rs`'s `Collector` + `fxrank-lang-python/src/functions.rs`'s `Ctx`): `visit_item_fn` (emit + recurse body), `visit_item_impl`/`visit_item_trait` (set type context, emit methods, recurse), `visit_item_mod` (thread `mod_path`, recurse — preserve today's inline-mod behavior), `visit_local` (set `pending_name`), `visit_expr_closure` (emit with `pending_name.take()` or `<closure@..>`, recurse). Thread `in_test`. `collect(&file, path)` keeps its signature (`Vec<FnUnit>`) so `lib.rs` is untouched. Guard the closure-param seed seam below.
- [ ] **Step 4: Verify** — new + existing `functions.rs` tests green; `cargo insta test -p fxrank-lang-rust` shows **intended** deltas only (closures/nested fns now separate; each eyeballed as a re-home). Do **not** yet expect double-count to be gone — Task 4 fixes that.
- [ ] **Step 5: Commit** — `feat(rust): collect nested fns and closures as own units — intentional delta (#83)`

**Closure-param seed seam (must not regress `binding_forms_seed_locals_not_captured`).** Today a `|mut x: Vec<i32>| x.push(1)` is seeded via `mutation.rs::visit_expr_closure` → `seed_pat_locals` (which **honors `is_mut`** → `let_mut` → `local.mutation`). Once the closure is its **own unit**, its params seed via `FnSig.inputs` → `MutationWalker::seed`, which **discards `is_mut` for typed params** (`mutation.rs:107`, `for (name, _is_mut)`) — a by-value `mut` param would land in `locals` only → **no effect**, breaking spec 031 AC-C2. Two obligations for this task:
1. The existing `binding_forms_seed_locals_not_captured` test currently calls `detect` on the outer `f`. Its **closure case** must be **re-pointed to assert on the closure's own unit** (the 4 non-closure forms stay on `f`). Split or parametrize the test accordingly.
2. `MutationWalker::seed` must route a **by-value `mut`** param into `let_mut` (not only `locals`) **for closure/nested-unit params** — but **not** for named-fn signature by-value params (spec 031 Q1 keeps those silent; changing them would move unrelated snapshots). The clean cut: honor `is_mut` from `collect_pat_bindings` for **by-value** typed params (route `mut` → `let_mut`), and verify no named-fn snapshot moves (`cargo insta test` byte-identity outside the closure fixtures). **Flag this as an open question for review** (see Self-Review) — it is the one place the container reshape crosses into detector behavior.

### Task 3: Nested-unit identity guard (empty `canonical_path`; name-index decision)

A nested `fn helper` must **not** get a bare-`helper` importable canonical path: that would (i) let a `use crate::m::helper` false-resolve to the nested fn via `CanonicalIndex.primary`, and (ii) collide with a real module-level `helper`, breaking its qualified/`self::`/`super::` resolution. Mirror **exactly** what Python does for methods/nested defs/lambdas: give nested units an **empty `canonical_path`** (`detect/mod.rs` Python: `if !unit.is_module_level { vec![] }`) — empty ⇒ not inserted into `CanonicalIndex.primary` (`resolve.rs::CanonicalIndex::from_records` skips `canonical_path.is_empty()`), verified against core.

**Interfaces:**
- Add a **nesting flag** to `FnUnit` (frontend-internal `bool`, the Python `is_module_level` inverse — e.g. `is_nested: bool`, set `true` for any closure / nested `fn` / nested-impl method; `false` for top-level fns and top-level-impl/trait methods). The `Collector` sets it from its nesting depth (Python threads `def_depth`; here: any unit emitted while inside another unit's body is nested).
- `canonical_path_of(unit, module_tree)` returns `vec![]` when `unit.is_nested` (before the module lookup), else today's `["crate"] ++ file_mod ++ mod_path ++ symbol_segments`.

**Name-index (`SymbolIndex`) participation — the decision.** Empty `canonical_path` fixes the **precise** index. But `SymbolIndex::from_records` (core) indexes **every** record by simple name, so a nested `helper` still enters under `helper`. Consequence: a **bare, unqualified** `helper()` call to a same-named module-level fn now finds 2 entries → ambiguous → dropped (a **quieter FN**). Crucially, a **qualified** call — `use crate::m::helper; helper()`, `self::helper()`, `super::helper()` — resolves via `resolved_target` through `CanonicalIndex.primary`/aliases, which the nested unit (empty path) does **not** populate, so **those do NOT regress**. This matches Python (which accepts the same name-index ambiguity for nested defs). **Recommendation: mirror Python — accept the bare-name degradation, do not add a core exclusion** (keeps the load-bearing invariant untouched, no new `UnitRecord` field, cross-language-consistent). Record the FN honestly: a parent's bare `inner()`/`go()`/`run()` to a nested unit resolves only when that name is corpus-wide unique. **Flag the alternative** (exclude nested units from the name index — needs a core signal) for review if the dogfood shows real edge loss.

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
```

- [ ] **Step 2: Verify** — the first fails (nested unit currently gets a non-empty path or none at all depending on Task 2); the second pins the no-regression contract.
- [ ] **Step 3: Implement** — add `is_nested` to `FnUnit` (set in the `Collector`); short-circuit `canonical_path_of` to `vec![]` for nested units. **First verify** against `resolve.rs` that empty `canonical_path` is skipped by `CanonicalIndex::from_records` (it is — `if !rec.canonical_path.is_empty()`), so no core change is needed for the precise path. Do **not** add a name-index exclusion (recommended path above).
- [ ] **Step 4: Verify** — both tests green; `cargo test --workspace` green (the 025/3e resolution tests still pass).
- [ ] **Step 5: Commit** — `fix(rust): nested units get empty canonical_path (identity guard) (#83)`

---

## Phase 2 — The detector boundary + records

### Task 4: Own-body walk stops at nested-unit boundaries (the load-bearing change)

The enclosing unit must **not** count effects that now belong to a nested unit. Rust has **no def-time evaluation** (no decorator/param-default headers — the Python exception does not apply), so the clean, complete boundary is: **stop at every nested item AND every closure**. `syn`'s default recursion descends into nested `Item::Fn`/`Item::Impl`/`Item::Trait`/`Item::Mod` **and** `ExprClosure` bodies, so stopping only at `visit_item_fn` would still double-count a nested `impl` method or a closure — the boundary must cover **both** item and closure positions. Six walkers descend today: `CallWalker`, `MacroWalker`, `MutationWalker`, `RiskWalker`, `RefsWalker`, and `AwaitCounter` (an async closure's awaits would otherwise inflate the parent's `await_count`).

**Two implementations — evaluate and pick:**
- **Option A — per-walker no-ops.** In each of the 6 walkers override `visit_item` → no-op and `visit_expr_closure` → no-op (12 overrides). Local and explicit, but every **future** detector must remember both no-ops or it silently double-counts — a standing maintenance hazard, and the cross-detector fixture below only guards *today's* detectors.
- **Option B — prune once at collection (RECOMMENDED).** In `functions.rs`, when cloning a unit's body for `FnBodyOwned`, run a single `syn::visit_mut` pass that replaces every **nested** `Item::Fn`/`Item::Impl`/`Item::Trait` block and every `ExprClosure` body with an empty block/`()` expr (the unit's own top-level body and its own signature are untouched; each nested unit is separately collected with its **own** un-pruned body). One choke point, **safe-by-construction for all current and future detectors**, at the cost of an AST transform. `functions.rs` already clones the block, so the transform slots into the existing clone.

**Recommendation: Option B.** It converts "every detector must remember the boundary" into a structural guarantee, matching the principle of materializing a check rather than carrying it as per-walker memory. **Critical scope of the prune:** prune `ExprClosure` bodies and nested `Item::*` fn/impl/trait bodies **only** — do **NOT** prune `ExprAsync` (`async { … }`) blocks (N4: they stay parent-attributed, awaits included), and do **NOT** prune the unit's own body or a nested `Item::Mod`'s items that are collected via the top-level tree. Verify the prune leaves parent-body calls that *reference* a nested unit (`f()`, `spawn(cl)`, `.map(cl)`) intact — only the closure/nested-fn **body** is emptied.

- [ ] **Step 1: Write the failing cross-detector boundary fixture** — ONE fixture with an effect inside a nested `fn`, a closure, AND a nested-`impl` method, asserted against **all** detectors on the **parent**:

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
    assert!(calls.is_empty() && macros.is_empty() && muts.is_empty() && risks.is_empty(),
        "outer must own no nested-unit effects; calls={calls:?} macros={macros:?} muts={muts:?} risks={risks:?}");

    // N4: the bare async block's await IS the parent's.
    let awaiter = units.iter().find(|u| u.symbol == "awaiter").unwrap();
    assert!(crate::detect::analyze_unit(awaiter, &imports, &statics).await_count >= 1,
        "a bare async fn's await stays parent-attributed");
}
```

- [ ] **Step 2: Verify fails** — post-Task-2, `outer` double-counts every nested effect.
- [ ] **Step 3: Implement** — Option B: add the `visit_mut` prune in `functions.rs` applied to each unit's cloned body (prune nested `ExprClosure`/`Item::Fn`/`Item::Impl`/`Item::Trait` bodies; leave `ExprAsync` and own body intact). (If Option A is chosen instead, add the two no-op overrides to all six walkers and delete the interim `mutation.rs::visit_expr_closure` seeding, which becomes dead once closures are their own units.)
- [ ] **Step 4: Verify** — the fixture passes; `cargo insta test -p fxrank-lang-rust` shows the **intended** deltas (parent no longer carries re-homed effects); each accepted diff eyeballed as a de-double-count. Full `cargo test -p fxrank-lang-rust` green.
- [ ] **Step 5: Commit** — `feat(rust): own-body walk stops at nested-unit boundaries — intentional delta (#83)`

### Task 5: Records, propagation edges, and the accepted trade-offs (in writing)

Wire records so binding-named closure/nested-fn calls produce edges (the 025 fold then re-surfaces escaping effects to the caller), and **write down** the two deliberate non-reappearances so they are documented, not hand-waved.

**What to verify/wire:**
- A `let f = |…| effectful(); f();` — the closure adopts `f` (Task 2), so `refs::extract` on the parent emits a `RefKind::Free` ref with `base = "f"`; `SymbolIndex`/`resolve_ref` maps it to the closure's unit → an edge → the 025 fold re-surfaces the closure's escaping effect on the parent's `propagated_score`. Pin this end-to-end.
- **Documented non-reappearance 1 (inline HOF):** `xs.iter().map(|x| io(x))` / `spawn(cl)` — the `.map` site is a `RefKind::Method`, **hard-dropped in `resolve_ref`** (`fxrank-core/src/resolve.rs:85`), so the closure's IO does **not** reappear in the parent. This matches TS/Python exactly (their method refs resolve identically) and we **deliberately build no Rust absorption** (N2). State it in the record doc + a test asserting the parent gets no propagated edge from a `.map` closure.
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
    // The `.map` ref is RefKind::Method → hard-dropped; no first-party edge to the closure.
    assert!(parent.refs.iter().all(|r| !matches!(r.kind, fxrank_core::record::RefKind::Free) || r.base != "map"),
        "the .map closure must not become a resolvable parent edge (N2)");
}
```

- [ ] **Step 2: Verify** — the first fails if Task 2's `pending_name` adoption or the boundary broke the `writer` ref/edge; the second characterizes the accepted non-reappearance (should pass once the boundary is correct — pin it).
- [ ] **Step 3: Implement** — reconcile `refs::extract` so the binding-named closure call resolves (it should already, since `refs` runs on the pruned parent body and `writer()` is a `RefKind::Free` path call; verify the closure unit's `symbol`/`unit_id` land in the index). No new absorption. Ensure the closure unit's own record carries its escaping effect for the fold.
- [ ] **Step 4: Verify** — both tests green; `cargo test --workspace` green.
- [ ] **Step 5: Commit** — `feat(rust): binding-named closure/nested-fn calls produce fold edges (#83)`

---

## Phase 3 — Pre-registered dogfood gate (Q5 merge gate)

### Task 6: Closure FP-rate dogfood — pre-register the design, then run

Q5(a) ships per-closure `hidden.mutation "captured-binding"`; the **merge gate** is a dogfood of the closure FP rate — comparable to the documented **#55 ~76-site "unacceptable" baseline**. Pre-register the whole design **before** running so the number is not fitted after the fact; write results as a `docs/008-dogfood-deltas.md`-style doc that **doubles as spec 031's compat baseline artifact** (§5.5).

**Pre-registered design (fix all four before the scan):**
1. **FP definition.** A `<closure@…>` (or binding-named) unit's `captured-binding` emission is a **FP** when the captured binding is a plain **local of the immediately-enclosing fn** AND the closure is **consumed synchronously in the same expression** (the "morally-local" iterator/sort-key case — `sort_by_key`, `map`, `retain`). It is a **TP** when the closure is **stored, returned, or spawned** (escapes its enclosing frame). Decide per-site by the closure's consumption context.
2. **Corpora.** (a) the fxrank workspace itself (`cargo run -p fxrank -- scan crates/`), and (b) **≥1 iterator-heavy external crate** from the #55/008 dogfood set, so the count is comparable to the #55 ~76-site baseline.
3. **Metrics beyond FP rate.** Also record: (i) the share of top-20 hotspots that are `<closure@…>` units (**ranking churn** from re-homing), and (ii) the count of parent units that **drop ≥1 effect class** purely because effects re-homed to a nested unit (measures the concern-1 dilution — a parent looking "cleaner" only because its effect moved).
4. **Thresholds fixed before running.** Ship (a) as-is if closure `captured-binding` **FP count is materially below the #55 ~76-site baseline** and ranking churn does not bury real hotspots; if it approaches #55 scale, that data (not a re-argued opinion) reopens Q5 for the follow-up re-containment (option b) — **do not build (b) here regardless**; record the finding and let review decide merge-vs-defer.

- [ ] **Step 1: Write `docs/032-nested-unit-dogfood.md`** with the four pre-registered items above **filled in as the protocol** (empty results tables), committed **before** the scan — so the design is on record.
- [ ] **Step 2: Run** `cargo run -p fxrank -- scan crates/` and the chosen external crate; tabulate the FP/TP split, the two extra metrics, and the top-20 churn.
- [ ] **Step 3: Fill results** into the doc; classify each residual `captured-binding` as FP/TP per the definition; state the merge verdict against the fixed thresholds (ship (a) / flag for review).
- [ ] **Step 4: Verify** — `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` all clean; the doc is complete (no TBDs).
- [ ] **Step 5: Commit** — `docs(rust): pre-registered closure FP-rate dogfood gate + results (#83)`

---

## Self-Review

**Issue coverage** (#83 requirement → task):
- Descend into `fn` bodies **and** closures, one `UnitRecord` each → Task 2 (collection). ✓
- Enclosing own-body walk must not descend into a nested unit → Task 4 (boundary; Option B recommended, all 6 walkers + future detectors covered by construction). ✓
- Nested effects reach the caller only via the 025 fold → Task 5 (binding-named edges; inline-HOF non-reappearance documented). ✓
- The TS-only React absorption has no Rust analogue / not in scope → N2, Task 4. ✓
- Q5(a) settled = per-closure `hidden.mutation "captured-binding"`; merge gate = dogfood → Task 6 (pre-registered), N1 (031 only renames the origin later). ✓
- 031 dependency (post-#83 baseline) → header + N1 + Task 6 doubling as the 031 compat artifact. ✓

**Fable condition → task mapping** (all five, test-first):
1. `FnUnit` re-shape (foundational, first) → Task 1. ✓
2. Visitor-based exhaustive collection + `is_test` inheritance + binding-name adoption + `<closure@LxCy>` (two-same-line fixture) → Task 2. ✓
3. Nested-unit identity guard (empty `canonical_path`, name-index decision, same-name regression pins) → Task 3. ✓
4. Detector boundary (item + closure no-op, A-vs-B evaluated, B recommended, single cross-detector fixture, `async {}` scope-noted) → Task 4. ✓
5. Records + propagation + written trade-offs (inline-HOF non-reappearance; untyped-closure-param known-FN) → Task 5. ✓
6. Pre-registered Q5 dogfood gate (FP def, corpora, extra metrics, fixed thresholds, `008-dogfood-deltas.md`-style doc) → Task 6. ✓

**Sequencing:** reshape → collection+naming+is_test → identity guard → detector boundary → records/propagation → dogfood, each test-first (red fixture, then implement), one logical step. The one non-linearity — the **transient double-count between Task 2 and Task 4** — is called out in Task 2; an implementer may fold Task 4's Option-B prune into Task 2 (same file) to erase it.

**Placeholder scan:** no "TBD"/"handle edge cases" — each task carries its fixtures + exact commands. Two "verify against the code as you write" notes (Task 1 `unsafe_anchor` span preservation for `unsafe_cancel.snap`; Task 3 `CanonicalIndex::from_records` empty-skip — confirmed present at `resolve.rs`) are pointers to check, not deferred design.

**Open questions surfaced by turning Fable's conditions into tasks — for the review-loop to resolve:**
- **OQ1 (Task 2 seam, highest priority).** Making a closure its own unit routes its params through `MutationWalker::seed`, which **discards `is_mut` for typed params** (`mutation.rs:107`), whereas the interim `seed_pat_locals` path honored it. To keep spec 031 AC-C2 / `binding_forms_seed_locals_not_captured` green, `seed` must route a **by-value `mut`** param into `let_mut` — but scoped to **closure/nested-unit** params only, or a named-fn by-value `mut` param would gain a new `local.mutation` (spec 031 Q1 keeps those silent) and move unrelated snapshots. Confirm the cut and that no named-fn snapshot moves. This is the container reshape's one crossing into detector behavior.
- **OQ2 (Task 3).** Recommendation is to mirror Python and **accept** the bare-name name-index ambiguity (a bare `helper()` to a same-named module-level fn drops when a nested `helper` exists elsewhere). Confirm this is acceptable, or approve the alternative (exclude nested units from the core `SymbolIndex`, which needs a core signal and touches the load-bearing boundary).
- **OQ3 (Task 4).** Confirm **Option B (prune-at-collection)** over Option A (per-walker no-ops). B is safer-by-construction but is an AST transform; A is explicit but leaves every future detector one forgotten override away from double-counting.
- **OQ4 (Task 4 / N4).** Confirm the async boundary: bare `async {}` / `tokio::spawn(async move {})` stay parent-attributed (awaits included), while an `async ||` **closure** is its own unit whose inner awaits are the closure's. The prune must skip `ExprAsync` — verify no snapshot double-counts a spawned async block onto a closure and its parent both.
- **OQ5 (Task 6).** Q5(a) is settled, but the dogfood gate can still block merge if the closure `captured-binding` FP count approaches the #55 ~76-site baseline. Confirm the fixed thresholds before running; the finding — not a re-argued opinion — decides ship-(a)-now vs defer.
