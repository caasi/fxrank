# Spec 026 — TS cross-dialect resolution (`.tsx` ↔ `.ts`)

**Status:** draft v2 (2026-06-25, review-looped: Codex clean + opus design-confirmed, clarity/scope tightened) · **Issue:** #41 · **Extends:** spec
`025-3e-precise-module-resolution.md` (this finishes its TS story; surfaced by the Plan-5 dogfood,
recorded in 025-3e §9). **Precedence:** spec 001 governs base scoring; 025-3e governs
resolution/roots/propagation; this spec refines only the TS frontend's *dialect handling* so one
`TsModuleMap` spans both dialects. Code-vs-spec disagreements resolve to the spec.

## 1. Summary

The TS frontend resolves cross-file calls per **dialect group**: `dispatch_ts` partitions TS sources
into `Lang::Ts` vs `Lang::Tsx` and runs a separate `TsFrontend::analyze` for each, each building its
**own** `TsModuleMap` from only that group's files. But `.ts` and `.tsx` are the **same TS module
namespace** — a `.tsx` component routinely imports a `.ts` util/hook. So every `.tsx`→`.ts` reference
(relative `./util` **and** tsconfig alias `@/libs/utils`) computes `resolved_target = None` → opaque,
even though the `.ts` callee unit is in the same TS fold partition.

This spec dissolves the dialect grouping: `TsFrontend::analyze` selects each file's parse dialect
**per file** (from its path extension; `self.lang` is the stdin-only fallback), and `dispatch_ts`
passes **all** TS files to one `analyze` — so the `TsModuleMap` naturally spans both dialects and
`.tsx`→`.ts` resolves. (Chosen design "B"; the surgical "inject a shared map, keep grouping" was the
alternative.)

## 2. Motivation (dogfood)

`omni/114-kg-frontend` after spec 025-3e Plan 5 (`--project`): **523** residual `@/` alias reaches
are `.tsx`→`.ts` cross-dialect (led by `@/libs/utils` ×167). The relative `.tsx`→`.ts` case is hit the
same way. This is the dominant React pattern — a `.tsx` component importing `.ts` modules — so the
current per-dialect scoping leaves a large fraction of a real React app's first-party call graph
unresolved. It degrades safely (opaque, never misresolved), but the coverage loss is the point.

## 3. The bug (precisely)

- `dispatch_ts` (`fxrank-cli`) builds `groups: HashMap<Lang, Vec<SourceFile>>` keyed by
  `Lang::from_extension(ext)`, then for each `(lang, group)` constructs `TsFrontend { lang, … }` and
  calls `analyze(&group)`.
- `TsFrontend::analyze(files)` builds `module_map = TsModuleMap::build[_with_tsconfig](files, …)` from
  **its group only**, and parses every file with `self.lang`.
- Therefore the `.tsx` group's `module_map.keys` contains no `.ts` files. `resolve_import` for a
  `.tsx` file's `./util` (a `.ts` file) or `@/libs/utils` (a `.ts` module) misses `keys` → `None` →
  opaque.

`.ts`/`.tsx` already land in the SAME per-language fold partition at the CLI
(`partition_by_language` groups by `Language::Ts`), so the `CanonicalIndex` (fold) *does* hold the
cross-dialect units — only the frontend-side `module_map` that computes `resolved_target` is
dialect-scoped. The fix is entirely within the TS frontend's dialect handling.

## 4. Design (B — per-file dialect, one TS-wide map)

### 4.1 `TsFrontend::analyze` selects dialect per file

`parse_module(text, path, lang)` already takes a per-call `Lang`. Change `analyze` to derive each
file's dialect from **its own `source.path` extension**, falling back to `self.lang` only when the path
has no recognized extension (stdin). `Lang::from_extension` takes the **extension string** (no dot), so
`analyze` extracts it from the path:

```
ext_of(path)   = std::path::Path::new(path).extension().and_then(|e| e.to_str())   // "src/a.tsx" → Some("tsx"); "stdin" → None
dialect(file)  = ext_of(&file.path).and_then(Lang::from_extension).unwrap_or(self.lang)
```

Reading the extension off `source.path` inside `analyze` keeps the `Frontend::analyze(&[SourceFile])`
trait signature **fixed** (no trait change — §7). Build the `module_map` **once over all `files`**
(already the case — `analyze` builds from `files`); the change is that `files` now contains both
dialects (§4.2), so the single map spans them. Parse each file with its own `dialect(file)`. Everything
downstream (`collect`, the React two-pass `analyze_units`, `record_from_hotspot`, `refs::extract`) is
already per-file and unaffected.

`self.lang` is retained as the **stdin fallback** (a stdin `SourceFile` has a synthetic path with no
extension; `--lang` sets `self.lang`). For real files the extension always wins.

### 4.2 `dispatch_ts` stops grouping by dialect

Replace the `HashMap<Lang, Vec<SourceFile>>` grouping + per-group loop with a **single**
`TsFrontend::analyze` call over **all** routed TS sources:

```
let frontend = TsFrontend { lang: <stdin/default fallback>, include_tests, tsconfig: ts_cfg };
merge_output(&mut output, frontend.analyze(&all_ts_sources));
```

The tsconfig is already loaded once for the whole TS batch (unchanged). `self.lang` here is only the
stdin/no-extension fallback; for a file batch every file's dialect comes from its extension.

Because `analyze` now reads each file's extension off its path (§4.1), the per-file extension string the
CLI currently threads to `dispatch_ts` (the `(String, SourceFile)` pairs from `route_for_path` →
`dispatch`) becomes **unused for TS**. The plan MAY simplify that plumbing to plain `Vec<SourceFile>`
(a slightly wider edit through `Route::Ts`/`RoutedSource`/`dispatch`) or leave it threaded-but-ignored —
either is correct; pick one in the plan for honest scope. The minimal change is `dispatch_ts` + `analyze`.

### 4.3 What this does NOT change

- **Cross-LANGUAGE isolation stays.** The CLI's `partition_by_language` (Rust/TS/Python) is unchanged;
  this only merges the two TS *dialects* within the single TS partition. A TS unit still never resolves
  to a Rust/Python unit.
- **Own-body fields byte-identical.** Both the current per-group grouping and the new per-file path call
  the *same* `Lang::from_extension`, so every file is parsed with the identical dialect it received
  before — proven for every extension that reaches the frontend:

  | ext | routed by `route_for_path`? | current group `Lang` | new per-file `Lang` | match |
  |-----|-----|-----|-----|-----|
  | `ts` | yes | `Ts` | `Ts` | ✓ |
  | `tsx` | yes | `Tsx` | `Tsx` | ✓ |
  | `js` / `jsx` / `mjs` / `cjs` | yes | `Js` | `Js` | ✓ |
  | `mts` / `cts` | **no** (no routing arm — never collected) | — | — | ✓ (vacuous) |

  So each hotspot's effects/risks/own_score are unchanged; only `resolved_target`/`propagated_*` improve
  (more cross-dialect hits). **Hotspot ordering** is also unaffected: `Report::build` ranks output by
  `rank_key`, so input order doesn't reach the report (and removing the `HashMap<Lang,…>` group iteration
  makes the pre-rank order *more* deterministic — single walk order — not less). The plan verifies the
  `Report::build` ranking step.
- **React scoring preserved.** The React two-pass (`analyze_units`: component inheritance,
  `EffectInRender`, `useRef` hidden-mutation, `StateTransition`, inline-callback absorption + standalone-
  arrow suppression) is **per-file own-body** scoring, run inside the `for source in files` loop on each
  parsed module independently — it does not consult the cross-file `module_map`. Since every `.tsx`/`.jsx`
  file is still parsed with the identical dialect (the mapping table above), `analyze_units` produces the
  identical React augmentation regardless of which other files share the batch. So all React signals are
  unchanged (part of the byte-identical own-body). *Bonus:* a React-**absorbed** callback's outgoing ref
  (e.g. `useEffect(() => util())` where `util` is a `.ts` module) now also gets a cross-dialect
  `resolved_target` from the unified map — richer effect blast-radius, own-body still unchanged.
- **Never-guess preserved.** A `.tsx`→`.ts` reference resolves only if its expansion is an in-batch
  module key (now visible in the unified map); otherwise opaque, as today.
- **tsconfig / `--project`** (spec 025-3e Plan 5) is unchanged — the alias table feeds the one
  unified map.

## 5. Edge cases

- **stdin** — synthetic path, no extension → `dialect = self.lang` (set by `--lang`). Unchanged
  behavior; the single-file stdin batch builds a one-file map (no cross-dialect concern).
- **Mixed `.js`/`.jsx`/`.ts`/`.tsx`** in one project — all are the same TS module namespace; the unified
  map keys them uniformly via `module_key` (already extension-agnostic). A `.jsx`→`.ts` or `.ts`→`.tsx`
  import now resolves too. (`.mts`/`.cts` are NOT collected today — `route_for_path` has no arm for them,
  so they never enter a scan; extending routing to them is a separate change, out of scope here.)
- **An extension `from_extension` doesn't recognize** — cannot reach `analyze` (the CLI's
  `route_for_path` only routes recognized TS/JS extensions); the `unwrap_or(self.lang)` is the
  stdin-only path.

## 6. Testing

- **Unit (`fxrank-lang-ts`):** `analyze` over a 2-file batch `{src/app.tsx (import from './util'),
  src/util.ts (def x)}` → `app`'s ref to `x` carries `resolved_target = ["src","util","x"]` (was
  `None`). A relative AND an `@/`-alias (with tsconfig) variant.
- **Headline (the #41 fix):** a `.tsx` importing a `.ts` util resolves to `Edge::Resolved` through
  `CanonicalIndex`/`resolve_ref_precise` (was `Edge::Opaque`).
- **Regression:** existing single-dialect tests still pass; stdin + `--lang` still parses the right
  dialect. Note: no existing CLI snapshot exercises `dispatch_ts` (the `fxrank-lang-ts` insta snapshots
  call `TsFrontend::analyze` on a single file, bypassing the grouping), so the byte-identical guard is a
  **new** test the plan must author — a golden capture of `scan <mixed-dialect fixture dir>`'s own-body
  fields before vs after.
- **Dogfood (omni/114-kg-frontend, `--project`):** the `.tsx`→`.ts` `@/` reaches (the residual 523)
  drop sharply; inherited edges rise; `propagated_* ≥ own_*` holds (0 violations). Record before/after.

## 7. Out of scope / deferred

- Cross-language (TS↔Python/Rust) edges — stays out (025-3e); per-language partition unchanged.
- Any change to the Rust/Python frontends or the `Frontend` trait signature — this is TS-internal.
- Multiple-tsconfig / project-references / per-directory tsconfig — separate (025-3e §9).

## 8. Decomposition

A paired plan (`docs/superpowers/plans/026-ts-cross-dialect-resolution.md`, forthcoming via
`writing-plans`) decomposes this into: (1) `TsFrontend::analyze` per-file dialect + map over all files;
(2) `dispatch_ts` drop the dialect grouping (single `analyze`); (3) e2e + omni dogfood. Small, TS-only.
