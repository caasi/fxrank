# Cross-file resolution guideline (descriptive)

How FxRank resolves an imported name to its **definition**, grows a **call graph**
from visible entry points (**roots**), and folds effects along that graph
(**transitive propagation**) — stopping at boundaries it is not permitted to look
through (**opaque boundaries**, the one *unknown*).

This is a **descriptive reference** for the shared cross-language model. Part of it
is **implemented today** (the per-frontend symbol tables in *The floor* below); the
rest is the **target model** that spec 025 builds. It is not a contract; nothing is
required to conform. Where this disagrees with the code, the code wins — but during
025 this file and the code are written together.

Related shared-model docs: `docs/mutation-classification-guideline.md` (write
classification), `docs/corpus-profile-guideline.md` (which files are scanned). This
doc governs *what happens between* units once they are scanned.

## The floor — per-frontend symbol knowledge that exists today

Every frontend already resolves a local name to a **module string** and stops there.
It never reads the target module, so it cannot see the *definition*. This is the
floor 025 builds the second hop on.

| | Rust (`syn`) | TS/JS (`swc`) | Python (`libcst`) | Shell (`brush-parser`) |
|---|---|---|---|---|
| import table | `ImportTable::from_file` | `ImportTable::from_module` | `Imports::build` | *(none — no import syntax)* |
| local → … | `::` path (`fs`→`std::fs`) | module string (`fs`→`"node:fs"`) | dotted path (`run`→`"subprocess.run"`) | *(n/a)* |
| dynamic flag | `has_glob` | `has_dynamic` | `dynamic` | *(n/a)* |
| module bindings | `collect_static_names` (real `static` only) | `module_bindings` (top-level `const`/`let`/`var`/`fn`/`class`) | `module_bindings` (assign targets + `def`/`class`) | *(n/a — see mutation guideline's declaration-vs-hidden gate)* |
| second hop (name → **definition**) | **✗** | **✗** | **✗** | **same-file only** (see below) |

The module-binding sets already drive same-file `global.mutation` (spec 008 F2/F5,
#29). 025 generalizes the *resolution* itself — the missing second hop — and reuses it
for the call graph.

## Shared model — the cross-file layer

### In-scope set

One scan defines an **in-scope set**: the union of all sources routed to a frontend
(a directory walk, or — after 025 — an arbitrary list of file paths). The export
index, the call graph, and reachability are all computed **over exactly this set**.
A scan is **per-language**: `dispatch` partitions sources by language and hands each
frontend its whole batch, so a TS file never resolves into a Rust definition. What is
"language-neutral" is the index *machinery* in `fxrank-core` (parser-free); each
frontend populates it from its own batch.

### Roots — the agent's observation focus

A **root** is any unit whose file the agent named explicitly on the CLI (an explicit FILE
arg, or stdin) — the observation entry point the agent chose. Directory-walked files are
**context**, not roots. It is a **language-neutral, CLI-level** flag (no program-entry
heuristic). See *Roots — the agent's observation focus* below for the full model + history.

### Edges — name resolved to definition

An **edge** connects a unit to a definition it references. Intra-file edges are
already available (same module); the cross-file edge is the new second hop:
`import local name → module string → defining file's exported entry`. Module-string →
file resolution is **per-frontend** (TS extension/index ladder, Python dotted-module,
Rust module tree); the *index* that stores `(module, exported-name) → definition` is
language-neutral.

### Module-init units — code that runs at import time

Code that **executes at module-evaluation (import) time** is folded into one synthetic
**module-init unit** per module — the anonymous body the module "is." (Its `root`-ness, like
any unit's, follows whether its file was named explicitly on the CLI — it is **not**
automatically a root; see *Roots*.) The dividing line is **executes-at-import vs
executes-when-called**, *not* statement-vs-definition:

- **In** the module-init unit: top-level statements (calls, side-effectful initializers,
  top-level `await`) **and definition-time-evaluated expressions** — decorators,
  default-argument expressions, Python class bodies and base-class expressions, JS/TS
  class static blocks, field initializers, and computed member names. These all run when
  the module loads.
- **Out** (stays its own unit): the **callable body** of a function or method, which runs
  only when *called*.

A module-init unit is synthesized **only** when such import-time code exists. This closes
a real gap: import-time IO is invisible today — not just a top-level `fetch(…)` /
`requests.get(…)` / `db.connect()`, but a `@decorator` that calls out and a
`field = compute()` class-field initializer.

### Propagation — transitive join, not sum

A unit's **boundary summary** is the **union** (join) of its own **escaping** signals and the
boundary summaries of its first-party callees. **`contained` signals stay in `own` and do NOT
propagate** — this is the cross-unit twin of the within-unit boundary-containment discount
(spec 003): contained = not observable to a caller = zero contribution upstream. A unit's own
**propagated** (ranked) score is `own ∪ ⋃ summary(callee)`. "Signals" covers **both `effects`
and `risk_features`**: an **effect** escapes when `contained == false`; a **risk** has no
containment flag, so a **per-`RiskKind` predicate** decides — *capability* risks
(`dynamic.code`/`ffi.call`/`html.injection`/`proto.pollution`/`effect.in.render`) escape,
*encapsulated* risks (`unsafe.*`/`transmute`/`raw.ptr.*`/…) do not (calling an `unsafe`-using
fn doesn't make the caller unsafe; calling one that `eval`s does). Each signal is keyed by a
**stable site identity** `(unit_id, line, col, kind)` — `unit_id` already encodes the path
(`path:line:col:symbol`), so `Effect`/`RiskFeature` need only gain `col`; line alone collapses
two same-line effects.

```
summary(u)    = escaping(own(u)) ∪ ⋃ summary(callee)   for each first-party callee
propagated(u) = own(u) ∪ ⋃ summary(callee)             u's own ranked score
```

It is a **join, never a sum**. Consequences:

- **Recursion / cycles converge for free.** `summary(u)` appears on its own
  right-hand side, but `∪` is idempotent, so the self-edge adds nothing. The signal
  lattice is finite, so the fixpoint is reached in finite steps. All members of a
  strongly-connected component (mutual recursion) converge to the **same** summary
  — one value per SCC.
- **No path-multiplicity blow-up.** A definition reachable by many paths (a diamond)
  is counted **once** by site. Recursion depth and fan-in never inflate the score.
- **Provenance must be bounded.** The *signal set* converges, but recording every
  `via <path>` does **not** — a cycle generates unboundedly many paths even while the
  set is stable. Provenance is therefore **exemplar / SCC-summarized** (one representative
  path per inherited site, e.g. shortest), never the full path set.
- The numeric score is recomputed from the unioned multiset by the existing
  `own_score = max + 0.5×rest` damping; `max_class` is the join (max) it already is.
  Inherited signals carry that bounded provenance and the confidence-relevant metadata
  of their origin (async penalty, dynamic-feature reducers) so the absorbing unit never
  looks more confident than warranted.

Implementation is a memoized fixpoint over the call graph with SCC handling; single
units are computed once and shared.

### The opaque boundary — the one *unknown*

An edge that leaves the in-scope set is a boundary FxRank is **not permitted to look
through**:

- **first-party** (relative `./`, `../`) but **out-of-scope** → resolvable by widening
  the scan; emit the unknown default **plus a diagnostic** ("expand scan to resolve").
- **third-party** (bare / scoped package) → opaque **by policy**; do not follow.
  Following into dependencies explodes scope and emits un-actionable third-party
  hotspots (often absent/minified anyway).

Deciding *which* bucket an out-of-scope import falls in is a **per-frontend first-party
vs third-party classifier**. The corpus pass showed the naive "relative `./` = first-party,
bare = third-party" rule is **wrong at scale** and each frontend must read project config:

- **TS** — the naive rule misclassifies thousands of imports (one monorepo: 2343 `~/`,
  661 `@/`, 588 `@scope/*` workspace imports — all first-party but bare-looking). Build a
  per-directory alias table from the nearest `tsconfig.json` `compilerOptions.paths`
  (`@/*`→`src/*`, `~/*`→`src/*`) **and** a workspace-name set from `pnpm-workspace.yaml`
  globs (each `package.json.name`, incl. `@scope/*`). First-party = relative **or** matches
  an alias prefix **or** matches a workspace name; else third-party.
- **Python** — `__init__.py` marks a package (no PEP-420 namespace packages in the surveyed
  trees); resolve dotted modules against package paths within scope. Treat `csrc/`,
  `examples/`, `benchmarks/`, `tests/`, `scripts/`, `gen_*` as non-first-party adjuncts.
- **Rust** — crate / workspace boundaries decide: a path rooted at `crate::`/`self::`/`super::`
  or another **workspace member** is first-party; an external crate name is third-party.

When a frontend cannot tell, it degrades to the opaque default (never an error). Reading
`tsconfig.json` / `pnpm-workspace.yaml` / `pyproject.toml` / `Cargo.toml` is a **new
capability** these frontends do not have today — called out as a spec-025 work item.

"Stop" must **not** mean "assume pure." An opaque callee is scored as a **bounded
known-unknown**: effect kind `external.unresolved`, **class 2** (the cross-language
analogue of Rust's `unknown.macro`, also class 2), plus a heuristic-tier confidence
penalty. Each opaque reach is also **recorded on a retained `external_reaches` list** (the
app's outward surface: specifier + kind + site) — not just counted. Known-effectful packages
keep their real severity (`axios`/`fetch`/`requests`/`subprocess`/… already classified by
name); the class-2 default is only for the **genuinely** unknown.

### What becomes a reach — the meaningful-outward-surface filter

**Not every unresolved reference is an external reach.** A reach records the app's
*meaningful outward surface* — qualified calls that genuinely reach into another module or
package. Bare unresolved references that are **not** outward-module references — language
builtin **methods**, prelude **intrinsics**, unqualified local-ish names — are **filtered**:
they produce **no edge, no reach, and no `external.unresolved` effect**. Two reasons: (1) a
`.clone()` / `.push()` / `Some` is not the app's import surface; (2) a *genuine* effect
reached through a builtin method (file IO via a method call) is already captured by the
**effect detectors**, so dropping the reach loses no real signal — it only removes noise from
the surface *and* from the propagated score. **Ambiguous in-scope matches** (a callee name
that collides with several scanned units) are likewise dropped — that is internal ambiguity,
not an outward reach; phase-3 module-tree precision disambiguates.

**Architecture — the frontend identifies, the core filters.** The per-language syntactic
judgment lives in the **frontend** (pass 1): when it builds each `CallSiteRef` it tags it with
a neutral `qualified: bool` — "is this a qualified outward reference, eligible to become a
reach if unresolved?" The **core** then applies the filter **uniformly** on that tag (no
language-specific syntax in core): unresolved + `qualified` → reach; unresolved + not-qualified
→ dropped; ambiguous → dropped; unique → resolved. This keeps `fxrank-core` free of any `::`
/ `.` / import-syntax knowledge, and lets each frontend set `qualified` its own way:

The per-frontend classification — the **analogue of the mutation guideline's per-language
mutating-method allowlists**:

- **Rust** — a reach is recorded only for **qualified path calls** (callee base contains
  `::`: `std::fs::write`, `crate::x::y`, `Type::method`). Bare single-segment calls and method
  calls (`.push()`, `Some`, `g`) are dropped. *Residual:* stdlib prelude constructors
  (`Vec::new`, `String::new`) are `::`-qualified so currently kept; a future
  stdlib-constructor allowlist (or the phase-3 module tree) can trim them.
- **TS/JS** (when records land) — a reach is recorded for **import-specifier-resolved**
  references (relative/aliased/workspace specifiers that fall out of scope, or bare package
  names from the import table); bare member/method accesses are dropped.
- **Python** (when records land) — a reach is recorded for **dotted-module / imported** names;
  bare attribute and method calls are dropped.
- **Shell** — a reach is recorded only for `source`/`.` with a **literal path argument**
  (absolute, slash-relative, or a bare filename — the sourced script's identity is knowable even
  though shell has no import syntax); a *computed* `source` path (`source "$dir/x"`) gets no ref
  at all (a `DynamicCode` risk instead — the `base` would be non-literal garbage). Same-file
  function calls are never reaches — they resolve via `canonical_path` (see below), not the
  reach mechanism.

Dogfood evidence (Rust, `scan crates/fxrank-cli/src`): the filter took the surface from **160
reaches** (dominated by `.clone()`/`.push()`/`Some`/`Ok`) down to **32** meaningful qualified
reaches (`std::fs::*`, `std::io::stdin`, `fxrank_core::CorpusMatcher::build`, …).

### Lattice note — why *unknown* is not ⊥

The opaque boundary means "could do **any** effect" — in the effect may-lattice that
is **⊤ (top)**, not ⊥. ⊥ in that lattice is the *empty* effect set = **pure** = cost 0.
Writing the unknown as ⊥ would denote "assume pure" — the exact false-confidence trap.

**Keep two things separate:** the *denotational* role of the unknown is genuinely ⊤;
how FxRank *scores* it is a **ranking-policy choice**. FxRank refuses to score true ⊤ at
its weight (class 7 cries wolf at every dependency touch) and instead represents it with
a **capped policy token** — `external.unresolved`/class 2 — plus a retained
`external_reaches` record. That token is a deliberate scoring/ranking device; it is **not** a
claim that class 2 is the lattice top.

So in this model:

| concept | denotational role | scoring representation |
|---|---|---|
| pure function | ⊥ (empty effect set) | `own_score` 0, no effects |
| fixpoint init (before a node is computed) | ⊥ | internal only, never serialized |
| opaque boundary (the *unknown*) | ⊤ (could be any effect) | capped policy token: `external.unresolved`/2 + recorded `external_reaches` |

There is **no** "unreachable-by-traversal = ⊥" axis in this model — *unknown* means
*opaque boundary*, not *dead code*.

## Roots — the agent's observation focus

A **root** is any unit whose file the agent passed to the CLI as an **explicit FILE
argument** (or stdin) — the starting point the agent chose to observe. Files discovered by
**walking a DIRECTORY argument** are **not** roots; they are resolution **context** (the
searchable corpus). Roots are **language-neutral** and set at the **CLI discovery seam**
(`hotspot.root` / `UnitRecord.is_root`), with **no per-language heuristic**. Every unit in an
explicit file is a root (the whole file is the focus).

**Rationale.** fxrank is a tool *for coding agents*. `fxrank scan Button.tsx` means "I'm
observing Button.tsx — show me its effect blast-radius, using the rest of the scanned corpus
as context." So `root` answers *"what is this query's focus?"* — more actionable to an agent
than guessing a program's real entry points. Examples: `scan Button.tsx` → all of Button.tsx
is root; `scan src/` → no roots (the whole directory is context).

`is_root` is **annotation only** — it does **not** seed the fold (the fold is Tarjan-SCC over
all nodes; `graph.roots()` is test-only). It rides on the record into `apply_fold`, which
copies it to `hotspot.root`; the CLI also sets `hotspot.root` directly so `--no-resolve`
(fold skipped) stays consistent.

> **CLI shape (current):** a single path arg — scan one file (→ its units are roots) **or**
> one directory (→ all context, no roots). A multi-path "focus file + context corpus"
> invocation (`scan focus.ts src/`) that yields focus-roots *plus* a resolvable corpus is a
> natural future enhancement, not yet implemented.

### History — why not heuristic program-entry detection?

An earlier model (spec-025 phase **3b**) computed roots as per-language *program* entry
points and was **corpus-validated** (Rust `agent-browser`/`fxrank`; TS a Next.js app + pnpm
monorepos; Python Django/PyTorch). It produced genuinely useful *observations* worth keeping
on record:

- **Rust:** `pub fn` over-approximates by up to ~100% in a binary crate (`agent-browser/cli`:
  all 355 `pub fn` are crate-internal); a real entry-point computation needs the module tree +
  visibility chain + crate target kind, plus inherent/trait-impl methods and `pub use` facades.
- **TS/JS:** no single source-level entry field (`package.json` points at `dist/`); the real
  entries are framework-convention files (`app/**/page.tsx`, `route.ts`, `*.config.*`),
  `package.json.bin`, and `createRoot(…).render(…)` bootstraps; `export const X = (arrow)` and
  `export default memo(C)` dominate, and barrels relocate the export site off the definition.
- **Python:** tiered — `console_scripts`/`setup.py` entries, `__main__` guards, static `__all__`,
  non-underscore convention, plus import-time `register`/decorator calls (Django 226, PyTorch 2377).

**But it answered the wrong question.** An agent doesn't want the *program's* entry points; it
wants *its own* observation focus. The heuristics were **removed** during the spec-025 review in
favor of the explicit-file rule above (a large net simplification, and it dissolved a string of
brittle edge cases: anonymous default-export roots, pre-fold vs `apply_fold` root consistency,
memo-wrapper unwrapping). If a future task genuinely needs *program*-entry detection, that is a
**different concept** from `root` — keep the two distinct, don't reconflate them.

## Honest per-language differences (intentional — not aligned)

- **Module-init units are TS/Python-shaped.** TS/JS and Python run code at import time —
  top-level statements *and* definition-time expressions (decorators, default args,
  class bodies, static blocks, field initializers). **Rust has essentially no import-time
  execution**: `static`/`const` initializers are *const-evaluated at compile time*, there
  are no decorators or executable class bodies, and only `fn main` runs — so Rust rarely
  synthesizes a module-init unit. This is an honest difference, not a gap. **Shell doesn't
  need a synthesized module-init unit at all**: a shell script's top-level statements execute
  every time the file runs, with no distinct "import" phase — so the frontend's synthetic
  `<script>` unit *is* the whole file's top-level body directly (built by `functions::collect`,
  not the core module-init synthesis path TS/Python use). Conceptually closest to Python's
  module-init unit (both capture "code that runs just by the file being reached"), but it's a
  frontend-local construct, not an instance of the shared mechanism.
- **Module-string → file resolution differs per frontend.** Rust resolves `use` paths
  through the module tree; TS resolves relative specifiers via an extension/index
  ladder (`./hooks` → `hooks.ts`/`hooks.tsx`/`hooks/index.ts`); Python resolves dotted
  modules against package paths. Each frontend owns its resolver; the index is neutral.
- **Reach-recording filter is per-frontend, one shared principle** (see *What becomes a
  reach*). Each frontend decides syntactically which unresolved references are meaningful
  outward reaches vs intra-language noise — Rust by `::`-qualification, TS by import-specifier
  resolution, Python by dotted-module/import. The shared principle (meaningful outward surface
  only; builtin methods/intrinsics dropped; real effects already caught by detectors) is the
  same; the syntactic vocabulary is language-specific, like the mutating-method allowlists.
- **Export site ≠ definition site — resolution must chain to the definition.** Rust
  `pub use` facades, TS re-export barrels (`export * from`), and Python `__all__` /
  import-alias re-exports all name a public symbol away from where it is defined. A
  recognized root or resolved edge must be followed through these facades to the real
  definition, or hotspots get attributed to empty barrel / facade files.
- **Roots conventions differ** (see *Roots per language*): Rust keys on crate-type +
  `pub`-visibility chain; TS on framework-convention files + bootstraps (not a keyword);
  Python on a confidence-tiered cascade (declared entry points → `__main__` → static
  `__all__` → non-underscore convention).
- **Graceful degradation is uniform.** When a definition is out of scope, every
  frontend falls back to the per-file heuristic floor (the module string) and the
  opaque-boundary default — never an error. The cross-file index only *adds* confidence
  when the definition is in scope.
- **Shell's function-call resolution is same-file only, and routes through the exact
  `CanonicalIndex`, not the flat `SymbolIndex`.** Shell has no import syntax, so there is no
  cross-file "name → module string → definition" hop to build at all — a shell function is only
  ever called by (a) another command in the same file, or (b) after being `source`d into a
  caller's shell, which the frontend cannot statically distinguish from "just runs whatever code
  is in that file." Each unit carries a `canonical_path` (`[path, "fn", name]` for a real
  function, `[path, "<script>"]` for the synthetic script unit) that is unique **within its own
  file**; `refs()` resolves a same-file call site directly to that path
  (`resolved_target: Some([path,"fn",name])`, first-party, not qualified). This makes the shell
  partition "adopted" by the core fold (`CanonicalIndex::adopted()`), so resolution runs the
  exact per-file lookup instead of the name-based `SymbolIndex` fallback the other frontends can
  fall back to. The reason is deliberate, not an oversight: shell scripts reuse helper names
  (`log`/`die`/`main`/`usage`) across unrelated files far more than the typed languages do, so
  the flat `SymbolIndex` would collide and drop even same-file calls as ambiguous. A call is
  only recognized under `ResMode::Normal` — a wrapper that forces `FunctionBypass`
  (`sudo greet`) or `BuiltinOnly` (`builtin`) means the wrapped word can never resolve to a local
  function.
- **`source`/`.` is a path-keyed opaque effect + reach, not a followed import — and its opaque
  token deliberately diverges from the guideline's standard `external.unresolved`/2.** Every
  other frontend's opaque-boundary default for an unresolved-but-real reference is
  `external.unresolved`/class 2 (§*The opaque boundary*). Shell's `source`/`.` is different in
  kind, not just unresolved: it **executes code in the current shell** (not a mere symbol
  reference), so at the `source` site the frontend emits an own **`process.control`/6** effect
  (not `external.unresolved`/2) — a `source` runs *something*, unknown to us, at a severity that
  reflects "arbitrary code in this process," not "an unresolved dependency touch." The literal
  path additionally becomes a **path-keyed** `ThirdParty` `external_reach` (`base` = the literal
  path text, not the bare word `"source"`), so the app's outward `source` surface is still
  recorded even though the target is never followed. Do not "align" the `process.control`/6
  token down to `external.unresolved`/2 — the divergence is intentional (see spec 029 §9).

## Per-frontend realization

- **Rust** — `use`-path → `pub` item via the module tree; `pub static` reads/writes
  resolved cross-file extend the existing `static`-set signals (spec 008 F2/F5).
  Module-init units are rare (no executable top-level).
- **TS/JS** — relative-specifier ladder → exported `function`/`const`/`class`;
  recognizes definition shapes (`export const X = createContext(…)`, custom hooks
  `export function useFoo`, components returning JSX). The existing React inheritance
  (#19) is the first, single-hop fold; 025 retrofits it onto the shared transitive
  fold. Module-init units capture top-level side effects.
- **Python** — `from … import` / dotted module → module-level `def`/`class`/assignment;
  module-init units capture import-time side effects. Out-of-scope dotted modules are
  opaque boundaries.
- **Shell** — no import table, no cross-file symbol resolution; `canonical_path` +
  `CanonicalIndex` resolve same-file function calls exactly (`resolved_target`); `source`/`.`
  is the only cross-unit linkage, and it stays opaque by design (`process.control`/6 own effect
  + a path-keyed `ThirdParty` reach for a literal path; `DynamicCode` risk, no reach, for a
  computed path). No synthesized module-init unit (see *Module-init units are TS/Python-shaped*
  above). `is_root` is always emitted `false` (per the shared *Roots* model above); the CLI sets
  roots from explicit FILE args.

## Schema additions this model requires (enumerated in spec 025)

None of these exist in the current core model; spec 025 must add and test them:

- **`EffectKind::ExternalUnresolved`** (wire `external.unresolved`, class 2) — the capped
  opaque-boundary token.
- **A retained `external_reaches` list** (`Scope`/`Summary` + per-`Hotspot`) — the recorded
  opaque-boundary surface (specifier + kind + site); no separate count field.
- **Inherited/propagated signals** on `Hotspot` — folded `effects`/`risk_features` carrying
  bounded provenance, kept distinguishable from own-body signals (so `own_score` vs an
  inherited/propagated score can both be reported).
- **A stable site key** `(unit_id, line, col, kind)` for de-duplicating signals across the
  fold — `unit_id` already encodes the path, so the only schema change is `Effect`/`RiskFeature`
  gaining **`col`** (both have `line`); an inherited effect's origin travels in `provenance`.
