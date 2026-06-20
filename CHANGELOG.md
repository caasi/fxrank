# Changelog

All notable changes to FxRank are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) (pre-1.0: the public output
schema may still change between releases, including patch releases — as the `id` format
did in 0.1.1).

## [0.1.1] - 2026-06-20

### Fixed

- **Hotspot `id`s are now unique for two anonymous functions on the same line**
  ([#9]). Previously, two anonymous arrows/functions sharing one physical line (e.g.
  `foo().then(() => {}).catch(() => {})`, nested JSX handlers, chained
  `.map()/.filter()/.find()`) collapsed to the same symbol fallback (`<arrow@L279>`)
  and therefore emitted an identical `id` — breaking addressability for any consumer
  that keys hotspots by `id`. See `specs/005-hotspot-id-column.md`.

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
  bundles, Storybook stories, and test-support files (`specs/004`). Test code is
  skipped by default (`--include-tests` to score it).
- Slim, feature-gated builds (`--features rust`, `--features ts`).

[0.1.1]: https://github.com/caasi/fxrank/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/caasi/fxrank/releases/tag/v0.1.0
[#9]: https://github.com/caasi/fxrank/issues/9
