# Skip Test Code by Default — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Exclude test code (`#[test]`/`#[bench]` fns and functions inside `#[cfg(test)]` modules) from FxRank scoring by default, report a `scope.skipped_tests` count, and add a `--include-tests` flag — so production effect-cost hotspots surface instead of being buried under test `assert!`→`panic` noise.

**Architecture:** Detect test code syntactically in the Rust frontend (`functions::collect` computes a new `FnUnit.is_test` from item attributes). `RustFrontend` gains an `include_tests` flag (struct field — the `Frontend::analyze` trait signature is unchanged); when false, `analyze` drops `is_test` units and counts them into a new `FrontendOutput.skipped_tests`, and `detect_module_risks` skips top-level `#[cfg(test)]` items. The count surfaces as a new `Scope.skipped_tests` wire field. The CLI exposes `--include-tests`.

**Tech Stack:** Rust 2024, `syn` 2 (attribute inspection), `serde`/`serde_json`, `clap`.

Spec: `specs/002-skip-test-code.md` — source of truth. When this plan and the spec disagree, the spec wins; fix the plan.

**Baseline (the "before", captured by dogfooding):** `fxrank scan crates/fxrank-lang-rust/src` currently ranks the inline `#[cfg(test)]` unit tests `qualified_builtins_match_on_last_segment`, `import_table_handles_groups`, `import_table_resolves_aliases_and_flags_glob` as the **top 3 hotspots** (all assert-only `panic`). Task 5 asserts these are gone and `scope.skipped_tests > 0`.

## Conventions

- **TDD**: failing test → red → minimal impl → green → commit. Stage explicitly (`git add <paths>`); never `git commit -am`.
- Gate before each task's final commit: `cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`.
- Implementation goes on a **feature branch in a worktree** (RAM disk), never on `main`.

## File Structure

```text
crates/fxrank-core/src/model.rs        # Scope gains skipped_tests (between functions and risk_features)
crates/fxrank-core/src/frontend.rs     # FrontendOutput gains skipped_tests
crates/fxrank-lang-rust/src/functions.rs   # FnUnit gains is_test; collect computes it
crates/fxrank-lang-rust/src/lib.rs     # RustFrontend { include_tests }; analyze filters + counts
crates/fxrank-lang-rust/src/detect/risk.rs # detect_module_risks(file, path, include_tests)
crates/fxrank-cli/src/main.rs          # --include-tests flag; map skipped_tests into Scope
crates/fxrank-lang-rust/tests/fixtures/skip_tests.rs  # new fixture
crates/fxrank-lang-rust/tests/rust_frontend.rs        # migrate analyze_fixture + new tests
crates/fxrank-cli/tests/cli.rs         # CLI integration test
```

---

### Task 1: Core wire fields — `skipped_tests`

**Files:** Modify `crates/fxrank-core/src/model.rs`, `crates/fxrank-core/src/frontend.rs`.

- [ ] **Step 1: Failing test** — add to `model.rs` tests that `skipped_tests` serializes in order between `functions` and `risk_features`.

```rust
#[test]
fn scope_serializes_skipped_tests_between_functions_and_risk_features() {
    let report = Report::build(
        Scope { input: "f".into(), files: 1, parsed: 1, functions: 2, skipped_tests: 3, risk_features: vec![] },
        vec![], vec![], None,
    );
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("\"functions\":2,\"skipped_tests\":3,\"risk_features\":"));
}
```

- [ ] **Step 2: Run red** — `cargo test -p fxrank-core scope_serializes_skipped_tests` → FAIL (missing field).

- [ ] **Step 3: Implement** — add `pub skipped_tests: usize` to `Scope`, declared **between `functions` and `risk_features`** (serde emits in declaration order). Set it to `0` in `Scope::empty`. Add `pub skipped_tests: usize` to `FrontendOutput` in `frontend.rs` (it derives `Default`, so `0`). Update every `Scope { .. }` literal in existing `model.rs` tests to include `skipped_tests: 0`.

- [ ] **Step 4: Run green** — `cargo test -p fxrank-core` → PASS (all model tests).

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/model.rs crates/fxrank-core/src/frontend.rs
git commit -m "feat(core): add scope.skipped_tests / FrontendOutput.skipped_tests"
```

---

### Task 2: Detect test code — `FnUnit.is_test`

**Files:** Modify `crates/fxrank-lang-rust/src/functions.rs`; create `crates/fxrank-lang-rust/tests/fixtures/skip_tests.rs`; add a test to `crates/fxrank-lang-rust/tests/rust_frontend.rs`.

- [ ] **Step 1: Fixture** — `crates/fxrank-lang-rust/tests/fixtures/skip_tests.rs`:

```rust
fn prod(p: &std::path::Path) { let _ = std::fs::read(p); }   // production: net.fs.db

#[test]
fn free_test() { assert!(true); }

#[cfg(test)]
mod tests {
    fn helper() { let _ = std::fs::read("x"); }              // in-module helper (no #[test])
    struct S;
    impl S { fn method(&self) { assert_eq!(1, 1); } }        // method inside cfg(test) mod
}
```

- [ ] **Step 2: Failing test** — `functions::collect` marks the right units `is_test`. (`functions::collect` is `pub`.)

```rust
#[test]
fn collect_marks_test_code() {
    let text = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/skip_tests.rs")).unwrap();
    let file = syn::parse_file(&text).unwrap();
    let units = fxrank_lang_rust::functions::collect(&file, "skip_tests.rs");
    let by = |s: &str| units.iter().find(|u| u.symbol == s).map(|u| u.is_test);
    assert_eq!(by("prod"),       Some(false));
    assert_eq!(by("free_test"),  Some(true));   // #[test]
    assert_eq!(by("helper"),     Some(true));   // inside #[cfg(test)] mod (symbols are NOT module-prefixed)
    assert_eq!(by("S::method"),  Some(true));   // method inside #[cfg(test)] mod
}
```
Note: `functions::collect` does **not** prefix in-module items with the module name
(verified by running `fxrank scan` on this fixture — the helper's symbol is `helper`,
not `tests::helper`). The unit's `id` (`path:line:symbol`) still disambiguates a
same-named function in another scope.

- [ ] **Step 3: Run red.**

- [ ] **Step 4: Implement** in `functions.rs`:
  - Add `pub is_test: bool` to `FnUnit`.
  - Helpers:
    ```rust
    fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|a| a.path().is_ident("test") || a.path().is_ident("bench"))
    }
    fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|a| {
            a.path().is_ident("cfg")
                && a.parse_args::<syn::Path>().map(|p| p.is_ident("test")).unwrap_or(false)
        })
    }
    ```
  - Thread an `in_cfg_test: bool` parameter through `collect_items` → `collect_from_impl` / `collect_from_trait` (and the recursion into `Item::Mod`). When descending into an `Item::Mod` whose attrs satisfy `is_cfg_test`, pass `in_cfg_test = true` (OR with the inherited value). Top-level `collect` starts `in_cfg_test = false`.
  - For each function item, set `is_test = in_cfg_test || has_test_attr(&item.attrs)`. (Attributes are on `ItemFn.attrs` / `ImplItemFn.attrs` / `TraitItemFn.attrs` — **not** on the signature.)
  - `RustFrontend::analyze` (Task 3) is the only consumer that acts on `is_test`; for now nothing else changes behavior.

- [ ] **Step 5: Run green** — `cargo test -p fxrank-lang-rust collect_marks_test_code` and the full frontend suite.

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-rust/src/functions.rs crates/fxrank-lang-rust/tests/
git commit -m "feat(rust): detect test code (is_test) from #[test]/#[bench]/#[cfg(test)]"
```

---

### Task 3: Skip in `analyze` + module risks + the `RustFrontend` migration

**Files:** Modify `crates/fxrank-lang-rust/src/lib.rs`, `crates/fxrank-lang-rust/src/detect/risk.rs`, and migrate `RustFrontend` call sites in `crates/fxrank-cli/src/main.rs` + `crates/fxrank-lang-rust/tests/rust_frontend.rs`.

- [ ] **Step 1: Failing test** — default skips test code and counts it; `--include-tests` keeps it.

```rust
#[test]
fn default_skips_tests_and_counts_them() {
    let out = analyze_fixture("skip_tests.rs");                 // default: include_tests = false
    let syms: Vec<_> = out.functions.iter().map(|f| f.symbol.clone()).collect();
    assert!(syms.contains(&"prod".to_string()));
    assert!(!syms.iter().any(|s| s == "free_test" || s.contains("helper") || s == "S::method"));
    assert_eq!(out.skipped_tests, 3);                           // free_test + helper + S::method
}
#[test]
fn include_tests_keeps_everything() {
    let out = RustFrontend { include_tests: true }.analyze(&[source_of("skip_tests.rs")]);
    assert_eq!(out.skipped_tests, 0);
    assert!(out.functions.iter().any(|f| f.symbol == "free_test"));
}
```
(Provide a `source_of(name)` helper, or reuse the fixture-loading the existing helper uses.)

- [ ] **Step 2: Run red** — fails to compile first (struct change), which drives Step 3's migration.

- [ ] **Step 3: Migrate `RustFrontend` + implement the filter.**
  - In `lib.rs`: `#[derive(Default)] pub struct RustFrontend { pub include_tests: bool }`.
  - **Migrate every call site** (`RustFrontend` is currently a unit struct used as a bare value, so these are now compile errors):
    - `crates/fxrank-cli/src/main.rs`: `RustFrontend.analyze(...)` → `RustFrontend { include_tests }.analyze(...)` (the flag arrives in Task 4; for now `RustFrontend::default()` is fine and Task 4 wires the real flag).
    - `crates/fxrank-lang-rust/tests/rust_frontend.rs`: the `analyze_fixture` helper's `RustFrontend.analyze(...)` → `RustFrontend::default().analyze(...)`. Grep for any other `RustFrontend.` usages and migrate them too.
  - In `analyze`: after `functions::collect`, when `!self.include_tests`, partition out `is_test` units, set `output.skipped_tests += <count skipped>`, and score only the rest; when `self.include_tests`, score all and leave `skipped_tests = 0`.
  - Pass `self.include_tests` into `detect_module_risks(&file, &f.path, self.include_tests)`.

- [ ] **Step 4: Module-risk skip** — in `detect/risk.rs`, change `detect_module_risks(file, path)` → `detect_module_risks(file, path, include_tests: bool)`; for each top-level item it would report (`impl Drop`, `unsafe impl`, `extern` block), skip it when `!include_tests && is_cfg_test(&item_attrs)`. (Reuse / mirror the `is_cfg_test` check; each `syn::Item::{Impl,ForeignMod}` has `.attrs`.) Add a test:

```rust
#[test]
fn cfg_test_module_risks_skipped_by_default() {
    let src = "#[cfg(test)] impl Drop for T {} \n #[cfg(test)] unsafe impl Send for T {}";
    let def = RustFrontend::default().analyze(&[SourceFile { path: "m.rs".into(), text: src.into() }]);
    assert!(def.module_risks.is_empty());
    let inc = RustFrontend { include_tests: true }.analyze(&[SourceFile { path: "m.rs".into(), text: src.into() }]);
    assert_eq!(inc.module_risks.len(), 2);
}
```

- [ ] **Step 5: Run green** — full frontend suite + the existing tests (now via `RustFrontend::default()`), all snapshots unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-rust/src/lib.rs crates/fxrank-lang-rust/src/detect/risk.rs \
        crates/fxrank-cli/src/main.rs crates/fxrank-lang-rust/tests/rust_frontend.rs
git commit -m "feat(rust): skip test code by default; RustFrontend.include_tests; module-risk skip"
```

---

### Task 4: CLI `--include-tests` + wire `skipped_tests`

**Files:** Modify `crates/fxrank-cli/src/main.rs`; add a test to `crates/fxrank-cli/tests/cli.rs`.

- [ ] **Step 1: Failing integration test** (`assert_cmd`): a temp `.rs` with a `#[cfg(test)] mod tests { #[test] fn t() { std::fs::read("x"); } }` + a production fn:
  - `fxrank scan <file>` → JSON `scope.skipped_tests >= 1`, and no hotspot named `t`.
  - `fxrank scan <file> --include-tests` → `scope.skipped_tests == 0`, and `t` present.

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** — add `#[arg(long)] include_tests: bool` to the `Cmd::Scan` variant; build `RustFrontend { include_tests }`; set `Scope { ..., skipped_tests: output.skipped_tests, .. }` when constructing the report. (For the `#[cfg(not(feature = "rust"))]` branch, `skipped_tests` stays `0`.)

- [ ] **Step 4: Run green** — `cargo test -p fxrank`.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-cli/src/main.rs crates/fxrank-cli/tests/cli.rs
git commit -m "feat(cli): --include-tests flag; surface scope.skipped_tests"
```

---

### Task 5: Dogfood verification (before → after)

**Files:** none (verification); optionally a note in `crates/fxrank-cli/tests/cli.rs`.

- [ ] **Step 1: Build + scan** — `cargo build -p fxrank && ./target/debug/fxrank scan crates/fxrank-lang-rust/src | tee /tmp/fxrank-after.json`.

- [ ] **Step 2: Assert the cleanup** — the previously-top-3 inline test functions are gone, and the count is reported:

```bash
jq '{
  skipped_tests: .scope.skipped_tests,
  test_fns_still_present: [.hotspots[].symbol
    | select(. == "qualified_builtins_match_on_last_segment"
          or . == "import_table_handles_groups"
          or . == "import_table_resolves_aliases_and_flags_glob")]
}' /tmp/fxrank-after.json
```
Expected: `skipped_tests` > 0 (the inline `#[cfg(test)]` test fns), and `test_fns_still_present` is `[]`. Confirm the top hotspots are now production functions (`run_scan`/`walk_dir` when scanning the CLI, or the visitor/`walk_tree` functions here).

- [ ] **Step 3: Full gate** — `cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 4: (optional) Dogfood guard** — if cheap, add a CLI test that `scan crates/` yields `scope.skipped_tests > 0`, so a regression that stops skipping tests is caught.

---

## Verification (feature complete)

- [ ] `cargo test --workspace` (core + frontend + cli), `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` all green.
- [ ] `cargo build -p fxrank --no-default-features` still compiles (the no-frontend branch sets `skipped_tests: 0`).
- [ ] `fxrank scan crates/fxrank-lang-rust/src`: the inline `#[cfg(test)]` test functions no longer appear in `hotspots`, `scope.skipped_tests` reflects them, and production hotspots surface — matching the spec's worked outcome.
- [ ] Existing `insta` snapshots unchanged (fixtures are plain functions).

Then open a PR linking caasi/dong3#51 (or note this is Milestone-A follow-up 002).
