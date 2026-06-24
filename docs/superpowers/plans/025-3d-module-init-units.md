# Phase 3d — Module-Init Units (import-time effects) Plan

> **Post-review note (historical plan):** this plan predates the root-model
> simplification. The module-init **effect-capture** parts are current; but its
> root statements (e.g. "module-init is_root") are **superseded** — roots are now
> CLI explicit-file (a `<module>` unit's root-ness follows its file's explicit-ness,
> not an automatic flag). The authoritative current behavior is in spec 025 §6/§13c
> + the guideline *Roots — the agent's observation focus*.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Surface **import-time effects** — code that runs when a module is loaded (a module-level `requests.get(...)`, `createClient(...)`, `logging.basicConfig(...)`, `os.environ[...]`). Today fxrank only scores `def`/`function` units, so top-level side effects are invisible. Add a synthetic **module-init unit** (symbol `<module>`) per file whose body is the module's top-level statements, scored like any unit.

**Architecture:** Both frontends' detectors already have **own-body semantics** (they do NOT recurse into nested functions). So running them over a body containing the module's top-level statements captures exactly the import-time effects (nested `def`/`function` bodies are skipped — those are their own units). TS's `FnBodyOwned::Block(Vec<Stmt>)` is owned (clone the top-level stmts); Python's `FnBody<'a>` borrows the CST (add a variant for the module top-level). Emit the module-init unit **only if it has ≥1 effect** (avoid a class-0 `<module>` per pure file). It participates in propagation + is a root candidate.

**Tech Stack:** Rust + the TS (swc) and Python (libcst) frontends. (Rust module-init is out of scope — Rust top-level is only `static`/`const` init, minimal executable code; noted deferred.)

## Global Constraints

- `fxrank-core` stays parser-free. The module-init unit is a normal `Hotspot`/`UnitRecord` (symbol `<module>`) — no new core type.
- **Output changes**: a new `<module>` hotspot appears for files with import-time effects. Snapshots capturing the dogfood fixtures MAY gain a `<module>` entry IF a fixture has top-level effects — re-accept and confirm the new entry is a legitimate module-init with real top-level effects. Per-function scores UNCHANGED (the module-init is additive; nested function units are unaffected).
- Emit the module-init unit ONLY if its own body has ≥1 effect (no noisy class-0 `<module>` for pure modules).
- Module-init `id` = `path:LINE:COL:<module>` (use the first top-level statement's line/col, or 1:1). `is_root`: a module-init IS import-time entry code → mark `is_root = true` (it runs on import). `qualified`/refs: extract refs from the top-level body so import-time calls to in-scope functions propagate.
- CI gates: fmt/clippy/test/slim-builds.
- Do NOT git-commit the SDD report file.

---

### Task 1: TS module-init unit

**Files:** `crates/fxrank-lang-ts/src/functions.rs` (collect the module-init unit) + `crates/fxrank-lang-ts/src/lib.rs` (score/emit it); test.

**Approach:** swc `Module.body: Vec<ModuleItem>`. The top-level executable statements are the `ModuleItem::Stmt(stmt)` items (NOT `ModuleItem::ModuleDecl` imports/exports — though an `export const x = createClient()` is an `ExportDecl` with a `Decl`; INCLUDE export-decl initializers if cheap, else at minimum the bare `Stmt` items). Build `FnBodyOwned::Block(<cloned top-level Stmts>)`. Create a synthetic `FnUnit { symbol: "<module>".into(), id: format!("{}:1:1:<module>", path), line: 1, col: 1, is_async: false, is_constructor: false, sig: <empty sig>, body, is_root: true }` (match the real `FnUnit` fields — check the struct). Run it through `analyze_unit` + `record_from_hotspot` like a normal unit. **Emit only if the resulting Hotspot has ≥1 effect** (`!h.effects.is_empty()`).

- [ ] **Step 1: Failing test** — scan/analyze a module:
  ```ts
  import { createClient } from './db';
  export const client = createClient();
  fetch('https://x');
  function pure() { return 1; }
  ```
  Assert: a `<module>` hotspot exists with effects (the top-level `fetch` → `net.fs.db`, and the `createClient()` call); `pure` is a separate unit with NO effects; the `<module>` unit's effects do NOT include anything from inside `pure` (own-body: top-level only). Also assert a PURE module (only imports + a function) produces NO `<module>` hotspot.
- [ ] **Step 2: Run** `cargo test -p fxrank-lang-ts` → FAIL.
- [ ] **Step 3: Implement** — collect the top-level Stmts into a `FnBodyOwned::Block`; build the synthetic unit; score + emit (guarded by `≥1 effect`). Thread it into `analyze_units` output (it's a normal unit + record, 1:1). Mind the React two-pass — the module-init is NOT a component/arrow, so it flows through the normal `analyze_unit` path; just add it to the unit list (or emit after the loop).
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-ts` → PASS; re-accept any dogfood snapshot that gains a legitimate `<module>` entry (confirm it's real top-level effects).
- [ ] **Step 5: Commit** `feat(ts): module-init unit for import-time effects`

---

### Task 2: Python module-init unit

**Files:** `crates/fxrank-lang-python/src/functions.rs` (`FnBody` variant + collect) + `crates/fxrank-lang-python/src/detect/mod.rs` or `lib.rs` (emit); test.

**Approach:** libcst `Module.body: Vec<Statement>`. Add a `FnBody` variant `Module(&'a [Statement])` (alongside `Suite`/`Expr`) that the own-body walker treats like a suite of statements (walking each, skipping nested `def`/`class` — the own-body recursion already stops at function/class boundaries). Build a synthetic `FnUnit { symbol: "<module>", body: FnBody::Module(&module.body), line: 1, col: 1, is_root: false, .. }` (match the real fields). Run through `gather`/`analyze_unit` + `build_record`. **Emit only if ≥1 effect.**
- VERIFY the own-body walker (the `walk_own_body`/`EffectSink` driver) handles the new `FnBody::Module` arm — it must walk the top-level statements and NOT descend into nested `def`/`class` (those are separate units). If the walker matches on `FnBody`, add the arm; if it can't skip nested defs at module level, that's the crux — make the module-init walk skip `FunctionDef`/`ClassDef` statements (their bodies are other units).

- [ ] **Step 1: Failing test** — analyze a module:
  ```python
  import os
  CONFIG = os.environ["X"]
  print("loading")
  def pure():
      return 1
  ```
  Assert: a `<module>` hotspot with effects (the `os.environ` read + `print`); `pure` is a separate unit; the `<module>` effects do NOT include anything from inside `pure`. A pure module (only `import` + `def`) → NO `<module>` hotspot.
- [ ] **Step 2: Run** `cargo test -p fxrank-lang-python` → FAIL.
- [ ] **Step 3: Implement** — add the `FnBody::Module` variant + walker arm; collect the synthetic unit; score + emit (guarded by ≥1 effect). Thread into the analyze output (hotspot + record).
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-python` → PASS; re-accept any dogfood snapshot gaining a legitimate `<module>`.
- [ ] **Step 5: Commit** `feat(python): module-init unit for import-time effects`

---

### Task 3: Dogfood + gate

- [ ] **Step 1: Dogfood (record)** — show import-time hotspots:
  - Python: `cargo run -q -p fxrank --no-default-features --features python -- scan /home/caasi/GitHub/django/django/conf/__init__.py | jq '[.hotspots[] | select(.symbol=="<module>")] | map({symbol, max_class, prop:.propagated_max_class})'` (or any module with top-level config code). Expect a `<module>` entry where the module does import-time work.
  - TS: scan a dir with module-level side effects (e.g. an app entry / a store setup); `... --features ts -- scan <dir> | jq '[.hotspots[] | select(.symbol=="<module>")] | length'`. Record observations (the `<module>` units correspond to real import-time effects, pure modules have none).
- [ ] **Step 2: Gate** — fmt/clippy/test (0 failed; snapshots re-accepted are legit `<module>` additions)/slim builds.
- [ ] **Step 3: Commit** snapshot re-accepts + fmt/clippy.

---

## Self-Review

**Spec coverage (3d):** TS module-init (Task 1) + Python module-init (Task 2) + dogfood (Task 3). Closes the import-time-effects blind spot. **Deferred:** Rust module-init (top-level is `static`/`const` only — minimal; noted). Top-level `await` / IIFE nuances beyond a straight statement walk are accepted approximations.

**Placeholder scan:** the body-construction crux is named per frontend (TS owned `FnBodyOwned::Block` clone; Python new `FnBody::Module` borrow variant + walker arm). The Python walker-arm requirement is flagged as the crux to verify, not hand-waved.

**Type consistency:** module-init is a normal `FnUnit`→`Hotspot`/`UnitRecord` with symbol `<module>`, `is_root: true`, emitted only if `≥1 effect`. Own-body semantics (skip nested defs) make it top-level-only. Per-function units unaffected; only an additive `<module>` hotspot per effectful file.
