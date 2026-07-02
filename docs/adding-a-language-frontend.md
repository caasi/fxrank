# Adding a language frontend (authoring guide)

This is the prescriptive companion to the three descriptive shared-knowledge models. It
enumerates every decision a new frontend must make and points each at the doc or code that
governs it. **You should not need to read any `docs/superpowers/specs/*` to build a frontend** —
the specs are history; this guide + the guidelines + the code (`fxrank-core`) are the live
contract. Where this guide and the code disagree, the code wins — tell us so we fix the guide.

Work an existing frontend beside you as a worked example: **Rust**
(`crates/fxrank-lang-rust`) is the smallest (syntactic; uses the mut-channel discount but
no coverage-based boundary discount, hence no `coverage.rs`);
**Python** (`crates/fxrank-lang-python`) is the fullest (coverage, module-init, dual
test-skip); **Shell** (`crates/fxrank-lang-shell`) is the contrast case for the boundary
discount — an untyped language with **no signature to measure coverage over at all**, so
it applies `apply_boundary_discount(_, BoundaryCoverage::None, contained)` unconditionally
(a permanent no-op shift-0, not a computed floor) and ships no `coverage.rs`. It's also the
example for "no import syntax, no `ImportTable`, same-file-only cross-file resolution" (see
the cross-file guideline's *Per-frontend realization*) and for a corpus profile with only
`test_file_globs` populated (no vendor dir, no venv-style marker). Mirror the one whose
language shape is closest to yours.

## 0. The one invariant that must hold

FxRank's whole thesis is an **anti-Goodhart inversion**: *hidden* state scores **higher**
than *honestly declared* state. Concretely, a hidden interior-mutability write
(`hidden.mutation` / class 3, no discount) must out-rank an honest declared `&mut self`
mutation (`param.mutation` / class 3 discounted to 2; a bare `&mut param` discounts further,
to 1). If your frontend ever lets a language's
"declare your effects" construct score *above* its "hide your effects" construct, the
frontend is wrong regardless of what any test says. Every per-axis decision below serves
this invariant. (See the mutation guideline's *The differentiator*.)

FxRank is a **measuring instrument**: report facts (effect kind, class, evidence,
confidence, containment), never refactoring advice. It is **primarily syntactic** — no type
checker, no borrow checker. Anything that truly needs type/flow info is tier `heuristic`
and takes a confidence penalty; never claim a type-dependent signal is `exact`.

## 1. The contract: what a frontend *is*

One crate `crates/fxrank-lang-<lang>` behind a Cargo feature, exposing:

- `pub const CORPUS_PROFILE: CorpusProfile` — your ecosystem's hygiene declaration.
- a type implementing `fxrank_core::frontend::Frontend`:

```rust
pub trait Frontend {
    fn language(&self) -> Language;
    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput;
    fn corpus_profile(&self) -> CorpusProfile { CorpusProfile::EMPTY } // override with your const
}
```

`analyze` must **never panic** — an un-parseable file becomes a `Diagnostic { parsed:
false, .. }`, not a crash. It returns a `FrontendOutput`:

| field | what you fill it with |
|---|---|
| `functions: Vec<Hotspot>` | one scored hotspot per function unit (skip test units unless `include_tests`) |
| `records: Vec<UnitRecord>` | one neutral record per same unit — the cross-file fold's input |
| `module_risks: Vec<RiskFeature>` | module-level risks (e.g. `impl Drop`, `extern`) |
| `diagnostics: Vec<Diagnostic>` | one per file that failed to parse |
| `skipped_tests: usize` | count of units your test-skip dropped |

`fxrank-core` stays **parser-free** — your parser (`syn`/`swc`/`libcst`/…) lives only in
your crate and must never leak into core.

## 2. Crate skeleton (mirror the siblings)

```
crates/fxrank-lang-<lang>/src/
  lib.rs            CORPUS_PROFILE + the Frontend impl (the analyze loop below)
  functions.rs      collect function units (fn/method/closure/lambda) — retains each body
  imports.rs        ImportTable: local name → module string (+ dynamic flag, module bindings)
  coverage.rs       signature typed-slot coverage → boundary discount  (typed langs only; omit for untyped)
  module_map.rs     module-string → in-scope file resolution (Rust: module_tree.rs)
  detect/
    mod.rs          analyze_unit (own owner of unit→Hotspot) + gather + build_record
    calls.rs        call/IO/world-effect detector
    mutation.rs     write-site → mutation effect       → mutation-classification-guideline.md
    risk.rs         risk-feature detector (+ module-level risks)
    refs.rs         outgoing CallSiteRefs for the fold  → cross-file-resolution-guideline.md
```

The `analyze` loop is similar in shape across the frontends — Rust and Python are the
closest (see `crates/fxrank-lang-rust/src/lib.rs`): for each `SourceFile` — parse (→
diagnostic on error), build the `ImportTable` and module-binding set, `functions::collect`,
then per unit call `detect::analyze_unit(unit, imports, bindings) -> Hotspot` **and**
`detect::build_record(unit, …) -> UnitRecord`, routing test units to `skipped_tests`.
(Argument lists here are illustrative — mirror the sibling's exact signature, which threads
a few more inputs such as the static set and the `include_tests` flag.) **The TS frontend
deliberately diverges**: it runs a React two-pass (`analyze_units`) and emits records via
`record_from_hotspot` instead of `build_record`. Mirror Rust/Python unless your language
genuinely needs a comparable second pass.

**Detectors stay pure** (each returns `Vec<Effect>` / `Vec<RiskFeature>`); `analyze_unit`
is the *single owner* that folds effects+risks into a scored `Hotspot`. Adding a detector =
one `effects.extend(<detector>::detect(...))` line in `gather`. Never hand-write wire
strings — add kinds to `EffectKind`/`RiskKind` in core (each has `wire()` + a class).

## 3. Per-axis decisions (each → its governing doc)

| Axis | Decision you make | Governed by |
|---|---|---|
| **Effect vocabulary** | Which world effects (IO/net/panic — never discountable) vs state effects (mutation — boundary-containable) your language exposes; map each to an existing `EffectKind` or add one to core | `fxrank_core::effect` + spec 003/006 thesis |
| **Mutation model** | Fill a new column in the canonical mapping: local/param/this/global/hidden mutation, `contained`/`hidden` flags, subreason vocab (`interior-mut`/`captured-binding`/`ref-cell-write`). Collect your module-binding set for `global.mutation` | **`mutation-classification-guideline.md`** |
| **Boundary / containment discount** | Typed langs: a `coverage.rs` measuring signature typed-slot coverage → `apply_boundary_discount` (floor 0 for contained). Rust-style ownership langs: a mut-channel `apply_discount` instead. State *which* your language uses and why | `fxrank_core::score` (`apply_discount` vs `apply_boundary_discount`) |
| **Corpus profile** | Your `CORPUS_PROFILE` four channels (`prune_dirs`, `exclude_file_globs`, `test_file_globs`, `prune_marker_files`); whether test-skip is name-based, source-based (AST), or both | **`corpus-profile-guideline.md`** |
| **Imports & bindings** | `ImportTable` (local name → module string, dynamic flag) and the module-level binding set | cross-file guideline *The floor* |
| **Cross-file refs** | Per `CallSiteRef`: set `qualified` (is this a qualified outward reference?) and `first_party` (in-codebase but maybe out-of-scope?). Core filters on these bools — no language syntax in core | **`cross-file-resolution-guideline.md`** (*What becomes a reach*, first/third-party classifier) |
| **Module-init units** | If your language runs code at import time (top-level statements, decorators, default args, class bodies), synthesize one `<module>` unit per module. Rust rarely needs this (const-eval at compile time) | cross-file guideline *Module-init units* |
| **Roots** | Nothing — frontends always emit `is_root: false`; the CLI sets roots from explicit FILE args | cross-file guideline *Roots* |

For each axis, add your language as a new **column** in that guideline's per-language table
and a **bullet** under its *Honest per-language differences* / *Per-frontend realization* —
and preserve any *intentional* difference already recorded there rather than "aligning" it.

## 4. The scoring model (no spec needed)

Normative source: `crates/fxrank-core/src/score.rs`. You emit effects/risks with a
**class**; core does the arithmetic — you never compute a final score by hand.

- **Fibonacci class weights**: `CLASS_WEIGHTS = [0,1,2,3,5,8,13,21,34]` (class 0..8).
- **`own_score = max_weight + 0.5 × Σ(rest)`** — the top effect dominates, siblings damp.
- **`max_class`** = the join (max) over effect classes and the risk class.
- **`rank_key`** — the composite sort key core uses to rank hotspots: a tuple
  `(max_class, own_score×2 rounded, risk_weight, confidence×100 rounded)` of type
  `(u8, u64, u32, u32)`, ordered lexicographically — not a single scalar.
- **Discounts are a class down-shift, not point subtraction**, and touch **only the
  mutation channel**: `apply_discount` (Rust `&mut` −2 / `&mut self` −1, floor 1, cancelled
  inside `unsafe`) vs `apply_boundary_discount` (typed langs, floor 0 for `contained`).
- **`Hotspot.id` is `path:line:col:symbol`** — an opaque within-report key; `col` is the
  1-based char column of the name anchor. Anonymous units get a `C{col}` suffix. Don't
  reconstruct coordinates by splitting the id (both `path` and `symbol` can contain `:`);
  read the structured fields. `Effect`/`RiskFeature` carry `col` too (site identity for the fold).
- **Per-effect confidence is not serialized** — it's computed per detection but surfaced
  only at the function level (`hotspots[].confidence`, weakest-link min).

## 5. Wiring it into the tool

1. **Workspace** — add `crates/fxrank-lang-<lang>` to `members` in the root `Cargo.toml`.
2. **Core** — add a `Language::<Lang>` variant (`fxrank-core/src/frontend.rs`).
3. **CLI feature** — in `crates/fxrank-cli/Cargo.toml`: an optional dep and a feature
   `<lang> = ["dep:fxrank-lang-<lang>"]`; include it in the default feature set.
4. **CLI dispatch** (`crates/fxrank-cli/src/main.rs`):
   - add a `Route::<Lang>` variant to the CLI-local `enum Route` (distinct from
     `Language`), and map your extension(s) to it in `route_for_path`.
   - `--lang <lang>` — accept your language for the stdin path (`scan --lang <lang> -`);
     also extend the CLI `about`/`--lang` help strings that list the languages.
   - a `#[cfg(feature = "<lang>")] fn dispatch_<lang>(…)` (+ a `#[cfg(not)]` no-op stub)
     with a matching arm in `dispatch`, and push your `CORPUS_PROFILE` in
     `default_corpus_profiles`.
   - carry the `--include-tests` toggle as a `pub include_tests: bool` field on your
     frontend struct (it is **not** an `analyze` argument), which `dispatch_<lang>`
     constructs: `<Lang>Frontend { include_tests }.analyze(&sources)`.
5. **CI** — add your slim-build line
   (`cargo build -p fxrank --no-default-features --features <lang>`) and a dogfood-scan
   line to `.github/workflows/ci.yml`, mirroring the existing per-language entries;
   otherwise CI never exercises your frontend.
6. **Publishing** — the workspace publishes every crate in dependency order; bump `version`
   in both `[workspace.package]` and the internal dep pins per the release notes in
   `CLAUDE.md` (*Releasing to crates.io*).

## 6. The verification bar (small loop before you ever ask for review)

- **Fixtures** — put sample sources under `tests/fixtures/` (a subdir cargo won't compile
  as test targets) and drive them through a fixture-reading test helper (name/signature
  varies per crate: `analyze_fixture` in `fxrank-lang-rust`, `analyze_fixture_unit` in
  `fxrank-lang-ts`, `analyze_fixture_as` in `fxrank-lang-python`). Use **`insta`** snapshots
  for output shape (`cargo insta review` to accept).
- **RED→GREEN** — write the failing test first for each detector/decision (TDD).
- **Gates that must pass** (CI enforces all): `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and
  the **slim build** `cargo build -p fxrank --no-default-features --features <lang>`.
- **Dogfood** — run `cargo run -p fxrank -- scan <a real codebase in your language>` and
  sanity-check the top hotspots by hand. The IO boundaries should surface high; the
  pervasive accumulate-into-`&mut self`/receiver pattern should stay *low* (that's the
  discount working). Record any intentional deltas or caught false-positives the way
  `008-dogfood-deltas.md` did.

## 7. When you genuinely need the history

Everything above is enough to build and ship. **Reach for a spec only when the shared
knowledge contradicts itself, or contradicts the code** — the spec is the tie-breaker of
record. (When you find such a contradiction, fix the guideline so the next author doesn't
have to make the same trip.) The archive: 001 (scoring model), 003 (TS / the typed-boundary
thesis), 006 (Python), 008 (mutation alignment), 025-series (cross-file resolution +
propagation). Read the one relevant section — never the whole tree.
