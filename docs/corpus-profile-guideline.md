# Corpus-profile guideline (descriptive)

How FxRank decides which files to scan and which to skip during a directory walk.
This is a **descriptive reference** — it documents the shared model, the per-language
`CORPUS_PROFILE` constants, and the honest per-language differences. It is not a
contract; nothing is required to conform to this file.

**The `CORPUS_PROFILE` constants in each frontend crate are the normative
single source of truth.** The per-language table below is illustrative only and
must agree with those constants but does not override them. Always read the actual
consts (`crates/fxrank-lang-{rust,ts,python}/src/lib.rs`) when the exact entries
matter.

## Shared model

Each frontend crate declares one `pub const CORPUS_PROFILE: CorpusProfile`
(`fxrank_core::CorpusProfile`) that captures its ecosystem's hygiene needs as
pure `&'static` data — `Copy`, no parser, no runtime allocation. The struct has
four channels:

| Channel | Type | Purpose |
|---|---|---|
| `prune_dirs` | `&'static [&'static str]` | Directory **base names** to skip entirely during the walk (no recursion, no read). Literals only — globs never prune dirs. |
| `exclude_file_globs` | `&'static [&'static str]` | Filename globs (and literal filenames) to exclude as file noise. Never prunes a directory. |
| `test_file_globs` | `&'static [&'static str]` | Name-based test-file patterns applied by the **frontend** (not the walk) → `skipped_tests`. |
| `prune_marker_files` | `&'static [&'static str]` | Content-marker filenames: a directory that **contains** one of these is pruned at walk-entry time. |

### Two-phase split

Corpus hygiene splits cleanly across two phases:

1. **Name-based — profile channels, applied by the walk and the frontend.** The
   CLI unions `prune_dirs` and `exclude_file_globs` from all enabled frontends (plus
   `CorpusProfile::COMMON`) and builds a `CorpusMatcher` for the walk. The frontend
   applies `test_file_globs` to route whole files into `skipped_tests` before scoring.
   The CLI unions `prune_marker_files` to drive the content-marker prune.

2. **Source-based — applied inside `analyze_unit`.** Rust's `#[test]` / `#[cfg(test)]`
   detection happens at the AST level inside `detect::analyze_unit`, after a file has
   been read and parsed. Python also has a source-based layer for individual test
   methods inside non-test-named files (see *Per-language differences* below). These
   are orthogonal to the name-based profile channels.

## Per-language table

Values below are copied from the actual `CORPUS_PROFILE` constants and verified
against the source. **If this table ever disagrees with the const, the const wins.**

### `CorpusProfile::COMMON` (language-neutral baseline, owned by no frontend)

Included in every build regardless of enabled features. VCS metadata only.

| Channel | Entries |
|---|---|
| `prune_dirs` | `.git` |
| `exclude_file_globs` | *(empty)* |
| `test_file_globs` | *(empty)* |
| `prune_marker_files` | *(empty)* |

### Rust frontend (`fxrank-lang-rust`)

| Channel | Entries |
|---|---|
| `prune_dirs` | `target` |
| `exclude_file_globs` | *(empty)* |
| `test_file_globs` | *(empty — tests are source-based; see below)* |
| `prune_marker_files` | *(empty)* |

### TypeScript/JavaScript frontend (`fxrank-lang-ts`)

| Channel | Entries |
|---|---|
| `prune_dirs` | `node_modules`, `__mocks__` |
| `exclude_file_globs` | `*.min.js`, `*.min.mjs`, `*.min.cjs`, `*.stories.*`, `mockServiceWorker.js`, `jest.setup.*`, `jest.config.*` |
| `test_file_globs` | `*.test.*`, `*.spec.*`, `__tests__` |
| `prune_marker_files` | *(empty)* |

Note: `__mocks__` is in `prune_dirs` (not `exclude_file_globs`) because it is a
directory name. Under the flat union a literal in either channel behaves identically
(prune + exclude), but keeping channel semantics honest means `exclude_file_globs`
never holds a real directory name.

### Python frontend (`fxrank-lang-python`)

| Channel | Entries |
|---|---|
| `prune_dirs` | `.venv`, `venv`, `.tox`, `.nox`, `__pycache__`, `.eggs`, `build`, `dist`, `.mypy_cache`, `.pytest_cache`, `.ruff_cache`, `site-packages` |
| `exclude_file_globs` | `*_pb2.py`, `*_pb2_grpc.py` |
| `test_file_globs` | `test_*.py`, `*_test.py`, `conftest.py`, `tests` |
| `prune_marker_files` | `pyvenv.cfg` |

`pyvenv.cfg` catches arbitrarily-named virtual environments (`.env3/`, `myenv/`,
etc.) that `prune_dirs` cannot enumerate. A directory containing `pyvenv.cfg` is
pruned at walk-entry time before any of its contents are read.

## Honest per-language differences (intentional — not aligned)

- **Rust tests are source-based; `test_file_globs` is empty.** Rust projects put
  unit tests in the same files as production code (`#[test]`, `#[cfg(test)]`). There
  is no per-file naming convention to skip. Test detection happens inside
  `detect::analyze_unit` and `functions::collect` via attribute inspection. In
  contrast, TS and Python use separate test files and can skip them by name.

- **Python has dual test skipping.** The Python frontend applies both a name-based
  path skip (from `test_file_globs` via `is_test_file`) *and* a source-based skip for
  individual test functions/methods inside non-test-named files (methods of `Test*`
  classes and `unittest.TestCase` subclasses, `test_*`-named functions). Rust and TS
  do not have a comparable second layer.

- **`.git` is a common baseline, owned by no frontend.** The `CorpusProfile::COMMON`
  constant is always included in the CLI union. No individual frontend claims it —
  `.git` is language-neutral VCS metadata.

- **Union follows the compiled-in feature set.** A TS-only binary (`--features ts`)
  does not include Rust's `target` or Python's `.venv` / `pyvenv.cfg`. The default
  skip list adapts to what the binary was built for: only the profiles of enabled
  frontends are unioned, plus `COMMON`.

- **`pyvenv.cfg` marker prune is independent of `--exclude` and always on when the
  `python` feature is compiled in.** (It comes from Python's `prune_marker_files`, so a
  slim `--features ts` build has no marker prune — `default_prune_markers()` is empty.)
  The content-marker prune runs separately from the `CorpusMatcher`; passing `--exclude`
  does not disable it (see *CLI behavior* below).

## CLI behavior

### Default union

When `--exclude` is absent, the CLI calls `default_exclude_entries()`, which:
1. Starts with `CorpusProfile::COMMON`.
2. Appends each enabled frontend's `CORPUS_PROFILE` (conditional on compiled-in features).
3. Unions `prune_dirs` and `exclude_file_globs` from all profiles into one flat entry list.
4. Sorts and deduplicates the list.
5. Builds a `CorpusMatcher` from the result.

The `default_prune_markers()` function performs the same union over `prune_marker_files`
and is always built independently of `--exclude`.

### `--exclude` replaces the glob list (not the marker prune)

Passing `--exclude a,b,c` replaces `default_exclude_entries()` entirely — the resulting
`CorpusMatcher` is built from the user's entries only. However, **the content-marker
prune (`default_prune_markers()`) is always-on and unaffected by `--exclude`**: a
directory containing `pyvenv.cfg` is still pruned even when `--exclude` is given.

### `CorpusMatcher` — the spec-004 three-class matcher

Lives in `fxrank_core::corpus::CorpusMatcher`. Entries are classified by shape:

| Entry shape | Applies to directories | Applies to files |
|---|---|---|
| No `/`, no glob meta (literal) — e.g. `target`, `__mocks__` | Prunes matching dir (no read, uncounted) | Excludes matching file (counted in `skipped_excluded`) |
| No `/`, has glob meta — e.g. `*.min.js`, `*.stories.*` | Never prunes | Excludes matching file (counted in `skipped_excluded`) |
| Contains `/` — e.g. `src/gen/**` | Never prunes | Excludes file whose root-relative `/`-normalized path matches (counted in `skipped_excluded`) |

### `skipped_excluded` vs `skipped_tests` routing

- **`skipped_excluded`** — files (and only files) skipped by the `CorpusMatcher` during
  the walk. Directory prunes are not counted (never read). Content-marker prunes are not
  counted (entire subtree never entered).
- **`skipped_tests`** — function units skipped by the frontend's test-skip mechanism
  (source-based Rust `#[test]` detection; name-based TS/Python file skip via
  `test_file_globs`; Python's additional source-based unit skip).

If a path matches both an exclude entry and a test-file pattern, the discovery exclude
wins (it runs first): the file is counted in `skipped_excluded` and never reaches the
frontend's test-skip.

### `--exclude` applies to directory scans only

A single explicitly named file (`fxrank scan a.min.js`) and stdin (`scan --lang ts -`)
are always scanned. Exclusion is a directory-walk concern; the file and stdin branches
never consult the matcher.

## Per-frontend realization

- **Rust** — `CORPUS_PROFILE.test_file_globs` is empty; test detection is source-based
  (`#[test]`/`#[bench]` attributes + `#[cfg(test)]` module items). The `target/`
  build-artifact directory is the only prune entry. No prune markers.

- **TypeScript/JavaScript** — `test_file_globs` drives `is_test_file` via a lazily-built
  `CorpusMatcher` (segment matching enabled so `__tests__` matches any path segment).
  Named minified bundles, Storybook stories, the MSW worker, and Jest support files are
  in `exclude_file_globs`. `__mocks__` is a dir prune.

- **Python** — `test_file_globs` drives `is_test_file` for the path-based skip (segment
  matching for `tests`). `pyvenv.cfg` in `prune_marker_files` catches renamed venvs.
  Generated protobuf files (`*_pb2.py`, `*_pb2_grpc.py`) are in `exclude_file_globs`.
  Multiple build/cache artifact directories are in `prune_dirs`.
