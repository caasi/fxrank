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

The fix space is **default-exclude lists + a richer glob matcher**, not the scoring
model. This is a **scope decision** (what we scan), consistent with 002's stance:
FxRank is an effect-cost profiler, not a linter, and it should not waste an agent's
attention ranking code the agent will never refactor.

## Scope

In scope:

- Grow `--exclude` from **directory-name-only** matching to a matcher that handles
  **literal filenames, filename globs, and full-path globs** — mixed freely — while
  preserving today's directory-prune behavior for ordinary literal directory names
  (back-compatible; an entry containing glob metacharacters, e.g. `foo*`, now globs
  rather than denoting a literal directory named `foo*`).
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
- **Windows path support.** This is a macOS/Unix-first side project. Full-path globs
  are matched against a `/`-normalized relative path (so a future Windows port has
  one well-defined seam), but Windows is not a tested target here.

## Ownership: two skip layers, distinct concerns

| Layer | Where it lives | Flag | Owns |
| --- | --- | --- | --- |
| File-discovery exclude | CLI (`main.rs` walk) | `--exclude` (replace) | **general ecosystem file noise**: vendored, minified (named), stories, generated, JS test-support |
| Test-skip | frontends (`is_test`/`is_test_file`) | `--include-tests` | the **test mechanism**: Rust `#[test]`/`#[cfg(test)]`, TS `.test.`/`.spec.`/`__tests__` |

They own distinct concerns and no new flag is introduced. If a path would match both
layers (e.g. `src/__tests__/foo.stories.tsx`), the **discovery exclude wins** because
it runs first — the file is counted in `skipped_excluded` and never reaches the
frontend's test-skip. N3's test-support files
(`jest.setup.*`, `jest.config.*`, `__mocks__`) live in the `--exclude` default list
because they are JS-ecosystem *files*, not the test mechanism — so re-including them
is `--exclude`'s override, not `--include-tests`. `--exclude` is **not** the test
mechanism: a `*.stories.tsx` stays excluded even under `--include-tests`.

## The matcher: split on `/`

`--exclude` takes a comma-separated list. Each entry is classified first **by whether
it contains a `/`**; no-`/` entries are then split into *literal* (no glob
metacharacter) vs *wildcard*:

- **No `/`** (a literal filename, a directory name, or a *filename* glob) → matched
  against each entry's **base name** during the walk. Whether it can prune a
  *directory* depends on wildcards:
  - **Literal, no wildcard** (`node_modules`, `target`, `__mocks__`, `jest.setup.js`):
    a base-name match on a **directory** → **prune** that subtree (no recursion, never
    read, counted in nothing); a base-name match on a **file** → **exclude** it (skip
    before reading) and count it in `skipped_excluded`.
  - **Wildcard glob** (contains any glob metacharacter — `*`, `?`, `[`, `{` — e.g.
    `*.min.js`, `*.stories.*`, `jest.config.*`): matches **files only** — a base-name file match excludes+counts
    it; it **never prunes a directory**. This is deliberate: nobody writes
    `*.stories.*` to prune a tree, and letting a filename glob match a same-named
    directory (`x.stories.d/`) would silently drop real source *uncounted* — the
    mirror of the literal-filename footgun this design removes. To prune a tree, use a
    literal directory name. (Because the base name is a separator-free string, a `*`
    in a no-`/` glob spans the whole base name.)
- **Contains `/`** (`src/vendor/**`, `**/*.stories.*`, `packages/*/generated/**`) →
  glob matched against the file's **path relative to the scan root**, `/`-normalized.
  Path globs are **file filters, not traversal prunes** — the walk still descends the
  tree and skips matching *files*. To prune a whole subtree cheaply, use its **bare
  directory name** (e.g. `vendor`, `dist`) as a no-`/` entry.

This is why "mix single files and filename globs" just works: literal names and
filename globs are both no-`/` basename entries, matched the same way. It also kills
the footgun where `--exclude jest.setup.js` would otherwise be read as a directory
name and silently skip nothing — a no-`/` entry matches *files* too.

**Override semantics: replace (unchanged from today).** Passing `--exclude` replaces
the entire default list. The defaults are printed in `--help` so an agent can copy
and extend them. (Additive semantics + an escape hatch were considered and rejected:
the "restate the whole list to add one entry" cost falls only on humans typing by
hand, not on the agent that is the primary consumer.)

**Implementation choice:** use the `globset` crate (BurntSushi / ripgrep — the
gold-standard, actively-maintained glob engine). It lives only in `fxrank-cli` (core
stays parser- and dependency-free). Partition the entries three ways: (1) **literal**
no-`/` names → a `HashSet<String>`, used for **both** directory pruning and file
exclusion by base-name equality; (2) **wildcard** no-`/` globs → a `GlobSet` matched
against each **file** base name (file exclusion only — never prunes); (3) `/`-bearing
entries → a `GlobSet` matched against the `/`-normalized relative path (file exclusion
only).

**`--exclude` applies to directory scans only.** A single explicit file
(`fxrank scan a.min.js`) and stdin (`scan --lang ts -`) are **always honored** — an
explicitly named target is never silently dropped by a default. Exclusion is a
directory-walk concern; `run_scan`'s file and stdin branches do not consult
`--exclude`. (Consequently there are no single-file/stdin exclusion test cases — it
is a no-op there by design.)

## Default exclude list (per-language)

The effective default is the union across languages. A no-`/` filename glob is
matched on the base name, so `*.stories.*` *can* lexically match a hypothetical
`foo.stories.rs`; this residual cross-language overlap is accepted (the names are JS
conventions; collisions in real Rust trees are vanishingly rare and, for the
directory entry `__mocks__`, harmless). The defaults are **all no-`/` entries**:

- **Common (all languages):** `node_modules`, `.git`, `target`
- **TS / JS ecosystem:** `*.min.js`, `*.min.mjs`, `*.min.cjs`, `*.stories.*`,
  `mockServiceWorker.js`, `jest.setup.*`, `jest.config.*`, `__mocks__`
- **Rust ecosystem:** (none beyond common — `target` already covers it)

This resolves:

- **N1** (named bundles) via `*.min.js` / `*.min.mjs` / `*.min.cjs` (the routed
  JS-family minified extensions). Unnamed bundles (`swagger-ui.js`) are *not*
  auto-skipped — a documented, accepted limitation (minified code isn't a target;
  add a manual `--exclude` entry if needed).
- **N3** via `jest.setup.*`, `jest.config.*`, and the `__mocks__` directory prune.
- **N4** via `*.stories.*` (covers the routed `.stories.{ts,tsx,js,jsx,mjs,cjs}`;
  `.stories.mdx` is already not a routed extension, so it never reaches the walk's
  read step regardless).
- **N5** via `mockServiceWorker.js` (the specific generated file; a broad `public/`
  prune is *not* applied — `public/` hosts hand-written source too).

The literal default string is `node_modules,.git,target,*.min.js,*.min.mjs,*.min.cjs,*.stories.*,mockServiceWorker.js,jest.setup.*,jest.config.*,__mocks__`.

## Behavior

- **Default:** the union default list above is the active exclude set. No-`/`
  *literal* entries prune matching directories and exclude matching files; no-`/`
  *wildcard* entries exclude matching files only; `/`-globs exclude matching files by
  path.
- **`scope.skipped_excluded`** counts **files** skipped by any exclude entry (a
  `*.stories.tsx`, a `*.min.js`, a `jest.setup.js`). **Directory prunes are not
  counted** — a pruned tree (`node_modules`, `__mocks__`) may be arbitrarily large
  and is never read; counting it would require descending into it, defeating the
  prune. Documented asymmetry (same as today's `node_modules` handling).
- **`scope.files`** continues to count files that were **read** (parsed set + read
  errors). An excluded file never enters the collected `sources`, so it contributes
  to **neither** `files` **nor** `read_errors` — it is reflected only in
  `skipped_excluded`. A pruned directory's contents appear in none of the three.
- **`--exclude <list>`** replaces the default entirely. To keep the defaults and add
  one entry, the caller restates the documented default list plus the new entry.
- **`--include-tests`** is unchanged and orthogonal — it governs the test
  *mechanism* only and does not re-include `--exclude`-skipped files.

## Architecture

- **CLI (`fxrank-cli`).** The `--exclude` arg keeps its comma `value_delimiter` and a
  `default_value` updated to the documented union string. In `run_scan`, build the
  matcher from the entries via the three-way partition described under *Implementation
  choice* above: no-`/` literals → a `HashSet<String>`; no-`/` wildcards → a
  `globset::GlobSet`; `/`-bearing entries → a second `globset::GlobSet`. Empty entries
  (from `--exclude ''` or a trailing comma) are **ignored** (inert) before
  compilation.
- **Walk integration (`walk_dir`).** Thread the three matchers and the scan-root
  prefix through the walk:
  - For each **directory** entry: prune iff its **base name** is in the literal set
    (1). Wildcard globs (2) and `/`-globs (3) never prune. This subsumes today's
    `HashSet` segment check (which was already literal-only).
  - For each **routable file** (after `route_for_path` returns `Some` — see invariant
    below): exclude iff its base name is in the literal set (1) **or** matches the
    wildcard base-name set (2) **or** its `/`-normalized root-relative path matches
    the path set (3). On a match, **do not read** the file and increment a
    `skipped_excluded` tally instead of pushing to `sources`.
- **Exclusion runs after extension routing (invariant).** The glob check is applied
  only to files `route_for_path` already accepts. Non-routable files are skipped
  anyway, so excluding them is moot; placing the check after routing keeps "what
  could be excluded" ⊆ "what would be read." An implementer must not move the check
  before routing.
- **Relative-path computation.** The walk currently carries absolute-ish
  `entry.path()`s; thread the scan-root prefix so `/`-globs match the path *relative
  to the root* (so `**/` behaves predictably regardless of the invoking cwd). The
  relative path is `/`-normalized before matching (the one Windows seam; basename
  matching needs no normalization — a file name carries no separator).
- **Core (`fxrank-core`).** `Scope` gains `skipped_excluded: usize`, declared
  **after `skipped_tests`** and before `risk_features` (serde emits in declaration
  order; see schema below). It is purely a CLI-supplied count — no frontend or
  scoring change. `FrontendOutput` is **not** modified (exclusion happens before any
  source reaches a frontend).
- **No frontend changes.** Excluded files are never read, so neither `RustFrontend`
  nor `TsFrontend` sees them; `is_test`/`is_test_file` are untouched.

## Output schema change

`scope` gains one field, declared after `skipped_tests`:

```json
"scope": { "input": "src", "files": 40, "parsed": 40, "functions": 120, "skipped_tests": 8, "skipped_excluded": 12, "risk_features": [] }
```

## Error Handling

- **One new startup-validation failure mode:** an invalid glob in `--exclude` is a
  `globset` compile error, surfaced as the standard JSON `{ "error": ... }` object
  with a non-zero exit — fail fast, never silently ignore a malformed pattern. (This
  refines spec 001's "no new failure modes" for the scan command.)
- **Comma is the list delimiter, so an entry cannot contain a literal comma.** clap's
  `value_delimiter = ','` splits on every comma, so brace alternation that contains a
  comma (`*.{js,ts}`) is **not** expressible as one entry — it would split into
  `*.{js` and `ts}` (both invalid globs → startup error). Use separate entries
  (`*.js`, `*.ts`) instead. Brace groups without commas, and all other glob
  metacharacters, are fine.
- **Empty entries are inert.** `--exclude ''` or a trailing comma yields an empty
  string, which is dropped before compilation (matches nothing, prunes nothing).
  Since override is *replace*, `--exclude ''` with no other entry therefore disables
  **all** default excludes — an empty effective matcher (every file is scanned).
- The walk's existing read/permission diagnostics are unchanged.

## Testing Strategy

- **Dir-name back-compat:** `--exclude node_modules,.git,target` prunes those dirs
  exactly as today; no file under them is read; `skipped_excluded == 0`.
- **Literal filename (the resolved footgun):** `--exclude jest.setup.js` on a tree
  containing `src/jest.setup.js` excludes that file (basename match) and counts it —
  it is **not** treated as a directory name.
- **Filename glob:** a tree with `a.ts`, `a.min.js`, `b.stories.tsx`,
  `mockServiceWorker.js` → with defaults, only `a.ts` is scored; `skipped_excluded ==
  3`.
- **Mixed entries:** `--exclude '*.min.js,vendor.js,src/legacy/**'` skips by glob,
  literal name, and full-path glob in one run.
- **Full-path glob is a file filter, not a prune:** `--exclude 'src/legacy/**'`
  skips routable files under `src/legacy/` (counted in `skipped_excluded`) while the
  walk still descends it; a bare `legacy` instead prunes the subtree (uncounted).
- **Replace semantics:** `--exclude '*.foo'` makes `a.min.js`/`b.stories.tsx`
  reappear in the scored set (defaults dropped) — proves replace, not additive.
- **Relative-path anchoring:** `**/*.stories.*` matches `pkg/ui/x.stories.tsx`
  regardless of the directory passed to `scan`.
- **`__mocks__` dir-prune:** files under `src/__mocks__/` are pruned and **not**
  counted in `skipped_excluded` (dir-prune asymmetry).
- **Wildcard never prunes a dir:** a directory named `x.stories.d/` containing `a.ts`
  is **not** pruned by the default `*.stories.*` (wildcard → files only); `a.ts` is
  still scored. A literal `legacy` entry, by contrast, prunes `src/legacy/`.
- **jest support files:** `jest.setup.js`, `jest.config.ts` skipped by default and
  counted in `skipped_excluded`.
- **`files` accounting:** excluded files contribute to neither `files` nor
  `read_errors`; `files == parsed` when every excluded file is well-formed.
- **Invalid glob:** `--exclude '['` exits non-zero with a JSON `error`.
- **Empty entry:** `--exclude ''` (and a trailing comma) is inert — equivalent to no
  excludes for that slot.
- **`--include-tests` orthogonality:** a `*.stories.tsx` stays excluded even with
  `--include-tests` (exclude is not the test mechanism).
- **Single-file / stdin no-op:** `fxrank scan a.min.js` and `scan --lang ts -` ignore
  `--exclude` (explicit target honored) — asserted as a no-op, not as exclusion.
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
| Matcher classification | Split on `/`; among no-`/`, only **literal** names prune directories, **wildcard** names match files only; `/` → full-path glob (file filter) | Lets users mix literal filenames and filename globs; removes both the "bare filename silently prunes nothing" and "filename glob silently prunes a dir" footguns. |
| Glob engine | `globset` (ripgrep), CLI-only; a literal `HashSet` + two `GlobSet`s | Gold-standard, maintained; core stays parser/dep-free. |
| Override | Replace (unchanged); defaults in `--help` | The restate cost falls on humans, not the agent consumer; simplest model. |
| N3 test-support placement | `--exclude` default list, not test-skip | They are JS-ecosystem *files*, not the test *mechanism*; keeps `--include-tests` clean. |
| Minified detector | Patterns only, no density heuristic; cover `.min.{js,mjs,cjs}` | Zero magic, zero false-skip; minified code isn't a target. |
| N2 ID collision | Deferred | Only occurs in minified files we now skip; tool targets unminified source. |
| Config file | Deferred | Agent regenerates the command per run; persistence is a human nicety for later. |
| `public/` / `fixtures/` / `*.config.*` blanket | Omitted | Host hand-written source too often — false-skip risk. Target the specific generated file (`mockServiceWorker.js`) instead. |
| Directory pruning via globs | Not added; use bare dir names | Bare names already prune (`vendor`, `dist`); `/`-globs are file filters. YAGNI on deep-pattern prunes. |
| `--exclude` on single-file/stdin | No-op; explicit target always honored | A named target should never be silently dropped by a default. |
| Windows `\` paths | `/`-normalize the relative path for `/`-globs; Windows untested | macOS-first side project; one well-defined seam without claiming support. |
| Transparency | `scope.skipped_excluded` (files only) | Never a silent drop; dir-prunes uncounted because never read (matches `node_modules`). |

## Open Questions

- Should `skipped_excluded` break down *by pattern* (how many per glob) for richer
  diagnostics? v1 reports a single total; revisit if agents want the breakdown.
- A density/heuristic backstop for *unnamed* bundles (N1 residual) — revisit only
  with real data showing unnamed bundles are common enough to matter.
- `.fxrankignore` / config-file persistence — revisit if humans (not agents) become
  a meaningful consumer.
