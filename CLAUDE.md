# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**FxRank** is an *effect-cost profiler for coding agents*. `fxrank scan <path>` analyzes
Rust and TypeScript/JavaScript source and emits compact JSON ranking each function by its **own-body effect cost**
(IO, mutation, panic, risk, …), so an agent can find hotspots and refactor toward purer
cores. It is a *measuring instrument* — it reports facts (effect kind, severity class,
discount rationale, evidence, confidence, risk), and deliberately gives **no refactoring
advice**.

The differentiator from a naïve purity checker is the **containment discount**: Rust's
`&mut` / `&self` / ownership make some effects *declared and bounded*, so they score
*lower*; conversely `&self` + interior mutability is *hidden* and scores *higher* than an
honest `&mut self`. Validated end-to-end (and by dogfooding — see below).

Milestone A ships two frontends — Rust (`syn`) and TS/JS (`swc`) — both **primarily
syntactic** (no borrow-checker / type inference); type-dependent signals are heuristic and
carry a confidence penalty.

## Workspace layout

A Cargo workspace, one shipped binary, language frontends feature-gated:

- **`crates/fxrank-core`** — language-neutral scoring model. **Depends on no parser** (the
  compiler enforces this; `syn` must never leak here). Holds: `effect` (kind/risk
  vocabulary + the `Effect`/`RiskFeature` wire types), `score` (Fibonacci weights, the
  containment discount, `own_score`, the integer `rank_key`), `confidence`, `model` (the
  JSON `Report`/`Scope`/`Hotspot`/`Summary` + `Report::build`), and the `Frontend` trait.
- **`crates/fxrank-lang-rust`** — the `syn`-based Rust frontend (behind the `rust`
  feature). `functions` (function-unit collection), `imports` (a `use` table), and
  `detect/{calls,macros,mutation,risk}` detectors orchestrated by `detect::analyze_unit`.
- **`crates/fxrank-lang-ts`** — the `swc`-based JS/TS frontend (behind the `ts` feature).
  `functions` (function-unit collection incl. arrows/methods/getters), `imports` (ES
  `import` + `require` table), `coverage` (signature typed-slot coverage for the boundary
  discount), and `detect/{calls,mutation,risk}` orchestrated by `detect::analyze_unit`.
  Parses with swc, runs no type-checker (syntactic, like the Rust frontend).
- **`crates/fxrank-cli`** (package/binary `fxrank`) — args (clap), file discovery,
  feature-gated dispatch, compact JSON to stdout.

## Commands

```bash
cargo build
cargo test --workspace                                   # all tests (~90)
cargo test -p fxrank-core own_score_damps_non_max_weights # one test by name
cargo fmt --check                                        # CI gate
cargo clippy --workspace --all-targets -- -D warnings    # CI gate — warnings are hard errors
cargo build -p fxrank --no-default-features --features rust  # slim Rust-only build
cargo build -p fxrank --no-default-features --features ts    # slim TS-only build
cargo run -p fxrank -- scan <path>                       # run the tool
cargo run -p fxrank -- scan crates/ | jq                 # dogfood on our own source
cargo run -p fxrank -- scan crates/ --include-tests      # score test code too (skipped by default)
cargo run -p fxrank -- scan <path> --exclude 'node_modules,*.min.js,*.stories.*'  # replaces the default skip list
echo 'function f(): void {}' | cargo run -p fxrank -- scan --lang ts -  # scan a TS fragment from stdin
cargo insta review                                       # accept snapshot test changes (insta)
cargo install fxrank                                     # install the released binary from crates.io
cargo install --git https://github.com/caasi/fxrank fxrank  # install the latest unreleased binary from git
```

CI (`.github/workflows/ci.yml`) gates `fmt --check`, `clippy --workspace --all-targets -D
warnings`, `test --workspace`, all slim builds (`--features rust`, `--features ts`,
no-features), a Rust dogfood `scan crates/`, and a TS dogfood scan over the committed
fixtures. Run the first three locally before pushing.

## Releasing to crates.io

The workspace publishes **four** crates; the binary `fxrank` depends on the three
library crates, so all four are published in dependency order. Shared package metadata
(`license = "MIT OR Apache-2.0"`, `repository`, `authors`, `rust-version`, `keywords`,
`categories`) lives in `[workspace.package]` and is inherited via `field.workspace =
true`; each crate sets its own `description`. Internal deps carry both `path` and
`version` so crates.io can resolve them. Bump `version` in `[workspace.package]` (one
place) for every release.

```bash
# one-time: cargo login <crates-token>   (or set CARGO_REGISTRY_TOKEN)
cargo publish -p fxrank-core
cargo publish -p fxrank-lang-rust
cargo publish -p fxrank-lang-ts
cargo publish -p fxrank            # the binary; depends on the three above
```

Validate `fxrank-core` first without uploading: `cargo publish -p fxrank-core
--dry-run`. The downstream crates **cannot** be dry-run-validated until `fxrank-core` is
on crates.io — their `version` dep resolves from the registry index, so packaging fails
with "no matching package" beforehand; they are verified by publishing in dependency
order. Publishes are **permanent** — a bad version can only be `cargo yank`ed, never
deleted.

## Architecture: how a scan flows

```
SourceFile(s) → RustFrontend::analyze (parse + per-file `use` table + static names)
  → functions::collect → FnUnit { symbol, id, path, line, sig, block }   (retains the body)
  → detect::analyze_unit(unit, imports, statics):
        gather  → calls/macros/mutation/risk detectors push Vec<Effect> (+ risks)
        fold    → own_score, max_class (incl. risk_class), risk_weight, confidence, async
  → FrontendOutput { functions: Vec<Hotspot>, module_risks, diagnostics }
CLI → core::Report::build(scope, hotspots, diagnostics, limit)  → compact JSON to stdout
```

- **Adding a detector** = one `effects.extend(<detector>::detect(...))` line in
  `detect/mod.rs::gather`. Each detector is a `syn::visit::Visit` walker following the
  `classify_* → push` shape (see `detect/calls.rs`); always call the default
  `syn::visit::visit_*` so nested expressions are still visited.
- **`detect::analyze_unit` is the single owner** of turning a function's effects/risks into
  a scored `Hotspot`. Detectors stay pure (return `Vec<Effect>`); assembly lives there.

## Conventions & non-obvious gotchas

- **`proc-macro2` needs the `span-locations` feature** (set in `fxrank-lang-rust/Cargo.toml`)
  or every `span.start().line` is `0`. **`syn` needs the `visit` feature** for the walkers.
  Both are load-bearing.
- **Wire-format decisions** (locked in the spec): `own_score` is an `f64`, so whole values
  render as `3.0` (not `3`). **Per-effect `confidence` is NOT serialized** — confidence is
  computed per detection but only surfaced at the function level (`hotspots[].confidence`,
  the weakest-link min). `effects[]` carry no `confidence` field.
- **Detectability tiers** — every signal is `exact` / `path` / `heuristic`. Anything that
  truly needs type info (interior mutability, `.lock()`/`.set()` method-name effects,
  `unwrap`/`expect`, `&mut` write-through) is `heuristic` and takes a confidence penalty.
  Don't claim a type-dependent signal is exact.
- **The containment discount** is a class down-shift, not point subtraction: `&mut param`
  shifts down 2, `&mut self` down 1, clamped at class 1, and **cancelled inside `unsafe`**.
  The discount touches only the mutation channel, never sibling effects.
- **Centralize new vocabulary**: add effect kinds to `EffectKind` and risk kinds to
  `RiskKind` (both have `wire()` + class) — never hand-write wire strings at call sites.
- **`--exclude` is a three-class matcher** (spec 004): a no-`/` literal prunes a
  matching directory and excludes a matching file; a no-`/` glob (`*.min.js`,
  `*.stories.*`) excludes files only (never prunes a same-named dir); a `/`-bearing
  glob filters files by relative path. It **replaces** the default list when given.
  The default skips vendored bundles, Storybook stories, `jest.setup`/`jest.config`,
  `__mocks__`, and the MSW worker. Files dropped this way are counted in
  `scope.skipped_excluded` (directory prunes are not counted — they are never read).
  `--exclude` applies only to directory scans; an explicitly named file/stdin is
  always scanned.
- Tests use a shared `analyze_fixture(name)` helper reading `tests/fixtures/*.rs`
  (subdir — cargo does not compile those as test targets). Snapshot tests use `insta`.

## Dogfooding caveat (real, from running `fxrank scan crates/`)

The tool correctly surfaces genuine production hotspots (`run_scan`, `walk_dir` — the IO
boundaries) with accurate evidence, and the discount correctly keeps the pervasive
`&mut self` visitor accumulation *low* (no false alarms). Test code is now **skipped by
default** (`#[test]`/`#[bench]` functions and `#[cfg(test)]` modules; pass `--include-tests`
to score it), so the old "`assert!`/`assert_eq!` register as `panic` and dominate raw
rankings" noise is gone for normal scans. One gap remains: a bare top-level
`#[cfg(test)] fn` (a helper *outside* a `#[cfg(test)] mod`) is still collected as a normal
function — see the TS dogfooding caveat below.

## Dogfooding the TS frontend (running `fxrank scan crates/fxrank-lang-ts/src/`)

Dogfooding the Rust frontend on the new TS-frontend Rust code validated the containment
discount on our own visitor pattern: every swc walker (`CallWalker`, `RiskWalker`,
`AnyBodyWalker`, `Collector`) lands at class 2 because their `&mut self` `param.mutation`
is correctly discounted — the pervasive visitor-accumulation pattern stays low with no
false alarms. The real IO boundaries (`run_scan`, `walk_dir`) surface at class 7 as
expected. Core scoring functions score near zero.

**Caveat surfaced:** standalone module-level `#[cfg(test)] fn` helpers are *not* skipped
by the Rust frontend's test detection — it skips `#[test]` functions and `#[cfg(test)]`
*modules*, but not bare `#[cfg(test)] fn` items, so they appeared as hotspots in the scan
output. Workaround: move test helpers inside the `#[cfg(test)] mod tests` block (done for
`imports::table` and `source::test_file`). Extending test-skip to bare `#[cfg(test)] fn`
is a Milestone-B candidate.

## Design artifacts & workflow

Specs live in `specs/`, implementation plans in `plans/`, with matching 3-digit prefixes
(`specs/001-*` ↔ `plans/001-*`). `specs/001-fxrank-rust-effect-scanner.md` is the
source of truth for every score, class, discount, and schema field — **read it before
changing scoring behavior**; when code and spec disagree, the spec wins. Its *Known
Limitations* section records the accepted deferrals (call-graph propagation /
`inherited_score`, FFI call-site detection, `global.mutation` class-4 downgrade, and a
full semantic/type-resolution pass).
