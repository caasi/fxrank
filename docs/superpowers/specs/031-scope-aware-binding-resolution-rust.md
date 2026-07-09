# Spec 031 — Scope-aware binding resolution, Rust frontend (#78)

Status: **Draft** · Issue: #78 (Rust step; #55 is the symptom catalogue) · Milestone: A (syntactic only)

Reference material this spec builds on — read these first:

- `docs/scope-aware-binding-resolution-guideline.md` — the shared #78 model
  (`BindingOrigin` vocabulary, binding-id scheme, resolution rule, load-bearing
  invariant). This spec instantiates it for Rust; where Rust diverges, the
  divergence is stated here and belongs in that doc's Rust bullet when it lands.
- `docs/superpowers/plans/030-scope-aware-binding-resolution.md` (the Python
  pilot's plan — renumbered from its on-disk `078-…` name; same document) +
  `crates/fxrank-lang-python/src/scope.rs` — the **Python pilot**, the one
  proven witness. Its shape (frontend-internal `scope.rs`, compat-mode port,
  labelled deltas) is the shape this spec follows; its *scope model* is not
  (Python is function-scoped; Rust is block-scoped — §6).
- Commit `eaa9298` — the **interim Rust binding seed** (flat, function-scoped
  seeding of `for`/`if let`/`while let`/`match`-arm/closure bindings). This spec
  **supersedes** that stepping-stone with real lexical scoping; its FP-removal
  test (`binding_forms_seed_locals_not_captured`) is part of the compat baseline
  that must stay green (§5).

## 1. Problem

The Rust mutation detector (`crates/fxrank-lang-rust/src/detect/mutation.rs`)
classifies each write site by the **bare string** of its base ident against flat
per-function sets (`locals`, `let_mut`, `mut_params`, `shared_refs`) plus two
file-level tables (`statics`, the `use`-table). The sets are **seeded during the
body walk and never popped**, so they are a *walk-order* approximation of
scope: roughly right for straight-line code, wrong at every scope boundary.
Concretely:

1. **`let` shadowing / rebinding is unrepresentable.** Rust deliberately allows
   `let x = …; let mut x = x;` in one block — *distinct bindings, one name*. A
   flat `HashSet<String>` can hold at most one fact per name, so a rebinding's
   mutability is smeared over the whole function (a later non-`mut` rebinding
   still reads as `let_mut`), and a local shadowing a `&mut` param never wins
   (`mut_params` is checked first, unconditionally). This is a headline
   motivator Python did not have: Python has no `let`-rebinding; Rust's flat
   model breaks on a feature the language *encourages*.
2. **Bindings leak out of their scope.** A `match`-arm binding, a block-local
   `let mut`, or a closure param stays in the sets for the rest of the walk, so
   a same-named later write to a file `static` is misread as `local.mutation` —
   a dropped `global.mutation` signal. This is the Rust analogue of Python's
   cross-function import collision: same root cause (flat table outliving its
   scope), one granularity down. (Rust has **no** direct cross-*function* twin:
   the walker is per-unit and the `use`-table collects top-level items only —
   §6.4.)
3. **Nested items are mis-scoped.** A nested `fn` (or a method in a nested
   `impl`) is walked as part of the enclosing unit, but its params are never
   seeded → `hidden.mutation "captured-binding"` FPs (the #55 family), and —
   worse — the enclosing function's locals *are* visible to it, though Rust
   nested items **cannot capture**. Block-level `static` items aren't in the
   file-level `statics` set, so writes to them fall to the captured-binding FP
   instead of `global.mutation`.

The interim seed (`eaa9298`) fixed the ~76× #55 misfires by widening the flat
sets to all binding forms — acknowledged there as the Milestone-A stepping-stone.
This spec is the finished subsystem: replace flat-set membership with resolution
to the **nearest enclosing binding** over a real Rust scope tree, per the
guideline.

## 2. Goals

- **G1.** A frontend-internal resolver (`crates/fxrank-lang-rust/src/scope.rs`)
  that answers `resolve(from_scope, name) → (BindingOrigin, BindingId)` over a
  lexical scope tree built from the unit's `syn` AST — mirroring the Python
  `scope.rs` pattern (same vocabulary, same id convention, own scope model).
- **G2.** Port the mutation classifier onto the resolver **compat-first**:
  byte-identical snapshot output before any behavior change (§5.1).
- **G3.** Land the precision fixes as **separately labelled intentional deltas**
  with their own fixtures (§5.2–5.4): shadowing/rebinding precision, cross-scope
  leak close, nested-item scoping.
- **G4.** Model the real Rust binding forms: signature params, `let` (incl.
  `let else`), `for`, `if let`/`while let`, `match` arms (incl. `mut`, `ref`,
  `@`-bindings, struct/tuple/slice destructuring, or-patterns), closure params,
  block items (`fn`/`impl`/`static`/`const`/`use`) — §4.
- **G5.** Preserve the load-bearing invariant: nothing crosses the `UnitRecord`
  boundary; `fxrank-core` gains zero scope/binding types or fields (§6.6).

## 3. Non-goals

Named so they are deferred, not silently half-done:

- **N1. Type / trait / method resolution.** Milestone A is syntactic. No
  receiver types, no `Deref`/auto-ref semantics, no trait-method dispatch. The
  mutating-method and interior-mutator name lists stay heuristic name lists.
- **N2. Scoped `CallSiteRef` emission (the guideline's consumer 2).** Exactly
  as in the Python step: `refs.rs`/`calls.rs` keep consuming the flat
  `ImportTable`; only the **mutation classifier** (consumer 1) moves onto the
  resolver. Rerouting call refs would add a call-graph snapshot-delta axis —
  a follow-up on top of this resolver, not part of it.
- **N3. Unit attribution changes.** Closures and nested `fn`s/`impl`s remain
  charged to their enclosing collected unit (`functions::collect` is untouched).
  This spec changes *which binding a name resolves to*, not *which unit an
  effect lands on*.
- **N4. Macro-expansion scoping.** Bindings created or used inside unexpanded
  macro token streams stay invisible (existing `macros.rs` behavior). Accepted
  Milestone-A miss.
- **N5. `ref`/`ref mut` scrutinee-write precision.** A write through a
  `ref mut` pattern binding really mutates the matched place (the scrutinee's
  owner). Milestone A treats it as a mutable `Local` of the arm scope — a
  documented approximation, FN-leaning, consistent with the syntactic charter.
- **N6. Cross-unit shared-global identity (#81)** and anything cross-file
  (#36 shipped and is not touched). This resolver emits nothing for #81.
- **N7. No shared resolver crate, no cross-frontend contract.** Same line the
  guideline and the #31 close-out drew: this is Rust's own `scope.rs` sharing a
  *vocabulary and conventions* with Python's, not a type, a crate, or a
  normative profile future frontends must conform to. Two real implementations
  are the evidence base; a governing abstraction is explicitly out.

## 4. Requirements

### 4.1 Scope model — Rust is block-scoped, position-aware, no hoisting of `let`

The resolver builds a scope tree per the guideline's `module → function →
block` shape, instantiated for Rust as:

```
file scope  →  unit scope (fn body root)  →  block / arm / closure / nested-item scopes …
```

- **R1 — block scope.** Every `{ … }` block is a child scope. A binding
  introduced inside it is invisible outside it. (Contrast note for the *later*
  TS frontend, recorded only to scope Rust correctly: Rust has no `var`
  hoisting and no TDZ — a `let` binding simply does not exist before its
  statement. Nothing TS-shaped is designed here.)
- **R2 — `let` sequential visibility and rebinding.** A `let` binding is
  visible from the statement *after* the `let` to the end of its block. A
  second `let` of the same name in the same block is a **new, distinct
  binding** (with its own mutability) shadowing the first for the remainder.
  The resolver must keep the bindings distinct — this is the requirement the
  flat set cannot meet. `let … else { … }`: the bound names are visible in the
  enclosing block after the statement; the diverging `else` block does **not**
  see them.
- **R3 — pattern scopes.** `for <pat>` binds in the loop body (not the iterator
  expression). `if let <pat>` binds in the then-branch only (not the `else`);
  `while let <pat>` in the loop body. A `match` arm's pattern binds in **that
  arm only** — its guard and its body — never in sibling arms or after the
  `match`. Closure params bind in the closure body.
- **R4 — item scoping and hoisting.** Items declared in a block (`fn`,
  `impl`, `static`, `const`, `use`) are visible **throughout that block**,
  including before their textual position (items hoist; `let` does not).
- **R5 — nested items do not capture.** Resolution from inside a nested
  `fn`/`impl` method must **not** see the enclosing function's `let`/param
  bindings — only enclosing *items* (R4) and the file scope. (Rust closures,
  by contrast, do capture: a closure body scope is an ordinary child of its
  enclosing block scope, so an outer `Local` is reached by the normal walk.
  That is why Rust needs no `EnclosingCaptured` origin — see §6.2.)
- **R6 — file scope.** The root scope holds the file-level tables the detector
  already receives: top-level `static` names → `ModuleBinding`; `use`-table
  names → `FromImport` (§6.2 for why never `ModuleImport`).

### 4.2 Binding forms — what seeds where, what `resolve` returns

Every form below must be seeded by the resolver; each row states the scope it
binds in and the `BindingOrigin` a use of that name resolves to. Per-binding
metadata (frontend-internal, carried alongside the origin): **mutability**
(`mut` vs not) and, for params, the **channel** (`&mut` / shared `&` /
by-value) — the classifier needs both (§4.3).

| Form | Example | Binds in | `resolve` returns |
|---|---|---|---|
| signature param, `&mut T` | `fn f(v: &mut Vec<i32>)` | unit scope | `Param` (channel `&mut`) |
| signature param, shared `&T` | `fn f(s: &S)` | unit scope | `Param` (channel shared) |
| signature param, by-value | `fn f(x: T)` / `fn f(mut y: T)` | unit scope | `Param` (channel by-value; `mut`ness kept) |
| `self` receiver | `&self` / `&mut self` / `self` | handled **before** `resolve`, unchanged from today (incl. the existing nested-`impl` misattribution guards) | — |
| `let` / `let mut` (destructuring incl.) | `let (a, mut b) = t;` | enclosing block, from next stmt (R2) | `Local` |
| `let else` | `let Some(x) = o else { return };` | enclosing block after stmt; not the `else` block | `Local` |
| `for` pattern | `for mut x in v { … }` | loop body | `Local` |
| `if let` / `while let` pattern | `if let Some(mut x) = o { … }` | then-branch / loop body | `Local` |
| `match` arm pattern (incl. `mut`, `ref`/`ref mut`, `@`, struct/tuple/slice, or-patterns, `*rest`-like slice tails) | `Ok(mut x)`, `P { x, .. }`, `[a, rest @ ..]`, `n @ 1..=9` | that arm (guard + body) | `Local` (or-pattern names bind once per arm) |
| closure param | `\|mut acc: Vec<_>\| acc.push(1)` | closure body | `Local` (closure shares its parent unit — guideline's Rust rule) |
| nested `fn` param / nested `impl` method param | `fn outer() { fn helper(v: &mut Vec<i32>) {…} }` | the nested item's own scope | `Local` (Milestone-A rule, mirrors the closure row — the nested item is not a collected unit; see Open Question Q2) |
| block-level `static` | `fn f() { static HITS: AtomicUsize = …; }` | its block, hoisted (R4) | `ModuleBinding` — Rust's global-write channel regardless of nesting depth |
| block-level `const` | `fn f() { const N: usize = 3; }` | its block, hoisted | `Local`, non-`mut` (a const cannot be written; the binding exists so the *name* shadows correctly during resolution) |
| block-level `use` | `fn f() { use std::mem::swap; }` | its block, hoisted | `FromImport` (today: invisible — the `use`-table is top-level-only — so such a base falls to the captured-binding FP; small bundled correctness gain under Delta 3) |
| top-level `static` | file item | file scope | `ModuleBinding` |
| top-level `use` name | `use std::fs;` / `use m::CACHE as C;` | file scope | `FromImport` |
| nothing in scope | — | — | `Unresolved` — the honest fallback |

Pattern-binding collection reuses `collect_pat_bindings` (already recursion-
complete over tuple/struct/tuple-struct/slice/reference/or/paren/type patterns
and `@`-subpatterns) — the resolver seeds *scopes* from it instead of flat sets.

### 4.3 Origin → effect: the compat map

The resolver owns **name → origin**; `mutation.rs` keeps owning **origin →
effect**. The compat-mode map must reproduce today's `record_write` cascade
exactly (evidence strings, tiers, discounts, containment, the `unsafe`
discount-cancel — all byte-identical):

| Resolved origin (+ metadata) | Effect | Notes (today's arm) |
|---|---|---|
| `Param`, channel `&mut` | `param.mutation`, `Discount::MutParam` | `mut_params` arm |
| (`self` + `&mut self`) | `param.mutation`, `Discount::MutSelf` | pre-resolve, unchanged |
| `Local`, `mut` | `local.mutation`, Exact, contained | `let_mut` arm |
| `Local`, non-`mut` | **no effect** | today's fall-through silence (`locals` membership suppresses every later arm) |
| `Param`, by-value | **no effect** | same silence today (`seed` ignores `is_mut` for params) — see Open Question Q1 |
| `ModuleBinding` | `global.mutation`, Heuristic, `write to global {base}` | `statics` arm |
| `FromImport` | `global.mutation`, Heuristic, `write to imported {base}` | `use`-table arm (008-F5; near-vacuous in Rust, kept for symmetry) |
| `Unresolved` | `hidden.mutation`, subreason `captured-binding` | 008-F1 arm |

The resolver is also the base oracle for the **other two** flat-set consumers
in `mutation.rs`, with the same compat requirement:

- the interior-mutator receiver check (`borrow_mut`/`set`/`store`/… on a
  shared-`&` base → `hidden.mutation "interior-mut"`) consults
  `Param`-with-shared-channel instead of `shared_refs` membership — the
  anti-Goodhart `&self`/`&T` inversion must be preserved verbatim;
- the interior-mutator-on-static check consults `ModuleBinding` instead of
  `statics` membership.

`ModuleImport`, `Global`, `EnclosingCaptured`, and `HostGlobalIo` are **never
constructed by the Rust resolver** (§6.2) — the match arms for them are
`unreachable!` (or folded, clippy-clean), mirroring Python's `HostGlobalIo`
handling.

### 4.4 Resolver architecture

- **`scope.rs` is frontend-internal** (`pub(crate)`), one responsibility:
  name-use → `(BindingOrigin, BindingId)` over a scope tree. Same module shape,
  vocabulary spelling (`BindingOrigin`, `BindingId`, `ScopeTree`, `ScopeId`,
  `resolve(from_scope, name)`) as Python's `scope.rs`.
- **The tree is built per unit**, over the unit's own `sig` + `block`, rooted
  at a file scope seeded from the `statics` set and `ImportTable` the detector
  already receives. Rationale — a genuine divergence from Python's per-file
  tree: (i) Rust has no shared module-executable scope (no `<module>` unit, no
  top-level statements), so nothing forces a file-wide tree; (ii) `FnUnit`
  **clones** its `sig`/`block` out of the parsed `syn::File`, so Python's
  body-pointer `unit_scope_of` identity trick is unavailable by construction —
  and unnecessary. `detect(block, sig, statics, imports)`'s signature can stay
  as-is (the tree is built inside `detect`), so no `lib.rs`/`mod.rs` threading
  is needed. If profiling ever shows per-unit builds too costly, that is a
  plan-level optimization, not a spec change.
- **The walker carries a scope cursor.** Because Rust has block scopes and
  positional `let` visibility, `MutationWalker` tracks its current scope while
  descending (push on block/arm/closure/nested-item entry, pop on exit) — a
  divergence from Python's single `unit_scope`, required by the scope model,
  and honest to state: Python resolves every use from one function scope; Rust
  resolves from the use's innermost scope.
- **`let` sequential visibility** (R2) should be modeled so that
  `resolve(from_scope, name)` keeps its two-argument shape — e.g. each `let`
  opens an implicit child scope covering the remainder of its block (rustc's
  own desugaring), so every scope holds at most one binding per name and
  resolution is a pure parent walk. The exact mechanism is the plan's choice;
  the observable requirements R2/R4 are what this spec pins.

### 4.5 Binding-id scheme

Reuse the guideline's convention verbatim: `full_binding_id =
unit.canonical_path ++ scope_segments ++ ["kind:name"]`, with `scope_segments`
as **structural lexical labels** below the unit's body root (block ordinals,
`arm2`, closure markers, `let`-split ordinals) — never `line:col`. As in the
Python step, nothing composes or persists the full id in this work; it exists
so two same-named bindings are distinct values and so the three frontends
reason alike. Kind prefixes reuse Python's (`param:`, `local:`, `from:`,
`module:`, `unresolved:`); Rust adds none.

## 5. Acceptance criteria

The migration discipline is the guideline's §*Migration discipline*, verbatim:
**compat first, then labelled deltas** — each delta touches only its named
fixtures/snapshots, and its commit says *intentional delta*, never fixes mixed
into the port.

### 5.1 Compat gate (must land first, in full, before any delta)

- **AC-C1.** All mutation classification is routed through
  `ScopeTree::resolve` (all three consumers in §4.3), and `cargo insta test -p
  fxrank-lang-rust` reports **zero pending snapshots** — byte-identical output,
  including evidence strings. The compat tree may be deliberately degenerate
  (unit scope mirroring today's walk-order flat seeding + file scope); the real
  lexical tree then lands under the deltas below.
- **AC-C2.** `binding_forms_seed_locals_not_captured` (the `eaa9298` #55
  baseline — `for`/`if let`/`while let`/`match`-arm/closure writes are
  `local.mutation`, never `hidden.mutation`) stays green at **every** commit of
  this work. Superseding the interim seed is behavior-preserving for its cases.
- **AC-C3.** The load-bearing invariant guard stays green: `fxrank-core` (and
  `UnitRecord`) reference no `BindingOrigin`/`ScopeTree`/`scope_segments`. The
  existing grep-guard (`fxrank-lang-python/tests/invariant.rs`) already covers
  the core; this work keeps it green and mirrors it from the Rust crate (or
  extends its needle list) so the Rust step is self-guarding too.
- **AC-C4.** `cargo test --workspace`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo fmt --check` — clean throughout.

### 5.2 Delta 1 — shadowing / rebinding precision (labelled)

Nearest-enclosing binding decides; the channel-set check order (`mut_params`
first, unconditionally) stops overriding a closer binding.

- **AC-D1a.** A local shadowing a `&mut` param: writes to the local classify
  `local.mutation` (contained); writes to the param outside the shadow stay
  `param.mutation`.

  ```rust
  fn f(buf: &mut Vec<i32>) {
      { let mut buf = Vec::new(); buf.push(1); } // → local.mutation  (today: param.mutation FP)
      buf.push(2);                                // → param.mutation (unchanged)
  }
  ```

- **AC-D1b.** `let` rebinding is per-binding, not per-name: a non-`mut`
  rebinding after a `mut` one stops emitting.

  ```rust
  fn f(v: Vec<i32>) {
      let mut x = v;    x.push(1);   // → local.mutation
      let x = freeze(x); x.push(2);  // → no effect: non-mut binding
                                     //   (today: stale let_mut entry → local.mutation)
  }
  ```

- **AC-D1c.** Position-awareness against a `static`: a use *before* a
  shadowing `let` still resolves to the static.

  ```rust
  static CACHE: Registry = Registry::new();
  fn f() {
      CACHE.insert(1);              // → global.mutation (the static)
      let mut CACHE = Registry::new();
      CACHE.insert(2);              // → local.mutation (the shadowing local)
  }
  ```

  (Today's walk-order seeding happens to get this straight-line case right;
  the fixture pins that the lexical model keeps it right — it is the cross-
  block variants in Delta 2 where the models part ways.)

Snapshot diffs accepted under this delta: only shadow-region reclassifications
(param→local, stale-mut silences), each eyeballed.

### 5.3 Delta 2 — cross-scope leak close (labelled; the Rust analogue of Python's cross-function collision delta)

A binding introduced in a block / arm / `if let` / `for` / closure no longer
exists outside its scope, so a same-named later write to a file `static`
recovers its `global.mutation` signal.

- **AC-D2a.** Block leak closed:

  ```rust
  static CACHE: Registry = Registry::new();
  fn f() {
      { let mut CACHE = Registry::new(); CACHE.insert(1); } // → local.mutation
      CACHE.insert(2);  // → global.mutation  (today: leaked let_mut → local.mutation, dropped signal)
  }
  ```

- **AC-D2b.** `match`-arm leak closed (arm bindings visible in guard + arm body
  only, never in sibling arms or after the `match`) — same shape with the
  shadowing binding in an arm pattern (`Ok(mut CACHE) => …`), and the
  post-`match` write recovering `global.mutation`.
- **AC-D2c.** Closure-param leak closed — same shape with `|mut CACHE: …| …`,
  and the write after the closure recovering `global.mutation`.
- **AC-D2d.** Explicitly recorded: Rust has **no cross-function collision
  fixture** — the mutation walker is per-unit and the `use`-table is
  top-level-only, so one function's bindings never reached another's
  classification in the first place. The collision Python's Delta closed is,
  in Rust, this *cross-scope* leak; the spec names the correspondence so
  nobody goes hunting for a missing Rust twin of the Python pinned test.

Snapshot diffs accepted under this delta: only recovered `global.mutation`
signals (each a leaked suppression removed), eyeballed individually.

### 5.4 Delta 3 — nested-item scoping (labelled; #55 residual close)

Nested `fn`s / nested-`impl` methods walked inside a unit get their own scope;
block-level items resolve as items.

- **AC-D3a.** Nested-`fn` params stop falling to the captured-binding FP:

  ```rust
  fn outer() {
      fn helper(v: &mut Vec<i32>) { v.push(1); } // → local.mutation
                                                 //   (today: hidden.mutation "captured-binding" FP)
  }
  ```

- **AC-D3b.** The no-capture rule (R5): a name inside a nested `fn` never
  resolves to the enclosing function's locals.

  ```rust
  static REGISTRY: Registry = Registry::new();
  fn outer() {
      let mut REGISTRY = Registry::new();
      fn helper() { REGISTRY.insert(1); }  // → global.mutation: the nested fn sees the STATIC
                                           //   (today: outer's leaked let_mut → local.mutation)
      REGISTRY.insert(2);                  // → local.mutation: outer's own shadowing local
  }
  ```

- **AC-D3c.** Block-level `static` items are the global channel at any depth:

  ```rust
  fn f() {
      static HITS: AtomicUsize = AtomicUsize::new(0);
      HITS.store(1, Ordering::Relaxed);  // → global.mutation (interior write to global)
                                         //   (today: not in the file statics set → no effect)
  }
  ```

- **AC-D3d.** Closure captures keep resolving naturally (regression guard, not
  a change): `let mut v = vec![]; let c = || v.push(1);` — the closure-body use
  of `v` walks up to the enclosing `Local` → `local.mutation`, as today.

Snapshot diffs accepted: captured-binding FPs turning into `local.mutation` or
`global.mutation`, and recovered block-static globals — each eyeballed.

### 5.5 Post-delta honesty check

After all three deltas, remaining `captured-binding` emissions in the Rust
fixtures/corpus must be genuinely out-of-lexical-reach names (macro-derived,
glob-import-obscured, or truly undeclared) — spot-check the dogfood scan (this
repo) and record the residual shapes in the PR description, the way
`008-dogfood-deltas.md` did.

## 6. Design constraints

### 6.1 Shared with Python (deliberately)

- The `BindingOrigin` vocabulary (all nine variants declared; Rust constructs
  five — §6.2), the `BindingId` segment convention, the
  `resolve(from_scope, name)` shape, the frontend-internal `scope.rs` module
  pattern, the compat-then-labelled-deltas migration, and the load-bearing
  invariant. Sharing is by **convention, not type**: no type moves to
  `fxrank-core`, no shared crate exists, nothing obliges the TS frontend later
  — it will have its own spec against the same guideline.

### 6.2 Rust-specific (deliberately not aligned)

- **Origins Rust never constructs:** `ModuleImport` (a Rust `use` path names a
  module *or* an item — syntactically indistinguishable, and Rust has no
  Python-#50-style module-method suppression to feed — so all `use`-bound
  names are uniformly `FromImport`, reproducing today's single `use`-table
  arm); `Global` (no `global`-declaration keyword — `static` is
  `ModuleBinding`, per the guideline); `EnclosingCaptured` (closures share
  their parent unit, so a capture is just an outer-scope `Local` reached by
  the walk; nested `fn`s cannot capture at all); `HostGlobalIo` (TS-only).
- **Per-binding metadata** (mutability, param channel) rides alongside the
  origin — Rust's classifier needs it (§4.3); Python's does not. Frontend-
  internal, so this asymmetry costs nothing.
- **Position-aware block scoping** (R1–R4) versus Python's two-kind
  function/module tree; the **scope cursor** in the walker versus Python's
  single `unit_scope`; the **per-unit tree** versus Python's per-file tree
  (§4.4 rationale). All three are Rust semantics or Rust-frontend structure,
  not model drift.

### 6.3 What this supersedes

`eaa9298`'s `seed_pat_locals` flat seeding (and, under the deltas, the
walk-order `locals`/`let_mut` sets themselves) are replaced by scope-tree
seeding. The interim commit's *outcomes* are preserved (AC-C2); its
*mechanism* is retired. Dead flat-set fields are deleted when their last
consumer moves over — no parallel bookkeeping left behind.

### 6.4 What already doesn't exist in Rust (recorded to prevent phantom work)

No `<module>`/`<script>` unit (Rust files have no top-level statements) — so
none of Python's module-unit-scope (`M`) machinery. No file-wide import
*binding* leak between functions (per-unit walker; top-level-only
`use`-table) — the collision axis is intra-function cross-scope (Delta 2).

### 6.5 Prior art / identity discipline

Id-equality resolution over scope-qualified ids (SCIP / rustc `DefPath`
shape), never bare-name matching — the same discipline #36 applied one level
up and Python applied at this level. The `BindingId` scheme is #36's
`canonical_path` extended below the unit; this spec reuses it unchanged.

### 6.6 Load-bearing invariant (mechanically checked)

Binding resolution never crosses the `UnitRecord` boundary. `fxrank-core` and
`UnitRecord` gain zero scope/binding types or fields; there is no
`fxrank-analysis` crate; a diff adding either is a review stop. Guarded by the
grep test (AC-C3). Only the resolver's *results* — the `BindingOrigin` consumed
by `mutation.rs` before the record is emitted — ever leave `scope.rs`.

## 7. Open questions (left for review, deliberately)

- **Q1 — by-value `mut` param writes.** Today `fn f(mut v: Vec<i32>) {
  v.push(1) }` emits **nothing** (`seed` discards param `is_mut`); the
  guideline's Rust note says a by-value param is a `Local` channel, which would
  make it `local.mutation` (contained, class 1). Adopting that is a *fourth*
  labelled delta (new low-weight signals across snapshots). Default proposal:
  keep today's silence through this spec, revisit on dogfood pressure — but
  the reviewer should confirm, since the guideline's wording leans the other
  way.
- **Q2 — nested-`fn` param origin.** §4.2 maps nested-item params to `Local`
  (closure symmetry; the nested item is not a collected unit, so a
  `param.mutation` on the *enclosing* unit's record would claim a caller
  channel the enclosing fn doesn't have). Alternative: `Param` with the `&mut`
  discount, reading the nested fn as an honest declared channel. Local is
  proposed; confirm at spec review.
- **Q3 — `let`-chains** (`if let Some(a) = x && let Some(b) = y`, Rust 2024).
  Coverage depends on the workspace `syn`'s representation; if representable,
  each pattern's bindings are visible to the rest of the chain and the body.
  Best-effort acceptance (a miss falls to `Unresolved` → the honest fallback),
  or a named Milestone-A exclusion — decide when the plan pins the `syn`
  version's actual AST.
- **Q4 — compat-tree mechanics.** AC-C1 allows a degenerate compat tree
  (flat unit scope, byte-identity by construction) before the lexical tree
  lands with the deltas. If, when writing the plan, routing straight to the
  lexical tree turns out to be snapshot-identical on the existing corpus
  anyway (plausible: the corpus may hold no leak-shaped code), the two-step
  can collapse — but only with the byte-identity assertion still run and
  passing as its own commit gate.
