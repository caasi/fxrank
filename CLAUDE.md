# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**FxRank** is an *effect-cost profiler for coding agents*. `fxrank scan <path>` analyzes
Rust, TypeScript/JavaScript, and Python source and emits compact JSON ranking each function by its **own-body effect cost**
(IO, mutation, panic, risk, …), so an agent can find hotspots and refactor toward purer
cores. It is a *measuring instrument* — it reports facts (effect kind, severity class,
discount rationale, evidence, confidence, risk), and deliberately gives **no refactoring
advice**.

The differentiator from a naïve purity checker is the **containment discount**: Rust's
`&mut` / `&self` / ownership make some effects *declared and bounded*, so they score
*lower*; conversely `&self` + interior mutability is *hidden* and scores *higher* than an
honest `&mut self`. Validated end-to-end (and by dogfooding — see below).

Milestone A ships three frontends — Rust (`syn`), TS/JS (`swc`), and Python (`libcst`) —
all **primarily syntactic** (no borrow-checker / type inference); type-dependent signals
are heuristic and carry a confidence penalty.

**Cross-file propagation (spec 025).** On top of own-body scoring, a scan now resolves calls
across the scanned files and folds **escaping** effects along the call graph, so a function's
`propagated_score` reflects the effect blast-radius of everything it (transitively) calls — closing
the extract-method score-washing hole (pushing IO into a callee no longer hides it from the caller).
The pipeline: each frontend emits language-neutral `UnitRecord`s (own effects + outgoing call refs,
tagged `qualified`/`first_party`); the CLI **partitions records per language**, builds a
`SymbolIndex` + `CallGraph`, and runs a Tarjan-SCC-condensed **fold** (`fxrank-core`, parser-free).
Output gains: `propagated_score`/`propagated_max_class` + an `inherited[]` provenance list
(`{kind, class, from, via}`) per hotspot; a `root: bool` annotation (the agent's observation
focus — set at the CLI from explicit FILE args; directory-walked files are context, not roots —
language-neutral, no program-entry heuristic); and `scope.external_reaches[]`
(`{specifier, kind, site}`, `kind` ∈ `FirstPartyOutOfScope`|`ThirdParty`) recording the app's
outward dependency surface. Third-party boundaries stay opaque (class-2 `external.unresolved`), not
followed. `--no-resolve` disables the whole pass (per-file own scores only). **Own-body output is
byte-identical to pre-025** — propagation only *adds* fields. Deferred: module-tree precise
resolution (#36), React-inheritance fold retrofit (#37); see spec 025 §13a.

## Public docs strategy (README & homepage) — keep them slim

The `README.md` and the GitHub Pages homepage are **public PR / marketing surfaces for fast
comprehension** (by both humans and coding agents), **not manuals**. They carry **durable facts
only**; everything that drifts is deferred to where it stays current. This is deliberate (issues
#12, #58) — the rationale is *cadence separation*: anything that restates the CLI surface (flags,
the output schema, exact invocations) goes stale by the next flag we add, so it must come from the
tool, not a hand-maintained page.

- **README keeps:** the one-line what, the thesis (the `&mut`/`&self` containment-discount
  inversion), a *minimal* install + a 3–4 line quick-start, the scoring model briefly, and a short
  status. Target ~90 lines.
- **README defers (do NOT re-add):** the exhaustive `--exclude` matcher rules + default list, the
  full output-JSON example / field-by-field schema, the per-language test-skip rules, and the
  install-variant matrix → **`fxrank scan --help`**; the step-by-step lab protocol → the planned
  **`lower-effect-score` skill**; cross-language mutation detail → the **mutation guideline**; full
  scoring/schema → **spec 001**; known-limitations + roadmap churn → the **issue tracker**.
- **Homepage** (orphan `gh-pages` branch, served at `https://caasi.github.io/fxrank/`; PRs target
  `gh-pages`) is the *more radical* version: durable *why* only, zero styling, pure semantic HTML
  that is machine-readable (a `<meta name="fxrank:*">` contract + JSON-LD + an embedded
  `fxrank-concept` JSON block). It defers **even install/usage** to the crate and `--help`.

**Future agents:** when updating these surfaces, resist re-bloating them. If you're tempted to
paste a flag table, a schema dump, or a roadmap section into the README, that detail belongs in
`--help` / the spec / the issue tracker — add a pointer, not the content.

## Workspace layout

A Cargo workspace, one shipped binary, language frontends feature-gated:

- **`crates/fxrank-core`** — language-neutral scoring model. **Depends on no parser** (the
  compiler enforces this; `syn` must never leak here). Holds: `effect` (kind/risk
  vocabulary + the `Effect`/`RiskFeature` wire types), `score` (Fibonacci weights, the
  containment discount, `own_score`, the composite `rank_key` sort tuple `(u8, u64, u32,
  u32)`), `confidence`, `model` (the
  JSON `Report`/`Scope`/`Hotspot`/`Summary` + `Report::build`), and the `Frontend` trait.
- **`crates/fxrank-lang-rust`** — the `syn`-based Rust frontend (behind the `rust`
  feature). `functions` (function-unit collection), `imports` (a `use` table), and
  `detect/{calls,macros,mutation,risk}` detectors orchestrated by `detect::analyze_unit`.
- **`crates/fxrank-lang-ts`** — the `swc`-based JS/TS frontend (behind the `ts` feature).
  `functions` (function-unit collection incl. arrows/methods/getters), `imports` (ES
  `import` + `require` table), `coverage` (signature typed-slot coverage for the boundary
  discount), and `detect/{calls,mutation,risk}` orchestrated by `detect::analyze_unit`.
  Parses with swc, runs no type-checker (syntactic, like the Rust frontend).
- **`crates/fxrank-lang-python`** — the `libcst`-based Python frontend (behind the
  `python` feature). `functions` (function-unit collection for `def`/`async def`,
  methods, nested `def`, and `lambda`, including the own-body recursion driver),
  `imports` (module-level `import`/`from … import` table), `coverage` (signature
  typed-slot coverage for the boundary discount), and `detect/{calls,mutation,risk}`
  orchestrated by `detect::analyze_unit`. Parses with libcst (crate dep `libcst`, lib
  name `libcst_native`, `default-features = false` for a pure-Rust build), syntactic
  (no type checker), like the other frontends.
- **`crates/fxrank-cli`** (package/binary `fxrank`) — args (clap), file discovery,
  feature-gated dispatch, compact JSON to stdout.

## Commands

```bash
cargo build
cargo test --workspace                                   # all tests (~90)
cargo test -p fxrank-core own_score_damps_non_max_weights # one test by name
cargo fmt --check                                        # CI gate
cargo clippy --workspace --all-targets -- -D warnings    # CI gate — warnings are hard errors
cargo build -p fxrank --no-default-features --features rust    # slim Rust-only build
cargo build -p fxrank --no-default-features --features ts      # slim TS-only build
cargo build -p fxrank --no-default-features --features python  # slim Python-only build
cargo run -p fxrank -- scan <path>                       # run the tool
cargo run -p fxrank -- scan crates/ | jq                 # dogfood on our own source (with cross-file propagation)
cargo run -p fxrank -- scan crates/ --no-resolve         # per-file own scores only (skip cross-file resolution + propagation)
cargo run -p fxrank -- scan crates/ --include-tests      # score test code too (skipped by default)
cargo run -p fxrank -- scan crates/fxrank-lang-python/src/  # dogfood the Python frontend's own Rust source
cargo run -p fxrank -- scan <path> --exclude 'node_modules,*.min.js,*.stories.*'  # replaces the default skip list
echo 'function f(): void {}' | cargo run -p fxrank -- scan --lang ts -      # scan a TS fragment from stdin
echo 'def f(): pass' | cargo run -p fxrank -- scan --lang python -           # scan a Python fragment from stdin
cargo insta review                                       # accept snapshot test changes (insta)
cargo install fxrank                                     # install the released binary from crates.io
cargo install --git https://github.com/caasi/fxrank fxrank  # install the latest unreleased binary from git
```

CI (`.github/workflows/ci.yml`) gates `fmt --check`, `clippy --workspace --all-targets -D
warnings`, `test --workspace`, all slim builds (`--features rust`, `--features ts`,
no-features), a Rust dogfood `scan crates/`, and a TS dogfood scan over the committed
fixtures. Run the first three locally before pushing.

## Releasing to crates.io

The workspace publishes **five** crates; the binary `fxrank` depends on the four
library crates, so all five are published in dependency order. Shared package metadata
(`license = "MIT OR Apache-2.0"`, `repository`, `authors`, `rust-version`, `keywords`,
`categories`) lives in `[workspace.package]` and is inherited via `field.workspace =
true`; each crate sets its own `description`. Internal deps carry both `path` and
`version` so crates.io can resolve them. For every release bump `version` in
**two** lockstep places: `[workspace.package].version`, **and** the `version = "X.Y.Z"`
pin on each internal dependency (`fxrank-core`/`fxrank-lang-rust`/`fxrank-lang-ts`/`fxrank-lang-python`
in the consuming crates' `Cargo.toml` — including `fxrank-lang-python/Cargo.toml`'s
own `fxrank-core` pin and the `fxrank` binary's `fxrank-lang-python` pin). The internal
pins are a caret range, so a stale one still *builds*, but the published crate would
advertise the wrong (older) internal-dep requirement — bump all of them. (`cargo build`
afterward refreshes `Cargo.lock`.)

```bash
# one-time: cargo login <crates-token>   (or set CARGO_REGISTRY_TOKEN)
cargo publish -p fxrank-core
cargo publish -p fxrank-lang-rust
cargo publish -p fxrank-lang-ts
cargo publish -p fxrank-lang-python
cargo publish -p fxrank            # the binary; depends on the four above
```

Validate `fxrank-core` first without uploading: `cargo publish -p fxrank-core
--dry-run`. The downstream crates **cannot** be dry-run-validated until `fxrank-core` is
on crates.io — their `version` dep resolves from the registry index, so packaging fails
with "no matching package" beforehand; they are verified by publishing in dependency
order. Publishes are **permanent** — a bad version can only be `cargo yank`ed, never
deleted.

The full release flow as an Arrow DSL pipeline (validate with `ocaml-compose-dsl`; see
the `compose` skill) — preflight gates, ordered publish, verify, then tag/release:

```arrow
-- Publish fxrank to crates.io. Run from repo root on `main`, in sync with origin.
-- The five crates publish in dependency order: core has no internal deps; the three
-- language frontends depend on core; the fxrank binary depends on all four.

let preflight =
  (on_branch(name: main) &&& in_sync(with: "origin/main") &&& tree_clean)   -- ref: git rev-parse / status
    >>> gate(require: [pass, pass, pass])                                    -- abort if any fails
in
let dry_run =
  publish(crate: "fxrank-core", mode: dry_run)?                             -- ref: cargo publish -p fxrank-core --dry-run
    >>> (ok ||| abort(reason: "core dry-run failed"))
in
let publish_all =
  publish(crate: "fxrank-core")                                             -- ref: cargo publish -p fxrank-core
    >>> publish(crate: "fxrank-lang-rust")                                  -- waits for index between each
    >>> publish(crate: "fxrank-lang-ts")
    >>> publish(crate: "fxrank-lang-python")
    >>> publish(crate: "fxrank")                                            -- the binary; depends on the four above
in
let verify =
  install(crate: "fxrank")?                                                 -- ref: cargo install fxrank && fxrank scan --help
    >>> (ok ||| abort(reason: "install verify failed"))
in
preflight
  >>> login(registry: crates_io)                                            -- ref: cargo login <token> (interactive)
  >>> dry_run
  >>> publish_all
  >>> verify
  >>> tag(version: "vX.Y.Z")                                                -- ref: git tag vX.Y.Z && git push origin vX.Y.Z
  >>> release(version: "vX.Y.Z")                                            -- ref: gh release create --generate-notes
```

## Architecture: how a scan flows

```
SourceFile(s) → RustFrontend::analyze (parse + per-file `use` table + static names)
  → functions::collect → FnUnit { symbol, id, path, line, sig, block }   (retains the body)
  → detect::analyze_unit(unit, imports, statics):
        gather  → calls/macros/mutation/risk detectors push Vec<Effect> (+ risks)
        fold    → own_score, max_class (incl. risk_class), risk_weight, confidence, async
  → also detect::build_record(unit, …) → UnitRecord { effects, refs: Vec<CallSiteRef>, qualified/first_party, language, is_root }
  → FrontendOutput { functions: Vec<Hotspot>, records: Vec<UnitRecord>, module_risks, diagnostics }
CLI → cross-file fold (spec 025, unless --no-resolve):
        partition_by_language(records) → per language group:
          SymbolIndex::from_records → CallGraph::from_records (Tarjan SCC condense)
          → fold (escaping-only, reverse-topo, join-not-sum) → apply_fold(&mut hotspots)  (adds propagated_*/inherited/root by unit_id; own-body untouched)
        external_reaches unioned once over all groups
  → core::Report::build(scope, hotspots, diagnostics, limit)  → compact JSON to stdout
```

The cross-file layer lives in `fxrank-core` (`record`/`resolve`/`graph`/`fold`) and stays
**parser-free**: frontends emit neutral `UnitRecord`s and tag refs `qualified`/`first_party`
(a `bool`); core only branches on those bools — no language syntax in resolution. A module's
import-time code is scored as a synthetic `<module>` unit (own-body = top-level statements only).
See spec 025 + `docs/cross-file-resolution-guideline.md`.

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
- **Hotspot `id` is `path:line:col:symbol`** (spec 005): a uniform 4-field shape across both
  frontends, where `col` is the 1-based **character** column of the function's name anchor
  (same span that produces `line`). Anonymous TS symbols carry a `C{col}` suffix
  (`<arrow@L279C55>`). The `id` is a **unique opaque key within a report** — it encodes
  position, so it changes when code moves (not stable across edits). Do *not* recover
  coordinates by splitting it, since both `path` (verbatim) and Rust `symbol` (`::`) can
  contain `:`; read the structured `path`/`line`/`symbol` fields (`col` is the only
  coordinate that lives solely inside the `id`). Adding a column is what makes two same-line
  anonymous functions distinct (`line` alone is not enough).
- **Detectability tiers** — every signal is `exact` / `path` / `heuristic`. Anything that
  truly needs type info (interior mutability, `.lock()`/`.set()` method-name effects,
  `unwrap`/`expect`, `&mut` write-through) is `heuristic` and takes a confidence penalty.
  Don't claim a type-dependent signal is exact.
- **The containment discount** is a class down-shift, not point subtraction: `&mut param`
  shifts down 2, `&mut self` down 1, clamped at class 1, and **cancelled inside `unsafe`**.
  The discount touches only the mutation channel, never sibling effects.
- **Centralize new vocabulary**: add effect kinds to `EffectKind` and risk kinds to
  `RiskKind` (both have `wire()` + class) — never hand-write wire strings at call sites. The
  Shell frontend (spec 029) added two `RiskKind`s this way: `DestructiveFs` (wire
  `destructive.fs`, class 5 — `rm -rf`/`mkfs`/`dd`-style irreversible filesystem ops) and
  `PrivilegeEscalation` (wire `privilege.escalation`, class 6 — `sudo`/`su`/`doas`). Both are
  `escapes() == true` (capability risks that propagate through the cross-file fold, spec 025)
  — calling a function that invokes `sudo rm -rf` should make the caller's propagated score
  reflect that, not encapsulate it the way an `unsafe`-only risk would.
- **`--exclude` is a three-class matcher** (spec 004, `fxrank_core::corpus::CorpusMatcher`):
  a no-`/` literal prunes a matching directory and excludes a matching file; a no-`/` glob
  (`*.min.js`, `*.stories.*`) excludes files only (never prunes a same-named dir); a
  `/`-bearing glob filters files by relative path. It **replaces** the default glob list
  when given. The **default is the union of the enabled frontends' `CorpusProfile`
  constants** (`pub const CORPUS_PROFILE` in each `fxrank-lang-*` crate) plus a
  language-neutral `.git` baseline (`CorpusProfile::COMMON`). In a full build this covers:
  `target` (Rust), `node_modules`/`__mocks__`/minified bundles/stories/jest files (TS), and
  `.venv`/`__pycache__`/build-artifact dirs/protobuf generated files (Python). A slim build
  (`--features ts`) only includes what its enabled frontends declare. In addition, a
  **content-marker prune** (`pyvenv.cfg` → prune that dir; from Python's profile, so present
  whenever the `python` feature is compiled in) runs independently of `--exclude` and is
  not disabled when `--exclude` is given. Files dropped by the
  matcher are counted in `scope.skipped_excluded` (directory prunes and marker prunes are
  not counted — they are never read). `--exclude` applies only to directory scans; an
  explicitly named file/stdin is always scanned. See `docs/corpus-profile-guideline.md`
  for the full per-language channel breakdown.
- Tests use a shared `analyze_fixture(name)` helper reading `tests/fixtures/*.rs`
  (subdir — cargo does not compile those as test targets). Snapshot tests use `insta`.

## Dogfooding caveat (real, from running `fxrank scan crates/`)

The tool correctly surfaces genuine production hotspots (`run_scan`, `walk_dir` — the IO
boundaries) with accurate evidence, and the discount correctly keeps the pervasive
`&mut self` visitor accumulation *low* (no false alarms). Test code is now **skipped by
default** (`#[test]`/`#[bench]` functions and `#[cfg(test)]` modules; pass `--include-tests`
to score it), so the old "`assert!`/`assert_eq!` register as `panic` and dominate raw
rankings" noise is gone for normal scans. Test-skip now also covers a **bare
`#[cfg(test)]`** on a free `fn`, an `impl`/`trait` block, or an `impl`/`trait` method
(not just `#[test]`/`#[bench]` and `#[cfg(test)] mod`) — #53. Remaining gap: an
**out-of-line `#[cfg(test)] mod foo;`** (body in a separate file) still can't be resolved
here (deferred — tracked by #53); and compound `#[cfg(all(test, …))]` is intentionally
not matched.

## Dogfooding the TS frontend (running `fxrank scan crates/fxrank-lang-ts/src/`)

Dogfooding the Rust frontend on the new TS-frontend Rust code validated the containment
discount on our own visitor pattern: every swc walker (`CallWalker`, `RiskWalker`,
`AnyBodyWalker`, `Collector`) lands at class 2 because their `&mut self` `param.mutation`
is correctly discounted — the pervasive visitor-accumulation pattern stays low with no
false alarms. The real IO boundaries (`run_scan`, `walk_dir`) surface at class 7 as
expected. Core scoring functions score near zero.

**Caveat surfaced (now fixed, #53):** standalone module-level `#[cfg(test)] fn` helpers
*were* not skipped by the Rust frontend's test detection — it skipped `#[test]` functions
and `#[cfg(test)]` *modules*, but not bare `#[cfg(test)]` items, so they appeared as
hotspots. `functions::collect` now propagates a bare `#[cfg(test)]` on a free `fn`, an
`impl`/`trait` block, and an `impl`/`trait` method into `is_test`, so the workaround (moving
helpers into a `#[cfg(test)] mod tests` block) is no longer required. Only the out-of-line
`#[cfg(test)] mod foo;` case remains open (tracked by #53).

## Dogfooding the React signals (running `.tsx` fixtures through the two-pass)

The React two-pass (`lib.rs::analyze_units`) extends the TS frontend with four signal
families. The `tests/fixtures/react/` acceptance fixtures validate each family end-to-end:

- **Component inheritance** (`counter.tsx`, `effects.tsx`): inline arrows passed directly
  to `useEffect`, `useMemo`, `useCallback`, `useLayoutEffect`, and `useState` (lazy form)
  are absorbed into the owning component's score and suppressed as standalone hotspots.
  The component's `max_class` rises to the highest inherited effect. No `<arrow@…>` entries
  appear in the output for absorbed callbacks.

- **`EffectInRender`** (`effects.tsx`): a `fetch` inside `useMemo(() => fetch(…))` earns
  an `effect.in.render` risk (class 4) on the component because `useMemo` callbacks run
  during the render phase; the same `fetch` inside `useEffect(() => fetch(…))` does NOT
  (effect-phase callbacks are the honest baseline). The `FetchData` component in
  `effects.tsx` carries both inherited `net.fs.db` effects (class 7) and one
  `effect.in.render` risk, proving the phase distinction.

- **`useRef` cell → hidden mutation** (`uncontrolled_cell.tsx`): a `useRef` binding whose
  `.current` is written in the component body is detected as `hidden.mutation` (class 3)
  with `subreason: "ref-cell-write"` and `hidden: true`. The mutation walker's
  `ref_bindings` set is seeded at collection time, so the write is correctly distinguished
  from a plain captured-outer-binding write (also `hidden.mutation` class 3, but with
  `subreason: "captured-binding"` since spec 008 — see *Cross-language mutation alignment*
  below). For writes inside absorbed hook callbacks the owning component's
  `ref_bindings` are threaded via `extra_refs` (see `detect::raw_signals`).

- **`StateTransition`** (`counter.tsx`): every `const [v, setV] = useState(…)` declaration
  in a component body emits a `state.transition` effect (class 1, subreason: "useState")
  attributed to the DECLARATION LINE (not to setter call sites). The signal is "component
  holds traced state" — the score is intentionally low (class 1) because traced state is
  declared and bounded.

**Documented misses (all Milestone-B candidates):**
- Single-hop limit: effects inside a callback that is itself inside a recognized hook
  callback are NOT absorbed (only the outermost arrow is a single-hop callback).
- Custom-hook callbacks → issue #25: a `useCustomHook(() => fetch(…))` where the custom
  hook internally calls `useEffect` is not recognized; only literal `use{Effect,Memo,…}`
  callees match.
- All-null components: a component that always returns `null` is not detected as a
  component (`returns_jsx` is false) and gets no React augmentation.
- `useMemo(() => <jsx/>)` self-referential arrow: the arrow both is a hook callback (render
  phase) and itself returns JSX — it is suppressed as a standalone hotspot and absorbed,
  but `returns_jsx` on the absorbed arrow body does not cause it to be treated as a
  component (which is correct; it is not a component).
- Namespace `React.useEffect(…)`, `React.useState(…)`, etc.: the hook recognizers match
  bare callee identifiers only; qualified `React.*` forms are an accepted miss.

## Cross-language mutation alignment (spec 008)

The three `detect/mutation.rs` mutation detectors (Rust/TS/Python) are **aligned to one
canonical model**: the same write *concept* produces the same `EffectKind`/class/`contained`/
`hidden` across frontends, with a consistent `hidden.mutation` subreason vocabulary —
`interior-mut` (interior mutability through a shared `&`/receiver), `captured-binding`
(a captured/unresolved base), `ref-cell-write` (TS `useRef().current`). The descriptive
**source of truth is `docs/mutation-classification-guideline.md`** (the shared model + the
intentional per-language differences that are *kept*, not unified).

Each frontend keeps its **own native walk** — there is no shared classifier crate. What changed:
the real file-level **`static` set** and the **`ImportTable`** are now threaded into
`mutation::detect` (Rust gets both; Python gets imports), and the classification cascade consults
them. Concretely (the F1–F5 of spec 008):
- **F1** — a captured/unresolved base (none of own-local/param/receiver/static/import) →
  `hidden.mutation`/3/`captured-binding` in all three (this is **Python's first `HiddenMutation`**;
  before, Python silently emitted nothing — a false purity).
- **F2** — Rust scores a write to a **real `static`** (incl. `static mut` and interior-mutable
  atomics via `.store()`) as `global.mutation`/6 by the actual static-name set; the old
  `SCREAMING_SNAKE_CASE` proxy is **retired** (no more UPPERCASE-local false positives).
- **F5** — a write whose base resolves through the import table → `global.mutation`/6 (Python/Rust;
  near-vacuous for Rust). Guard: a misattributed `self` base (e.g. from `use m::{self, …}` putting
  `self` in the import table) is **not** globalized — it's dropped.
- **F4** — in a TS/Python constructor only a **direct** field-init (`this.x = …` / `self.attr = …`,
  a plain `=`) stays contained `local.mutation`/1; method/subscript/compound/update writes on the
  receiver (`this.x.push()`, `this[i]=`, `this.x += 1`, `this.x++`) escape to `this.mutation`/3.
- **Honest differences kept** (do not "fix"): Rust mut-channel discount vs TS/Python typed-boundary
  discount; Python `nonlocal`→`this.mutation`/Exact; plain rebind (Python no-emit vs TS/Rust
  `local.mutation`); per-language mutating-method allowlists.

**Issue #29 resolved cross-language** (plan `docs/superpowers/plans/009-cross-language-module-binding-global.md`, extending spec 008's F2)**:** a module-top-level binding write now classifies
as `global.mutation`/6 in all three frontends — Rust via the `static` set (F2, pre-existing), TS
via the `module_bindings` set (#29), Python via the module-level-name set for the content-mutation
case (the explicit-`global` rebind already escalated). **Residual heuristic limit:** a
function-scoped binding shadowing a module-level name resolves to local — flat syntactic binding
sets (TS traversal-order; Python whole-function local pre-scan) stop short of full lexical-scope
modeling, so the shadow wins in both frontends.

## Design artifacts & workflow

Specs live in `docs/superpowers/specs/`, implementation plans in `docs/superpowers/plans/`
(the `superpowers` skill default location), with matching 3-digit prefixes
(`docs/superpowers/specs/001-*` ↔ `docs/superpowers/plans/001-*`). `docs/superpowers/specs/001-fxrank-rust-effect-scanner.md` is the
source of truth for every score, class, discount, and schema field — **read it before
changing scoring behavior**; when code and spec disagree, the spec wins. (For mutation
classification specifically, `docs/superpowers/specs/008-cross-language-mutation-alignment.md` + the guideline
above govern the cross-frontend behavior. For **cross-file resolution + call-graph propagation**,
`docs/superpowers/specs/025-cross-file-resolution-and-propagation.md` + `docs/cross-file-resolution-guideline.md`
govern — read §13/§13a for the current Known Limitations and the deferred 3e/3f.) Spec 001's *Known
Limitations* section records the older deferrals; note **call-graph propagation / `inherited_score` is
now DELIVERED by spec 025** (no longer deferred), while FFI call-site detection, `global.mutation` class-4
downgrade, and a full semantic/type-resolution pass remain.

The **specs are historical reference** — a contributor reads one only when the shared-knowledge
docs contradict themselves or the code (the spec is the tie-breaker of record). Day-to-day work
starts from `docs/README.md` (the shared-knowledge index) and the descriptive guidelines it maps;
adding a new language frontend is fully driven by `docs/adding-a-language-frontend.md` (a
prescriptive checklist that composes the mutation / corpus / cross-file guidelines + the core code).

## Reviewing changes: use model-diverse reviewers

When running the review gate on a change (the `review-loop` big loop), **do not review with only the
same model as the author.** Run at least two *different* models as independent reviewers. Evidence
from the docs work that produced `docs/adding-a-language-frontend.md`: an Opus-4.8 and a Sonnet-5
reviewer, given the identical diff and prompt, shared only **1 of 8** findings — Opus caught
semantic-precision bugs (a mis-stated discount class), Sonnet caught repo-wide completeness gaps
(a missing per-language CI line that would have let the new frontend silently skip CI). Their blind
spots barely overlap, so same-model review misses roughly half of what's findable. Default the local
reviewer to a model other than the main loop's, and treat a final independent pass (+ Copilot on the
PR) as the merge gate — not "one model went quiet."
