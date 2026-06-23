# Spec 008 — dogfood ranking deltas

Observed by running `fxrank scan` on the workspace's own source (`crates/`) with the
aligned build, cross-checked against the per-task RED/GREEN test evidence (the authoritative
before/after). Every delta below is intentional behavioral parity (spec 008 §3 F1–F5),
**except** one false-positive that dogfood caught and we fixed (see bottom).

## Intended deltas (per frontend)

- **Python** — gains effects it never emitted before:
  - Captured / module-level writes (closed-over or undeclared-global roots) → `hidden.mutation`
    class 3, subreason `captured-binding` (F1). Previously **no emission** (false purity).
  - Import-rooted writes → `global.mutation` class 6 (F5). Previously no emission.
  - Python emits `HiddenMutation` for the first time.
- **Rust** — static/global resolution moved off the casing proxy:
  - Real `static`/`static mut` writes → `global.mutation` class 6, by the **real static set**
    (F2), including a *lowercase* `static mut` and an interior-mutable atomic static via
    `.store()` (`interior write to global … via .store`). The `SCREAMING_SNAKE` proxy is retired,
    so an UPPERCASE *local* is no longer a false global.
  - Unresolved (captured/module/unknown) bases → `hidden.mutation` class 3, subreason
    `captured-binding` (F1). Previously dropped (or UPPER→global via the proxy).
  - Import-resolved bases → `global.mutation` (F5; near-vacuous in practice).
  - Interior-mutability writes on shared `&` receivers → `hidden.mutation` subreason
    `interior-mut` (F3).
- **TS** — constructor breadth tightened (F4): a method-call receiver or subscript write on
  `this` in a constructor now escapes to `this.mutation` class 3; only a *direct* `this.<field>`
  init stays contained `local.mutation`. Captured fallback gains subreason `captured-binding`
  (F3). (No committed-fixture snapshot moved — proven by unit tests.)

Subreason vocabulary is consistent across frontends: `interior-mut`, `captured-binding`,
`ref-cell-write` (TS `useRef().current`, unchanged).

## Self-dogfood evidence (aligned build, `scan crates/`)

`hidden.mutation` subreasons: 12 `captured-binding`, 3 `interior-mut`, 1 `ref-cell-write`.
`global.mutation` evidence: `write to global COUNT`, `write to global counter_cell` (lowercase),
`interior write to global HITS via .store` (atomic), `write to global globalThis` (TS fixture),
`write to imported imported_cell` (F5 fixture). All intended; no anomalies remain.

## False-positive caught and fixed

Dogfood (not the test suite) surfaced `count_awaits` (`crates/fxrank-lang-ts/src/detect/mod.rs`)
emitting `global.mutation` "write to imported self". Root cause: a nested
`impl … { fn …(&mut self) { self.0 += 1 } }` inside a self-less free fn is attributed to the
enclosing unit (the accepted Rust closure/nested-fn traversal, spec §1), and the file's
`use crate::react::{self, …}` puts `"self"` in the ImportTable, so F5 resolved the misattributed
`self` base to `global.mutation`/6 (pre-008 it was silently dropped). **Fix** (`fix(rust): F5/F1
must not classify a misattributed \`self\` base as global/hidden`): guard the F5 import arm and
the F1 captured tail with `base != "self"` — a `self` base reaching those arms is a misattributed
receiver write and is dropped, while real `&mut self` / `&self`-interior writes are caught earlier
(unaffected). Regression test added; dogfood now clean.
