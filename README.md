# FxRank

**An effect-cost profiler for coding agents.**

`fxrank scan <path>` analyzes Rust, TypeScript/JavaScript, and Python source and emits
compact JSON ranking each function by its **effect cost** — how much IO, mutation, panic,
and risk it carries in its own body — so an agent (or a human) can find the hotspots
worth refactoring toward a purer core.

FxRank is a *measuring instrument*, not a linter. It reports facts — effect kind, severity
class, why a discount applied, the evidence, a confidence, and risk flags — and
deliberately offers **no refactoring advice**. The decision is yours.

## Why it's not just a purity checker

A binary "pure vs impure" label is too coarse to refactor against. FxRank gives a
**gradient**, and it understands that a language's type system makes some effects safer than
others:

- A **declared `&mut`** mutation is visible and bounded at the call site, so it is
  **discounted** — it scores *lower*.
- A **`&self` method that mutates through interior mutability** (`RefCell`, `Cell`,
  `Mutex`, atomics) is **hidden** from the signature, so it scores *higher*.

So FxRank **inverts** a naïve checker: the honest `&mut self` mutation ranks *below* the
sneaky `&self` + `borrow_mut()` one. That anti-Goodhart inversion is the whole thesis. The
TS/JS frontend applies an analogous **boundary discount** driven by how much of a
function's signature is typed — an `any` at the boundary poisons it.

All three frontends — Rust, TS/JS, and Python — classify mutation against **one canonical
model**: the same effect kinds and classes, the same anti-Goodhart inversion, and a shared
`hidden.mutation` subreason vocabulary, while keeping each language's honest differences. One
rule that holds across all three: a write to a **module top-level binding** (a Rust `static`, a
TS module `const`/`let`/`var`/`fn`/`class`, a Python module-level name) is **`global.mutation`**
(class 6, "wild global") — the "module var used for cross-component communication" anti-pattern —
while a genuinely captured *enclosing-function* local stays `hidden.mutation`. The shared model
and the intentional per-language differences are documented in
[`docs/mutation-classification-guideline.md`](docs/mutation-classification-guideline.md).

## Install

Requires a stable Rust toolchain (edition 2024, Rust ≥ 1.85). If you don't have one,
install it with [rustup](https://rustup.rs).

**Install the binary** (recommended — puts `fxrank` on your `PATH` at `~/.cargo/bin`):

```bash
cargo install fxrank
```

Re-run with `cargo install fxrank --force` to update; `cargo uninstall fxrank` removes it.

By default the binary ships **all three** frontends (Rust + TS/JS + Python). For a slimmer
build, install just one:

```bash
cargo install fxrank --no-default-features --features rust    # Rust only
cargo install fxrank --no-default-features --features ts      # TS/JS only
cargo install fxrank --no-default-features --features python  # Python only
```

To install the latest unreleased version straight from git:

```bash
cargo install --git https://github.com/caasi/fxrank fxrank
```

**Or build from a clone** (for development):

```bash
git clone https://github.com/caasi/fxrank
cd fxrank
cargo build --release        # binary at target/release/fxrank
```

## Usage

```bash
fxrank scan src/                 # scan a directory (recurses by extension, symlink-safe)
fxrank scan src/lib.rs           # scan one Rust file
fxrank scan app/                 # .rs → Rust; .ts/.tsx/.js/.jsx → TS/JS; .py → Python
fxrank scan src/ --limit 20      # keep only the top-20 hotspots
cat foo.rs | fxrank scan         # read Rust from stdin
cat foo.ts | fxrank scan --lang ts      # read TS/JS from stdin (--lang: ts, tsx, js, jsx)
cat foo.py | fxrank scan --lang python  # read Python from stdin
```

The frontend is chosen by **file extension** for paths; `--lang` selects it for stdin.
Other flags:

- `--include-tests` — test code is **excluded by default** (`#[test]`/`#[bench]` and
  `#[cfg(test)]` modules for Rust; `*.test.*` / `*.spec.*` / `__tests__` paths for TS/JS;
  `test_*.py` / `*_test.py` / `conftest.py` files and `tests/` directory segments for
  Python, plus source-based skipping of `test_*` functions, `Test*`-named class methods,
  and `unittest.TestCase` subclass methods). Pass this to score tests too.
- `--exclude a,b,c` — comma-separated patterns to skip during directory scans
  (**replaces** the default list when given). Each entry is classified by whether it
  contains a `/`:
  - **No `/`, no wildcard** (`node_modules`, `__mocks__`, `jest.setup.js`): prunes a
    matching directory **and** excludes a matching file by base name.
  - **No `/`, has a wildcard** (`*.min.js`, `*.stories.*`, `jest.config.*`): excludes
    matching **files** only — never prunes a directory.
  - **Contains `/`** (`src/legacy/**`, `packages/*/generated/**`): a segment-aware
    path glob matched against the file's root-relative path (`*` stays within one
    segment; `**` crosses `/`). File filter only — does not prune the directory.

  Files skipped by any pattern are counted in `scope.skipped_excluded` (directory
  prunes are not counted). `--exclude` applies to directory scans only; an explicitly
  named file or stdin is always scanned.

  Default (JS/TS + Python corpus hygiene): `node_modules,.git,target,*.min.js,*.min.mjs,*.min.cjs,*.stories.*,mockServiceWorker.js,jest.setup.*,jest.config.*,__mocks__,.venv,venv,.tox,.nox,__pycache__,.eggs,build,dist,.mypy_cache,.pytest_cache,.ruff_cache,site-packages,*_pb2.py,*_pb2_grpc.py`

Output is **compact JSON on stdout** (built for agents — pipe through `jq` to read it):

```jsonc
{
  "scope":   { "input": "src", "files": 6, "parsed": 6, "functions": 37, "skipped_tests": 0, "skipped_excluded": 0, "risk_features": [] },
  "summary": { "own_score": 42.5, "max_class": 7, "risk_weight": 0, "confidence": 0.6 },
  "hotspots": [
    {
      "id": "src/main.rs:48:4:run_scan",
      "symbol": "run_scan",
      "max_class": 7, "own_score": 42.5, "confidence": 0.6,
      "effects": [
        { "kind": "net.fs.db", "class": 7, "line": 72, "tier": "path",
          "evidence": "std::fs::read_to_string" },
        { "kind": "net.fs.db", "class": 7, "line": 56, "tier": "path",
          "evidence": "std::io::stdin" },
        { "kind": "net.fs.db", "class": 7, "line": 56, "tier": "heuristic",
          "evidence": ".read_to_string" },
        { "kind": "local.mutation", "class": 1, "line": 94, "tier": "exact",
          "evidence": "write to local all_diagnostics" }
      ]
    }
  ],
  "diagnostics": []
}
```

*(This is FxRank scanning its own CLI: `run_scan` is correctly flagged as the top hotspot
for mixing stdin/file IO with diagnostic accumulation — a real "extract the pure
report-building from the IO boundary" candidate.)*

Each hotspot `id` (`path:line:col:symbol`) is a **unique, addressable key** within a
report — even two anonymous arrows on the same line get distinct ids (`col` is the
1-based character column). Because it encodes position, an `id` changes when the
function moves, so it identifies a function *within a scan*, not across edits. Treat the
`id` as opaque: each hotspot also emits `path`, `line`, and `symbol` as their own
top-level fields (trimmed from the abbreviated example above) — read those rather than
splitting the `id` string.

## Using it well (the lab protocol)

FxRank is a precision instrument, not a crawler — **don't point it at a whole repo
blindly.** It only makes sense on hand-written, unminified source; the scores are
**meaningless on minified, generated, or vendored code**. The reliable way to use it:

0. **Discover first.** Map the repo before measuring: find the hand-written source, and
   identify what's *not* it — vendored / `third_party`, build output (`dist`, `build`),
   generated files, minified bundles, test scaffolding. That map decides what to scan.
1. **Scan the source.** Point FxRank at the real source dirs (or scan from the root and
   let `--exclude` drop the noise). **Never aim it at minified or generated code** — a
   minified file named directly is still scanned (`--exclude` is a no-op for an explicit
   file), and its scores are garbage.
2. **Verify, don't trust.** Open the top hotspots and confirm them against the source —
   true *and* false positives. The JSON is a measurement, not a verdict.
3. **Separate noise from signal.** Vendored / minified / test-scaffold / stories aren't
   refactor targets. The defaults skip the common ones; anything that slips through (e.g.
   an unnamed bundle like `swagger-ui.js`) you catch here.
4. **Re-run with excludes for a clean list.** `--exclude` **replaces** the default list,
   so restate the defaults and append the repo's own noise. Use literal directory names
   for cheap prunes (`dist`, `build`, `third_party`) and literal filenames or globs for
   files (`swagger-ui.js`, `*.generated.ts`):

   ```bash
   fxrank scan . --exclude 'node_modules,.git,target,*.min.js,*.min.mjs,*.min.cjs,*.stories.*,mockServiceWorker.js,jest.setup.*,jest.config.*,__mocks__,dist,build,third_party,swagger-ui.js'
   ```

   If the cleaned top results still include generated, vendored, or minified paths, update
   the exclude list and rerun before choosing refactor targets.
5. **Pick refactor targets.** Use the clean ranking to choose what's worth refactoring
   toward a purer core.

## The scoring model, briefly

- **Severity class `0..=8`** rates the *kind* of effect (`0` pure … `7` net/fs/db …), each
  mapped to a convex **Fibonacci weight** so one real IO boundary outweighs a pile of
  trivial effects.
- **`own_score`** = `max_weight + 0.5 · Σ(other weights)`; functions are ranked by
  `(max_class, own_score, risk_weight, confidence)` — so a single class-7 IO always
  outranks any amount of class-4 logging.
- **Risk features** (`unsafe`, `transmute`, raw-pointer ops, `mem::forget`, …) carry their
  own class so a risk-only function isn't ranked as cheap.
- **Confidence** reflects how much a signal relied on syntax-only heuristics (FxRank does
  no type inference); the function-level value is the weakest link.

The full spec lives in [`docs/superpowers/specs/001-fxrank-rust-effect-scanner.md`](docs/superpowers/specs/001-fxrank-rust-effect-scanner.md).

## Status & roadmap

**Milestone A:** a primarily-syntactic analyzer — effect & risk detection, the containment
discount, the hidden-mutation inversion, async/confidence metadata, diagnostics, and the
`fxrank scan` CLI. Ships **three frontends**: Rust (`syn`), TypeScript/JavaScript (`swc`),
and Python (`libcst`), each syntactic (no type-checker or borrow-checker). Mutation
classification is **aligned across all three frontends** (spec 008, extended by #29): real
`static`/import facts are threaded into detection, and captured/unresolved, global,
constructor-init, and **module top-level binding** writes are classified consistently
(the last → `global.mutation`/6 in every frontend) — see the guideline above.

**Milestone B — cross-file propagation (spec 025, all three frontends):** a scan now resolves
calls across the scanned files and folds **escaping** effects along the call graph, so each
function gets a `propagated_score` (its effect blast-radius) alongside `own_score` — closing the
extract-method laundering hole. Output also gains `root` (the files you named explicitly on the
CLI — your observation focus; directory-walked files are context, not roots), `inherited[]`
provenance, and `scope.external_reaches[]` (the app's outward dependency surface, split
`FirstPartyOutOfScope` vs `ThirdParty`); import-time top-level code is scored as a `<module>` unit.
Third-party boundaries stay opaque (class-2 `external.unresolved`), not followed. `--no-resolve`
turns the pass off (own scores only). Own-body **scores** are unchanged; the wire format gains the
propagation fields above, plus a `col` on each `effect`/`risk` (effect-location precision) — so a
cross-version JSON diff will show those additions.

Known limitations (accepted): the cross-file resolver is **name-based** — a qualified call can
false-resolve to a lone same-named local (precise module-tree resolution deferred → #36);
type-dependent signals are heuristic; macro-generated effects are invisible to `syn`. Test code is
skipped by default, but a bare top-level `#[cfg(test)] fn` is not yet detected as test.

**Next:** module-tree precise resolution (#36, also unlocks Rust public-API roots), the React
fold retrofit (#37), and a `lower-effect-score` agent skill (the "lab protocol" for using FxRank
safely). Research directions in #4 (protocol axis, evidence-mass confidence, boundary-slice delta).
