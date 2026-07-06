# Scope-aware binding resolution guideline (descriptive)

How FxRank's language frontends resolve a **name use inside a unit body** to the
**binding it denotes** (a param, a local, an import, a module top-level binding, a
captured enclosing local, an ambient host global, …). This is the **intra-file** twin
of the cross-file resolver: `cross-file-resolution-guideline.md` answers *"which unit
does this call reach?"* over `UnitRecord`s in the core; this doc answers *"which binding
does this name denote?"* within a file, over the frontend's own AST (its scope tree) —
entirely in the frontend, so its results never reach the core (§*The load-bearing
invariant*).

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
function-wide string sets. This is the **within-file analogue of the pre-025 name-based
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

The resolver is a **pass-1, per-frontend** component over the unit's native AST — the same
phase that already produces mutation effects and `CallSiteRef`s (one walk or several, per
frontend; TS, e.g., runs separate mutation/ref walkers and a React two-pass). It produces
two things consumed **before the `UnitRecord` leaves the frontend**:

1. a `BindingOrigin` per name use — consumed by the mutation classifier (§*Vocabulary*);
2. a correctly-scoped `CallSiteRef` — a call whose base resolves to a param/local is a
   bare local ref, **not** a qualified outward import ref (feeds the cross-file resolver).

*(Staging note — Python 078: the first Python step delivers consumer 1 only. Consumer 2
(scoped `CallSiteRef`) is a **named non-goal** of that step — `calls.rs` keeps consuming the
file-wide `Imports` table, so the cross-function import collision still exists on the
call-resolution side until a follow-up moves `calls.rs` onto the resolver. Deferring it
keeps 078's Python step to two labelled deltas rather than adding a call-graph delta axis;
the resolver it builds is the prerequisite. This is recorded so the half is deferred, not
silently unfinished.)*

## The `BindingOrigin` vocabulary

The canonical result classes every frontend's resolver returns. The resolver owns
**name → origin**; the mutation classifier (`mutation-classification-guideline.md`) owns
**origin → effect** — this table names the origins and states the consuming rule they
already imply, so the two docs stay aligned.

| `BindingOrigin` | denotes | consuming rule (mutation classifier) |
|---|---|---|
| `Param` | a parameter of this unit | a caller-shared param mutation — but the **channel is language-specific**: Rust escalates only a `&mut` / `&mut self` param (a by-value param is a `Local`); Python/TS treat any param write as `param.mutation` |
| `Local` | a body-local binding — `let`/`=`, `for`/`if let`/`while let`/`match`-arm, `with … as`, `except … as`, **and Rust closure params** (a Rust closure shares its parent unit; a **TS/JS closure is its own unit**, so its params are that unit's `Param`) | `local.mutation` |
| `ModuleImport` | a plain `import module` name (`os`, `np`) | a mutating-method **call** on it is **not** a mutation *of* it — the effect is the callee's → **suppress** (subsumes the #50 module-import guard). *(Module objects are mutable in general; this is about the call, not immutability.)* |
| `FromImport` | a `from m import n` name — an **arbitrary, possibly-mutable** member | content-mutation → `global.mutation` (module-shared object) |
| `ModuleBinding` | a module top-level binding (`const`/`let`/`var`/`fn`/`class`; Python assign/`def`/`class`; **Rust `static` / `static mut` / interior-mut statics** — Rust's only global-write channel) | content-mutation → `global.mutation` (Rust: `Heuristic`) |
| `Global` | an **explicitly declared** global rebind — **Python `global x` only** (Rust has no global-declaration keyword; its `static` is `ModuleBinding`) | `global.mutation` (Exact) |
| `EnclosingCaptured` | a binding from an **enclosing function** scope, captured | implicit capture → `hidden.mutation` `"captured-binding"`; an explicitly declared capture (Python `nonlocal`) → `this.mutation` (Exact, not hidden) — the declared/implicit split is the classifier's, layered on this origin |
| `HostGlobalIO` | an ambient **IO** host global, never declared or imported (`localStorage` / `sessionStorage` / `indexedDB`) — its IO is owned by the call detector | a mutating-method **call** on it is **not** a mutation *of* it → **suppress** (subsumes the D4 web-global name list) |
| `Unresolved` | nothing in scope matched — the honest fallback | `hidden.mutation` `"captured-binding"` |

**Not mutation origins (do not `HostGlobalIO`-suppress them):** `window` / `globalThis` are the **global object** — a *write* to one (`window.x = 1`, `window.arr.push()`) is `global.mutation` (the classifier's global-object arm, matching `mutation-classification-guideline.md`), **never** suppressed. `process` appears only in the *call* detector (`process.exit` → `process.control`, `process.env.X` → `env.read`) and has no mutation origin. Keep `HostGlobalIO` suppression strictly to the three ambient-storage names.

`Unresolved` is the proxy the flat model uses for *everything* it can't place; the scoped
resolver shrinks it to its true meaning (genuinely out of this unit's lexical reach), so
`captured-binding` stops over-firing (#55).

## The binding-id scheme — the unit's `canonical_path` extended below

Resolution runs over the **file's scope tree** — `module → function/method → block` — in
the frontend. Every binding lives at some node of that tree; its id is that node's path,
split at the **unit boundary** into two parts that compose:

- the **unit's `canonical_path`** — the FQ path *of the enclosing unit* (function/method/
  module-init), already the cross-file identity (a `Vec<String>` ending in the unit's own
  name, e.g. `["mod","f"]`);
- the **`scope_segments`** — the path from the unit's body root *down to* the binding,
  strictly **below** the unit (block ordinals, comprehension/arm/closure markers). A
  binding at the unit's body root has **empty** `scope_segments`.

```
scope_segments      = path below the unit's body root (empty at the root)     # strictly below the unit
full_binding_id     = unit.canonical_path ++ scope_segments ++ [ "kind:name" ]
```

`scope_segments` never repeats the unit's own name — that name lives once, in
`canonical_path` — so the composition cannot double-count.

```
                                canonical_path   scope_segs   kind:name        full_binding_id
f's param os:                   ["mod","f"]      []           "param:os"    →  ["mod","f","param:os"]
a for-loop binding x in f:      ["mod","f"]      ["b3"]       "local:x"     →  ["mod","f","b3","local:x"]   (b3 = a block label)
module `import os`:             ["mod"]          []           "import:os"   →  ["mod","import:os"]
module `from cfg import CACHE`: ["mod"]          []           "from:CACHE"  →  ["mod","from:CACHE"]
g's `import os as CACHE`:       ["mod","g"]      []           "import:CACHE"→  ["mod","g","import:CACHE"]
```

Module-level bindings (`import os`, `from cfg import CACHE`) live at **module scope** — the
module-init unit's `canonical_path` (`["mod"]`) — and are shared by every function-unit in
the file. A function-local import (`g`'s `import os as CACHE`) lives inside `g`'s unit
(`["mod","g"]`), **not** module scope — which is exactly why it cannot leak to `f` (see the
collision corollary).

When the unit's `canonical_path` is **empty** — a Python method / nested `def`, or a Rust
file with no crate root in scope (spec 025-3e's degradation) — the id is still well-formed
`scope_segments ++ [ "kind:name" ]`, just **file-relative rather than globally unique**.
That is fine: #78's results never leave the frontend (§*The load-bearing invariant*), so
file-relative uniqueness is all resolution needs; the `canonical_path` prefix is a free
global tie-in to #36 when available, not a dependency.

Conventions:

- **Position-independent (structurally).** `scope_segments` are **lexical labels** (an
  ordinal among sibling scopes in the same parent, comprehension/arm/closure markers) —
  **never `line:col`**. An edit that moves a binding without changing its lexical nesting
  must not change its id. (Contrast the hotspot `unit_id` = `path:line:col:symbol`, which is
  deliberately position-*bearing*.) The labels are *structural*, not stable across edits
  that reorder or insert sibling scopes — that churn is acceptable because the id is
  frontend-internal and recomputed each scan; it is never persisted or compared across scans.
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
- **Ambient IO globals (subsumes the D4 name list).** `localStorage`/`sessionStorage`/
  `indexedDB` resolve to `HostGlobalIO`, so "a mutating call on a known ambient IO global
  is not a mutation of it" is a **resolver outcome**, not a hardcoded name list in
  `mutation.rs`; adding a new one is a one-line addition to the frontend's host-global set.
  (`window`/`globalThis` are the global *object* — a write to one is `global.mutation`, not
  suppressed; see the vocabulary note. Whether a `HostGlobalIO` *call* is an external reach
  is a separate reach-classification concern, not resolved here.)

## Honest per-language differences (intentional — not aligned)

Following `mutation-classification-guideline.md`'s "shared model, intentional differences
kept" structure — the **result vocabulary and the id convention are shared**; the **scope
model each resolver builds over its own AST is not**, and must not be.

- **Python (`libcst`)** — `global`/`nonlocal` declarations bind at module/enclosing scope
  (→ `Global`/`EnclosingCaptured`, not `Local`); comprehension and generator expressions
  get **their own scope** (Python 3), so a comprehension target does not leak to the
  enclosing function; `import` vs `from`-import is the `ModuleImport`/`FromImport` split.
  These are Python semantics — no other frontend has them. *(078 status: `match`-arm
  captures, walrus `:=`, and comprehension targets — **uncollected residuals** in the flat
  code — are collected in 078's Python step. **Deliberate divergence, code wins:** comp
  targets are collected into the **function scope** as `Local`, not a comprehension-own
  scope. It is **effect-identical** for mutation classification (`local.mutation` either
  way), and the only observable difference — a comp target's name reused outside the
  comprehension — is impossible in valid Python 3 (targets don't leak) and, in broken code,
  misresolves toward a false negative, never an FP (charter: FP ≫ FN). The comprehension
  scope in this bullet is the **target model**, load-bearing only once a consumer must
  distinguish comp-scope from function-scope, e.g. shared-global identity (#81) or an
  implicit-capture walk; build it then, not now.)*
- **TS/JS (`swc`)** — `var` **hoists** to the function scope while `let`/`const` are
  **block-scoped with a TDZ**; closures capture by reference; a namespace import
  (`import * as V`) is an **immutable module namespace** (→ its own suppression, like
  `ModuleImport`) while default/named/`require` bind possibly-mutable objects. Block/TDZ
  distinctions are JS semantics — no other frontend models them.
- **Rust (`syn`)** — bindings come from **patterns**, not just `let`: `for <pat> in`,
  `if let`/`while let`, `match`-arm patterns (`Ok(mut x)`), and closure params all
  introduce `Local` bindings; item/block scoping and pattern destructuring are Rust's.
  The mutable-static set (`static`/`static mut`/interior-mut statics) is the `ModuleBinding`
  channel (`Heuristic`, by the real static-name set — no casing proxy); Rust has no
  `global`-declaration keyword, so nothing is the `Exact` `Global` origin. These are Rust
  semantics — no other frontend has patterns or `static`.
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
platform; `UnitRecord` exists precisely to keep language detail out of the core.
Resolution runs over the file's scope tree and needs the full AST and the language's scope
rules — so it stays in the frontend, where mutation classification already lives, and only
its *results* (the `BindingOrigin`, the scoped `CallSiteRef`) ever leave, consumed before
the record is emitted. Note this resolution is **intra-file but not intra-unit**: module
scope is shared across a file's function-units, so a name in `f`'s body can resolve to a
module-level binding, and the cross-function-collision fix reasons across `f` and `g` at
once — all still frontend-internal, none of it on the record. If a future consumer in the
**core** ever genuinely needs a binding id (none does today — resolution is frontend-local,
call resolution is #36-covered), that is a fresh, separately-justified issue built from
real pressure, not a field quietly added here.

**Non-goal — cross-unit / cross-file *shared-global* identity.** Recognizing foo's `x` and
bar's `foo.x` as the *same* global, or coupling functions through a shared mutable global,
is deliberately **not** #78: #36 resolves cross-file *calls* (not variable identity), and
#78 is intra-file. That capability — a shared global as a cross-unit dependency node — is
the sibling axis **#81**, a clean *additive* layer on this finished resolver (a resolved
`GlobalRead`/`GlobalWrite` ref, #36-shaped), designed-and-deferred until dogfood pressure.
#78 emits nothing for it now; it is the prerequisite (its `BindingOrigin` is what would
decide "global vs shadowing param"), not the host.

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
