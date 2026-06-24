# Cross-file Resolution — Phase 2c (TS/JS frontend records) Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make the **TS/JS** frontend (swc) emit `UnitRecord`s (call-site extraction + `qualified` tag), so `fxrank scan <ts>` produces real propagation. The `.ts`/`.tsx` dialects pool automatically (all `Language::Ts` → one partition group). The driver/fold/resolve and the per-language partition (2b) are reused unchanged.

**Architecture:** Mirrors Rust/Python phase-2, with ONE twist: the TS **React two-pass** (`analyze_units`) suppresses inherited-callback arrows (they get no standalone Hotspot; their signals are absorbed into the owning component). Records MUST stay **1:1 with Hotspots** (a record iff a Hotspot), and each record's own-body must equal the FINAL Hotspot's — so we **build each record from the final Hotspot** (copy `effects`/`risks`/async/await) + `refs::extract(unit)`, rather than re-running `gather` (which wouldn't include a component's absorbed inherited signals).

**Tech Stack:** Rust, `swc` (TS/JS AST), the phase-1/2/2b `fxrank-core` fold + `resolve` + `graph`.

## Global Constraints

- `fxrank-core` stays parser-free AND free of language syntax in resolution (the `qualified` judgment stays in the frontend).
- **Own-body `fxrank scan` output stays byte-identical** — records are additive; the fold only adds propagated fields by `unit_id`. Existing TS snapshot tests (incl. the React fixtures) must pass unchanged.
- CI gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Build slim: `cargo build -p fxrank --no-default-features --features ts`.
- Effect/risk vocab centralized — no hand-written wire strings/class numbers.
- **Records 1:1 with Hotspots** — a record is emitted exactly when a Hotspot is (suppressed React-callback arrows produce neither).
- **DO NOT git-commit the SDD report file** (it's gitignored scratch) — only commit code.

---

### Task 1: TS `detect::refs` — call-reference extraction

**Files:** Create `crates/fxrank-lang-ts/src/detect/refs.rs`; modify `crates/fxrank-lang-ts/src/detect/mod.rs` (`pub mod refs;`).

**Interfaces:** `pub fn extract(body: &FnBodyOwned, imports: &ImportTable, lines: &SpanLines) -> Vec<fxrank_core::record::CallSiteRef>`.

**Pattern to mirror:** `crates/fxrank-lang-ts/src/detect/calls.rs` — the `CallWalker` (`body.walk_with(&mut walker)`, `visit_call_expr` uses `render_expr(callee)` for the dotted base, `self.lines.line(node.span)` for the line). Read it. Your walker records a `CallSiteRef` for every call with a renderable callee instead of classifying effects. Reuse `render_expr`/`render_member` (copy them or make `pub(crate)`).

For each `CallExpr` with `Callee::Expr(callee)` where `render_expr(callee) = Some(base)`:
- `root = base.split('.').next().unwrap()`; `module = imports.resolve(root).map(str::to_string)`.
- **`qualified = module.is_some()`** — the TS rule (same shape as Python): qualified iff the leading name resolves to an ES import (`useState`→`react`, `React`→`react`, `fs`→`node:fs`, relative `./util`). Bare globals (`fetch`, `foo`) and member calls on non-imported receivers (`obj.method`) → `qualified = false`.
- `kind = RefKind::Method` if `base.contains('.')` AND `module.is_none()` (a member call on a non-imported receiver, e.g. `obj.method`/`this.foo`); else `RefKind::Free`.
- `line = lines.line(node.span)`; `col`: from the span via `lines` (mirror how calls.rs / functions.rs get col from a span; if only line is readily available, set col best-effort — line is the important one).
- Recurse (`node.visit_children_with(self)`) so nested calls (`f(g())`, `a.b().c()`) are captured.

- [ ] **Step 1: Failing test** — parse a module + grab a fn body (use the crate's parse+collect test helpers — see existing `detect` tests). Source:
  ```ts
  import { useState } from 'react';
  import fs from 'node:fs';
  function f() { useState(); fs.readFile('x'); obj.method(); fetch('y'); bare(); }
  ```
  Assert refs include: `{base:"useState", module:Some("react"), qualified:true}`; `{base:"fs.readFile", module:Some("node:fs"), qualified:true}`; `{base:"obj.method", module:None, qualified:false, kind:Method}`; `{base:"fetch", module:None, qualified:false}`; `{base:"bare", module:None, qualified:false}`.
- [ ] **Step 2: Run** `cargo test -p fxrank-lang-ts` → FAIL (no `refs`).
- [ ] **Step 3: Implement** the walker; add `pub mod refs;`.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-ts` → PASS.
- [ ] **Step 5: Commit** `feat(ts): detect::refs — extract outgoing call references`

---

### Task 2: TS records — `record_from_hotspot` + emit through the React two-pass

**Files:** Modify `crates/fxrank-lang-ts/src/detect/mod.rs` (add a record builder); `crates/fxrank-lang-ts/src/lib.rs` (`analyze` + `analyze_units` — thread records).

**Interfaces:**
- `pub fn record_from_hotspot(unit: &FnUnit, h: &Hotspot, imports: &ImportTable, lines: &SpanLines) -> fxrank_core::record::UnitRecord` — builds a record whose own-body is COPIED from the final Hotspot `h` (`effects: h.effects.clone()`, `risks: h.risk_features.clone()`, `async_boundary: h.async_boundary`, `await_count: h.await_count`), `unit_id: h.id.clone()`, `path/line/col/symbol` from `unit`, `language: Language::Ts`, `is_root: false`, `export: None`, `refs: refs::extract(&unit.body, imports, lines)`. This GUARANTEES record own-data == Hotspot own-data (incl. a component's absorbed inherited signals) by construction.
- `analyze_units` gains a `records: &mut Vec<UnitRecord>` out-param (alongside `out: &mut Vec<Hotspot>`), and `analyze` passes `&mut output.records`.

**The React-two-pass threading (the crux — read `analyze_units` in lib.rs:125-187 carefully):**
- Pass 1 unchanged.
- Pass 2: the SUPPRESSED inherited-callback arrows (`if let Some((comp_id, phase)) = inherited.get(&key) { … continue; }`) must produce NO record — leave the `continue` as-is (no record for them). Their effects are absorbed into the component's Hotspot, and thus into the component's record (since the record is built from the final Hotspot AFTER `absorb_inherited`).
- After the `absorb_inherited` loop (so component Hotspots are final), when emitting in `order`: for each `id`, you have the final Hotspot `h = by_id[id]`. You ALSO need the `unit` for that id (to call `refs::extract` and get path/col). Build an `id -> &FnUnit` map in Pass 2 (or keep units indexed) so you can call `record_from_hotspot(unit, h, imports, lines)` and push it to `records` in the SAME order/loop that pushes `h` to `out`. Push a record iff you push a Hotspot.

- [ ] **Step 1: Failing test** — (a) a `detect`-level test: build a unit + its Hotspot, call `record_from_hotspot`, assert `record.effects == hotspot.effects`, `unit_id == hotspot.id`, `language == Ts`, refs populated, `!is_root`. (b) Extend or add a `lib.rs`-level test (using the existing React/TS fixtures or inline source) asserting that `analyze_units` produces `records.len() == out.len()` (1:1) AND that a suppressed inherited-callback arrow has NO record (records contain the component id but not the arrow's id).
- [ ] **Step 2: Run** `cargo test -p fxrank-lang-ts` → FAIL.
- [ ] **Step 3: Implement** `record_from_hotspot` + thread `records` through `analyze_units` (build the `id->unit` map; emit records 1:1; suppressed arrows excluded). `analyze` passes `&mut output.records`.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-ts` → PASS; **existing TS snapshots (incl. React fixtures) UNCHANGED** (records additive; Hotspot path untouched).
- [ ] **Step 5: Commit** `feat(ts): emit UnitRecords 1:1 with Hotspots (React-suppression-aware)`

---

### Task 3: TS propagation fixture + dogfood

**Files:** a `run_scan`-level test (gated `feature="ts"`) + manual dogfood.

- [ ] **Step 1: Regression test** — temp `.ts` file: `function outer() { inner(); }  function inner() { fetch('x'); }` (fetch → NetFsDb class 7). Scan (no_resolve=false). Assert `inner` own max_class reflects the IO, `outer` own lower but `propagated_max_class` reflects inner's effect (intra-file resolved edge `inner`), `outer.inherited` non-empty. Plus a `--no-resolve` assertion (propagated == own). (Confirm `fetch` is the class the TS detector assigns — check `detect/calls.rs::classify_call`; set thresholds to match.)
- [ ] **Step 2: Run** → should pass once Tasks 1-2 landed. If RED, fix.
- [ ] **Step 3: Dogfood (record, don't assert)** — `cargo run -q -p fxrank --no-default-features --features ts -- scan /home/caasi/GitLab/omni/114-kg-frontend/src 2>/dev/null | jq '.summary, (.hotspots[0:3] | map({symbol, own:.max_class, prop:.propagated_max_class, n_inherited:(.inherited|length)})), (.scope.external_reaches[0:10] | map(.specifier))'` (or a smaller TS dir like `exp-app-element/src`). Confirm: propagated scores appear; external reaches are meaningful imported specifiers (`react`, `node:fs`, `./...`, package names), NOT bare-name/member noise; provenance present. Record in the report. Also `--no-resolve` collapses to own. (If the omni path isn't present on this host, fall back to `crates/fxrank-lang-ts/tests/fixtures` or `agent-browser/packages`.)
- [ ] **Step 4: Commit** the fixture test: `test(ts): intra-file propagation fixture + dogfood notes`

---

### Task 4: Phase-2c gate

- [ ] **Step 1: fmt** `cargo fmt --all` then `--check` → clean.
- [ ] **Step 2: clippy** `cargo clippy --workspace --all-targets -- -D warnings` → clean.
- [ ] **Step 3: test** `cargo test --workspace` → 0 failed; TS snapshots (incl. React) unchanged.
- [ ] **Step 4: slim builds** `--features ts`, `--features rust`, `--features python`, no-features all compile.
- [ ] **Step 5: Commit** if fmt/clippy touched anything.

---

## Self-Review

**Spec coverage (2c):** TS call-site extraction (Task 1); TS records 1:1 with Hotspots through the React two-pass (Task 2); dogfoodable (Task 3). `.ts`/`.tsx` pooling is automatic (all `Language::Ts`). **Deferred (follow-on):** the React-inheritance RETROFIT onto the shared fold (#28's "retrofit last" — consolidating the within-file React absorption to go THROUGH the fold; the two coexist correctly today because suppressed arrows aren't separate units, so no double-count). **Deferred to phase 3:** TS roots (framework files/bootstraps/`package.json`), module-init units, real `contained`, config-aware first/third-party (tsconfig paths/workspace names).

**Placeholder scan:** the swc refs walker (Task 1) points at the concrete `calls.rs` pattern; the React-two-pass threading (Task 2) points at the exact `analyze_units` structure — real transcription, not TODOs.

**Type consistency:** `refs::extract -> Vec<CallSiteRef>` with `qualified = imports.resolve(root).is_some()`; `record_from_hotspot(unit, &Hotspot, imports, lines) -> UnitRecord` (own-body copied from `h`, `language: Ts`); `analyze_units(..., records: &mut Vec<UnitRecord>)`; `unit_id == h.id` (so apply_fold matches). Records 1:1 with Hotspots (suppressed arrows excluded).

## Note on the deferred React retrofit
The existing within-file React inheritance (component absorbs inline hook-callback arrows, single-hop) stays as-is for own-body. The shared transitive fold adds cross-file/transitive propagation on top, with NO double-count (suppressed arrows are not separate graph nodes — their effects live only in the component's own-body). A future task can retrofit the React inheritance to flow through the shared fold (so TS carries one fold), per spec §11.9 — out of 2c scope.
