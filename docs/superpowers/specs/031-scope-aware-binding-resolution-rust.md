# Spec 031 — Scope-aware binding resolution, Rust frontend (#78)

Status: **Draft** · Issue: #78 (Rust step; #55 is the symptom catalogue) · Milestone: A (syntactic only)

**Depends on:** #83 — *Rust nested-unit collection
alignment* — which brings `functions::collect` into line with the Python and TS
frontends by making nested `fn`s **and** closures their own `UnitRecord`s (§1.1).
This spec's per-file scope tree (§4.4), its `EnclosingCaptured` handling, and its
`param.mutation` classification of nested-unit params (§4.3) all assume that
alignment has landed. Spec 031 is the **resolver** built on top of it; it does
**not** implement the collection/propagation change itself (N3).

Reference material this spec builds on — read these first:

- `docs/scope-aware-binding-resolution-guideline.md` — the shared #78 model
  (`BindingOrigin` vocabulary, binding-id scheme, resolution rule, load-bearing
  invariant). This spec instantiates it for Rust; where Rust diverges, the
  divergence is stated here and belongs in that doc's Rust bullet when it lands.
- `docs/superpowers/plans/030-scope-aware-binding-resolution.md` (the Python
  pilot's plan — renumbered from its on-disk `078-…` name; same document) +
  `crates/fxrank-lang-python/src/scope.rs` — the **Python pilot**, the one
  proven witness. Its shape (frontend-internal `scope.rs`, **one parented tree
  per file**, compat-mode port, labelled deltas) is the shape this spec follows;
  its *scope model* is not (Python is function-scoped; Rust is block-scoped — §6).
- Commit `eaa9298` — the **interim Rust binding seed** (flat, function-scoped
  seeding of `for`/`if let`/`while let`/`match`-arm/closure bindings). This spec
  **supersedes** that stepping-stone with real lexical scoping; its FP-removal
  test (`binding_forms_seed_locals_not_captured`) is part of the compat baseline
  that must stay green (§5.1, and the AC-C2 note on the closure form).

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
   scope), one granularity down.
3. **Nested items are mis-scoped.** Today a nested `fn` (or a method in a nested
   `impl`) is walked as part of the enclosing unit, so its params are never
   seeded → `hidden.mutation "captured-binding"` FPs (the #55 family), and —
   worse — the enclosing function's locals *are* visible to it, though Rust
   nested `fn`s **cannot capture**. Block-level `static` items aren't in the
   file-level `statics` set, so writes to them fall to the captured-binding FP
   instead of `global.mutation`. §1.1's alignment (nested `fn`s become their own
   units) plus the resolver's parented file-level tree fix all three at once.

The interim seed (`eaa9298`) fixed the ~76× #55 misfires by widening the flat
sets to all binding forms — acknowledged there as the Milestone-A stepping-stone.
This spec is the finished subsystem: replace flat-set membership with resolution
to the **nearest enclosing binding** over a real Rust scope tree, per the
guideline.

### 1.1 Cross-language alignment — nested `fn`s and closures are their own units

The Python and TS frontends already collect nested functions and closures as
**separate units**, and the enclosing own-body walk does **not** descend into
them:

- **Python** (`functions.rs`): a nested `def`/`lambda` is its own `FnUnit`; the
  own-body walker "does NOT descend into nested `def`/`class` bodies (those are
  separate units)" (the `FnBody::Module` doc comment).
- **TS** (`functions.rs`): "Nested functions are their own units … child effects
  are never rolled into the parent" — and the file is self-described as "the swc
  analog of `fxrank-lang-rust`'s `functions::collect`".

Rust is the **outlier**: `collect_items` recurses only into `Item::Mod`, never
into `fn` bodies or closures, so a nested `fn`/closure is walked as part of its
enclosing unit and its effects fold into that unit's own-body. **This spec
depends on bringing Rust in line** — the paired sibling issue #83 *Rust nested-unit
collection alignment*, which:

1. makes `functions::collect` **descend into `fn` bodies and closures**, emitting
   each as its own unit;
2. stops the **enclosing own-body walk from descending** into a nested unit; and
3. lets a nested unit's effects reach the enclosing caller **only via the
   spec-025 cross-file fold** (call-graph propagation), never by own-body
   inlining.

This is an **alignment, not a scope expansion**: Rust adopts the collection shape
Python and TS already have. The **one** cross-language exception is TS-only React
hook-callback absorption (spec 027), where a hook callback is deliberately folded
back into its component — there is no Rust analogue; it is mentioned only to scope
the alignment correctly.

Spec 031 is the **resolver** on top of that alignment. Its scope tree (§4.4) is
per-file and parented **precisely because** nested units must now resolve
captures and enclosing items **across** the unit boundary (§4.4 rationale).

## 2. Goals

- **G1.** A frontend-internal resolver (`crates/fxrank-lang-rust/src/scope.rs`)
  that answers `resolve(from_scope, name) → (BindingOrigin, BindingId)` over a
  lexical scope tree built **once per file** from its `syn` AST — mirroring the
  Python `scope.rs` pattern (same vocabulary, same id convention, one parented
  tree per file, own scope model).
- **G2.** Port the mutation classifier onto the resolver **compat-first**:
  byte-identical snapshot output — asserted against the (post-sibling) corpus —
  before any behavior change (§5.1).
- **G3.** Land the precision fixes as **separately labelled intentional deltas**
  with their own fixtures (§5.2–5.4): shadowing/rebinding precision, cross-scope
  leak close, nested-item scoping (incl. block-level `use`/`const`).
- **G4.** Model the real Rust binding forms: signature params, `let` (incl.
  `let else`), `for`, `if let`/`while let`, `match` arms (incl. `mut`, `ref`,
  `@`-bindings, struct/tuple/slice destructuring, or-patterns), closure params,
  nested-`fn`/`impl`-method params, block items (`fn`/`impl`/`static`/`const`/
  `use`), and `let`-chains — §4.
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
- **N3. The nested-unit collection + propagation change itself.** Making
  `functions::collect` descend into `fn`/closure bodies, and folding a nested
  unit's effects into its caller via the 025 cross-file graph, is the **paired
  sibling issue** this spec *depends on* (§1.1) — **not** this spec. Spec 031
  assumes that alignment has landed and changes only *which binding a name
  resolves to*, never *which unit an effect lands on* or how units are collected.
- **N4. Macro-expansion scoping.** Bindings created or used inside unexpanded
  macro token streams stay invisible (existing `macros.rs` behavior). Accepted
  Milestone-A miss. See also N10 for the macro-*adjacent* boundary.
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
- **N8. Nested `mod` blocks.** An inline `mod m { … }`'s items are **not**
  bare-name-visible outside `m` (they need `m::` qualification), so they are
  explicitly out of scope — distinct from block-level item hoisting (R4), which
  *is* bare-name-visible within its block. `collect_static_names` and the
  `use`-collector already do not recurse into inline modules; the resolver keeps
  that boundary.
- **N9. Generic type params and const generics.** A type param `T` and a
  const-generic `N` (`fn f<const N: usize>()`) are bare identifiers that must
  **not** resolve as value bindings: a `T` sits in the type namespace (never a
  write base), and a const-generic `N` is a read-only value (never a write
  target). Milestone A ignores both, as it ignores lifetimes (§4.3); they are
  simply not seeded, so a use in value position that somehow reaches
  `record_write` would fall to the honest `Unresolved` fallback rather than a
  spurious value binding. Recognizing a const-generic *read* as a first-class
  binding is left open (§7 Q4).
- **N10. Macro-adjacent identifiers.** Beyond N4: how `syn`-parsed tokens
  *around* a macro invocation (`println!(…)` args, `vec![…]` elements) are
  treated is not specified further — identifiers `syn` surfaces as ordinary
  expressions resolve by the ordinary rules; anything only inside an unexpanded
  macro body stays invisible. A documented Milestone-A boundary, flagged in §7.

## 4. Requirements

### 4.1 Scope model — Rust is block-scoped, position-aware, no hoisting of `let`

The resolver builds a scope tree per the guideline's `module → function →
block` shape, instantiated for Rust as **one parented tree per file**:

```
file scope
  →  unit scope (fn / method body root)
       →  block / arm / closure scopes …
       →  nested-fn / nested-impl-method scope (its own unit)
            →  its own blocks …
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
  `match`. Closure params bind in the closure body (which is now its own unit —
  R5).
- **R4 — item scoping and hoisting.** Items declared in a block (`fn`,
  `impl`, `static`, `const`, `use`) are visible **throughout that block**,
  including before their textual position (items hoist; `let` does not). With
  the per-file parented tree (§4.4), an enclosing block's items are reached from
  a nested unit by the ordinary parent walk — no separate item-only lookup
  dimension is needed (this supersedes the earlier draft's "separate item
  dimension"; item *hoisting*, below, still needs its own handling and fixture —
  §5.4).
- **R5 — capture rules differ by unit kind (Rust now constructs
  `EnclosingCaptured`).**
  - A **nested `fn` / nested-`impl` method** (its own unit) **cannot capture**:
    resolution from inside it must **not** see the enclosing function's
    `let`/param bindings — only enclosing *items* (R4) and the file scope. The
    parent walk therefore skips the enclosing unit's locals but reaches its
    items and file scope.
  - A **closure** (also its own unit now — §1.1) **does capture**: a use inside
    the closure body of an enclosing `let`/param resolves **across the unit
    boundary** to that enclosing binding → `EnclosingCaptured`. This is *why*
    Rust now constructs `EnclosingCaptured` (reversing the earlier draft, which
    said Rust never would — that draft was premised on closures sharing their
    parent unit, which §1.1 removes) and *why* the tree must be per-file and
    parented (§4.4).
- **R6 — file scope.** The root scope holds the file-level tables the detector
  already receives: top-level `static` names → `ModuleBinding`; `use`-table
  names → `FromImport` (§6.2 for why never `ModuleImport`); top-level `const`
  names → a non-writable file-scope binding (name-only, for correct shadowing —
  a `const` is never a write target; §4.2 row).
- **R7 — `let`-chains** (`if let Some(a) = x && let Some(b) = y { … }`, Rust
  2024). A **Milestone-A requirement when the pinned `syn` exposes their
  scopes**: each `let` operand's bindings are visible to the **later operands
  and the chain's body**, and not outside the chain. If the pinned `syn` version
  does **not** expose let-chain scopes cleanly, this becomes an **explicit,
  documented Milestone-A exclusion** carried by an `#[ignore]`d / pending
  fixture — **never** a silent best-effort that lets the bindings fall to
  `Unresolved` (which would reintroduce the `captured-binding` FP this whole
  spec removes). The plan pins which branch applies once it fixes the `syn`
  version. (Promoted from the earlier draft's open Q3 — now decided.)

### 4.2 Binding forms — what seeds where, what `resolve` returns

Every form below must be seeded by the resolver; each row states the scope it
binds in and the `BindingOrigin` a use of that name resolves to. Per-binding
metadata (frontend-internal, carried alongside the origin): **mutability**
(`mut` vs not) and, for params, the **channel** (`&mut` / shared `&` /
by-value) — the classifier needs both (§4.3). **Lifetimes** (`'a`) sit in a
separate namespace and are ignored by the resolver; they never change a score.

| Form | Example | Binds in | `resolve` returns |
|---|---|---|---|
| signature param, `&mut T` | `fn f(v: &mut Vec<i32>)` | unit scope | `Param` (channel `&mut`; `is_mut`/channel derived from the **`&mut` type**, not a `mut` keyword on the binding) |
| signature param, shared `&T` | `fn f(s: &S)` | unit scope | `Param` (channel shared) |
| signature param, by-value | `fn f(x: T)` / `fn f(mut y: T)` | unit scope | `Param` (channel by-value; `mut`ness kept) |
| `self` receiver | `&self` / `&mut self` / `self` | handled **before** `resolve`, unchanged from today (incl. the existing nested-`impl` misattribution guards); the shared-`&self` case still participates in the interior-mut check (§4.3) | — |
| `let` / `let mut` (destructuring incl.) | `let (a, mut b) = t;` | enclosing block, from next stmt (R2) | `Local` |
| `let else` | `let Some(x) = o else { return };` | enclosing block after stmt; **not** the `else` block | `Local` |
| `for` pattern | `for mut x in v { … }` | loop body | `Local` |
| `if let` / `while let` pattern | `if let Some(mut x) = o { … }` | then-branch / loop body | `Local` |
| `let`-chain operand pattern | `if let Some(a)=x && let Some(b)=y {…}` | later operands + chain body (R7) | `Local` |
| `match` arm pattern (incl. `mut`, `ref`/`ref mut`, `@`, struct/tuple/slice, or-patterns, `*rest`-like slice tails) | `Ok(mut x)`, `P { x, .. }`, `[a, rest @ ..]`, `n @ 1..=9` | that arm (guard + body) | `Local` (or-pattern names bind once per arm) |
| closure param, reference-typed (`&mut`/`&`) | `\|buf: &mut Vec<_>\| buf.push(1)` | the closure's **own unit** scope | `Param` of the closure unit (channel by type — §1.1 makes a Rust closure its own unit; **only a reference-typed param is `Param`**) |
| closure param, by-value | `\|mut acc: Vec<_>\| acc.push(1)` | the closure's **own unit** scope | `Local` of the closure unit (a by-value param is a `Local` — the guideline's Rust note; `mut` is honored via the interim `seed_pat_locals` path, so a by-value `mut` closure param is a `mut` `Local` → `local.mutation`, AC-C2) |
| nested `fn` / nested `impl` method param, reference-typed (`&mut`/`&`) | `fn outer() { fn helper(v: &mut Vec<i32>) {…} }` | the nested item's **own unit** scope | `Param` of the nested unit (channel by type; a `&mut` param → `param.mutation` with the mut-channel discount — §4.3, decision resolving the earlier Q2) |
| nested `fn` / nested `impl` method param, by-value | `fn outer() { fn helper(mut x: Vec<i32>) {…} }` | the nested item's **own unit** scope | `Local` of the nested unit (a by-value param is a `Local` — the guideline's Rust note; **origin `Param` is reserved for reference-typed params**) |
| capture of enclosing `let`/param (closure body) | `let mut v=…; \|\| v.push(1)` | resolves outward across the unit boundary | `EnclosingCaptured` (nested `fn`s cannot capture — R5) |
| block-level `static` | `fn f() { static HITS: AtomicUsize = …; }` | its block, hoisted (R4) | `ModuleBinding` — Rust's global-write channel regardless of nesting depth |
| block-level `const` | `fn f() { const N: usize = 3; }` | its block, hoisted | non-writable `Local` (a `const` cannot be written; the binding exists so the *name* shadows correctly during resolution) |
| block-level `use` | `fn f() { use std::mem::swap; }` | its block, hoisted | `FromImport` (today: invisible — the `use`-table is top-level-only — so such a base falls to the captured-binding FP; correctness gain landed under a labelled delta with named fixtures — §5.4) |
| top-level `static` | file item | file scope | `ModuleBinding` |
| top-level `const` | `const K: usize = 3;` | file scope | non-writable file-scope binding (name-only; mirrors the block-level `const` row — a `const` is never a write target, so it yields no write effect; seeded so the name shadows and never falls to `captured-binding`). **A write-shaped use of a top-level `const` name today falls to `captured-binding`, so seeding it is a behavior change — landed under a labelled delta (AC-D3e), not compat.** |
| top-level `use` name | `use std::fs;` / `use m::CACHE as C;` | file scope | `FromImport` |
| nothing in scope | — | — | `Unresolved` — the honest fallback |

Pattern-binding collection reuses `collect_pat_bindings` (already recursion-
complete over tuple/struct/tuple-struct/slice/reference/or/paren/type patterns
and `@`-subpatterns) — the resolver seeds *scopes* from it instead of flat sets.

**Known low-impact gap (recorded, not fixed):** unit-struct and fieldless-enum
value-namespace names (`struct U;`, `enum E { A }` used as the bare value `U` /
`A`) are value-namespace identifiers this table does not seed. A write *through*
one is not a realistic place expression, so the miss is FN-leaning and
low-impact; noted here for the "every form" completeness ledger.

### 4.3 Origin → effect: the compat map

The resolver owns **name → origin**; `mutation.rs` keeps owning **origin →
effect**. The compat-mode map must reproduce today's `record_write` cascade
exactly (evidence strings, tiers, discounts, containment, the `unsafe`
discount-cancel — all byte-identical):

| Resolved origin (+ metadata) | Effect | Notes (today's arm) |
|---|---|---|
| `Param`, channel `&mut` | `param.mutation`, `Discount::MutParam` | `mut_params` arm. Now also covers a `&mut`-typed **nested-`fn`/closure** param, attributed to that nested **unit's own** record (where the `&mut` is genuinely visible in its signature — decision resolving Q2). The discount is a **partial** containment discount, not full erasure: a `&mut` declaration means the callee *may* mutate but the channel is declared-and-bounded at the call site. |
| (`self` + `&mut self`) | `param.mutation`, `Discount::MutSelf` | pre-resolve, unchanged |
| `Local`, `mut` | `local.mutation`, Exact, contained | `let_mut` arm. Also a by-value `mut` **closure / nested-unit** param (its own unit's `Local`, `mut` honored via the interim `seed_pat_locals` path) — this is why AC-C2's `\|mut x\| x.push(1)` stays `local.mutation`. |
| `Local`, non-`mut` | **no effect** | today's fall-through silence (`locals` membership suppresses every later arm). Also a by-value non-`mut` closure / nested-unit param. |
| `Param`, by-value (**signature** param only) | **no effect** | same silence today (`seed` inserts a by-value signature param into `locals` only, discarding `mut`) — see Open Question Q1. Only **signature** params reach this row: origin `Param` is reserved for reference-typed (`&mut`/`&`) params, so a by-value **closure / nested-unit** param is a `Local` (above), never a `Param`. |
| `ModuleBinding` | `global.mutation`, Heuristic, `write to global {base}` | `statics` arm |
| `FromImport` | `global.mutation`, Heuristic, `write to imported {base}` | `use`-table arm (008-F5; near-vacuous in Rust, kept for symmetry) |
| `EnclosingCaptured` | `hidden.mutation`, subreason `captured-binding` | Rust has no `nonlocal`, so every capture is the guideline's **implicit** arm. A closure that mutates a captured enclosing `let` resolves here. **Attribution note:** relative to *today*, the reclassification of a captured-local mutation from the enclosing unit's `local.mutation` to the closure unit's `hidden.mutation` is produced by the **sibling** collection change (§1.1), which re-homes the closure into its own unit; 031's resolver only *names* the origin. Whether the 025 fold should re-contain such a write at the enclosing owner is a propagation question for the sibling — flagged for review (§7). |
| `Unresolved` | `hidden.mutation`, subreason `captured-binding` | 008-F1 arm |

The resolver is also the base oracle for the **other two** flat-set consumers
in `mutation.rs`, with the same compat requirement:

- **interior-mutator receiver check** (`borrow_mut`/`set`/`store`/… on a
  shared-`&` base → `hidden.mutation "interior-mut"`): consults
  `Param`-with-shared-channel instead of `shared_refs` membership — the
  anti-Goodhart `&self`/`&T` inversion must be preserved verbatim;
- **interior-mutator-on-static check**: consults `ModuleBinding` instead of
  `statics` membership.

**How `self` participates (must not silently drop).** `self` is handled before
`resolve`, as today. The shared-`&self` receiver is modeled so the interior-mut
check still fires on it: either `self` resolves to a `Param`-with-shared-channel
origin (seeded into the unit scope under the name `self`, the natural mirror of
today's `shared_refs.insert("self")`), **or** an explicit pre-resolve
`shared_self` set is retained and named. Whichever the plan picks, the
requirement is that the `&self` interior-mut inversion is preserved verbatim and
`self` never silently falls to `Unresolved`. The `&mut self` case stays
`param.mutation`/`MutSelf` (pre-resolve, unchanged).

**Interior-mutator path silence (compat-critical).** The interior-mutator path
emits for **exactly two** resolved origins — `Param`-shared (→ `interior-mut`
hidden mutation) and `ModuleBinding` (→ interior write to a global). For **every
other** origin it stays **silent**, exactly as today: a `RefCell` **local**
(`let x = RefCell::new(..); x.borrow_mut()`) resolves to `Local` and an unknown
base resolves to `Unresolved`, and **neither** may regress into a spurious
`local.mutation`/`hidden.mutation`. The resolver replaces the
`shared_refs`/`statics` membership tests with origin checks, but the
two-arms-then-silence shape is preserved verbatim.

`ModuleImport`, `Global`, and `HostGlobalIo` are **never constructed by the Rust
resolver** (§6.2) — the match arms for them are `unreachable!` (or folded,
clippy-clean), mirroring Python's `HostGlobalIo` handling. `EnclosingCaptured` is
now constructed (it left this list — §6.2).

### 4.4 Resolver architecture

- **`scope.rs` is frontend-internal** (`pub(crate)`), one responsibility:
  name-use → `(BindingOrigin, BindingId)` over a scope tree. Same module shape,
  vocabulary spelling (`BindingOrigin`, `BindingId`, `ScopeTree`, `ScopeId`,
  `resolve(from_scope, name)`) as Python's `scope.rs`.
- **The tree is per FILE, parented** — mirroring Python's `scope.rs` (one tree
  per parsed file, every `def`/`lambda` parented to its lexical enclosing scope).
  Parent links run `file/module scope → unit (fn body) scope → nested-fn scope →
  closure scope → block/arm scopes`. **Rationale (a genuine reversal of the
  earlier per-unit draft):** now that nested `fn`s and closures are their own
  units (§1.1), (i) a closure's capture of an enclosing `let` must resolve
  **across** the unit boundary to that enclosing binding → `EnclosingCaptured`,
  and (ii) a nested unit must see the enclosing block's **items** (R4) — both are
  cross-unit lookups only a parented, file-level tree can answer; a per-unit tree
  has no parent to walk into.
  - **Retraction of the earlier per-unit rationale.** The prior draft justified a
    per-unit tree by "Rust has no `<module>` unit" and "`FnUnit` clones its
    `sig`/`block`, so Python's body-pointer identity trick is unavailable." Both
    were premised on the *old* behavior — nested `fn`s staying inside the
    enclosing unit and never capturing — which §1.1 removes. With nested units
    and captures, the file-level parented tree is required, and (as in Python) it
    is built once over the parsed `syn::File`. Unit→scope mapping follows Python's
    body-pointer identity discipline where usable; if `FnUnit`'s cloned `block`
    breaks pointer identity, the plan seeds the mapping at build-time by
    structural position instead — a plan-level mechanism choice, not a spec
    divergence.
- **Compat seeding is walk-time and arm-order-faithful (compat-critical).**
  Today's flat model seeds its sets *during* the body walk (`visit_local`, etc.)
  and classifies by a fixed arm order. The compat resolver must therefore (a)
  **seed at walk-time**, not eagerly, so any position-sensitive membership is
  reproduced, and (b) reproduce today's **exact arm precedence** across the
  **two separate entry points** (mirroring how §4.3 already splits them):
    - the **`record_write` cascade** (assignments, compound-assign, and a
      mutating-method receiver): `mut_params` → `&mut self` → `let_mut` →
      `statics` (if not a local) → `imports` (if not a local) → captured-binding
      fallback;
    - the **interior-mutator method-call path** (`mutation.rs` ~406–446, a
      distinct entry point — *not* a `record_write` arm): `&self`/`&T`-shared
      (`shared_refs`) checked **first** → `statics` → otherwise silent. The
      shared-`&` inversion is checked before that path's **own** `statics` arm,
      which is why the anti-Goodhart `&self` interior-mut case stays `hidden`.
  Reproduce today's **dual set-membership**: a `&mut T` param name lives in
  **both** `locals` and `mut_params`; a `let mut x` lives in **both** `locals`
  and `let_mut`; a `&T` shared param (and `&self`) lives in **both** `locals`
  and `shared_refs`. The resolver's origin+metadata must encode every membership
  so the cascade lands on the same arm. **Interior-path shadow-blindness
  (compat-critical):** the interior-mutator path tests `shared_refs`/`statics`
  membership with **no `!locals` guard**, so a shadowing rebind does *not*
  suppress it today — `fn f(r: &S) { let r = other(); r.borrow_mut(); }` still
  emits `interior-mut`, and a same-named `static` + local still emits the
  interior write-to-global. Compat reproduces this **shadow-blind** behavior
  **byte-for-byte**; the shadow-aware correction (a closer shadowing local
  suppressing the shared-ref / static interior write) lands only as a named
  **Delta-1 / Delta-2** shape, never in the compat port. Cross-reference Python **plan 030**'s explicit insertion-order
  note (its `build_funcdef` inserts imports→locals→params→nonlocals→globals,
  low-to-high precedence, last-write-wins). This replaces the earlier
  "byte-identical **by construction**" phrasing with **"asserted byte-identical
  against the existing (post-sibling) corpus"** — AC-C1 runs the assertion as its
  own gate.
- **File-scope precedence — statics over imports.** A file-scope name present in
  **both** the `statics` set and the `ImportTable` resolves to `ModuleBinding`
  (the `statics` arm precedes the `imports` arm in today's cascade). A latent
  byte-identity assumption; a genuine `static X` + `use m::X` collision is
  `E0255` (a compile error) anyway, so the precedence is unobservable on valid
  input but pinned for determinism.
- **`ImportTable` seeding — lazy fallback (default) vs. enumeration.**
  `ImportTable` today exposes only `resolve(name)` / `has_glob()` (a point
  lookup); it cannot **enumerate** its local names. Seeding every `FromImport`
  binding eagerly into the file scope would need either a new enumeration
  accessor on `ImportTable`, or the route Python's `scope.rs` actually takes for
  the eager case: **ignore the `Imports` table and re-collect the top-level
  imports directly from the `syn` AST** (Python's `ScopeTree::build` takes an
  `Imports` parameter but leaves it unused — `_imports` — and collects the
  module's top-level imports straight from the parsed module instead; there is
  no `iter_local_names` accessor). The **default recommendation is
  the lazy fallback**: seed no imports up front, and at the point a name **misses**
  every other scope, consult `imports.resolve(name).is_some()` before falling to
  `Unresolved` — simpler, adds no API, and preserves `statics`-before-`imports`
  naturally (statics are seeded; imports are the last-resort lookup). The
  enumeration-accessor route is named as the alternative should a future consumer
  need the full import set materialized.
- **The walker carries a scope cursor.** Because Rust has block scopes and
  positional `let` visibility, the walker tracks its current scope while
  descending (push on block/arm/closure/nested-unit entry, pop on exit). For a
  nested unit the cursor enters that unit's own scope (whose parent is the
  enclosing scope), so a closure capture walks outward across the boundary while a
  nested-`fn` local does not leak to its enclosing unit (R5). This is a divergence
  from Python's single `unit_scope`, required by the scope model: Python resolves
  every use from one function scope; Rust resolves from the use's innermost scope.
- **`let` sequential visibility** (R2) should be modeled so that
  `resolve(from_scope, name)` keeps its two-argument shape — e.g. each `let`
  opens an implicit child scope covering the remainder of its block (rustc's own
  desugaring), so every scope holds at most one binding per name and resolution is
  a pure parent walk. The exact mechanism is the plan's choice; the observable
  requirements R2/R4/R7 are what this spec pins.

### 4.5 Binding-id scheme

Reuse the guideline's convention verbatim: `full_binding_id =
unit.canonical_path ++ scope_segments ++ ["kind:name"]`, with `scope_segments`
as **structural lexical labels** below the unit's body root (block ordinals,
`arm2`, closure markers, `let`-split ordinals) — never `line:col`. As in the
Python step, nothing composes or persists the full id in this work; it exists
so two same-named bindings are distinct values and so the three frontends
reason alike. Kind prefixes reuse Python's (`param:`, `local:`, `from:`,
`module:`, `captured:`, `unresolved:`); Rust adds none.

## 5. Acceptance criteria

The migration discipline is the guideline's §*Migration discipline*, verbatim:
**compat first, then labelled deltas** — each delta touches only its named
fixtures/snapshots, and its commit says *intentional delta*, never fixes mixed
into the port. Spec 031 sequences **after** the sibling alignment (§1.1); its
compat baseline is the **post-sibling** corpus.

### 5.1 Compat gate (must land first, in full, before any delta)

- **AC-C1.** All mutation classification is routed through
  `ScopeTree::resolve` (all three consumers in §4.3), and `cargo insta test -p
  fxrank-lang-rust` reports **zero pending snapshots** — output **asserted
  byte-identical** against the existing (post-sibling) corpus, including evidence
  strings. The assertion is its own commit gate (walk-time seeding + arm
  precedence + dual set-membership, §4.4). The compat tree may be deliberately
  degenerate (unit scope mirroring today's walk-order flat seeding + file scope);
  the real lexical tree then lands under the deltas below.
- **AC-C2.** `binding_forms_seed_locals_not_captured` (the `eaa9298` #55
  baseline — `for`/`if let`/`while let`/`match`-arm/closure writes are
  `local.mutation`, never `hidden.mutation`) stays green at **every** commit of
  this work. The four non-closure forms hold verbatim (Locals of the enclosing
  unit). The **closure form** (`let c = |mut x: Vec<i32>| x.push(1)`) is
  re-homed by the sibling: `x` is now the closure unit's by-value `mut` `Local`
  (a by-value param is a `Local`, §4.2/§4.3 — origin `Param` is reserved for
  reference-typed params).
  The invariant 031 preserves is the FP-removal essence — the closure form still
  **never** falls to `captured-binding` `hidden.mutation`; its `local.mutation`
  classification is inherited byte-for-byte from the sibling's collection
  baseline (which preserves the interim seed's outcome for by-value `mut` closure
  params — the `param.mutation` reclassification of decision §4.3 applies only to
  **`&mut`-typed** params and is a labelled delta, not the compat port, so it does
  not touch this test). Flagged as the one place the compat baseline meets the
  sibling seam (§7).
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

A binding introduced in a block / arm / `if let` / `for` / `while let` /
`let else` / closure no longer exists outside its scope, so a same-named later
write to a file `static` recovers its `global.mutation` signal.

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
- **AC-D2e — `for`-pattern leak closed.** `for mut CACHE in xs { CACHE.push(1); }`
  followed by a post-loop write to the `static CACHE` recovering
  `global.mutation`. (One of the four interim-seed forms the earlier Delta-2
  fixture set omitted — it covered only block/match-arm/closure.)
- **AC-D2f — `if let` leak closed.** `if let Some(mut CACHE) = o { CACHE.push(1); }`
  then a post-`if` write recovering `global.mutation`.
- **AC-D2g — `while let` leak closed.** `while let Some(mut CACHE) = it.next() { … }`
  then a post-loop write recovering `global.mutation`.
- **AC-D2h — `let else` shadowing a `static`.** `let Some(mut CACHE) = o else { return };`
  binds `CACHE` in the enclosing block after the statement (shadowing the static
  there), while the diverging `else` block does **not** see it — pinned by a use
  inside `else` resolving to the `static` (`global.mutation`), plus a probe that
  the diverging `else` block's non-visibility of the bound name holds (a use of
  `CACHE` inside `else` is the static, never the pattern binding).
- **AC-D2d.** Explicitly recorded: Rust has **no cross-function collision
  fixture** — the mutation walker is per-unit and the `use`-table is
  top-level-only, so one *top-level* function's bindings never reached another's
  classification in the first place. The collision Python's Delta closed is, in
  Rust, this *cross-scope* leak; the spec names the correspondence so nobody
  goes hunting for a missing Rust twin of the Python pinned test.

Snapshot diffs accepted under this delta: only recovered `global.mutation`
signals (each a leaked suppression removed), eyeballed individually.

### 5.4 Delta 3 — nested-item scoping (labelled; #55 residual close)

Nested `fn`s / nested-`impl` methods (now their own units — §1.1) get their own
scope; block-level items resolve as items; block-level `use`/`const` land here
under this labelled delta with named fixtures.

- **AC-D3a (post-sibling baseline regression PIN, not a 031 snapshot delta).**
  Nested-`fn` params classify as `param.mutation` on the nested fn's **own** unit:

  ```rust
  fn outer() {
      fn helper(v: &mut Vec<i32>) { v.push(1); } // → param.mutation (Discount::MutParam),
                                                 //   on helper's OWN unit
                                                 //   (pre-sibling: hidden.mutation "captured-binding" FP)
  }
  ```

  The `&mut` **type** supplies the channel; the binding `v` carries no `mut`
  keyword, so `is_mut`/channel derive from `&mut Vec<i32>` (§4.2). A lifetime on
  the type does not change the score. **This already holds in the post-sibling
  flat baseline** — once the sibling makes `helper` its own unit, `detect()` runs
  per-unit and `seed()` sees the `&mut` param directly → `mut_params` →
  `param.mutation`; 031 changes nothing here. It is pinned as a compat-gate
  regression guard, not a diff 031 owns.

- **AC-D3b (post-sibling baseline regression PIN, not a 031 snapshot delta).**
  The no-capture rule (R5): a name inside a nested `fn` never resolves to the
  enclosing function's locals — it sees the enclosing *static* (an item, R4)
  instead.

  ```rust
  static REGISTRY: Registry = Registry::new();
  fn outer() {
      let mut REGISTRY = Registry::new();
      fn helper() { REGISTRY.insert(1); }  // → global.mutation: the nested fn sees the STATIC
                                           //   (pre-sibling: outer's leaked let_mut → local.mutation)
      REGISTRY.insert(2);                  // → local.mutation: outer's own shadowing local
  }
  ```

  Like AC-D3a, **this already holds in the post-sibling flat baseline**: `helper`
  is its own unit, so `detect()` on it never sees `outer`'s `REGISTRY` local and
  resolves the static (`global.mutation`), while `outer`'s own shadow local stays
  `local.mutation`. Pinned as a compat-gate regression guard, not a 031 diff.

- **AC-D3c.** Block-level `static` items are the global channel at any depth:

  ```rust
  fn f() {
      static HITS: AtomicUsize = AtomicUsize::new(0);
      HITS.store(1, Ordering::Relaxed);  // → global.mutation (interior write to global)
                                         //   (today: not in the file statics set → no effect)
  }
  ```

- **AC-D3c2 — item hoisting (R4's headline claim, now tested).** A block-level
  item used **before** its textual declaration still resolves as that item:

  ```rust
  fn f() {
      HITS.store(1, Ordering::Relaxed);          // → global.mutation: the static below is hoisted
      static HITS: AtomicUsize = AtomicUsize::new(0);
  }
  ```

- **AC-D3d — closure capture resolves to `EnclosingCaptured` (regression PIN,
  produces NO snapshot diff).** A closure body's use of an enclosing `mut` local
  resolves **across the unit boundary** to `EnclosingCaptured` (Rust now
  constructs it — §4.3), not to a `Local` of a shared parent unit:

  ```rust
  fn f() {
      let mut v = vec![];
      let c = || v.push(1);  // closure body's `v` → EnclosingCaptured
      let _ = c;
  }
  ```

  Per the §4.3 map this yields `hidden.mutation "captured-binding"` at the
  closure's own unit. **This produces no snapshot diff:** the post-sibling flat
  baseline already emits `hidden.mutation "captured-binding"` for the unseeded
  captured base `v` (it misses every flat set → `Unresolved` fallback); 031 only
  renames the **internal** origin `Unresolved` → `EnclosingCaptured`, which the
  §4.3 map sends to the *same* effect and evidence. The *today → post-sibling*
  change (closure re-homed into its own unit) is owned by the sibling (§1.1); 031
  asserts only that the resolver returns `EnclosingCaptured` for the captured
  base. Whether the 025 fold should re-contain the write at `f` (the owner of
  `v`) is a sibling propagation question — flagged (§7).

- **AC-D3e — block-level `use`/`const` + top-level `const` (decision 5, F5).**
  Named fixtures:
  - `fn f() { use std::mem::swap; swap(&mut a, &mut b); … }` — a block-level
    `use` name resolves as `FromImport` instead of falling to the captured-binding
    FP;
  - `fn f() { const N: usize = 3; … }` — a block-level `const` is a non-writable
    binding that shadows correctly and yields no write effect;
  - `const K: usize = 3; fn f() { … }` — a **top-level** `const` is likewise a
    non-writable file-scope binding; a write-shaped use of its name yields no
    effect instead of falling to the captured-binding FP (a `hidden.mutation`
    removed — a genuine 031-owned snapshot diff, which is why top-level `const`
    seeding is folded here rather than into the compat port; §4.2 row).
  Because block-level `use`/`const` (and top-level `const`) widen the
  resolvable-name surface, the
  **new-FP-surface risk** (a bare name newly resolving where it used to fall
  through) is checked against the dogfood scan under §5.5.

Snapshot diffs accepted under this delta: **only the genuinely 031-owned diffs**
— **AC-D3c** (recovered block-level `static` globals), **AC-D3c2** (item
hoisting: a block-level static used before its declaration), and **AC-D3e**
(block-level `use` → `FromImport`, block-level `const` → non-writable, and
top-level `const` seeding — F5) — each eyeballed. **AC-D3a / AC-D3b / AC-D3d are
post-sibling baseline regression pins, not 031 snapshot deltas** — they already
hold in the post-sibling flat corpus (see each), so they contribute no diff here;
AC-D3d in particular produces **no** diff at all (031 only renames the internal
origin `Unresolved` → `EnclosingCaptured`, mapping to the same effect).

### 5.5 Post-delta honesty check

After all three deltas, remaining `captured-binding` emissions in the Rust
fixtures/corpus must be **either** genuinely out-of-reach names (macro-derived,
glob-import-obscured, or truly undeclared) **or** resolved captures
(`EnclosingCaptured`) — the latter are *in* lexical reach yet share the
`captured-binding` subreason, because §4.3 maps both `Unresolved` and
`EnclosingCaptured` to `hidden.mutation "captured-binding"`. So the honesty
criterion is not "all remaining emissions are out-of-reach"; a resolved capture
is a legitimate residual. Spot-check the dogfood scan (this repo), and, in the
same pass, review the **new-FP-surface** opened by block-level `use`/`const` and
by `EnclosingCaptured` (a name that newly resolves where it used to fall
through). Record the residual shapes in the PR description, the way
`008-dogfood-deltas.md` did. **Note:** differentiating the two residual buckets
by evidence / subreason (so the dogfood pass can separate a true out-of-reach
miss from a resolved capture) is a **future labelled delta**, not part of this
compat-preserving work — 031 keeps them sharing the subreason.

## 6. Design constraints

### 6.1 Shared with Python (deliberately)

- The `BindingOrigin` vocabulary (all nine variants declared; Rust now
  constructs **six** — §6.2), the `BindingId` segment convention, the
  `resolve(from_scope, name)` shape, the frontend-internal `scope.rs` module
  pattern, **one parented tree per file**, the compat-then-labelled-deltas
  migration, and the load-bearing invariant. Sharing is by **convention, not
  type**: no type moves to `fxrank-core`, no shared crate exists, nothing obliges
  the TS frontend later — it will have its own spec against the same guideline.

### 6.2 Rust-specific (deliberately not aligned)

- **Origins Rust never constructs:** `ModuleImport` (a Rust `use` path names a
  module *or* an item — syntactically indistinguishable, and Rust has no
  Python-#50-style module-method suppression to feed — so all `use`-bound names
  are uniformly `FromImport`, reproducing today's single `use`-table arm);
  `Global` (no `global`-declaration keyword — `static` is `ModuleBinding`, per
  the guideline); `HostGlobalIo` (TS-only).
- **Origin Rust now constructs that the earlier draft said it never would:**
  `EnclosingCaptured`. Since §1.1 makes closures their own units, a closure's use
  of an enclosing `let`/param is a genuine cross-unit capture → `EnclosingCaptured`
  (Rust has no `nonlocal`, so always the *implicit* arm → `hidden.mutation
  "captured-binding"`). Nested `fn`s / `impl` methods still cannot capture — from
  them an enclosing local is simply **not** in scope (the parent walk skips the
  enclosing unit's locals but reaches its items and the file scope — R5).
- **Per-binding metadata** (mutability, param channel) rides alongside the
  origin — Rust's classifier needs it (§4.3); Python's does not. Frontend-
  internal, so this asymmetry costs nothing.
- **Position-aware block scoping** (R1–R4, R7) versus Python's two-kind
  function/module tree; the **scope cursor** in the walker versus Python's single
  `unit_scope`. The tree topology itself — **one parented tree per file** — is now
  *shared* with Python (the earlier draft's "per-unit vs Python's per-file"
  divergence is retracted, §4.4). Block scoping and the cursor are Rust semantics
  or Rust-frontend structure, not model drift.

### 6.3 What this supersedes

`eaa9298`'s `seed_pat_locals` flat seeding (and, under the deltas, the
walk-order `locals`/`let_mut` sets themselves) are replaced by scope-tree
seeding. The interim commit's *outcomes* are preserved (AC-C2); its
*mechanism* is retired. Dead flat-set fields are deleted when their last
consumer moves over — no parallel bookkeeping left behind.

### 6.4 What already doesn't exist in Rust (recorded to prevent phantom work)

No `<module>`/`<script>` unit (Rust files have no top-level statements) — so
none of Python's module-unit-scope (`M`) machinery. With nested `fn`s and
closures now their own units (§1.1), the collision axis is the intra-function
*cross-scope* leak (Delta 2); there is still **no** file-wide import-*binding*
leak between top-level functions (the `use`-table is top-level-only). Inline
`mod` blocks are not descended into for bare-name visibility (N8).

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
by `mutation.rs` before the record is emitted — ever leave `scope.rs`. (The
per-file tree resolves *across* a file's units, but still entirely inside the
frontend — none of it reaches the record; the same intra-file-but-not-intra-unit
property the guideline states for Python.)

## 7. Open questions (left for review, deliberately)

**Resolved since the last draft (recorded, no longer open):** the earlier Q2
(nested-`fn` param origin) is decided — a **reference-typed** (`&mut`/`&`)
nested-`fn`/closure param is a `Param` of that nested **unit's own** record, a
`&mut` one classifying as `param.mutation` with the mut-channel discount; a
**by-value** nested-`fn`/closure param is a `Local` (origin `Param` is reserved
for reference-typed params — §4.2/§4.3), following §1.1. The earlier Q3
(`let`-chains) is decided and promoted to requirement **R7**.

- **Q1 — by-value `mut` param writes.** Today `fn f(mut v: Vec<i32>) {
  v.push(1) }` emits **nothing** (`seed` discards param `is_mut`); the
  guideline's Rust note says a by-value param is a `Local` channel, which would
  make it `local.mutation` (contained, class 1). Adopting that is a *further*
  labelled delta (new low-weight signals across snapshots). Default proposal:
  keep today's silence through this spec, revisit on dogfood pressure — but the
  reviewer should confirm, since the guideline's wording leans the other way.
  (Note the seam: the by-value **closure** param in AC-C2 is classified
  `local.mutation` today via the interim `seed_pat_locals` path, not the param
  path — this Q1 silence is about **signature** by-value params.)
- **Q2 — compat-tree mechanics.** AC-C1 allows a degenerate compat tree (flat
  unit scope, asserted byte-identical) before the lexical tree lands with the
  deltas. If, when writing the plan, routing straight to the lexical tree turns
  out to be snapshot-identical on the existing corpus anyway (plausible: the
  corpus may hold no leak-shaped code), the two-step can collapse — but only with
  the byte-identity assertion still run and passing as its own commit gate.
- **Q3 — discount as a first-class `kind` (future / not Milestone-A, do not
  design here).** Discounts currently live as a `subreason` string. They may
  deserve their own first-class `kind` (analogous to `EffectKind`/`RiskKind`) so
  distinct discount rationales — declared-`&mut`, interior-mut, typed-boundary —
  each carry identity rather than being pattern-matched out of prose. Recorded as
  a forward-looking design pressure only; no Milestone-A work here.
- **Q4 — const-generic reads and macro-adjacent identifiers (N9/N10 boundary).**
  Whether a const-generic use (`fn f<const N: usize>() { arr[N] … }`) should be a
  first-class read binding, and how `syn`-surfaced identifiers *around* macro
  invocations should be treated, are left open — both are FN-leaning misses under
  the syntactic charter, revisited only under dogfood pressure.
- **Q5 — `EnclosingCaptured` net attribution (sibling seam).** At the closure's
  own unit, a captured-local mutation is `hidden.mutation "captured-binding"`
  (§4.3). Whether the 025 cross-file fold (owned by the sibling, §1.1) should
  re-contain that write at the enclosing owner of the local — restoring today's
  net `local.mutation` at the *enclosing* unit — is a propagation question this
  spec does not settle; flagged so the sibling's design and this spec are reviewed
  together.
