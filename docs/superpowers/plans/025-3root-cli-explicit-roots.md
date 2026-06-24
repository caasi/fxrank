# Phase 3-root — CLI explicit-file roots (replace heuristic roots) Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Replace fxrank's per-language **heuristic** root detection (3b: Rust `fn main`/exports, TS framework files/bootstraps/memo, Python `__all__`/non-underscore) with one **language-neutral, CLI-level** rule: **a unit is a `root` iff its file was passed to the CLI as an explicit FILE argument (or stdin); files discovered by walking a DIRECTORY argument are NOT roots (they are resolution context).** `root` now means "the agent's chosen observation entry point," not "the program's real entry point." Authoritative behavior: `docs/cross-file-resolution-guideline.md` (*Roots — the agent's observation focus*) + spec 025 §6/§13c.

**Architecture:** `is_root` is annotation-only — the fold does NOT seed from it (Tarjan SCC over all nodes; `graph.roots()` is test-only). So it can be set entirely at the CLI level, post-dispatch: the CLI knows which paths were explicit file args; it sets `hotspot.root` + `record.is_root` from explicit-file membership. **Frontends become root-agnostic** — all per-language heuristic detection is removed. Every unit in an explicit file is a root (the whole file is the focus).

**Tech Stack:** Rust — `fxrank-cli` (the discovery seam), the 3 frontend crates (delete heuristics), `fxrank-core` (record/fold unchanged — `is_root`/`apply_fold` stay).

## Global Constraints

- `fxrank-core` parser-free; `UnitRecord.is_root`, `Hotspot.root`, `apply_fold`'s copy (`fold.rs:649`), and `graph.roots()` (test-only) all STAY unchanged.
- **Output change:** `hotspot.root` now reflects explicit-file membership, not heuristics. Snapshots that capture `root` may change (they don't — snapshot projections exclude `root`); the dogfood CLI output changes (e.g. `scan src/` → no roots; `scan a.ts` → a.ts units root). This is the intended behavior.
- CI gates: fmt/clippy/test/slim-builds.
- KEEP `default_export_symbol` (TS) — it's dual-purpose (module bindings), not only roots.
- Do NOT git-commit the SDD report file.

---

### Task 1: CLI sets `root` from explicit-file membership (the behavioral change)

**Files:** `crates/fxrank-cli/src/main.rs` (`run_scan` + `collect_source_files`/`walk_dir`); CLI tests.

**Approach:** `run_scan` already distinguishes: stdin (`-`), a single explicit FILE arg (~lines 170-181), and a DIRECTORY arg → `collect_source_files`→`walk_dir` (~182-200, 331-470).
- Build `explicit_files: HashSet<String>` during discovery: the stdin synthetic path (`"stdin"`) when stdin; a path arg that `is_file()` → its path string; directory-walked files → NOT added. (If the CLI accepts multiple path args, each file arg is explicit; each directory arg's walked files are not.)
- After `dispatch(...)` produces `output { functions: Vec<Hotspot>, records: Vec<UnitRecord> }` and BEFORE/around the fold, set for every record `record.is_root = explicit_files.contains(&record.path)` and for every hotspot `hotspot.root = explicit_files.contains(&hotspot.path)`. (Setting BOTH covers `--no-resolve`, where the fold/`apply_fold` is skipped; in the resolved path `apply_fold` re-copies `record.is_root`→`hotspot.root`, the same value — idempotent.)
- This OVERRIDES whatever the frontends currently set (Task 2 removes the now-dead frontend logic).

- [ ] **Step 1: Failing test** — `run_scan`-level tests: (a) scan a temp dir containing `a.rs` + `b.rs` as a DIRECTORY arg → NO hotspot has `root == true`. (b) scan `a.rs` as an explicit FILE arg → all of `a.rs`'s hotspots have `root == true`. (c) mixed: pass file `a.rs` AND directory `sub/` → `a.rs` units root, `sub/*` units not root. (d) stdin (`scan --lang ts -`) → the stdin unit(s) `root == true`. Run with and without `--no-resolve` for (b) to confirm consistency.
- [ ] **Step 2: Run** → FAIL (currently root is heuristic).
- [ ] **Step 3: Implement** — build `explicit_files`; set `record.is_root`/`hotspot.root` post-dispatch. (Thread `explicit_files` from discovery to the point where `output` is in hand; it's all within `run_scan`.)
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(cli): root = explicit file arg (language-neutral), not heuristic program-entry`

---

### Task 2: Remove the per-language heuristic root detection (cleanup)

**Files:** all 3 frontends. Records now emit `is_root: false` (the CLI sets the real value); delete the dead heuristic machinery + its tests.

**Rust** (`fxrank-lang-rust/src/functions.rs`, `detect/mod.rs`):
- Remove `FnUnit.is_root` field + the computations at the 3 sites (`has_export_attr() || (top_level && main)`, the two method sites) + `has_export_attr` (only used for roots) + the `top_level` threading IF only used for roots (verify — if `top_level` is also used elsewhere, keep it).
- `build_record`: set `is_root: false`.
- Remove test `is_root_main_and_exports`. Adjust `build_record_captures_own_and_refs`'s `!rec.is_root` assertion (now always false at the frontend — still passes, or remove that assertion line).

**TS** (`fxrank-lang-ts/src/lib.rs`, `functions.rs`):
- Remove `RootInfo` struct + `is_framework_root_file` + `contains_bootstrap_call` (bootstrap detection) + the memo-unwrap-for-root logic IF only used for roots. **KEEP `default_export_symbol`** (dual-purpose — module bindings). Remove the `h.root = is_root` / `record_from_hotspot(..., is_root, ...)` root-threading: pass `is_root: false` (or drop the param and default false in `record_from_hotspot`).
- Module-init: `record_from_hotspot(..., false, ...)` (CLI sets the real root).
- Remove tests: `framework_page_file_default_export_is_root`, `config_file_default_export_is_root`, `bootstrap_create_root_render_is_root`, `memo_wrapped_*_is_root_in_framework_file` (5 tests). Convert `module_init_record_is_root_true` to assert `record.is_root == false` at the frontend level (the CLI makes it root when the file is explicit) — OR move that assertion to a CLI-level explicit-file test. Keep the multi-dot-config / memo tests ONLY if they still test `default_export_symbol`/`is_framework_root_file` for a NON-root purpose; otherwise remove with the functions.
- NOTE: `is_framework_root_file`, `contains_bootstrap_call`, the memo-unwrap — if any is used ONLY for roots, delete it; the Explore inventory says all three are roots-only except `default_export_symbol`. Confirm before deleting each.

**Python** (`fxrank-lang-python/src/functions.rs`, `detect/mod.rs`):
- Remove `FnUnit.is_root` heuristic (the `__all__`/non-underscore computation, lines ~502-509) + `parse_all_names` (roots-only) + the `all_names`/`class_stack` threading IF only used for roots (verify `class_stack` isn't used elsewhere — it may be; keep what's shared).
- `build_record`: `is_root: false`. Module-init: `is_root: false`.
- Remove tests `is_root_with_all_list`, `is_root_without_all_convention`, `build_record_captures_own_and_refs`'s root assertion. KEEP `is_root_nested_def_always_false`? — under the new model a nested def in an explicit file IS a root (file-level), so this test's premise no longer holds; REMOVE it (roots are no longer about nesting). Convert/remove `module_init_record_is_root_true` like TS.

- [ ] **Step 1: Failing/red** — after deleting `FnUnit.is_root` etc., the crates won't compile until all references are cleaned. Work crate-by-crate to green.
- [ ] **Step 2: Implement** the removals; `build_record`/module-init emit `is_root: false`; delete/adjust the heuristic-root tests per above.
- [ ] **Step 3: Run** `cargo test -p fxrank-lang-rust -p fxrank-lang-ts -p fxrank-lang-python` → PASS; React + Python snapshots unchanged (root not serialized in projections).
- [ ] **Step 4: Run** `cargo clippy --workspace --all-targets -- -D warnings` → clean (no dead-code warnings from half-removed helpers).
- [ ] **Step 5: Commit** `refactor(frontends): remove heuristic root detection (root is now CLI explicit-file, set centrally)`

---

### Task 3: Docs — rewrite the roots model

**Files:** `docs/cross-file-resolution-guideline.md`, `docs/superpowers/specs/025-cross-file-resolution-and-propagation.md`, `CLAUDE.md`, `README.md`.

- Guideline *Roots per language* (~lines 229-270): REPLACE the per-language heuristics with: "A **root** is any unit whose file was passed to the CLI as an explicit FILE argument (or stdin) — the agent's chosen observation entry point. Files discovered by walking a DIRECTORY argument are not roots; they are resolution context. Language-neutral, set at the CLI discovery seam (`hotspot.root`/`record.is_root`), not by per-language heuristics." Update the high-level *Roots* intro (~50-52) + the module-init-as-root note (~294: module-init units are roots iff their file is explicit, like any other unit).
- Spec 025: update the root mentions (§ wherever roots/`is_root` are described, incl. the §13a "3b roots" line) to the explicit-file model. Add a short note in §13b/Known-Limitations that the heuristic root model (3b) was replaced during review by the explicit-file model (the agent-observation-entry clarification).
- CLAUDE.md: update the `root` annotation description (the Project intro + the scan-flow note) to "root = explicit CLI file arg, set at the discovery seam."
- README: update the Milestone-B `root` description ("entry-point annotation" → "the files you explicitly named — your observation focus; directory-walked files are context").

- [ ] **Step 1:** Rewrite each doc section per above.
- [ ] **Step 2: Commit** `docs(025): roots are explicit CLI file args (agent observation focus), not heuristic program entries`

---

### Task 4: Dogfood + gate

- [ ] **Step 1: Dogfood (record)** — show the new semantics:
  - `cargo run -q -p fxrank --features rust -- scan crates/fxrank-cli/src | jq '[.hotspots[]|select(.root)]|length'` → directory scan → **0 roots**.
  - `cargo run -q -p fxrank --features rust -- scan crates/fxrank-cli/src/main.rs | jq '[.hotspots[]|select(.root)]|length'` → explicit file → all main.rs units root.
  - A mixed `scan <file> <dir>` → file units root, dir units not. Record observations.
- [ ] **Step 2: Gate** — fmt/clippy/test (0 failed)/slim builds.
- [ ] **Step 3: Commit** any fmt/clippy/snapshot touch-ups.

---

## Self-Review

**Spec coverage:** CLI explicit-file roots (Task 1) + remove heuristics (Task 2) + docs (Task 3) + dogfood (Task 4). Makes the convergence-tail root findings (N2 pre-fold consistency, N3 anonymous default exports, Codex class-decorators) all MOOT — they were heuristic-root edge cases.

**Decision flagged for review:** *all* units in an explicit file are roots (not just top-level) — "the whole file is the focus." If only top-level/exported units should be roots, that's a refinement (raise it).

**Type consistency:** `record.is_root`/`hotspot.root` set by the CLI from `explicit_files: HashSet<String>`; frontends emit `is_root: false`; `apply_fold`'s copy + `graph.roots()` (test-only) unchanged. `default_export_symbol` kept (dual-purpose).
