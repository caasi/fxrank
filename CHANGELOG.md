# Changelog

All notable changes to FxRank are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) (pre-1.0: the public output
schema may still change between releases, including patch releases — as the `id` format
did in 0.1.1).

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

- **Module top-level binding writes → `global.mutation` (class 6), cross-language** ([#29]).
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

[0.3.0]: https://github.com/caasi/fxrank/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/caasi/fxrank/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/caasi/fxrank/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/caasi/fxrank/releases/tag/v0.1.0
[#9]: https://github.com/caasi/fxrank/issues/9
[#14]: https://github.com/caasi/fxrank/issues/14
[#29]: https://github.com/caasi/fxrank/issues/29
[#32]: https://github.com/caasi/fxrank/pull/32
