# Phase 3b — Roots (visible entry points) Plan

> **⚠️ SUPERSEDED (historical plan).** This per-language heuristic root model
> (Rust `fn main`/exports, TS framework files/bootstraps, Python `__all__`/
> non-underscore) was **removed during the spec-025 review** and replaced by the
> CLI explicit-file rule (root = a unit whose file was an explicit CLI FILE arg;
> the agent's observation focus). It is kept only as a record of the abandoned
> approach + its corpus insights. **Authoritative current behavior:** spec 025
> §6/§13c + the guideline *Roots — the agent's observation focus* + plan
> `025-3root-cli-explicit-roots.md`. Do not implement from this doc.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Compute the real `is_root` flag per frontend (currently always `false`), so `fxrank scan` annotates **visible entry points** (`root: true` in output) — realizing the "grow scores from roots" model's annotation. Scoped to the **tractable syntactic/path-based root signals** (the corpus-validated rules in `docs/cross-file-resolution-guideline.md` *Roots per language*); the module-tree-dependent Rust `pub`-visibility chain and Python `console_scripts` (need Cargo/pyproject parsing) are deferred to the module-tree / config tasks.

**Architecture:** Each frontend computes `is_root` for each unit when building its `UnitRecord` (`build_record`/`record_from_hotspot`), and the fold already copies `record.is_root → hotspot.root` (`apply_fold`). `is_root` is an **annotation** — it does NOT change scores (ranking is by propagated score; root is a flag). So this is additive output only.

**Tech Stack:** Rust + the three frontend crates (syn/libcst/swc) + the CLI for the scan path.

## Global Constraints

- `fxrank-core` stays parser-free. `is_root` is a per-record/per-hotspot bool the frontend computes; the fold/core treat it as opaque.
- **`Hotspot.root` IS serialized** (added in phase 1) — it currently emits `false` for every hotspot; this task flips real entry points to `true`. So scan OUTPUT changes (some hotspots gain `root:true`) — **snapshots that capture `root` will churn and must be re-accepted** (`cargo insta accept`); confirm the churn is ONLY `root: false→true` on genuine entry points, nothing else. Own-body scores unchanged.
- CI gates: fmt/clippy/test/slim-builds.
- Roots rules: `docs/cross-file-resolution-guideline.md` *Roots per language*. Defer module-tree-dependent Rust `pub`-chain + Python `console_scripts` (config task).
- Do NOT git-commit the SDD report file.

---

### Task 1: Rust roots — `fn main` + exported symbols

**Files:** `crates/fxrank-lang-rust/src/functions.rs` (or `detect/mod.rs::build_record`); test.

**Interfaces:** `is_root = true` for: a free `fn main` (the binary entry), and any item carrying `#[no_mangle]` / `#[export_name]` / `#[wasm_bindgen]` (FFI/wasm exports). All others `false` (the `pub`-visibility chain needs the module tree — DEFERRED). Compute it at collection time (the `FnUnit` already has the `syn` item attrs available — or thread a `is_root` flag onto `FnUnit`) and carry it into `build_record`.

- [ ] **Step 1: Failing test** — parse `fn main() {}  #[no_mangle] pub extern "C" fn ext() {}  fn helper() {}`; build records; assert `main` and `ext` have `is_root == true`, `helper` `false`.
- [ ] **Step 2: Run** → FAIL (all false).
- [ ] **Step 3: Implement** — add `is_root` to `FnUnit` (computed in `functions::collect`: `symbol == "main"` at module top level OR the item's attrs contain `no_mangle`/`export_name`/`wasm_bindgen`), and set `UnitRecord.is_root = unit.is_root` in `build_record` (replace the `false` stub). (Inspect `f.attrs` for the attr path's last segment.)
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-rust` → PASS. Re-accept any snapshot that now shows `root:true` on `main`/exports (`cargo insta accept` if needed; confirm ONLY root flips).
- [ ] **Step 5: Commit** `feat(rust): roots — fn main + #[no_mangle]/#[wasm_bindgen] exports`

---

### Task 2: TS roots — framework files + source bootstraps

**Files:** `crates/fxrank-lang-ts/src/lib.rs` / `functions.rs` (root detection); test.

**Interfaces:** `is_root = true` for units in a **framework-convention file** (by path/basename: `**/page.tsx`, `**/layout.tsx`, `**/template.tsx`, `**/loading.tsx`, `**/error.tsx`, `**/not-found.tsx`, `**/route.ts`, `**/middleware.ts`, `*.config.{ts,js,mjs,cjs}`) — the DEFAULT export of such a file is a root; AND units that are **source bootstraps** (a module whose top level calls `createRoot(...).render(...)` or `ReactDOM.render(...)`). Other units `false`. (Package.json `bin` needs reading package.json — defer to config task; framework-file + bootstrap are the tractable signals.)

- [ ] **Step 1: Failing test** — scan/collect a unit from a path ending `app/dashboard/page.tsx` with a default export → its default-export unit `is_root == true`; a unit in `components/Foo.tsx` → `false`; (optionally) a module with top-level `createRoot(el).render(<App/>)` → that module's relevant unit/bootstrap `is_root == true`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — a helper `fn is_framework_root_file(path: &str) -> bool` (basename/suffix match per the list); in the analyze/collect path, set `is_root` for the file's default-export unit when the file is a framework file. For bootstraps, detect a top-level `createRoot(...).render(...)` / `ReactDOM.render(...)` call (a small body/module check) and flag the enclosing module-scope unit. Carry `is_root` into `record_from_hotspot`/the record. (Keep it simple — framework-file detection is the primary signal; bootstrap detection is secondary, do it if cheap.)
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-ts` → PASS; re-accept snapshots showing `root:true` on framework-file default exports (confirm ONLY root flips).
- [ ] **Step 5: Commit** `feat(ts): roots — framework-convention files + bootstraps`

---

### Task 3: Python roots — `__main__` + `__all__` + non-underscore

**Files:** `crates/fxrank-lang-python/src/lib.rs` / `functions.rs` / a roots helper; test.

**Interfaces:** `is_root = true` for: (a) units reachable from an `if __name__ == "__main__":` block (or simpler: mark the module's top-level statements/the unit a `__main__` block calls — at minimum detect the presence of a `__main__` guard and flag... see Step 3); (b) a `def`/`class` whose name is in a **static-literal `__all__`** list; (c) fallback when no `__all__`: a **non-underscore** module-level `def`/`class` (public by convention). Nested defs / underscore-prefixed → `false`. (Tiered per the guideline; `console_scripts` from pyproject is DEFERRED to the config task.)

- [ ] **Step 1: Failing test** — parse a module with `__all__ = ["pub_fn"]`, `def pub_fn(): pass`, `def _priv(): pass`, `def other(): pass`; assert `pub_fn` (in `__all__`) `is_root == true`, `_priv` `false`. Then a module WITHOUT `__all__`: `def public(): pass`, `def _hidden(): pass` → `public` `true` (non-underscore convention), `_hidden` `false`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — a roots pass over the module: parse a static-literal `__all__` (a list/tuple of string literals) into a set; if present, module-level `def`/`class` whose name ∈ `__all__` → root. If `__all__` absent or dynamic, module-level `def`/`class` with a non-underscore name → root. (Optionally: also flag units inside/called-by an `if __name__=="__main__"` block as roots — do if cheap; else note as deferred.) Nested defs are not module-level → not roots. Thread `is_root` into `build_record`.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-python` → PASS; re-accept snapshots showing `root:true` (confirm only root flips).
- [ ] **Step 5: Commit** `feat(python): roots — static __all__ + non-underscore convention (+__main__)`

---

### Task 4: Dogfood roots + gate

- [ ] **Step 1: Dogfood (record)** — for each frontend, scan a real target and count/show roots:
  - Rust: `cargo run -q -p fxrank --no-default-features --features rust -- scan /dev/shm/fxrank-025/crates/fxrank-cli/src | jq '[.hotspots[] | select(.root)] | map(.symbol)'` — expect `main` (and any exports).
  - Python: `... --features python -- scan /home/caasi/GitHub/django/django/shortcuts.py | jq '[.hotspots[] | select(.root)] | map(.symbol)'` — expect the public non-underscore API (`render`, `redirect`, …).
  - TS: `... --features ts -- scan /home/caasi/GitLab/omni/exp-app-element/src | jq '[.hotspots[] | select(.root)] | length'` — expect the framework/component roots count > 0 (or a fixtures dir if omni absent).
  Record observations (roots look like real entry points, not random functions).
- [ ] **Step 2: Gate** — fmt/clippy/test (0 failed; snapshots re-accepted show ONLY `root` flips)/slim builds.
- [ ] **Step 3: Commit** any snapshot re-accepts + fmt/clippy.

---

## Self-Review

**Spec coverage (3b):** tractable roots per language (Tasks 1-3); dogfood (Task 4). **Deferred:** Rust `pub`-visibility chain + crate-type gate (needs module-tree + Cargo metadata → module-tree task); TS `package.json.bin` + Python `console_scripts` (need config parsing → config task).

**Placeholder scan:** each task names the concrete detection (attr names / path patterns / `__all__` parse). The Python `__main__`-block reachability is marked "do if cheap, else note deferred" — that's an honest scope flag, not a vague TODO.

**Type consistency:** `is_root: bool` on `FnUnit`/record set by each frontend; `apply_fold` already copies `record.is_root → hotspot.root` (serialized). Annotation only — no score change. Snapshot churn limited to `root: false→true` on genuine entry points.
