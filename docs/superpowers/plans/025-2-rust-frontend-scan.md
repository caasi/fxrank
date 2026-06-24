# Cross-file Resolution — Phase 2 (Rust frontend + CLI fold driver) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make the **Rust** frontend emit language-neutral `UnitRecord`s (with outgoing call references), and wire the phase-1 fold into the CLI so `fxrank scan crates/` produces **real propagated scores + external reaches** — the first end-to-end, dogfoodable slice. Python/TS frontends are separate follow-on plans.

**Architecture:** Frontends gain a `records: Vec<UnitRecord>` output alongside the existing `functions: Vec<Hotspot>`. The Rust frontend builds a record per function (own effects/risks reused from the existing detect pass + new call-ref extraction). The CLI pools records per language, builds a `CallGraph` via a symbol-name resolver (intra-file + basic cross-file export index; everything else → external reach), runs the phase-1 `fold`, and **merges** the propagated fields into the existing Hotspots by `unit_id` — own-body output stays byte-identical. `--no-resolve` skips the fold.

**Tech Stack:** Rust, `syn` (Rust frontend AST), the phase-1 `fxrank-core` fold (`record`/`graph`/`fold` modules), clap (CLI).

## Global Constraints

- `fxrank-core` stays parser-free (no `syn`/`swc`/`libcst`).
- **Own-body output must stay byte-identical** for `fxrank scan` (own_score, effects, confidence, max_class, risk_weight): the fold only *adds* propagated_score/propagated_max_class/inherited/external_reaches/root by matching `unit_id`; it never recomputes own-body fields. Existing snapshot tests must pass unchanged.
- CI gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Effect/risk class vocab centralized in `EffectKind`/`RiskKind` — no hand-written wire strings/class numbers.
- Phase-2 Rust resolution is **symbol-name-based** (the `UnitRecord` export index), accepting name-collision ambiguity (→ `Ambiguous` external reach). Module-tree precision and roots are **phase 3**; `is_root` stays the `false` stub this phase.
- Build/test the Rust frontend slim where useful: `cargo build -p fxrank --no-default-features --features rust`.

---

### Task 1: `FrontendOutput.records` field

**Files:**
- Modify: `crates/fxrank-core/src/frontend.rs` (`FrontendOutput`)
- Test: `crates/fxrank-core/src/frontend.rs` tests

**Interfaces:**
- Produces: `FrontendOutput.records: Vec<crate::record::UnitRecord>` (defaults empty via `#[derive(Default)]`). Frontends that don't emit records yet leave it empty.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn frontend_output_carries_records() {
    let o = FrontendOutput::default();
    assert!(o.records.is_empty());
}
```

- [ ] **Step 2: Run test, verify fail**

Run: `cargo test -p fxrank-core frontend_output_carries_records`
Expected: FAIL — no field `records`.

- [ ] **Step 3: Add the field**

In `FrontendOutput`, add `pub records: Vec<crate::record::UnitRecord>,`. (It's a plain data Vec — `Default` still derives.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-core/src/frontend.rs
git -C /dev/shm/fxrank-025 commit -m "feat(core): FrontendOutput.records for the cross-file fold input"
```

---

### Task 2: `col` on `FnUnit` (Rust)

**Files:**
- Modify: `crates/fxrank-lang-rust/src/functions.rs` (`FnUnit` struct + the two id-building sites)
- Test: `crates/fxrank-lang-rust/src/functions.rs` tests (or an existing collect test)

**Interfaces:**
- Produces: `FnUnit.col: usize` (1-based char column of the fn name, the same `col` already embedded in `id`).

- [ ] **Step 1: Write the failing test**

Add (or extend an existing `functions::collect` test):

```rust
#[test]
fn fnunit_exposes_col() {
    let file = syn::parse_file("fn foo() {}").unwrap();
    let units = collect(&file, "a.rs");
    assert_eq!(units[0].col, 4); // 1-based column of `foo`
    assert!(units[0].id.ends_with(":4:foo")); // col already in id
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p fxrank-lang-rust fnunit_exposes_col`
Expected: FAIL — no field `col`.

- [ ] **Step 3: Add the field + populate at both id sites**

Add `pub col: usize,` to `FnUnit`. At BOTH id-building sites (free fn ~line 75-89 and impl method ~line 124-145), the `col` local already exists (`let col = start.column + 1;`) — add `col,` to the `FnUnit { … }` literal. (Search for `id: format!("{path}:{line}:{col}:{symbol}")` to find both.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-lang-rust`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-lang-rust/src/functions.rs
git -C /dev/shm/fxrank-025 commit -m "feat(rust): FnUnit.col exposed for UnitRecord site keys"
```

---

### Task 3: Rust call-reference extraction (`detect::refs`)

**Files:**
- Create: `crates/fxrank-lang-rust/src/detect/refs.rs`
- Modify: `crates/fxrank-lang-rust/src/detect/mod.rs` (add `pub mod refs;`)
- Test: `crates/fxrank-lang-rust/src/detect/refs.rs` tests

**Interfaces:**
- Produces: `pub fn extract(block: &syn::Block, imports: &ImportTable) -> Vec<fxrank_core::record::CallSiteRef>`.
- Consumes: the `ImportTable` (Task-7-inventory: `resolve(local) -> Option<&str>`), `CallSiteRef`/`RefKind` (core record module).

**Pattern to follow:** This is a `syn::visit::Visit` walker exactly like `crates/fxrank-lang-rust/src/detect/calls.rs` (READ it first). Where `calls.rs` classifies a call into an `Effect`, `refs::extract` instead records a `CallSiteRef` for every call to a *named* callee:
- `visit_expr_call` with `Expr::Path(p)`: `base = render_path(&p.path)` (copy `render_path` from calls.rs, or make it `pub(crate)` and reuse); `module = imports.resolve(head_segment).map(str::to_string)` (head = first `::` segment); `kind = RefKind::Free`; `line = node.span().start().line`; `col = node.span().start().column + 1`.
- `visit_expr_method_call`: `base = node.method.to_string()`; `module = None`; `kind = RefKind::Method`.
- Always call the default `syn::visit::visit_*` after recording, so nested calls are captured (mind the `in_callee` subtlety only if you reuse calls.rs's static-name handling — for refs you do NOT need it; just record and recurse normally).
- Skip macro calls and closures-as-args is fine to capture (they're just more calls).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::imports::ImportTable;
    fn refs_of(src: &str) -> Vec<fxrank_core::record::CallSiteRef> {
        let f = syn::parse_file(src).unwrap();
        let imports = ImportTable::from_file(&f);
        // grab the first fn body
        let item = f.items.into_iter().find_map(|i| if let syn::Item::Fn(f}) = i { Some(*f}.block) } else { None }).unwrap();
        extract(&item, &imports)
    }
    #[test]
    fn extracts_free_and_method_calls() {
        let refs = refs_of("use std::fs; fn f() { fs::write(p, b); x.push(1); g(); }");
        // fs::write -> Free, module std::fs ; .push -> Method ; g -> Free
        assert!(refs.iter().any(|r| r.base == "fs::write" && r.module.as_deref() == Some("std::fs")));
        assert!(refs.iter().any(|r| matches!(r.kind, fxrank_core::record::RefKind::Method) && r.base == "push"));
        assert!(refs.iter().any(|r| r.base == "g"));
    }
}
```

(Fix the obvious typo in the helper — `syn::Item::Fn(f)` / `*f.block` — when you transcribe; the destructuring shown is illustrative.)

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p fxrank-lang-rust extracts_free_and_method_calls`
Expected: FAIL — module `refs` missing.

- [ ] **Step 3: Implement the walker**

Write `refs.rs` following the `calls.rs` visitor pattern above. Add `pub mod refs;` to `detect/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-lang-rust`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-lang-rust/src/detect/refs.rs crates/fxrank-lang-rust/src/detect/mod.rs
git -C /dev/shm/fxrank-025 commit -m "feat(rust): detect::refs — extract outgoing call references"
```

---

### Task 4: Rust frontend builds `UnitRecord`s

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/mod.rs` (add a `build_record` fn) and/or `crates/fxrank-lang-rust/src/lib.rs` (the analyze loop)
- Test: `crates/fxrank-lang-rust/src/detect/mod.rs` tests (a fixture-based unit test)

**Interfaces:**
- Produces: `pub fn build_record(unit: &FnUnit, imports: &ImportTable, statics: &HashSet<String>) -> fxrank_core::record::UnitRecord`. The analyze loop pushes one record per scored unit into `output.records` (same `unit_id` as the corresponding Hotspot).
- A record's fields: `unit_id/path/line/col/symbol` from `FnUnit`; `effects`/`risks` = the SAME `gather(unit, imports, statics)` + `risk::detect_fn_risks(...)` the Hotspot uses (so own data matches exactly — set each `Effect.contained` per the existing containment knowledge if cheaply available, else leave the detector's value; **do NOT recompute differently from `analyze_unit`**); `refs` = `refs::extract(&unit.block, imports)`; `async_boundary`/`await_count` as in `analyze_unit`; `is_root: false` (phase-3 stub); `export: None` (phase-3 sets real export identity; for now a `pub` heuristic is OPTIONAL — keep `None` unless trivial).

**Note on Effect.contained:** the Rust frontend currently stubs `contained: false` (phase-1 Task 4). Setting real containment for Rust is phase-3-adjacent; for phase 2 keep the detector's current `contained` values (mostly false). This means Rust propagation treats most effects as escaping — acceptable and conservative (over-propagates rather than hides).

- [ ] **Step 1: Write the failing test**

Use the existing `analyze_fixture`-style helper or inline source. Example inline:

```rust
#[test]
fn build_record_captures_own_and_refs() {
    let f = syn::parse_file("use std::fs; fn writer(p: &str) { fs::write(p, b\"x\"); }").unwrap();
    let imports = crate::imports::ImportTable::from_file(&f);
    let statics = std::collections::HashSet::new();
    let units = crate::functions::collect(&f, "a.rs");
    let rec = build_record(&units[0], &imports, &statics);
    assert_eq!(rec.symbol, "writer");
    assert!(rec.refs.iter().any(|r| r.base.contains("fs::write")));
    assert!(!rec.effects.is_empty()); // fs::write -> net.fs.db effect
    assert_eq!(rec.unit_id, units[0].id);
    assert!(!rec.is_root);
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p fxrank-lang-rust build_record_captures_own_and_refs`
Expected: FAIL — `build_record` missing.

- [ ] **Step 3: Implement `build_record` + wire into analyze**

Add `build_record` to `detect/mod.rs` reusing `gather`, `risk::detect_fn_risks`, `count_awaits`, and `refs::extract`. In `lib.rs::analyze`, in the scored-unit branch(es), after `output.functions.push(analyze_unit(...))`, also `output.records.push(detect::build_record(unit, &imports, &statics))`. (Skipped test units contribute no record, mirroring Hotspots.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-lang-rust`
Expected: PASS — and existing Hotspot tests/snapshots unchanged (records are additive).

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-lang-rust/src/detect/mod.rs crates/fxrank-lang-rust/src/lib.rs
git -C /dev/shm/fxrank-025 commit -m "feat(rust): emit UnitRecords alongside Hotspots"
```

---

### Task 5: Core export index + symbol-name resolver

**Files:**
- Modify: `crates/fxrank-core/src/resolve.rs` (create) + `crates/fxrank-core/src/lib.rs` (`pub mod resolve;`)
- Test: `crates/fxrank-core/src/resolve.rs` tests

**Interfaces:**
- Produces:
  - `pub struct SymbolIndex` built from `&[UnitRecord]`: maps a simple-name key (the last `::`/`.` segment of a unit's `symbol`) → the set of `unit_id`s defining it.
  - `pub fn resolve_ref(r: &CallSiteRef, idx: &SymbolIndex, nodes: &HashMap<UnitId, UnitRecord>) -> graph::Edge`:
    - compute the callee simple name from `r.base` (last segment after `::` or `.`);
    - if the index has exactly ONE unit_id for that name → `Edge::Resolved(that id)`;
    - if MORE than one → `Edge::Opaque(ExternalReach{ specifier: r.base, kind: Ambiguous, site })`;
    - if ZERO → `Edge::Opaque(ExternalReach{ specifier: r.module.clone().unwrap_or(r.base.clone()), kind: ThirdParty, site })`.
  - `site` = `format!("{}:{}:{}", <referencing unit path>, r.line, r.col)` — pass the referencing unit's path in, or build the site from `r` (line/col) plus a path the caller threads. Keep `site` a `String`.

This is language-neutral (operates on records), so it lives in core. It is deliberately crude (name-based); module-path precision is phase 3.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::*;
    fn rec(id: &str, sym: &str) -> UnitRecord { /* minimal UnitRecord with unit_id=id, symbol=sym, empty effects/risks/refs, is_root false */ }
    #[test]
    fn resolves_unique_cross_file_symbol_else_reach() {
        let recs = vec![rec("a.rs:1:1:helper", "helper"), rec("b.rs:1:1:caller", "caller")];
        let idx = SymbolIndex::from_records(&recs);
        let nodes = /* map by unit_id */;
        let call = CallSiteRef{ kind: RefKind::Free, base: "helper".into(), module: None, line: 2, col: 3 };
        assert!(matches!(resolve_ref(&call, &idx, &nodes), graph::Edge::Resolved(ref id) if id == "a.rs:1:1:helper"));
        let ext = CallSiteRef{ kind: RefKind::Free, base: "println".into(), module: Some("std".into()), line: 2, col: 3 };
        assert!(matches!(resolve_ref(&ext, &idx, &nodes), graph::Edge::Opaque(ref r) if matches!(r.kind, ReachKind::ThirdParty)));
    }
}
```

(Write the `rec` helper and `nodes` map inline.)

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p fxrank-core resolves_unique_cross_file_symbol_else_reach`
Expected: FAIL — module `resolve` missing.

- [ ] **Step 3: Implement**

Write `resolve.rs` with `SymbolIndex` + `resolve_ref`. Add `pub mod resolve;` to `lib.rs`. Keep it parser-free (operates on `UnitRecord`/`CallSiteRef` only).

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-core/src/resolve.rs crates/fxrank-core/src/lib.rs
git -C /dev/shm/fxrank-025 commit -m "feat(core): SymbolIndex + symbol-name resolver (phase-2 resolution)"
```

---

### Task 6: Core `apply_fold` — merge propagated fields into existing Hotspots

**Files:**
- Modify: `crates/fxrank-core/src/fold.rs` (add `apply_fold`)
- Test: `crates/fxrank-core/src/fold.rs` tests

**Interfaces:**
- Produces: `pub fn apply_fold(hotspots: &mut [Hotspot], g: &CallGraph, folded: &HashMap<UnitId, Propagated>)`.
  - For each hotspot, look up `folded[&hotspot.id]`; if present, set `propagated_score`/`propagated_max_class` (computed from the Propagated effects via `own_score`/`max_class`, exactly as `to_hotspots` does), `inherited` (mapped to `InheritedSignal`, reusing the same mapping `to_hotspots` uses — factor it into a shared helper to avoid duplication), `external_reaches`, and `root = g.nodes[&id].is_root`.
  - **Own-body fields are NOT touched.** A hotspot with no matching folded entry is left unchanged (own-seeded).

This is the augment-in-place sibling of `to_hotspots`; share the per-unit "compute propagated fields from Propagated" logic between them (extract a helper) so there is one definition.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn apply_fold_sets_propagated_without_touching_own() {
    // hotspot "root" own_score 0/class 0; graph root->b->c(io). After apply_fold, root.propagated_max_class==7, own_score still 0.
    let g = /* dashboard-ish graph */;
    let folded = fold(&g);
    let mut hs = vec![ /* own-seeded Hotspot with id "...root", own_score 0, max_class 0 */ ];
    apply_fold(&mut hs, &g, &folded);
    assert_eq!(hs[0].own_score, 0.0);
    assert_eq!(hs[0].propagated_max_class, 7);
    assert!(!hs[0].inherited.is_empty());
}
```

(Reuse the fold test helpers to build the graph; build the Hotspot via `Hotspot::own_seed` with the matching `id`.)

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p fxrank-core apply_fold_sets_propagated_without_touching_own`
Expected: FAIL — `apply_fold` missing.

- [ ] **Step 3: Implement**

Add `apply_fold`, extracting the shared "Propagated → (propagated_score, propagated_max_class, inherited, external_reaches)" helper used by `to_hotspots`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-core/src/fold.rs
git -C /dev/shm/fxrank-025 commit -m "feat(core): apply_fold augments Hotspots with propagated fields by id"
```

---

### Task 7: CLI `--no-resolve` flag

**Files:**
- Modify: `crates/fxrank-cli/src/main.rs` (clap `Cli` scan args + thread the bool to `run_scan`)
- Test: a CLI integration test (or manual; add a `#[test]` if the CLI has a testable arg-parse path)

**Interfaces:**
- Produces: `--no-resolve` boolean flag (default false). When true, the CLI skips the fold step (Task 8) and emits own-seeded Hotspots (current behavior). Threaded into `run_scan` as a `no_resolve: bool` param.

- [ ] **Step 1: Add the flag**

In the clap `Scan` args struct (near the existing `--exclude`/`--lang`), add:
```rust
/// Skip cross-file resolution + propagation; emit per-file own scores only.
#[arg(long)]
no_resolve: bool,
```
Thread it from `main` into `run_scan(path, limit, include_tests, lang, exclude, no_resolve)`.

- [ ] **Step 2: Build to verify it wires**

Run: `cargo build -p fxrank --no-default-features --features rust`
Expected: compiles (the param is unused until Task 8 — add `let _ = no_resolve;` or use it in the Task-8 branch immediately if doing both together).

- [ ] **Step 3: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-cli/src/main.rs
git -C /dev/shm/fxrank-025 commit -m "feat(cli): --no-resolve flag (pass-1-only output)"
```

---

### Task 8: CLI fold driver — pool records, fold, augment

**Files:**
- Modify: `crates/fxrank-cli/src/main.rs` (`run_scan`, after `dispatch`, before `Report::build`)
- Test: a CLI-level integration test in `crates/fxrank-cli/tests/` (or `crates/fxrank-cli/src/main.rs` `#[cfg(test)]`)

**Interfaces:**
- Consumes: `output.records` (Task 4), `SymbolIndex`/`resolve_ref` (Task 5), `CallGraph::from_records` (phase 1), `fold` (phase 1), `apply_fold` (Task 6).
- Produces: the default scan path now runs the fold and augments `output.functions`; `scope.external_reaches` is populated from the union of all hotspots' reaches (deduped).

Driver logic (insert after `let output = dispatch(...);`, gate on `!no_resolve` and non-empty records):
```rust
if !no_resolve && !output.records.is_empty() {
    let records = std::mem::take(&mut output.records);
    let idx = SymbolIndex::from_records(&records);
    // build nodes map for the resolver
    let graph = CallGraph::from_records(records, |r, nodes| resolve_ref(r, &idx, nodes));
    let folded = fold(&graph);
    apply_fold(&mut output.functions, &graph, &folded);
    // app-wide external surface
    scope_external_reaches = dedup union of output.functions[*].external_reaches;
}
```
Set `scope.external_reaches` from that union (replace the current `external_reaches: vec![]` stub in the `Scope { … }` construction). Note `SymbolIndex` borrows records; build it before moving records into `from_records`, or have `from_records` take `&[UnitRecord]` + clone into nodes — pick whichever satisfies the borrow checker cleanly (the simplest: build `idx` and the `nodes` map from a borrow, then pass owned records to `from_records`; if `from_records` consumes `records` and also needs `idx`, build `idx` from `&records` first since `from_records` takes ownership last).

- [ ] **Step 1: Write the failing integration test**

A CLI test that scans a 2-function Rust snippet where `caller` calls `helper`, and `helper` does `fs::write`. Assert the resulting `Report`'s `caller` hotspot has `propagated_max_class == 7` (inherited the IO) while its `own` max_class is lower, and that `external_reaches` is non-empty. (If a full CLI test harness is heavy, write this as a `run_scan`-level test with a temp file, or a core-level test that mimics the driver — but prefer exercising `run_scan` so the wiring is covered.)

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p fxrank` (the relevant test)
Expected: FAIL — propagation not wired / `caller` shows own-only.

- [ ] **Step 3: Implement the driver**

Wire the driver block above into `run_scan`; populate `scope.external_reaches`.

- [ ] **Step 4: Run tests**

Run: `cargo test --workspace`
Expected: PASS — including unchanged own-body snapshot tests (the fold only adds propagated fields).

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-cli/src/main.rs
git -C /dev/shm/fxrank-025 commit -m "feat(cli): wire fold into scan — real propagated scores + external reaches"
```

---

### Task 9: Dogfood validation + fixture

**Files:**
- Test: a Rust fixture under `crates/fxrank-lang-rust/tests/fixtures/` exercising intra-file propagation, or a `crates/fxrank-cli` integration test.

- [ ] **Step 1: Add an intra-file propagation fixture test**

A fixture (or inline scan) with two same-file functions where `outer()` calls `inner()` and `inner()` does IO; assert `outer`'s propagated_max_class reflects the IO and `inner` is a resolved edge (not an external reach).

- [ ] **Step 2: Run it**

Run: `cargo test -p fxrank-lang-rust` (or `-p fxrank`)
Expected: PASS.

- [ ] **Step 3: Dogfood manually (record output, do not assert)**

Run: `cargo run -p fxrank --no-default-features --features rust -- scan crates/fxrank-cli/src | jq '.summary, (.hotspots[0:3])'`
Confirm by eye: top hotspots show `propagated_score`/`propagated_max_class`; some carry `inherited` provenance; `scope.external_reaches` lists outward calls (e.g. `std::fs`, `std::io`). Also run with `--no-resolve` and confirm propagated == own + empty inherited/reaches. Record observations in the report (this is a sanity gate, not a hard assertion — fxrank output is a signal).

- [ ] **Step 4: Commit the fixture/test**

```bash
git -C /dev/shm/fxrank-025 add crates/fxrank-lang-rust/tests/
git -C /dev/shm/fxrank-025 commit -m "test(rust): intra-file propagation fixture + dogfood notes"
```

---

### Task 10: Phase-2 gate

**Files:** none (verification)

- [ ] **Step 1: fmt** — `cargo fmt --all` then `cargo fmt --check` → clean.
- [ ] **Step 2: clippy** — `cargo clippy --workspace --all-targets -- -D warnings` → no warnings.
- [ ] **Step 3: full test** — `cargo test --workspace` → 0 failed; existing own-body snapshot tests unchanged.
- [ ] **Step 4: slim builds** — `cargo build -p fxrank --no-default-features --features rust` (and `--features ts`, `--features python`, no-features) all compile.
- [ ] **Step 5: Commit** if fmt/clippy touched anything.

---

## Self-Review

**Spec coverage (phase-2 Rust slice):** `Frontend::scan`-equivalent via `FrontendOutput.records` (Task 1,4); call-site extraction (Task 3); export index + resolution (Task 5); pool→fold→augment driver wired into the CLI (Task 6,8); `--no-resolve` (Task 7,8); dogfoodable end-to-end (Task 9). **Deferred to phase 3:** Rust module-tree precise cross-file resolution, roots (Cargo/module-tree), module-init units, real `Effect.contained` for Rust, real `export` identity. **Deferred to 025-2b/2c:** Python and TS frontends emitting records (+ TS `.ts`/`.tsx` pooling, relative-path resolution, React retrofit).

**Placeholder scan:** the syn-walker (Task 3) and a couple of test helpers are described as "follow calls.rs / fill inline" — this is real transcription work pointing at a concrete existing pattern, not a TODO. No vague "handle errors" placeholders.

**Type consistency:** `FrontendOutput.records: Vec<UnitRecord>`, `build_record -> UnitRecord`, `refs::extract -> Vec<CallSiteRef>`, `SymbolIndex`, `resolve_ref -> graph::Edge`, `apply_fold(&mut [Hotspot], &CallGraph, &HashMap<UnitId,Propagated>)` are used consistently across Tasks 1–8. `unit_id` from `FnUnit.id` matches the Hotspot `id` (both `path:line:col:symbol`), which is what `apply_fold` keys on.

## Notes for 025-2b / 2c (follow-on plans)

- **2b Python:** `Imports::resolve`, `functions::collect` (def/class/lambda) → records; relative dotted-module resolution; same driver (already language-agnostic — it folds whatever records exist).
- **2c TS:** swc call extraction; **pool `.ts`/`.tsx` records together** (the dialect-dissolve); relative-specifier (`./`) resolution + barrels; retrofit React inheritance onto the shared fold.
- The CLI driver (Task 8) already folds per the pooled record set, so 2b/2c only add record producers — no driver change.
