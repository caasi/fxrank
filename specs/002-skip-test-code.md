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
- Exclude test-only **top-level** module-risk features (a `#[cfg(test)]`-attributed
  top-level `impl Drop` / `extern` block) by default.
- Report the excluded function count (`scope.skipped_tests`) — never a silent drop.
- A `--include-tests` flag to score test code (and include test-only module risks).

Out of scope (deferred):

- **Path-based skipping** (files under `tests/` / `benches/`) for *un-attributed*
  test helpers — deferred until real-world scans show it is worth the path-string
  fragility. (Attribute detection already catches every `#[test]` fn and
  `#[cfg(test)]` module, i.e. the dominant noise.)
- **Recursing module-risk detection into inline modules.** `detect_module_risks`
  scans only top-level `file.items` today, so an `impl Drop` *nested inside* a
  `#[cfg(test)] mod` is already not detected; this spec does not change that. Only
  top-level `#[cfg(test)]`-attributed items are affected.
- Any tuning of the `panic` / method-name heuristics — in production code those are
  largely *signal*; tune only with real false-positive data, not speculation.
- Tagging test hotspots in the output (the rejected "always score + tag" approach).

## Test-code detection (attribute-based, syntactic)

A function is **test code** if any of:

- it carries a `#[test]` or `#[bench]` attribute; **or**
- it is declared inside a module annotated `#[cfg(test)]` (an `Item::Mod` whose
  attributes include `#[cfg(test)]`), recursively — functions in modules nested
  inside a `#[cfg(test)]` module are also test code.

Detection reads attributes from the parsed AST, so it needs no type information and
is **`exact` for the recognized syntactic forms** (it intentionally does *not* catch
path-based integration tests, un-annotated helpers, compound `cfg` predicates, or
`cfg_attr` — those are deferred).

In `syn` 2.x: `#[test]`/`#[bench]` are detected as an attribute whose
`attr.path().is_ident("test")` / `is_ident("bench")`. `#[cfg(test)]` is an attribute
with `attr.path().is_ident("cfg")` whose parsed meta is a `Meta::List` containing a
**single nested path meta with ident `test`** (`#[cfg(test)]` only — compound forms
like `#[cfg(all(test, …))]` are a deferred refinement; see *Open Questions*).

Detection keys off attributes, **independent of the file path**.

## Behavior

- **Default:** test functions are excluded from `hotspots`. `--include-tests`
  disables all exclusion.
- **`scope.functions`** counts the **scored (non-test) functions**, and
  **`scope.skipped_tests`** counts the excluded test functions. (Total parsed
  functions = `functions + skipped_tests`.) This **refines** spec 001's definition
  of `scope.functions` ("functions in parsed files") to "*scored* functions"; before
  this change the two were identical (everything collected was scored), so the only
  observable effect is that `functions` no longer includes test functions and the
  excluded count moves to `skipped_tests`. A deliberate, documented wire-format
  refinement — not a silent change.
- **Module-level risks:** a top-level `#[cfg(test)]`-attributed `impl Drop` / `extern`
  block is excluded from `scope.risk_features` by default. (Risks nested inside a
  `#[cfg(test)] mod` are already not detected — top-level-only traversal, see *Scope*.)
- **`--include-tests`:** no exclusion — every function is scored, test-only module
  risks are included, and `skipped_tests` is `0`.
- **Summary roll-ups are unchanged and `Report::build` is not modified.** Filtering
  happens in the frontend *before* `Report::build`, so the summary is naturally
  computed over the surviving set: `summary.own_score`/`confidence` over the scored
  hotspots, and `summary.max_class`/`risk_weight` over those hotspots **and** the
  (non-skipped) `scope.risk_features` — exactly the existing semantics from 001.

## Architecture

- **Attributes live on the item, not the signature.** `FnUnit` currently stores a
  `syn::Signature`, but `#[test]`/`#[bench]` live on `syn::ItemFn.attrs`,
  `ImplItemFn.attrs`, `TraitItemFn.attrs`. So `functions::collect` computes
  `is_test` from the item's `.attrs` at collection time and stores a new
  **`is_test: bool`** on `FnUnit` (the raw attrs are not retained).
- **Thread the enclosing-`cfg(test)` flag.** `collect` already recurses
  `Item::Mod`; an `in_cfg_test: bool` parameter is threaded through the collection
  helpers (`collect_items`, `collect_from_impl`, `collect_from_trait`) so a method
  declared inside a `#[cfg(test)]` module is correctly marked `is_test`. A fn's
  `is_test` = (own `#[test]`/`#[bench]`) OR (`in_cfg_test`).
- **Config on the frontend struct.** `RustFrontend` becomes
  `#[derive(Default)] pub struct RustFrontend { pub include_tests: bool }`
  (`include_tests` defaults to `false`). The `Frontend::analyze` *trait method
  signature is unchanged* — policy rides on receiver state, not the interface.
- **`RustFrontend::analyze`:** when `!self.include_tests`, drop the `is_test`
  `FnUnit`s before scoring and accumulate their count into
  **`FrontendOutput.skipped_tests: usize`** (added next to `functions`).
  `detect_module_risks` likewise skips a top-level item carrying `#[cfg(test)]` when
  `!include_tests`.
- **Core:** `Scope` gains a `skipped_tests: usize` field, **declared between
  `functions` and `risk_features`** so the serialized order matches the schema below
  (serde emits fields in declaration order).
- **CLI:** add `include_tests: bool` with `#[arg(long)]` to the `Cmd::Scan` variant;
  build `RustFrontend { include_tests }`; map `FrontendOutput.skipped_tests` into
  `Scope.skipped_tests`.

### Required call-site migration (do not skip)

`RustFrontend` is presently a **unit struct** used as a bare value:
`RustFrontend.analyze(...)` in `crates/fxrank-cli/src/main.rs` and in the
`analyze_fixture` test helper (and every other test that constructs it). Once it has
a named field, `RustFrontend` is **no longer a valid value expression** and those
call sites become **compile errors**. They must be updated to
`RustFrontend::default().analyze(...)` (or `RustFrontend { include_tests: false }`).
This is a mechanical edit but it touches every existing frontend test call site —
the test *output* is unchanged (the fixtures are plain non-`#[test]` functions), but
the construction expressions must be migrated.

## Output schema change

`scope` gains one field (declared between `functions` and `risk_features`):

```json
"scope": { "input": "src", "files": 6, "parsed": 6, "functions": 31, "skipped_tests": 18, "risk_features": [] }
```

## Error Handling

No new failure modes. Test detection requires a parsed AST, so an un-parseable file
still becomes a `diagnostic` (it contributes nothing to `skipped_tests`).
`--include-tests` is a plain boolean flag (default false).

## Testing Strategy

- **Inline `#[cfg(test)]` module with multiple functions:** a fixture with
  `#[cfg(test)] mod tests { #[test] fn t() { std::fs::read("x"); } fn helper() { std::fs::read("y"); } }`
  next to a production fn that carries an effect → default `analyze` returns only the
  production fn with `skipped_tests == 2` (both the `#[test]` fn and the in-module
  helper are excluded); `RustFrontend { include_tests: true }` returns all three with
  `skipped_tests == 0`.
- **Impl method inside a `#[cfg(test)]` module:** `#[cfg(test)] mod t { struct S; impl S { fn m(&self){ std::fs::read("z"); } } }`
  → the method is excluded by default (verifies the `collect_from_impl` threading).
- **Top-level `#[test] fn`** → excluded by default.
- **Top-level `#[cfg(test)] impl Drop for T {}`** → **not** in `module_risks` by
  default; present with `--include-tests`.
- **CLI integration:** `fxrank scan <dir>` excludes tests and reports
  `scope.skipped_tests`; `--include-tests` includes them.
- **Regression:** existing frontend unit tests and `insta` snapshots produce
  unchanged *output* (their fixtures are plain functions). The only edits to existing
  tests are the mechanical `RustFrontend.analyze` → `RustFrontend::default().analyze`
  call-site migration.

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
| Detection | Attribute-based (`#[test]`/`#[bench]` + `#[cfg(test)]` module), computed in `collect` from item `.attrs` | Cheap, exact for the recognized forms, no type info, no path fragility. |
| `scope.functions` | Refined to "scored (non-test) functions"; excluded count moves to `skipped_tests` | Intuitive (`functions + skipped_tests` = parsed); documented refinement of 001, not a silent break. |
| Module-risk exclusion | Top-level `#[cfg(test)]` items only; no new recursion | Consistent with `detect_module_risks` being top-level-only today; nested-in-test-mod risks already undetected. |
| Path-based skip | Deferred | Un-attributed helpers in `tests/` are a smaller residual; defer until real scans justify the path heuristic. |
| Config location | `#[derive(Default)] RustFrontend { include_tests }` | Keeps the `Frontend::analyze` trait signature unchanged; call sites migrate to `::default()`. |
| Heuristic / panic tuning | Out of scope | FxRank is an effect profiler, not a linter; tune only with real false-positive data. |

## Open Questions

- Compound cfg predicates (`#[cfg(all(test, feature = "x"))]`, `cfg_attr`, a
  crate-level alias): v1 matches only the literal `#[cfg(test)]`. Generalize later if
  real code shows it matters.
- Should `--include-tests` eventually be expressible in a config file rather than a
  per-invocation flag? Out of scope now.
