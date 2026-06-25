# Spec 025 — Cross-file symbol resolution + transitive effect propagation

**Status:** draft v2 (revised after Codex review + escaping-summary refinement) · **Issue:**
#25 · **Absorbs:** #28 (cross-unit fold design — its three principles are normative here;
#28 to be closed pointing at this spec) · **Retrofits:** #19 (React inheritance becomes the
first consumer of the shared fold) · **Companion:** `docs/cross-file-resolution-guideline.md`
(descriptive shared model; read it first). **Precedence:** spec 001 governs base scoring;
this spec governs resolution, roots, and propagation; the more specific wins; code-vs-spec
disagreements resolve to the spec.

## 1. Summary

Today every frontend scores a function from its **own body** only, and resolves an imported
name to a **module string** then stops. This spec adds the missing second hop — *name →
definition* — and uses it to build a **call graph** rooted at **visible entry points**, then
**propagates escaping effects transitively** so a function's score reflects everything it can
transitively cause across the boundary. Boundaries the tool may not look through (third-party /
out-of-scope) are **recorded** as the app's external surface and scored as a bounded
`external.unresolved` known-unknown. Language-neutral machinery lives in `fxrank-core`
(parser-free); each of the three frontends populates and consumes it.

## 2. Motivation & the scoring model

- **Two quantities per unit.** Each unit keeps its **own score** (today's `own_score`,
  unchanged — all own-body effects incl. contained) *and* gains a **boundary summary** =
  the **escaping** effects it propagates to callers. Callers consume a callee's *boundary
  summary*, never its implementation — propagation is **modular**.
- **Escaping-only propagation.** Only effects that cross the boundary (IO, `global.mutation`,
  `panic`, `this.mutation`, hidden mutation, …) propagate; **`contained` effects stay local**
  (a private module-var / local mutation does not climb the graph). This bounds propagation
  and is exactly fxrank's containment thesis (contained = bounded = not observable to a caller).
- **Roots make the graph meaningful.** A unit's **propagated score** = its own score ∪ the
  boundary summaries of what it calls. Entry points show their blast radius; a private helper
  that does IO still surfaces (its escaping effect is real) — every unit is ranked, with a
  `root` annotation (decided in review).
- **Two blind spots closed:** import-time IO (the **module-init unit**), and the app's
  **external surface** (every unresolved outward `import`/`use` is recorded, not silently zeroed).

## 3. Scope

**In:** (1) language-neutral **`UnitRecord`** intermediate format + **call graph** + **fold**
in `fxrank-core`; (2) per-frontend **roots** (corpus-validated, §6); (3) **module-init units**
(§6, TS/Python); (4) **escaping-only transitive propagation**, memoized fixpoint + SCC, bounded
provenance (§7); (5) **external-surface recording** + `external.unresolved`/class 2 (§8) with a
per-frontend first/third-party classifier; (6) **CLI** N positional paths (§10); (7) all three
frontends populate + consume; **#19 retrofit** onto the shared fold.

**Out (accepted):** type inference / borrow-check / semantic pass; following *into* third-party;
full lexical-scope modeling (flat binding sets stay, spec 008 residual); cross-*language* edges;
sound parsing of executable config (`setup.py` is not executed — §8); dynamic resolution
(`import()`, `importlib`, dynamic `__all__`, runtime re-export) → recorded as `dynamic`, never
errors. **This is NOT the rejected shared-IR / scope-resolver path (#31):** `UnitRecord` is flat
pass-1 *output data* (scores + references), not a re-parseable semantic IR; each frontend keeps
its native walk.

## 4. Architecture — two passes, `UnitRecord` between them

`dispatch` partitions sources by language; within TS it further groups by **dialect**
(`.ts`/`.tsx`/`.js`/`.jsx`) because parsing needs the dialect. **The dialect split is a
parsing concern only** — and `UnitRecord` dissolves it: pass 1 runs per dialect group and
emits language-neutral `UnitRecord`s, then those records are **pooled per language** and pass 2
(the per-frontend resolver + the core fold) runs over the **pooled** set. A `.ts`↔`.tsx`
cross-file edge resolves because both files' records sit in the same pooled set and the TS
resolver/export index span all TS extensions — **no TS-specific "one-graph" batch API is
needed**. This makes all three frontends uniform (pass 1 → pool → pass 2). The pool is
per-**language**, never cross-language (a TS record never resolves into a Rust definition).

```
PASS 1  frontend.scan(&[SourceFile])  →  Vec<UnitRecord>   (per parse-group; TS per dialect)
  parse → units (incl. module-init unit, §6); for each unit emit a UnitRecord:
    { unit_id, path, line, col,
      own:      Vec<Effect> + Vec<RiskFeature>      (today's extraction, unchanged)
      escaping: subset of own that crosses the boundary (seed of the summary, §7)
      refs:     Vec<CallSiteRef>                    (outgoing references, NEW extraction)
      export:   Option<ExportKey>                   (its public identity, if any)
      is_root:  bool }                              (§6)

POOL    collect all UnitRecords of one language (across TS dialects) into one set
        [--no-resolve stops here: emit own scores + unresolved refs, §10]

PASS 2  per-frontend resolver + core fold, over the POOLED set
  build EXPORT INDEX: (module_id, name) → DefRef{unit, kind}   (collision rules §5)
  for each CallSiteRef: resolve to a unit_id (intra-file / via index) OR
    record an external reach (§8) when out-of-scope/third-party/dynamic/ambiguous
  fold boundary summaries bottom-up = memoized fixpoint + SCC (§7)
  assemble Hotspots (own + propagated + external reaches) → FrontendOutput
```

`fxrank-core` defines `UnitRecord`/`CallSiteRef`/the fold and stays parser-free
(compiler-enforced); the fold consumes only `UnitRecord`s — no parser leaks in. The
frontend↔core boundary is the `UnitRecord` set; because it is language-neutral, pooling across
dialect groups is what makes `.ts`↔`.tsx` edges resolve without any TS-specific graph code.

### How the `UnitRecord` boundary disposes of the review concerns

The contract partitions every known concern into one of three buckets — **dissolved** (a
non-issue once records exist), **contained** (real work, but confined to a frontend's pass-1
record-filling, invisible to core), or **centralized** (defined once in core, not per-frontend):

| concern | bucket | where it lives |
|---|---|---|
| TS `.ts`↔`.tsx` one graph | dissolved | pool records per language |
| call-graph input contract | dissolved | `UnitRecord`/`CallSiteRef` is the contract |
| module-init import edges | dissolved | `module_init→module_init` `CallSiteRef`s |
| module-init extraction path | dissolved | a module-init unit is an ordinary record |
| rooted-graph ranking | dissolved | `is_root` flag on the record |
| site key / schema | centralized | `SiteKey=(unit_id,line,col,kind)`; `Effect`/`Risk` gain `col` (§5) |
| risk escaping rule | centralized | one per-`RiskKind` table (§7) |
| `ExportIndex` collisions | centralized | one collision policy (§5) |
| `Report::build` aggregates | centralized | over assembled hotspots (§9) |
| roots | centralized | CLI sets `root` from explicit-file membership (§6/§13c); frontends are root-agnostic |
| Rust module-tree | designed → phase 3e (#36) | out-of-line `mod foo;` → module paths; name-based until then. See spec `025-3e` |
| first/third-party classifier | contained (pass 1) | frontend resolver reads config |
| method/member resolution | contained (pass 1) | syntactic only; a `RefKind::Method` ref never name-resolves (no receiver type) and is not a reach — its real IO is already an `Effect` |

**Consequence for sequencing:** only the two *contained* items carry heavy per-frontend work,
and core never depends on them — which is exactly why they live in **phase 3** while phases
1–2 build the core fold and cross-file resolution against simpler records (§15.6).

## 5. Core (`fxrank-core`) additions

Proposed shapes (names finalized in the plan; behavior is the contract):

- **`record::UnitRecord`** and **`record::CallSiteRef`** — the pass-1 intermediate format
  (flat data, **not** an IR; serializable to enable future `--emit`/incremental). `CallSiteRef
  = { kind: Free|Ctor|Method|Member, base: ResolvableName, site: SiteKey }`. **Resolution is
  syntactic only** (consistent with §3's no-type-inference rule): a `CallSiteRef` resolves when
  its receiver/base is syntactically determinable — a free call to an imported/intra-module
  name, a call on `self`/`this` or a known module path, a constructor of a named class. An
  arbitrary `expr.method()` whose receiver type is unknown is recorded **unresolved** (an
  external reach with kind `Dynamic`), never guessed. Per-frontend extraction lists which
  receiver shapes it resolves; everything else degrades.
- **`resolve::ExportIndex`** — `HashMap<ExportKey, DefRef>`, `ExportKey=(module_id, name)`,
  `DefRef={unit, kind: DefKind}`, `DefKind = Function|Value|Class|ModuleInit|Other`
  (neutral; framework shape info stays frontend-side). **Collisions** (barrels, `__all__`,
  `pub use`, duplicate/aliased exports): deterministic policy — a single unambiguous winner
  resolves; **ambiguous → treated as an external reach** (`kind: ambiguous`) + a diagnostic,
  never a silent arbitrary pick.
- **`graph` + `fold`** — the escaping-only transitive join fixpoint (§7) → per-unit
  `Propagated { effects, risks, provenance, external_reaches }`.
- **`EffectKind::ExternalUnresolved`** — wire `external.unresolved`, `base_class` **2**, tier
  `Heuristic`, confidence penalty.
- **Stable `SiteKey` = `(unit_id, line, col, kind)`.** `unit_id` already encodes the file path
  (`path:line:col:symbol` convention), so the only schema change is **`Effect` and `RiskFeature`
  gain `col`** (both have `line`) — path is sourced from the owning `UnitRecord`, and an inherited
  effect's origin travels in `provenance` (`{from: unit_id, via: path}`), not on the effect
  itself. `col` keeps two same-kind effects on one line distinct.

## 6. Roots & module-init units

**Roots are language-neutral and CLI-level** (full model in the guideline, *Roots — the agent's
observation focus*): a unit is a `root` **iff its file was an explicit CLI FILE argument** (or
stdin) — the agent's observation focus. Directory-walked files are **context**, not roots. Set at
the CLI discovery seam on `hotspot.root` + `record.is_root` (the CLI also sets `hotspot.root`
directly so `--no-resolve` is consistent; `apply_fold` copies `record.is_root` in the resolved
path). Every unit in an explicit file is a root. `is_root` is annotation-only — the fold never
seeds from it. **The frontends do NOT compute roots** (no `fn main`/framework/`__all__`
heuristic). *(See §13b: the earlier per-language program-entry heuristic was removed during
review — `root` answers "what is the agent observing?", not "what is the program's entry point?".)*

**Module-init units** stay per-frontend (TS/Python): a synthetic `<module>` unit captures
**import-time** effects (top-level statements + definition-time-evaluated expressions: decorators,
default args, Python class bodies & base-class exprs, JS/TS static blocks & field initializers),
emitted only when such effects exist. Its `root`-ness follows its file's explicit-ness like any
unit (not automatically a root). Rust module-init is N/A (top-level is `static`/`const` only).

**Module-init unit** (TS/Python): a synthetic unit per module whose body is import-time code —
top-level statements **and** definition-time expressions (decorators, default args, class
bodies, base-class exprs, JS/TS static blocks, field initializers, computed keys); the callable
body of a fn/method is **not** included. **Edges:** the module-init unit **calls** the
definitions it invokes (normal outgoing `CallSiteRef`s) — it does **not** blindly inherit every
exported function. **Plus** an edge `module_init(A) → module_init(B)` for each first-party
in-scope module `B` that `A` **statically imports**, because importing `B` executes `B`'s
init — without this a root misses import-time IO in its dependencies. (Re-export-only barrels
contribute no own effects, so the edge is cheap.) Synthesized only when import-time code exists
(essentially never in Rust).

## 7. Propagation algebra (the contract)

A unit's **boundary summary** is the **join (set union, never sum)** of its own **escaping**
signals and the boundary summaries of its first-party callees, over **both `effects` and
`risk_features`**, keyed by `SiteKey` (§5):

```
summary(u)    = escaping(own(u)) ∪ ⋃ summary(callee)      for each Resolved edge
propagated(u) = own(u) ∪ ⋃ summary(callee)                (u's own ranked score)
```

`escaping(·)` for **effects** keeps signals with `contained == false` (IO/`global`/`panic`/
`this`/hidden mutation, the `external.unresolved` token); `contained` effects stay in `own`
only. This is **not new vocabulary** — it is the cross-unit twin of the within-unit
boundary-containment discount (spec 003): the same containment notion at two scopes, where
*within* a unit a contained effect discounts toward class 0, and *between* units it simply
does not enter the boundary summary. A contained effect is, by definition, not observable to a
caller, so it contributes zero to anyone upstream. **Risks have no `contained` field**, so escaping is a **per-`RiskKind` predicate** (a
small static table, like `base_class`): *capability* risks that a caller transitively triggers
**escape** — `dynamic.code`, `ffi.call`, `html.injection`, `proto.pollution`, `effect.in.render`;
*encapsulated* risks the callee owns **do not** — `unsafe.block`/`fn`/`impl`, `transmute`,
`raw.ptr.*`, `maybe.uninit`, `from.raw`, `get.unchecked`, `asm`, `box.leak`, `mem.forget`,
`manually.drop`, `type.escape`, `impl.drop`, `extern.block`. (Calling an `unsafe`-using fn does
not make the caller unsafe; calling a fn that `eval`s does expose the caller to dynamic code.)
The classification is a judgment call recorded in §15.

**Principles (normative from #28):**
1. **Confidence-relevant metadata travels on fold** — async-await penalty + each frontend's
   dynamic-feature reducers ride along; absorbing unit never looks more confident than warranted.
2. **Stop at unanalyzable boundaries — opaque-penalize, don't follow** — first-party resolvable
   → fold; else record an external reach (§8). "Stop" ≠ "assume pure."
3. **Unknown default = bounded known-unknown** — `external.unresolved`/class 2 + confidence
   penalty; not 0, not class 7. Known-effectful packages keep real name-classified severity.

**Convergence:** signal **set** converges by idempotent union; memoized fixpoint with **SCC**
collapse (one summary per SCC); diamonds dedupe by `SiteKey`. **Provenance is bounded** —
exemplar/SCC-summarized (one representative path per inherited site), never the full path set.
Numeric score via existing `own_score = max + 0.5×rest`; `max_class` = max over the unioned
effect+risk classes.

**Blast-radius semantics (resolved):** `propagated` is a **blast-radius metric** — a root
reaching more escaping effect ranks higher, intended. `rank_key` orders by `propagated_max_class`
**first** (severity dominates: a class-7 path always outranks a sprawl of class-2), then
`propagated_score`. Escaping-only propagation already prevents contained-mutation accumulation;
no additional cap in this spec (revisit if dogfooding shows a critical focused hotspot buried).

## 8. External surface (the recorded unknown) & first/third-party classifier

**Not every** unresolved reference is a reach — only **meaningful outward references** are
recorded; intra-language **builtin methods, prelude intrinsics, and bare unqualified names**
are filtered (no edge, no reach, no `external.unresolved` effect), because they are not the
app's import surface and real effects through builtin methods are already captured by the
effect detectors. The filter is a **per-frontend syntactic vocabulary serving one shared
principle** — Rust by `::`-qualification, TS by import-specifier resolution, Python by
dotted-module/import; ambiguous in-scope matches are dropped (internal ambiguity, not a reach).
See `docs/cross-file-resolution-guideline.md` *What becomes a reach* for the shared model.

A reference that survives the filter and does not resolve to an in-scope unit is **recorded**
(not silently zeroed) on a retained list — the app's outward reach:

```
ExternalReach { specifier, kind: ThirdParty | FirstPartyOutOfScope | Dynamic | Ambiguous,
                site: SiteKey }
```

Each reach also contributes an `external.unresolved`/class-2 effect (+ confidence penalty) to
the referencing unit's escaping set, so it propagates. **There is no separate `unknown_count`
field** — the retained `external_reaches` list *is* the record; any count is its length.
**Known-effectful packages** (`axios`/`fetch`/`requests`/`subprocess`/…) keep their real
name-classified severity instead of the class-2 default.

**First/third-party classifier (per frontend), scoped to statically-parseable config only:**
- **TS** — nearest `tsconfig.json` `paths` (JSON) + workspace names from `pnpm-workspace.yaml`.
  First-party = relative ∨ alias-prefix ∨ workspace-name; else third-party.
- **Python** — `__init__.py` marks packages; resolve dotted modules against in-scope packages;
  `pyproject.toml` (TOML) where useful. **`setup.py` is executable and is NOT executed** — only
  a best-effort static scan; on failure, degrade to opaque.
- **Rust** — `crate::`/`self::`/`super::`/workspace-member → first-party; external crate name →
  third-party (read `Cargo.toml` workspace members, TOML).

**Phased sufficiency:** phase-1 classifier = relative/in-scope resolution + statically-parsed
aliases; everything else → `external.unresolved` with kind `FirstPartyOutOfScope` (+ "expand
scan" diagnostic) when it *looks* first-party, else `ThirdParty`. When undecidable, degrade to
opaque — never error.

## 9. Wire / schema additions

Additive; existing own-body fields unchanged.

- `Hotspot` gains: `propagated_score: f64`, `propagated_max_class: u8`, `inherited` (folded
  escaping `effects`/risks, each carrying bounded `provenance`), `root: bool`, and per-hotspot
  `external_reaches: Vec<ExternalReach>`. `own_score`/`max_class`/`effects`/`risk_features` keep
  their current own-body meaning, byte-stable.
- `Scope`/`Summary` gain: `external_reaches` (deduped app-wide list — the outward-surface
  record; no separate count field) and propagated aggregates. **`Report::build` is normatively
  updated**: summary `own_score`/
  `max_class`/`risk_weight`/`confidence` keep their current own-body computation; **new**
  propagated aggregates are computed analogously over `propagated_*` (max over hotspots,
  weakest-link confidence), `scope.risk_features` still participate in `max_class`/`risk_weight`;
  the sort key uses `propagated_max_class` then `propagated_score`; the limit truncation still
  happens **after** summary computation.
- New `Effect` wire kind `external.unresolved` (class 2). Provenance serialized compactly
  (`{from, via}`), bounded. Spec-001 conventions preserved (`3.0` rendering; per-effect
  confidence not serialized).

## 10. CLI changes

`scan` accepts **N positional paths** (`paths: Vec<PathBuf>`, variadic — files and/or dirs)
unioned into the routed source set = the in-scope set. `-`/omitted = stdin; `--lang` stdin-only;
`--exclude`/corpus profile apply to dir args; single-file/stdin skip the dir matcher.

**`--no-resolve`** — stop after **pass 1**: emit each unit's own (base) score + its outgoing
references (all left unresolved, i.e. every `CallSiteRef` becomes a recorded reach) and no
propagation. This is the per-file ground-truth view — it lets an agent confirm single-file
scoring and the extracted reference surface are correct **before** propagation composes them,
and it is cheap (no Pass 2). It is exactly the serialized `UnitRecord` set (§5).

## 11. Acceptance criteria

1. Per-language `UnitRecord`s + export index + call graph built once per scan from the in-scope
   set; `fxrank-core` carries the machinery and stays parser-free (compiler-enforced).
2. A first-party imported name resolves to its in-scope definition; the edge is folded.
3. **Transitive escaping propagation:** `A→B→C(io)` surfaces the IO on `A`'s propagated score;
   a `contained` local mutation in `C` does **not** climb to `A`; a *capability* risk
   (`dynamic.code`) in `C` climbs but an *encapsulated* risk (`unsafe.block`) does not; recursion
   + mutual recursion (SCC) terminate with one summary per SCC; a diamond counts a shared callee once.
4. **Module-init:** a top-level `fetch`/`requests.get`, a top-level `@decorator` that calls out,
   and a class-field initializer that calls out are captured by the synthetic `<module>` unit
   (its `root`-ness follows its file's explicit-ness, like any unit — see item 7); the unit
   *calls* what it invokes (not inherits-all-exports); a static `import` of a first-party module
   adds a `module_init→module_init` edge so a dependency's import-time IO reaches the importer;
   pure-definition modules synthesize none.
5. **External surface:** a third-party callee is recorded as an `ExternalReach{ThirdParty}` on
   the retained list and scores `external.unresolved`/2; a first-party-out-of-scope callee
   is recorded `FirstPartyOutOfScope` + diagnostic; an ambiguous export → `Ambiguous` +
   diagnostic; a dynamic import → `Dynamic`; none error. Known-effectful packages keep severity.
6. **First/third-party:** TS `@/`,`~/` alias + workspace-package imports → first-party; Rust
   external crate + Python site-packages → third-party; `setup.py`-only entry points degrade
   gracefully (not executed).
7. **Roots (CLI explicit-file — see §6/§13c):** a unit is a `root` iff its file was an explicit
   CLI FILE arg (or stdin); a directory-walked file's units are **not** roots (they are context).
   `scan a.rs` → every unit in `a.rs` is root (both `--resolve` and `--no-resolve`); `scan dir/`
   → no roots. Set centrally at the CLI; frontends compute no roots (no `fn main`/framework/`__all__`
   heuristic — that program-entry model was removed during review).
8. **TS dialect:** a `.ts`↔`.tsx` cross-file edge resolves and folds via the pooled `UnitRecord`
   set — no TS-specific graph code; the dialect split stays a parsing-only concern.
9. **#19 retrofit:** React component inheritance produces identical **own-body** semantics via
   the shared fold (a dedicated fixture asserts the inheritance, not whole-report byte-equality);
   TS carries one fold implementation, not two.
10. **CLI:** `scan a.ts b.tsx dir/` scans the union; partial scans degrade, never error.
11. **Own-body extraction unchanged:** `own_score`/`effects`/`risk_features` byte-stable per unit;
    a unit with no resolvable callees has `propagated_score == own_score`. Propagated fields +
    ranking change is expected (snapshots updated, §15).
12. `--limit` truncates hotspots **after** summary (incl. propagated aggregates) is computed.
13. **`--no-resolve`** emits per-unit own/base scores + outgoing references (all unresolved),
    runs no Pass 2, and equals the serialized `UnitRecord` set — the single-file ground-truth
    debug view.

## 12. Test plan

- **Core unit tests** (pure, no parser): escaping-filter; join idempotency; SCC convergence
  (self-loop, 2-cycle, diamond); bounded-provenance termination on a cycle; `external.unresolved`
  class/weight; `SiteKey` de-dup (two same-kind effects on one line stay distinct);
  `Report::build` propagated-aggregate + limit-before-truncation.
- **Per-frontend fixtures:** cross-file resolve+fold; contained-vs-escaping propagation;
  module-init unit + its edges; external reaches (third-party / first-party-out-of-scope /
  ambiguous re-export alias / dynamic); roots edge cases (Rust private-module `pub fn`,
  out-of-line module, bin vs lib; TS barrel + alias + Next.js page + `.ts`/`.tsx` edge; Python
  dynamic `__all__`, `console_scripts`, relative import).
- **Snapshot tests (`insta`):** a React-inheritance fixture asserting own-body semantics survive
  the retrofit.
- **Dogfood (manual gate, `docs/dogfood-repos`):** agent-browser (Rust+TS, `.ts`/`.tsx`),
  explore-ui (aliases/barrels/workspace pkgs), django + pytorch (import-time registration).
  Record root counts, the external-reach list (its length = outward-reach count), top
  propagated hotspots; check no propagation blow-up and no false-pure at dependency edges.

## 13. Known limitations (accepted)

- Transitive propagation is **first-party only**; third-party stays opaque + recorded.
- Flat binding sets (no full lexical scope) — spec 008 residual.
- Dynamic resolution (`import()`, `importlib`, dynamic `__all__`, runtime re-export) → recorded
  as `Dynamic`, not followed.
- `setup.py` console-scripts not statically recoverable are missed; TS `package.json.exports`
  pointing at `dist/` ignored for source roots; re-exported underscore Python imports under-counted.
- Provenance is exemplar (one path), not the full path set.
- **Symbol-name resolution can *falsely* resolve** (not just under-resolve) — a property of the
  shared name-based resolver, so it affects **all three frontends** (Rust `::`, Python dotted, TS
  member): a qualified call matched by its last segment resolves to a lone in-scope unit of that
  name if exactly one exists (only *multiple* collisions → `Ambiguous`/drop). E.g. `std::fs::write`
  → a lone `Foo::write`; Python `mod.write()` → a lone `write`; TS `fs.readFile()` → a lone
  `readFile`. This mis-attributes effects and suppresses the real external reach. The phase-3
  module tree (precise path/module resolution) fixes it; until then it is a wrong-resolution
  limitation, not merely imprecise.
- **Per-language pooling — RESOLVED in 2b.** The CLI driver now partitions `FrontendOutput.records`
  by `UnitRecord.language` (`partition_by_language`) and runs `SymbolIndex`/`CallGraph`/`fold` per
  language group, so a TS `helper` can't resolve to a Rust `helper` (proven by
  `run_scan_mixed_rust_python_no_cross_language_resolution`). Upholds the guideline's "a scan is
  per-language" invariant across all three record-emitting frontends.
- **`Effect.contained` — RESOLVED in 3a.** All three frontends now set the real `contained` (TS/Python
  from the `(Effect, bool)` mutation tuple; Rust `local.mutation` = contained). The escaping-only fold
  drops contained mutations from propagation, so `propagated_score` no longer over-reports (dogfood:
  Rust `main` inherited 109→36, prop_score 154.5→86.5, max_class unchanged). `contained` stays
  `#[serde(skip)]` so own-body output is byte-identical.

## 13a. Phase-3 status (as landed)

**Done & landed:** 3a real `Effect.contained` (de-noise); **roots = CLI explicit-file** (language-neutral —
a unit is a root iff its file was an explicit CLI FILE arg / stdin; directory-walked files are context; set
centrally at the CLI, frontends are root-agnostic — see §6 + §13c. *Supersedes the original 3b per-language
program-entry heuristic, which was removed during review.*); 3c first/third-party reach classifier (frontend
tags `CallSiteRef.first_party`: Rust `crate::`/`super::`/`self::`, Python relative imports, TS relative `./`+
`@/`/`~/` aliases; core `resolve_ref` picks `ReachKind`); 3d module-init units (synthetic `<module>` scoring
top-level/import-time effects, emitted only if ≥1 effect, own-body-isolated from nested defs).

**Phase status:**
- **3e module-tree / precise resolution** (tracked in **#36**) — **designed; see spec `025-3e-precise-module-resolution.md`.**
  Replaces the name-based resolver with **path-precise resolution** (Shape A / SCIP-Kythe string identity):
  frontends emit a canonical fully-qualified path per unit + import-resolved target per ref; core resolves by
  path equality over a neutral index, with a re-export **alias** index. Fixes the false-resolve across all three
  frontends (Rust module-tree reconstruction is the bulk — filesystem-convention, **no `cargo` shell-out**). The
  **workspace-member `first_party`** bonus survives (mostly free: in-scope siblings now resolve). The other two
  #36 bonuses — **pub-visibility-chain roots and the crate-type (bin/lib) root gate — are DROPPED**: both compute
  *program-entry* roots, which §13c deliberately removed in favour of `root` = CLI explicit file. Program-entry
  detection, if ever wanted, is a **distinct concept** from `root` (guideline *Roots — History*), not a #36 bonus.
- **3f React-inheritance retrofit** (tracked in **#37**) — route the within-file React absorption through the
  shared fold (§11.9, #28 step 4 "retrofit last"). Low value: pure internal consolidation, no new signal; the
  bespoke React two-pass and the shared fold coexist today with NO double-count (suppressed arrows are not graph
  nodes).

This feature closes **#25** (cross-file symbol resolution — fully delivered) and **#28** (cross-unit fold
design — principles 1–3 + Rust/Python/TS fold delivered; step-4 retrofit → #37) on merge, and delivers
recommendation #1 of the **#4** dual-layer-ranking research (SCC-condensed call-graph propagation).

### 13b. Review-loop outcomes (pre-merge, external Codex gate)

The pre-merge external review (Codex, headless) caught issues the internal subagent reviews missed; all fixed before merge:
- **F1 (correctness, important):** `propagated_*` was computed escaping-only, so a unit whose own effects were all *contained* (e.g. `local.mutation`) came out with `propagated_score`/`propagated_max_class` **below** own. Fixed — each unit's own propagated now = full own signals ∪ inherited (escaping) callee summaries; the boundary summary to callers stays escaping-only. **Invariant now enforced + tested: `propagated_* ≥ own_*` for every unit.**
- **F2 (incompleteness):** suppressed React hook-callback arrows absorbed their direct effects into the component but dropped their outgoing call refs, so a component didn't inherit transitive effects of helpers called inside `useEffect`/`useMemo`. Fixed — absorbed-arrow refs are routed into the component record (verified e2e: a component calling an IO helper inside `useEffect` now propagates class 7).
- **F4 (minor):** synthesized `external.unresolved` used a `line:0/col:0` site key, collapsing multiple same-package opaque calls from one unit. Fixed — uses the real call-site (parsed from `ExternalReach.site`).
- **F5 (minor):** TS module-init dropped exported/default **class** declarations, missing static-block/field-init import-time effects. Fixed — those decls enter the `<module>` body; static-init calls captured, method bodies still isolated.
- **F3 (no action — false positive):** `col` on `Effect`/`RiskFeature` is the **intentional** spec-§5/§9 wire addition (effect location precision), not a violation of own-body byte-stability.
- **N1 (correctness):** the symbol index (`simple_name_of`) split only on `::` while the callee lookup (`simple_callee_of`) split on `:`/`.` — so dot-qualified units (`C.m`) never resolved. Fixed — both split on `[':', '.']`; same-segment collisions correctly go ambiguous.

### 13c. Root model — replaced during review (the agent-observation clarification)

The original per-language program-entry root heuristic (the "3b" work — Rust `fn main`/exports/pub-chain, TS framework files/bootstraps/memo unwrap, Python `console_scripts`/`__main__`/`__all__`/non-underscore) was **removed** mid-review and replaced by a single language-neutral rule: **a unit is a `root` iff its file was an explicit CLI FILE argument (or stdin); directory-walked files are context.** `root` means "the agent's observation focus," not "the program's real entry point." This was a **clarification/simplification of the tool's intent**, not a late design pivot. It is a large net simplification (frontends become root-agnostic; ~−870 lines) and dissolved a string of brittle heuristic edge cases the review-loop kept surfacing (pre-fold vs `apply_fold` root consistency, anonymous default-export roots, memo-wrapper unwrapping, class-decorator roots). The full model + the preserved corpus insights from the abandoned heuristic live in the guideline (*Roots — the agent's observation focus*). **Multi-path CLI** (`scan focus.ts src/` → focus-roots + resolvable context corpus) is the natural next enhancement — the model's richest mode — and is not yet implemented (single path arg today).

## 14. Relationship to other work

- **#28** — fold algebra + three principles made normative (§7); on landing, #28 closed with a
  pointer here (rationale preserved offline).
- **#19** — first single-hop within-file fold; §7 generalizes it, §11.9 requires the retrofit.
- **spec 001 *Known Limitations*** — delivers the `inherited_score` propagation half; index is
  the prerequisite the deferred FFI call-site detection builds on.

## 15. Decisions (confirmed in review)

1. **Escaping-only boundary summary** — only `contained == false` signals propagate; contained
   stay in `own`. Bounds propagation; aligns with the containment thesis. **Confirmed.**
2. **Rank all units; `root` is an annotation** — every unit ranked by `propagated` score; a
   private helper doing IO still surfaces; roots are flagged, not the only ranked items. **Confirmed.**
3. **Blast-radius metric, severity-gated** — `propagated_max_class` dominates ranking; no extra
   cap this spec (§7). **Confirmed; revisit on dogfood.**
4. **`UnitRecord` intermediate format, not an IR** — flat pass-1 output data (scores + refs),
   serializable for future `--emit`/incremental; per-frontend native walks unchanged; **not** the
   rejected shared-IR path. **Confirmed.**
5. **External reaches recorded, not zeroed** — the app's outward surface is a retained list, not
   just a count. **Confirmed.**
6. **One spec, phased plan** — ① core `UnitRecord`/graph/fold + **intra-file** propagation +
   schema/ranking + `SiteKey`/type fields + **`--no-resolve`** (verifiable single-file); ②
   cross-file resolution + call-site extraction + **the pass-1/pass-2 split** (frontend emits
   `UnitRecord`s; a driver pools them per language and runs pass 2 — this is what resolves
   `.ts`↔`.tsx` edges, no TS-specific graph code) + export index; ③ roots (Cargo/module-tree,
   config parsing, tiered) + first/third-party classifier +
   module-init units (incl. import edges). Rust+Python are the two reference consumers for the
   core interface; TS-React retrofits last (#28 ordering). **Confirmed.**
7. **Per-`RiskKind` escaping predicate** (§7) — capability risks (`dynamic.code`, `ffi.call`,
   `html.injection`, `proto.pollution`, `effect.in.render`) escape; encapsulated risks
   (`unsafe.*`, `transmute`, `raw.ptr.*`, `mem.forget`, …) do not. A judgment call; revisit if
   dogfooding shows a transitive risk that should (or shouldn't) carry.
8. **`--no-resolve` debug mode** (§10) — pass-1-only output (own scores + unresolved references)
   for single-file self-checking; falls out of the two-pass `UnitRecord` design. **Confirmed.**
