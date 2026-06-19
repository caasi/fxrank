# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**FxRank** is an *effect-cost profiler for coding agents*. `fxrank scan <path>` analyzes
Rust source and emits compact JSON ranking each function by its **own-body effect cost**
(IO, mutation, panic, risk, …), so an agent can find hotspots and refactor toward purer
cores. It is a *measuring instrument* — it reports facts (effect kind, severity class,
discount rationale, evidence, confidence, risk), and deliberately gives **no refactoring
advice**.

The differentiator from a naïve purity checker is the **containment discount**: Rust's
`&mut` / `&self` / ownership make some effects *declared and bounded*, so they score
*lower*; conversely `&self` + interior mutability is *hidden* and scores *higher* than an
honest `&mut self`. Validated end-to-end (and by dogfooding — see below).

Milestone A is Rust-only and **primarily syntactic** (`syn`, no borrow-checker / type
inference); type-dependent signals are heuristic and carry a confidence penalty.

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
echo 'function f(): void {}' | cargo run -p fxrank -- scan --lang ts -  # scan a TS fragment from stdin
cargo insta review                                       # accept snapshot test changes (insta)
```

CI (`.github/workflows/ci.yml`) gates `fmt --check`, `clippy --workspace --all-targets -D
warnings`, `test --workspace`, all slim builds (`--features rust`, `--features ts`,
no-features), a Rust dogfood `scan crates/`, and a TS dogfood scan over the committed
fixtures. Run the first three locally before pushing.

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
- Tests use a shared `analyze_fixture(name)` helper reading `tests/fixtures/*.rs`
  (subdir — cargo does not compile those as test targets). Snapshot tests use `insta`.

## Dogfooding caveat (real, from running `fxrank scan crates/`)

The tool correctly surfaces genuine production hotspots (`run_scan`, `walk_dir` — the IO
boundaries) with accurate evidence, and the discount correctly keeps the pervasive
`&mut self` visitor accumulation *low* (no false alarms). **But scanning `src/` also scans
inline `#[cfg(test)]` modules, where `assert!`/`assert_eq!` register as `panic` effects and
dominate raw rankings.** When using the tool to find smells, filter test functions (e.g.
drop hotspots whose only effect kind is `panic`), or scan non-test code. Skipping
`#[cfg(test)]` is a Milestone-B candidate.

## Design artifacts & workflow

Specs live in `specs/`, implementation plans in `plans/`, with matching 3-digit prefixes
(`specs/001-*` ↔ `plans/001-*`). `specs/001-fxrank-rust-effect-scanner.md` is the
source of truth for every score, class, discount, and schema field — **read it before
changing scoring behavior**; when code and spec disagree, the spec wins. Its *Known
Limitations* section records the accepted deferrals (call-graph propagation /
`inherited_score`, FFI call-site detection, `global.mutation` class-4 downgrade, and a
full semantic/type-resolution pass).
