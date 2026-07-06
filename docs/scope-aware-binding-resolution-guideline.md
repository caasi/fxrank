# Scope-aware binding resolution guideline (descriptive)

How FxRank's language frontends resolve a **name use inside a unit body** to the
**binding it denotes** (a param, a local, an import, a module top-level binding, a
captured enclosing local, an ambient host global, …). This is the **intra-unit** twin
of the cross-file resolver: `cross-file-resolution-guideline.md` answers *"which unit
does this call reach?"* over `UnitRecord`s in the core; this doc answers *"which binding
does this name denote?"* inside one unit, over the frontend's own AST.

This is a **descriptive reference** for the shared cross-language model. It is **mostly a
target model** — the design spec 078 builds; nothing conforms yet. The **floor today** is
the flat per-unit binding *sets* (`locals`/`params`/`globals`/… and the file-wide
`Imports` table) documented in `mutation-classification-guideline.md` *(Shared model)*,
plus the interim Rust binding-form seeding on branch `feat/078-scope-resolution`. It is
not a contract; nothing is required to conform, and where this disagrees with the code,
the code wins — but during 078 this file and the code are written together.

Related shared-model docs: `mutation-classification-guideline.md` (this doc feeds it —
resolution produces the `BindingOrigin` that guideline's cascade maps to an effect);
`cross-file-resolution-guideline.md` (the `canonical_path` identity discipline this doc
extends *downward*, and the `CallSiteRef` emitter this doc also feeds).

## Shared model — nearest-enclosing-binding replaces flat-set membership

Every frontend today reduces a name use to a **bare string** and asks flat per-unit sets
"is this name a local? a param? an import?" — membership tests on file-wide or
function-wide string sets. This is the **intra-unit analogue of the pre-025 name-based
cross-file resolver** (match a call by its last path segment), and it has the same two
failure modes: it **mis-resolves under shadowing** (a param `os` and an imported `os` are
the same string) and it **leaks across scopes** (a function-local `import x as NAME`
pollutes the whole file's `NAME`).

The shared model replaces membership with **resolution**: a use resolves to the
**nearest enclosing binding** of that name, and that binding carries a **stable identity**
(§*Binding-id scheme*). This is exactly what #36 did one level up — it replaced
equality-over-last-segment-name with equality-over-canonical-path. Here we replace
set-membership-over-bare-name with resolution-to-nearest-binding. Same discipline
(position-independent canonical identity, matched by equality, never by bare name), one
granularity down.

The resolver is a **pass-1, per-frontend** component that walks the unit's native AST
(the same walk that already does mutation classification and `CallSiteRef` emission). It
produces two things consumed **before the `UnitRecord` leaves the frontend**:

1. a `BindingOrigin` per name use — consumed by the mutation classifier (§*Vocabulary*);
2. a correctly-scoped `CallSiteRef` — a call whose base resolves to a param/local is a
   bare local ref, **not** a qualified outward import ref (feeds the cross-file resolver).

## The `BindingOrigin` vocabulary

The canonical result classes every frontend's resolver returns. The resolver owns
**name → origin**; the mutation classifier (`mutation-classification-guideline.md`) owns
**origin → effect** — this table names the origins and states the consuming rule they
already imply, so the two docs stay aligned.

| `BindingOrigin` | denotes | consuming rule (mutation classifier) |
|---|---|---|
| `Param` | a parameter of this unit | `param.mutation` (unless a declared `&mut` channel) |
| `Local` | a body-local binding — `let`/`=`, `for`/`if let`/`while let`/`match`-arm, closure param, `with … as`, `except … as` | `local.mutation` |
| `ModuleImport` | a plain `import module` name (`os`, `np`) — an **immutable module object** | a mutating-method *call* on it is **not** a mutation of it → **suppress** (subsumes the #50 module-import guard) |
| `FromImport` | a `from m import n` name — an **arbitrary, possibly-mutable** member | content-mutation → `global.mutation` (module-shared object) |
| `ModuleBinding` | a module top-level binding (`const`/`let`/`var`/`fn`/`class`; Python assign/`def`/`class`; Rust `static`) | content-mutation → `global.mutation` |
| `Global` | an explicitly **declared** global/static rebind (Python `global x`; Rust `static`) | `global.mutation` (Exact) |
| `EnclosingCaptured` | a binding from an **enclosing function** scope, captured | implicit capture → `hidden.mutation` `"captured-binding"`; an explicitly declared capture (Python `nonlocal`) → `this.mutation` (Exact, not hidden) — the declared/implicit split is the classifier's, layered on this origin |
| `HostGlobal` | an ambient language/host global, never declared or imported (`window`, `globalThis`, `localStorage`/`sessionStorage`/`indexedDB`, `process`) | a mutating-method *call* on it is **not** a mutation of it → **suppress** (subsumes the D4 web-global name list) |
| `Unresolved` | nothing in scope matched — the honest fallback | `hidden.mutation` `"captured-binding"` |

`Unresolved` is the proxy the flat model uses for *everything* it can't place; the scoped
resolver shrinks it to its true meaning (genuinely out of this unit's lexical reach), so
`captured-binding` stops over-firing (#55).

## The binding-id scheme — a downward extension of `canonical_path`

A binding's identity **extends the unit's `canonical_path`** (the cross-file identity, a
`Vec<String>` of FQ segments) downward with its lexical scope path and a kind-tagged name:

```
binding_id = unit.canonical_path ++ scope_segments ++ [ "kind:name" ]
```

```
f's param os:                 ["mod","f","param:os"]
module `import os`:           ["mod","import:os"]              (scope_segments empty ⇒ module scope)
module `from cfg import CACHE`:["mod","from:CACHE"]
g's `import os as CACHE`:      ["mod","g","import:CACHE"]
a for-loop binding x in f:    ["mod","f","b3","local:x"]        (b3 = a lexical scope label)
```

Conventions:

- **Position-independent.** `scope_segments` are **lexical labels** (block ordinal within
  parent, comprehension/arm/closure markers) — **never `line:col`**. An edit that moves a
  binding without changing its lexical nesting must not change its id. (Contrast the
  hotspot `unit_id` = `path:line:col:symbol`, which is deliberately position-*bearing*;
  the binding id is not.)
- **The unit's `canonical_path` is the exact prefix** of every binding inside it — the id
  is one identity scheme at two granularities, not a parallel scheme.
- **Kind-tagged names** (`param:` / `local:` / `import:` / `from:` / `module:`) keep two
  bindings of the same name but different origin distinct even in the same scope.

**Why a shared convention, not a shared type.** The id is **frontend-internal**. There is
no `BindingId` type in `fxrank-core`, no shared crate that computes it — each frontend
mirrors this segment convention over its own AST, exactly as each already computes
`canonical_path` its own way. Sharing the *convention* (so the three resolvers reason
alike and this doc can describe them once) costs nothing at the core boundary; sharing a
*type* would drag the language-specific scope logic toward a common engine — the platform
line §*The load-bearing invariant* forbids.

## The resolution rule — and how the three bugs fall out

**Rule:** a name use resolves to the **nearest enclosing binding** of that name; its
`BindingOrigin` and `binding_id` are that binding's. All three tracked defects are
corollaries — none needs a special case:

- **Shadowing (subsumes the #50 guards).** `def f(os): os.append(1)` — the use `os`
  resolves to the nearest binding `["mod","f","param:os"]` (`Param`), so the
  module-import suppression simply never applies; no "check params/locals/globals before
  the import arm" guard is needed, because nearest-enclosing *is* that check, structural
  and complete. Same for TS `is_bare_namespace_import`'s locals/params pre-check.
- **Cross-function collision (flips the pinned characterization test).** `g`'s
  `import os as CACHE` is `["mod","g","import:CACHE"]`; `f`'s `CACHE` resolves to
  `["mod","from:CACHE"]` — **distinct ids**, so `g`'s local import cannot suppress `f`'s
  mutation. The file-wide `Imports` table becomes a **per-scope** environment; the
  `cross_function_flat_import_collision_drops_signal_accepted_limitation` test flips from
  asserting the dropped signal to asserting `GlobalMutation`.
- **Ambient/host globals (subsumes the D4 name list).** `localStorage`/`sessionStorage`/
  `indexedDB` resolve to `HostGlobal`, so "a call on a known global is not a mutation of
  it" is a **resolver outcome**, not a hardcoded name list in `mutation.rs`. Adding a new
  ambient global is a one-line addition to the frontend's host-global set, and it fixes
  the mutation FP **and** the reach classification at once.

## Honest per-language differences (intentional — not aligned)

Following `mutation-classification-guideline.md`'s "shared model, intentional differences
kept" structure — the **result vocabulary and the id convention are shared**; the **scope
model each resolver builds over its own AST is not**, and must not be.

- **Python (`libcst`)** — `global`/`nonlocal` declarations bind at module/enclosing scope
  (→ `Global`/`EnclosingCaptured`, not `Local`); comprehension and generator expressions
  get **their own scope** (Python 3), so a comprehension target does not leak to the
  enclosing function; `import` vs `from`-import is the `ModuleImport`/`FromImport` split.
  These are Python semantics — no other frontend has them.
- **TS/JS (`swc`)** — `var` **hoists** to the function scope while `let`/`const` are
  **block-scoped with a TDZ**; closures capture by reference; a namespace import
  (`import * as V`) is an **immutable module namespace** (→ its own suppression, like
  `ModuleImport`) while default/named/`require` bind possibly-mutable objects. Block/TDZ
  distinctions are JS semantics — no other frontend models them.
- **Rust (`syn`)** — bindings come from **patterns**, not just `let`: `for <pat> in`,
  `if let`/`while let`, `match`-arm patterns (`Ok(mut x)`), and closure params all
  introduce `Local` bindings; item/block scoping and pattern destructuring are Rust's.
  The mutable-static set (`static`/`static mut`/interior-mut statics) is the `Global`
  channel (no casing proxy). These are Rust semantics — no other frontend has patterns or
  `static`.
- **Shell (`brush-parser`)** — has **no lexical binding cascade at all** (dynamic scoping;
  see the mutation guideline's declaration-vs-hidden gate). It does **not** implement this
  resolver; its `is_script`/`local`-declaration gate stands in for binding origin. Listed
  here so the absence is on record, not an oversight.

## The load-bearing invariant

**Binding resolution never crosses the `UnitRecord` boundary.**

- Binding ids and `BindingOrigin` are **frontend-internal**. The mutation classifier and
  the `CallSiteRef` emitter consume them **before** the record is emitted. `UnitRecord`
  and `fxrank-core` gain **zero** scope/binding fields or types. There is **no**
  `fxrank-analysis` crate.
- **Mechanically checkable:** a diff that adds a binding/scope field to `UnitRecord`, or a
  scope/binding type to `fxrank-core`, is a **review stop**. `fxrank-core` must reference
  no binding-origin/scope type (`grep`-assertable). The record's only identity knowledge
  stays `canonical_path` equality for **inter-unit** refs — unchanged from #36.

This is **exactly the #31 line not to cross.** #31's shared-resolver-over-a-normalized-
scope-graph was deferred because that scope graph is a premature IR that creeps toward a
platform; `UnitRecord` exists precisely to keep language detail out of the core. Intra-unit
resolution needs the full AST and the language's scope rules — so it stays in the frontend,
where mutation classification already lives. If a future **inter-unit** consumer ever
genuinely needs a binding id (none does today — mutation is intra-unit, call resolution is
#36-covered), that is a fresh, separately-justified issue built from real pressure, not a
field quietly added here.

## Migration discipline

**Compat-mode first, then labelled intentional deltas** — the spec-008 migration shape.

1. **Build the resolver; port existing consumers onto it with byte-identical output.** The
   flat-set consumers — the #50 suppression guards, Python `Imports::is_module_import`, TS
   `is_bare_namespace_import`, the ambient-global lists, the interim Rust binding-form
   seeding — move onto the scoped resolver. **Assert existing snapshots are byte-identical
   first** (no ranking change from the port itself).
2. **Then land the precision fixes as labelled intentional deltas**, touching only
   targeted snapshots: shadowing precision, the cross-function-collision close (flip the
   pinned test), ambient-global resolution (retire the D4 name list). Never call these
   "regressions."

**Order: Python → TS → Rust.**

- **Python leads** — it already has the richest partial scope model (explicit
  `global`/`nonlocal` prescan + per-function local/param sets) and owns both named #78
  repros (the #50 shadow guard and the cross-function-collision characterization test), so
  it is the best conformance canary.
- **TS second** — block/hoist/TDZ/closure, the #29 family.
- **Rust last** — the interim `seed_pat_locals` patch (branch `feat/078-scope-resolution`)
  is the thing to **supersede**: its flat function-scoped insertion of `for`/`if let`/
  `match`-arm/closure bindings becomes proper scope-pushed bindings in the resolver. Its
  FP-removal tests (`binding_forms_seed_locals_not_captured`) are the **compat baseline**
  that must still pass; the resolver additionally gains the shadowing cases the flat set
  cannot express.
