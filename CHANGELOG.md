# Changelog

All notable changes to FxRank are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) (pre-1.0: the public output
schema may still change between releases, including patch releases — as the `id` format
did in 0.1.1).

## [Unreleased]

### Added

- **Short aliases for every `fxrank scan` flag** ([#40]) — `-n` (`--limit`), `-t`
  (`--include-tests`), `-L` (`--lang`), `-e` (`--exclude`), and `-R` (`--no-resolve`),
  joining the existing `-p` (`--project`). Letters were chosen collision-free (`-L`/`-R`
  uppercase to avoid the `--limit`/`--lang` ambiguity of `-l`), and a startup
  `debug_assert` test guards against duplicates as flags are added. Long forms remain the
  convention for scripts and docs; the shorts are purely an interactive convenience.

### Fixed

- **Deterministic scan output** ([#46]) — `scope.external_reaches[]`, each
  `hotspots[].inherited[]`, and the summary/per-hotspot external-reach lists were
  serialized in hash-container iteration order, so output varied run-to-run on identical
  input (the *sets* were identical; only the order differed) — defeating before/after
  diffs, CLI golden tests, and reproducible CI artifacts. `Report::build` now sorts each
  by a stable key before serialization (external reaches by `(specifier, kind, site)`,
  inherited by `(kind, class, from, via)`). Ordering only — no scoring or set-membership
  change. The per-hotspot sort runs after `--limit` truncation, so only the retained
  top-N pay for it.
- **Rust: pure value / compile-time macros no longer flagged `unknown.macro`** ([#54]) —
  `serde_json::json!`, `env!`, and `option_env!` are whitelisted (matched on the last path
  segment, so qualified forms classify the same), removing a large class of false
  `unknown.macro` (class 2) noise. Dogfood: `unknown.macro` effects on `agent-browser/cli`
  dropped 1445 → 20.

## [0.4.0] - 2026-06-26

A large release: effects now propagate **across files**, module resolution is
**precise** (no more name-based false-resolves) in all three frontends, and the
**React effect-scoring model is redesigned** alongside a cross-language scoring-table
rebaseline. **Pre-1.0 output change (substantial):** scores, rankings, and the report
schema all shift — a component now owns the effects of the handlers/hooks it defines,
logging is re-weighted, and per-function `propagated_*`/`inherited[]`/`external_reaches[]`
fields are added.

### Added

- **Cross-file resolution + transitive effect propagation** (specs 025 / 025-3e,
  [#25]/[#28]/[#36]). A scan now resolves calls across the scanned files and folds
  **escaping** effects along the call graph, so a function's `propagated_score` reflects
  the effect blast-radius of everything it (transitively) calls — closing the
  extract-method score-washing hole. New report fields: `propagated_score` /
  `propagated_max_class`, an `inherited[]` provenance list per hotspot, a `root`
  annotation (the agent's observation focus — explicit FILE args), and
  `scope.external_reaches[]` (the app's outward dependency surface). `--no-resolve`
  disables the pass (own-body scores only). Own-body output is byte-identical to pre-025.
- **Precise cross-file module resolution** ([#36], spec 025-3e) across Rust/TS/Python —
  replaces last-path-segment name matching (which false-resolved `std::fs::write` to a
  lone `Foo::write`) with path-precise, never-guess resolution.
- **TS cross-dialect resolution** ([#41]) — `.tsx`↔`.ts` imports (relative and alias)
  now resolve as one TS module namespace.
- **TS `tsconfig.json` `paths` aliases** via a tsc-compatible `--project`/`-p` flag
  (025-3e) — `@/…`-style imports fold into propagation.
- **`CorpusProfile`** ([#21]) — frontend-owned corpus hygiene: the `--exclude` default is
  the union of the enabled frontends' profiles, plus a `pyvenv.cfg` content-marker prune.

### Changed

- **React effect-scoring redesign** ([#37], spec 027). A component is scored as CPS
  (`value` props in, `onXxx`/JSX out): it **owns every effect lexically defined in its
  body** (render + all hooks + all handlers, any depth, incl. `useMutation`/`useQuery`
  object-arg callbacks and `return null` components) **minus** what it hands across the
  boundary (received callbacks/refs are the consumer's). Adds a **CPS containment
  discount** (internal state/ref/memo stay in `own`; world effects propagate) and an
  **event-phase conditionality discount** (capped 1 class, floored, recorded). Replaces
  the allowlist/single-hop two-pass and unifies on the shared fold. *Effect:* the
  effect-blind 84%-own=0 problem and 55 orphan IO handlers (dogfood) collapse — handlers'
  effects now attribute to their components.
- **Cross-language scoring-table rebaseline** (spec 028). `Logging` class **4 → 2** (a
  benign output, no longer dominating); `time`/`random` codified as escaping world
  effects. Affects Rust/Python/TS rankings.

[#21]: https://github.com/caasi/fxrank/issues/21
[#25]: https://github.com/caasi/fxrank/issues/25
[#28]: https://github.com/caasi/fxrank/issues/28
[#36]: https://github.com/caasi/fxrank/issues/36
[#37]: https://github.com/caasi/fxrank/issues/37
[#40]: https://github.com/caasi/fxrank/issues/40
[#41]: https://github.com/caasi/fxrank/issues/41
[#46]: https://github.com/caasi/fxrank/issues/46
[#54]: https://github.com/caasi/fxrank/issues/54

## [0.3.0] - 2026-06-23

This release aligns mutation classification across all three frontends and adds a
cross-language rule for module-shared state. **Pre-1.0 output change:** writes that were
previously silent or `hidden.mutation` now surface as `global.mutation`, so scores and
rankings shift for affected code.

### Changed

- **Cross-language mutation-classification alignment** (spec 008, [#32]). Rust (`syn`),
  TS/JS (`swc`), and Python (`libcst`) now classify a write against **one canonical model** —
  the same `EffectKind`/class/`contained`/`hidden` and a shared `hidden.mutation` subreason
  vocabulary (`interior-mut` / `ref-cell-write` / `captured-binding`) — keeping each language's
  intentional differences, documented in
  [`docs/mutation-classification-guideline.md`](docs/mutation-classification-guideline.md):
  - A captured/unresolved base → `hidden.mutation`/3/`captured-binding` in all three
    (**Python's first `HiddenMutation`** — previously a false purity).
  - Rust scores a write to a **real `static`** (incl. `static mut` and interior-mut atomics via
    `.store()`) as `global.mutation`/6 by the actual static-name set; the old
    `SCREAMING_SNAKE_CASE` proxy is retired (no more uppercase-local false positives).
  - A write whose base resolves through the import table → `global.mutation`/6 (Python/Rust).
  - Constructor breadth (TS aligned to Python): only a **direct** field-init
    (`this.x =` / `self.attr =`) stays contained `local.mutation`/1; method/subscript/compound/
    update writes on the receiver escape to `this.mutation`/3.

- **Module top-level binding writes → `global.mutation` (class 6), cross-language** ([#29], [#33]).
  A write to a module top-level binding now classifies as `global.mutation`/6 in every frontend —
  the "module var used for cross-component communication" anti-pattern — while a genuinely
  captured *enclosing-function* local stays `hidden.mutation`/3. Realizations: Rust via the real
  `static` set (the spec-008 F2, generalized); TS via a `const`/`let`/`var`/`function`/`class`
  module-binding set (incl. `export` + named default, and destructuring); Python via the
  module-level assign-target + `def`/`class` name set, covering the **content-mutation without
  `global`** case (`_cache["k"]=1`, `shared.append(1)`) — the explicit `global x` rebind already
  escalated. Heuristic limit (shared): a function-scoped binding shadowing a module name resolves
  to local (flat-scope, best-effort). The Python local pre-scan recognizes all function-local
  binding forms — assignment (incl. destructuring), augmented assign, and `for` / `with … as` /
  `except … as` targets — while `match` patterns, comprehension scopes, and walrus (`:=`) remain
  documented accepted misses.

## [0.2.0] - 2026-06-22

### Added

- **Python frontend** ([#14]). `fxrank scan` now profiles Python (`.py`) source via
  [`libcst`](https://github.com/Instagram/LibCST) (pure-Rust, `default-features =
  false`), at parity with the Rust and TS/JS frontends. It reuses the existing
  effect/risk vocabulary — **no new wire kinds**. Includes: the boundary-containment
  discount driven by annotation coverage; the `Any` two-case poison (emits
  `type.escape`); escape analysis for `global` / `nonlocal` / `self` mutation
  (`__init__` direct self-init is contained, `self.x.method()` escapes);
  `import` / `from … import` / `as` resolution (incl. function-local imports); and
  dynamic-code risk detection (`eval` / `exec` / `compile` / `pickle` /
  `subprocess(shell=True)` / …). Anonymous `lambda`s are collected and anchored as
  `<lambda@LxCy>`. See `docs/superpowers/specs/006-fxrank-python-frontend.md`.
- **CLI / CI**: `.py` files route to the Python frontend; `--lang python` scans a
  Python fragment from stdin (`.pyi` stubs excluded). The `--exclude` default list
  gains Python corpus-hygiene entries (`.venv`, `venv`, `.tox`, `__pycache__`,
  `build`, `dist`, cache dirs, `site-packages`, `*_pb2.py`, …). A `--features python`
  slim build and a Python dogfood scan were added to CI.
- Python **test-code skipping** by convention: `test_*.py` / `*_test.py` /
  `conftest.py` files and `tests/` directory segments, plus `test_*` functions,
  `Test*`-named class methods, and `unittest.TestCase` subclass methods
  (`--include-tests` to score them).

### Notes

- The workspace now publishes **five** crates; `fxrank-lang-python` is new. All crates
  share one workspace version and publish in dependency order (`fxrank-core` →
  `fxrank-lang-rust` → `fxrank-lang-ts` → `fxrank-lang-python` → `fxrank`).

## [0.1.1] - 2026-06-20

### Fixed

- **Hotspot `id`s are now unique for two anonymous functions on the same line**
  ([#9]). Previously, two anonymous arrows/functions sharing one physical line (e.g.
  `foo().then(() => {}).catch(() => {})`, nested JSX handlers, chained
  `.map()/.filter()/.find()`) collapsed to the same symbol fallback (`<arrow@L279>`)
  and therefore emitted an identical `id` — breaking addressability for any consumer
  that keys hotspots by `id`. See `docs/superpowers/specs/005-hotspot-id-column.md`.

### Changed

- **`id` wire format is now `path:line:col:symbol`** (was `path:line:symbol`), a
  uniform 4-field shape across both the Rust and TS/JS frontends. `col` is the
  1-based **character** column of the function's name anchor. Anonymous TS symbols
  additionally carry a `C{col}` suffix (`<arrow@L279C55>`). The `id` is a unique
  **opaque** key within a report (it encodes position, so it changes when code moves —
  not stable across edits). Read `path`/`line`/`symbol` from their own structured
  `Hotspot` fields rather than splitting the `id` string (both `path` and Rust `symbol`
  can contain `:`). No new wire field was added; `col` is the only coordinate that lives
  solely inside the `id`.

## [0.1.0] - 2026-06-20

### Added

- Initial release. `fxrank scan <path>` profiles **own-body effect cost** (IO,
  mutation, panic, risk, …) for Rust (`syn`) and TS/JS (`swc`) source, emitting
  compact JSON that ranks each function as a refactoring hotspot.
- The **containment discount**: `&mut`/`&self`/ownership make some effects *declared
  and bounded* (they score lower), while hidden interior mutability scores *higher*.
- `--exclude` three-class matcher and a documented default skip list for vendored
  bundles, Storybook stories, and test-support files (`docs/superpowers/specs/004`). Test code is
  skipped by default (`--include-tests` to score it).
- Slim, feature-gated builds (`--features rust`, `--features ts`).

[Unreleased]: https://github.com/caasi/fxrank/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/caasi/fxrank/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/caasi/fxrank/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/caasi/fxrank/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/caasi/fxrank/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/caasi/fxrank/releases/tag/v0.1.0
[#9]: https://github.com/caasi/fxrank/issues/9
[#14]: https://github.com/caasi/fxrank/issues/14
[#29]: https://github.com/caasi/fxrank/issues/29
[#32]: https://github.com/caasi/fxrank/pull/32
[#33]: https://github.com/caasi/fxrank/pull/33
