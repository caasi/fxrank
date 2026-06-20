# 004 — Corpus-Hygiene Excludes (default skip lists + path globs)

## Goal

A field test (issue #6) ran `fxrank scan` against 11 production TS/JS repos
(~4,500 files). The **effect classification was sound** — 100% parse rate, 0
diagnostics, every hand-checked high-severity label a true positive. The failure
was **corpus hygiene, not scoring**: fxrank ranked files that should never be
ranked, and in two repos that *buried or beat* the real signal.

- **N1 — vendored/minified bundles dominate.** One backend's top-50 hotspots were
  100% vendored libs (`swagger-ui.min.js`, `jquery-1.8.0.min.js`, `lodash.min.js`,
  …) under a Swagger resources dir. `real=0/50`.
- **N3 — test-support files aren't excluded.** A `jest.setup.js` was the **#1
  hotspot in a whole repo** (own_score 305, ~13× the runner-up).
- **N4 — Storybook stories aren't excluded.** In a pure component library, **31 of
  the top 50** hotspots were `*.stories.tsx` demo handlers.
- **N5 — generated files in `public/`** (`mockServiceWorker.js`, header says "do
  NOT modify") flagged class-7 and duplicated across packages.

The fix space is **default-exclude lists + a path-glob matcher**, not the scoring
model. This is a **scope decision** (what we scan), consistent with 002's stance:
FxRank is an effect-cost profiler, not a linter, and it should not waste an agent's
attention ranking code the agent will never refactor.

## Scope

In scope:

- Grow `--exclude` from **directory-name** matching to **path-glob** matching, while
  preserving today's bare-name behavior (back-compatible).
- A richer, **documented, overridable** default exclude list covering vendored
  bundles (named), Storybook stories, the MSW generated worker, and JS test-support
  files (N1/N3/N4/N5).
- Report a `scope.skipped_excluded` count — never a silent drop.

Out of scope (deferred, Milestone-B candidates):

- **N2 — anonymous-fn ID collision in single-line/minified files.** The collision
  only occurs in minified bundles, which this spec skips by default; **minified code
  is not a target** (FxRank exists to help agents write better *unminified* source).
  Adding a column offset to the `id` is tracked separately.
- **Content/density heuristics** (e.g. "200+ fns on 3 physical lines = a bundle") to
  catch *unnamed* minified files (`swagger-ui.js`, `handlebars-4.0.5.js`). Patterns
  only; an unnamed bundle needs a manual `--exclude` entry. Rationale: zero magic,
  zero false-skip, and minified code isn't a target.
- **A config file** (`.fxrankignore` / `fxrank.toml`). The primary consumer is a
  coding agent that regenerates the command per run, so per-invocation flags +
  documented defaults suffice; persistence is a human convenience added only if
  demand appears.
- **Broadening test-*detection*.** `--include-tests` keeps owning the test
  *mechanism* (Rust `#[test]`/`#[cfg(test)]` from 002; the shipped TS
  `.test.`/`.spec.`/`__tests__` from 003). It is **not** touched here. JS
  test-*support* files (jest setup/config, `__mocks__`) are **general ecosystem file
  noise** and belong to `--exclude`, not the test mechanism.
- A blanket `fixtures/` segment or generic `*.config.*` glob in the defaults — those
  names host real source too often (false-skip risk). Deliberately omitted.

## Ownership: two skip layers, distinct concerns

| Layer | Where it lives | Flag | Owns |
| --- | --- | --- | --- |
| File-discovery exclude | CLI (`main.rs` walk) | `--exclude` (replace, glob) | **general ecosystem file noise**: vendored, minified (named), stories, generated, JS test-support |
| Test-skip | frontends (`is_test`/`is_test_file`) | `--include-tests` | the **test mechanism**: Rust `#[test]`/`#[cfg(test)]`, TS `.test.`/`.spec.`/`__tests__` |

The two never overlap and no new flag is introduced. N3's test-support files
(`jest.setup.*`, `jest.config.*`, `__mocks__`) live in the `--exclude` default list
because they are JS-ecosystem *files*, not the test mechanism — so re-including them
is `--exclude`'s override, not `--include-tests`.

## The matcher: bare-name (today) + path-glob (new)

`--exclude` takes a comma-separated list. Each entry is classified once:

- **Bare name** — no `/` and no glob metacharacters (`*`, `?`, `[`, `{`):
  e.g. `node_modules`, `.git`, `target`, `__mocks__`. **Prunes any directory segment
  equal to the name** during the walk (no recursion into it). This is *exactly*
  today's behavior, preserved verbatim, and it never reads — so a pruned tree
  contributes nothing and is **not** counted in `skipped_excluded`.
- **Glob** — contains `/` or a glob metacharacter: e.g. `**/*.min.js`,
  `**/*.stories.*`, `**/jest.setup.*`. **Matched against the file's path relative to
  the scan root.** A matched *file* is skipped before reading and counted in
  `skipped_excluded`.

Globs match the **path relative to the scan root** (so `**/` anchors anywhere
beneath it). The classification is purely lexical (presence of metacharacters), so
no behavior is hidden behind detection.

**Override semantics: replace (unchanged from today).** Passing `--exclude` replaces
the entire default list. The defaults are printed in `--help` so an agent can copy
and extend them. (Additive semantics + an escape hatch were considered and rejected:
the "restate the whole list to add one entry" cost falls only on humans typing by
hand, not on the agent that is the primary consumer.)

**Implementation choice:** use the `globset` crate (BurntSushi / ripgrep — the
gold-standard, actively-maintained glob engine) for the glob arm. It lives only in
`fxrank-cli` (core stays parser- and dependency-free). Bare names bypass `globset`
entirely (segment-equality during the walk, as today).

## Default exclude list (per-language, applied as a union)

Globs are extension-scoped, so unioning every language's list is safe
(`**/*.min.js` can never match a `.rs` file). The effective default is the union:

- **Common (all languages):** `node_modules`, `.git`, `target`
- **TS / JS ecosystem:** `**/*.min.js`, `**/*.stories.*`, `**/mockServiceWorker.js`,
  `**/jest.setup.*`, `**/jest.config.*`, `__mocks__`
- **Rust ecosystem:** (none beyond common — `target` already covers it)

This resolves:

- **N1** (named bundles) via `**/*.min.js`. Unnamed bundles (`swagger-ui.js`) are
  *not* auto-skipped — a documented, accepted limitation (minified code isn't a
  target; add a manual `--exclude` entry if needed).
- **N3** via `**/jest.setup.*`, `**/jest.config.*`, `__mocks__`.
- **N4** via `**/*.stories.*` (covers `.stories.tsx`/`.ts`/`.jsx`/`.js`/`.mdx`).
- **N5** via `**/mockServiceWorker.js` (the specific generated file; a broad
  `public/` prune is *not* applied — `public/` hosts hand-written source too).

`__mocks__` is a directory convention, so it is expressed as a **bare-name dir-prune**
(consistent with `node_modules`); the rest are file globs.

## Behavior

- **Default:** the union default list above is the active exclude set. Bare names
  prune dirs (as today); file globs skip individual files.
- **`scope.skipped_excluded`** counts files skipped by a **file glob** (e.g. a
  `*.stories.tsx`, a `*.min.js`). Dir-pruned trees (`node_modules`, `__mocks__`) are
  **not** counted — they may be arbitrarily large and were never read; counting them
  would require descending into them, defeating the prune. Documented asymmetry.
- **`--exclude <list>`** replaces the default entirely. To keep the defaults and add
  one entry, the caller restates the documented default list plus the new entry.
- **`--include-tests`** is unchanged and orthogonal — it governs the test
  *mechanism* only and does not re-include `--exclude`-skipped files.

## Architecture

- **CLI (`fxrank-cli`).** The `--exclude` arg keeps its comma `value_delimiter` and a
  `default_value` updated to the documented union list. In `run_scan`, build a matcher
  from the entries: partition into a `HashSet<String>` of bare names (existing
  fast-path) and a `globset::GlobSet` of glob entries (each compiled relative-path).
  `walk_dir` gains the glob set; for each directory it still checks the bare-name set
  (prune); for each routable **file** it additionally checks the glob set against the
  path **relative to the scan root** and, on match, increments a
  `skipped_excluded` tally instead of reading the file.
- **Relative-path computation.** The walk currently carries absolute-ish
  `entry.path()`s; thread the scan-root prefix so the glob is matched against the
  path *relative to the root* (so `**/` behaves predictably regardless of where the
  user invoked the tool). Bare-name dir matching stays on the segment file name and is
  unaffected.
- **Core (`fxrank-core`).** `Scope` gains `skipped_excluded: usize`, declared
  **after `skipped_tests`** (serde emits in declaration order; see schema below). It
  is purely a CLI-supplied count — no frontend or scoring change. `FrontendOutput`
  is **not** modified (exclusion happens before any source reaches a frontend).
- **No frontend changes.** Excluded files are never read, so neither
  `RustFrontend` nor `TsFrontend` sees them; `is_test`/`is_test_file` are untouched.

## Output schema change

`scope` gains one field, declared after `skipped_tests`:

```json
"scope": { "input": "src", "files": 40, "parsed": 40, "functions": 120, "skipped_tests": 8, "skipped_excluded": 12, "risk_features": [] }
```

`files` continues to count files that were **read** (parsed set + read errors);
glob-excluded files are reflected only in `skipped_excluded`, and dir-pruned trees in
neither (never read, never counted) — same as today's `node_modules` handling.

## Error Handling

No new failure modes. An invalid glob in `--exclude` is a **startup error**
(`globset` compile failure) surfaced as the standard JSON `{ "error": ... }` object
with a non-zero exit — fail fast, never silently ignore a malformed pattern. Bare
names cannot fail to compile. The walk's existing read/permission diagnostics are
unchanged.

## Testing Strategy

- **Bare-name back-compat:** `--exclude node_modules,.git,target` prunes those dirs
  exactly as today; no file under them is read; `skipped_excluded == 0`.
- **File glob:** a tree with `a.ts`, `a.min.js`, `b.stories.tsx`, `mockServiceWorker.js`
  → with defaults, only `a.ts` is scored; `skipped_excluded == 3`.
- **Replace semantics:** `--exclude '**/*.foo'` makes `a.min.js`/`b.stories.tsx`
  reappear in the scored set (defaults dropped) — proves replace, not additive.
- **Relative-path anchoring:** `**/*.stories.*` matches `pkg/ui/x.stories.tsx`
  regardless of the directory passed to `scan`.
- **`__mocks__` dir-prune:** files under `src/__mocks__/` are pruned and **not**
  counted in `skipped_excluded` (dir-prune asymmetry).
- **jest support files:** `jest.setup.js`, `jest.config.ts` skipped by default and
  counted in `skipped_excluded`.
- **Invalid glob:** `--exclude '['` exits non-zero with a JSON `error`.
- **`--include-tests` orthogonality:** a `*.stories.tsx` stays excluded even with
  `--include-tests` (exclude is not the test mechanism).
- **Dogfood regression:** `scan crates/` output is unchanged (our tree has no
  `*.min.js`/stories/jest files; `target` already pruned).

## Verification

- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check` all green.
- All slim builds still compile (`--features rust`, `--features ts`, no-features) —
  `globset` is in `fxrank-cli`, present under every feature combination.
- `fxrank scan <repo>` on a Storybook library now surfaces real components at the
  top (stories gone); `--help` documents the default exclude list verbatim.

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| Stance | Skip noise by default, opt-out via `--exclude` | Out-of-box reports are usable; an agent shouldn't sift vendored/story/setup noise. |
| Matcher | Bare-name (prune dir) + path-glob (skip file), classified lexically | Preserves today's behavior; adds file patterns with no hidden detection. |
| Glob engine | `globset` (ripgrep), CLI-only | Gold-standard, maintained; core stays parser/dep-free. |
| Override | Replace (unchanged); defaults in `--help` | The restate cost falls on humans, not the agent consumer; simplest model. |
| N3 test-support placement | `--exclude` default list, not test-skip | They are JS-ecosystem *files*, not the test *mechanism*; keeps `--include-tests` clean. |
| Minified detector | Patterns only, no density heuristic | Zero magic, zero false-skip; minified code isn't a target. |
| N2 ID collision | Deferred | Only occurs in minified files we now skip; tool targets unminified source. |
| Config file | Deferred | Agent regenerates the command per run; persistence is a human nicety for later. |
| `public/` / `fixtures/` / `*.config.*` blanket | Omitted | Host hand-written source too often — false-skip risk. Target the specific generated file (`mockServiceWorker.js`) instead. |
| Transparency | `scope.skipped_excluded` (file globs only) | Never a silent drop; dir-prunes uncounted because never read (matches `node_modules`). |

## Open Questions

- Should `skipped_excluded` break down *by pattern* (how many per glob) for richer
  diagnostics? v1 reports a single total; revisit if agents want the breakdown.
- A density/heuristic backstop for *unnamed* bundles (N1 residual) — revisit only
  with real data showing unnamed bundles are common enough to matter.
- `.fxrankignore` / config-file persistence — revisit if humans (not agents) become
  a meaningful consumer.
