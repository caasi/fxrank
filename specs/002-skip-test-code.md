# 002 — Skip Test Code by Default

## Goal

FxRank profiles the side-effect cost of the code you *ship*. A test's effect-cost
is never an actionable smell, yet dogfooding `fxrank scan` on its own source showed
test code dominating the rankings — inline `#[cfg(test)]` modules, whose
`assert!`/`assert_eq!` register as `panic` effects, were **13 of the top 15** hotspots
when scanning `src/`, drowning the real production signal (`run_scan`, `walk_dir`).

Exclude test code from scoring by default so the production signal is what surfaces.
This is a **scope decision** (what we profile), not a new lint — FxRank is an
effect-cost profiler, not a general linter.

## Scope

In scope:

- Detect test functions **syntactically** (attribute-based) and exclude them from
  scoring by default.
- Report the excluded count (`scope.skipped_tests`) — never a silent drop.
- A `--include-tests` flag to score test code anyway.

Out of scope (deferred):

- **Path-based skipping** (files under `tests/` / `benches/`) for *un-attributed*
  test helpers — deferred until real-world scans show it is worth the path-string
  fragility. (Attribute detection already catches every `#[test]` fn and
  `#[cfg(test)]` module, i.e. the dominant noise.)
- Any tuning of the `panic` / method-name heuristics — in production code those are
  largely *signal*; tune only with real false-positive data, not speculation.
- Tagging test hotspots in the output (the rejected "always score + tag" approach).

## Test-code detection (attribute-based, syntactic, `exact`)

A function is **test code** if any of:

- it carries a `#[test]` or `#[bench]` attribute; **or**
- it is declared inside a module annotated `#[cfg(test)]` (an `Item::Mod` whose
  attributes include `#[cfg(test)]`), recursively — functions in modules nested
  inside a `#[cfg(test)]` module are also test code.

Detection is purely syntactic (attributes on items), so it is `exact`-tier and needs
no type information. `#[cfg(test)]` recognition matches the literal attribute
`#[cfg(test)]` (attribute path `cfg`, meta containing the single `test` token). FxRank
does **not** evaluate arbitrary `cfg` predicates; compound forms like
`#[cfg(all(test, …))]` are a deferred refinement (see *Open Questions*).

Detection keys off attributes in the parsed source, **independent of the file path**.

## Behavior

- **Default:** test functions are excluded from `hotspots`; `scope.functions` counts
  only scored (non-test) functions; `scope.skipped_tests` = the number excluded.
- **Module-level risks** (`impl Drop`, `extern` blocks) declared inside a
  `#[cfg(test)]` module are test-only and are likewise excluded from
  `scope.risk_features`.
- **`--include-tests`:** no exclusion — every function is scored and
  `skipped_tests` is `0`.
- **Summary roll-ups** (`own_score`, `max_class`, `risk_weight`, `confidence`) are
  computed over the scored hotspots only — unchanged semantics (they were always
  over the scored set).

## Architecture

- `functions::collect` tracks whether it is descending inside a `#[cfg(test)]`
  module and records, per `FnUnit`, an **`is_test: bool`** (true if the fn carries
  `#[test]`/`#[bench]` **or** has an enclosing `#[cfg(test)]` module). Likewise,
  `detect_module_risks` skips items inside a `#[cfg(test)]` module.
- Config rides on the **frontend struct** — `RustFrontend { include_tests: bool }`
  — so the `Frontend::analyze` *trait method signature is unchanged* (policy in the
  struct, not the interface). `RustFrontend::default()` ⇒ `include_tests: false`.
- `RustFrontend::analyze`: when `!include_tests`, drop the `is_test` units before
  scoring and accumulate their count into **`FrontendOutput.skipped_tests: usize`**.
- **CLI:** a `--include-tests` flag (clap) builds `RustFrontend { include_tests }`,
  and maps `FrontendOutput.skipped_tests` into `Scope.skipped_tests`.
- **Core:** `Scope` gains a `skipped_tests: usize` field (wire); `FrontendOutput`
  gains `skipped_tests: usize`.

The existing frontend tests pass bare fixture filenames and the fixtures contain
plain (non-`#[test]`) functions, so they are unaffected by attribute-based detection.

## Output schema change

`scope` gains one field (placed after `functions`):

```json
"scope": { "input": "src", "files": 6, "parsed": 6, "functions": 31, "skipped_tests": 18, "risk_features": [] }
```

## Error Handling

No new failure modes. Test detection requires a parsed AST, so an un-parseable file
still becomes a `diagnostic` (it contributes nothing to `skipped_tests`).
`--include-tests` is a plain boolean flag (default false).

## Testing Strategy

- **Fixture** with a `#[cfg(test)] mod tests { #[test] fn t() { std::fs::read("x"); } }`
  next to a production fn carrying an effect → default `analyze` returns only the
  production fn with `skipped_tests == 1`; `RustFrontend { include_tests: true }`
  returns both with `skipped_tests == 0`.
- A **top-level `#[test] fn`** → skipped by default.
- A `#[cfg(test)]` module containing an `impl Drop` → **not** in `module_risks` by
  default.
- **CLI integration:** `fxrank scan <dir>` excludes tests and reports
  `scope.skipped_tests`; `--include-tests` includes them.
- **Regression:** existing frontend unit tests and `insta` snapshots are unchanged
  (their fixtures are plain functions) — verify this holds.

## Verification

- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check` all green.
- `fxrank scan crates/fxrank-cli/src` now surfaces production hotspots (`run_scan`,
  `walk_dir`) at the top, with `scope.skipped_tests` reflecting the excluded inline
  test functions (previously they buried the signal).

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| Default behavior | Exclude test code from scoring | A test's effect-cost is never an actionable side-effect smell; dogfood showed it drowning the signal. |
| Detection | Attribute-based (`#[test]`/`#[bench]` + `#[cfg(test)]` module), syntactic | Cheap, exact, catches the dominant noise; no path fragility, no type info. |
| Path-based skip | Deferred | Un-attributed helpers in `tests/` are a smaller residual; defer until real scans justify the path heuristic. |
| Not silent | `scope.skipped_tests` count | Honors the spec's no-silent-drops value. |
| Config location | `RustFrontend { include_tests }` struct field | Keeps the `Frontend::analyze` trait signature unchanged. |
| Heuristic / panic tuning | Out of scope | FxRank is an effect profiler, not a linter; tune only with real false-positive data. |

## Open Questions

- Compound cfg predicates (`#[cfg(all(test, feature = "x"))]`, `#[cfg(test)]` via a
  crate-level alias): v1 matches only the literal `#[cfg(test)]`. Generalize later if
  real code shows it matters.
- Should `--include-tests` eventually be expressible in a config file rather than a
  per-invocation flag? Out of scope now.
