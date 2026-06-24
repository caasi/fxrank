# Cross-file Resolution — Phase 2b (Python frontend + per-language pooling) Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make the **Python** frontend emit `UnitRecord`s (libcst call-site extraction + `qualified` tag), and **partition the CLI record pool per-language** (now load-bearing with two record-emitting frontends), so `fxrank scan <py>` produces real propagation without a Python ref ever resolving to a Rust definition.

**Architecture:** Mirrors phase-2 Rust. The CLI fold driver is already language-agnostic; this plan (a) adds `UnitRecord.language` and partitions the pool by it, and (b) adds Python record production. No core fold/resolve change beyond the new `language` field.

**Tech Stack:** Rust, `libcst` (Python AST), the phase-1/2 `fxrank-core` fold + `resolve` + `graph`.

## Global Constraints

- `fxrank-core` stays parser-free AND free of language-specific syntax in resolution (the `qualified` judgment stays in the frontend).
- **Own-body `fxrank scan` output stays byte-identical** — records are additive; the fold only adds propagated fields by `unit_id`. Existing Python snapshot tests must pass unchanged.
- CI gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Build slim too: `cargo build -p fxrank --no-default-features --features python`.
- Effect/risk class vocab centralized — no hand-written wire strings/class numbers.
- Per-language resolution is an invariant (guideline: "a TS file never resolves into a Rust definition"). Task 2 enforces it.

---

### Task 1: `UnitRecord.language` field

**Files:** Modify `crates/fxrank-core/src/record.rs` (UnitRecord); `crates/fxrank-lang-rust/src/detect/mod.rs` (build_record sets it); every `UnitRecord { … }` literal in the workspace.

**Interfaces:** Produces `UnitRecord.language: crate::frontend::Language`. Also: derive `Hash` on `Language` (for partitioning by it) — `crates/fxrank-core/src/frontend.rs` (`Language` already derives `Clone, Copy, PartialEq, Eq`; add `Hash`).

- [ ] **Step 1: Failing test** — in `record.rs` tests, build a `UnitRecord` and assert `r.language == Language::Rust` (pick any). Also assert `Language` is `Hash` by inserting into a `HashSet`.
- [ ] **Step 2: Run** `cargo test -p fxrank-core` → FAIL (no field).
- [ ] **Step 3: Implement** — add `pub language: crate::frontend::Language,` to `UnitRecord`; add `Hash` to `Language`'s derive list. Update Rust `build_record` (detect/mod.rs) to set `language: fxrank_core::frontend::Language::Rust`. Update every `UnitRecord { … }` literal (`rg "UnitRecord \{" crates`) — core tests, graph.rs/fold.rs test helpers, the rust build_record — with a sensible `language` (tests can use `Language::Rust`).
- [ ] **Step 4: Run** `cargo test --workspace` → PASS.
- [ ] **Step 5: Commit** `feat(core): UnitRecord.language for per-language pool partitioning`

---

### Task 2: CLI driver partitions the record pool by language

**Files:** Modify `crates/fxrank-cli/src/main.rs` (`run_scan` fold-driver block).

**Interfaces:** Consumes `UnitRecord.language`. The driver groups the pooled records by language and runs `SymbolIndex` → `CallGraph::from_records` → `fold` → `apply_fold` **per language group**; `scope.external_reaches` is the union across all groups. `apply_fold` matches by `unit_id`, so each group only augments its own hotspots.

- [ ] **Step 1: Failing test** — a `run_scan`-level test (gated `#[cfg(all(feature="rust", feature="python"))]`) over a temp dir with BOTH a Rust file defining `fn helper(){ std::fs::write(...) }` + a caller, AND a Python file defining `def helper(): ...` (no IO) + a Python caller calling `helper()`. Assert the Rust `caller` gets `propagated_max_class==7` (resolves Rust `helper`), and the Python caller does NOT inherit the Rust IO (its propagated stays low) — i.e. cross-language resolution did NOT happen. (If gating both features in one test is awkward, write a focused unit test on a helper that partitions a mixed `Vec<UnitRecord>` and asserts each group's `SymbolIndex` only contains its language's symbols.)
- [ ] **Step 2: Run** → FAIL (today the single pool would let Python `helper` and Rust `helper` collide → Ambiguous-drop or cross-resolve).
- [ ] **Step 3: Implement** — in the driver block, after `let records = std::mem::take(&mut output.records);`, group `records` by `r.language` (a `HashMap<Language, Vec<UnitRecord>>` or three `match`-partitioned Vecs). For each group: build `SymbolIndex::from_records(&group)`, `CallGraph::from_records(group, |r,owner,_| resolve_ref(r,&idx,&owner.path))`, `fold`, `apply_fold(&mut output.functions, &graph, &folded)`. After all groups, set `scope.external_reaches` to the deduped union of all hotspots' reaches (as today).
- [ ] **Step 4: Run** `cargo test --workspace` → PASS (and existing single-language behavior unchanged — Rust-only scans still work since they form one group).
- [ ] **Step 5: Commit** `feat(cli): partition the fold pool per-language (no cross-language resolution)`

---

### Task 3: Python `detect::refs` — call-reference extraction

**Files:** Create `crates/fxrank-lang-python/src/detect/refs.rs`; modify `crates/fxrank-lang-python/src/detect/mod.rs` (`pub mod refs;`).

**Interfaces:** `pub fn extract(unit: &FnUnit, imports: &Imports, span: &SpanIndex) -> Vec<fxrank_core::record::CallSiteRef>`.

**Pattern to mirror:** `crates/fxrank-lang-python/src/detect/calls.rs` (the call visitor: `on_call` uses `leftmost_name(&call.func)` for the line anchor and `render_expr(&call.func)` for the dotted base) and `crates/fxrank-lang-python/src/detect/expr.rs` (`render_expr`, `leftmost_name`). Walk the function body via the same mechanism `calls::detect` uses (an `EffectSink`/visitor over `walk_own_body`, or reuse the `on_call` visitor shape). READ calls.rs first.

For each call node, emit a `CallSiteRef`:
- `base = render_expr(&call.func)` (e.g. `"os.getcwd"`, `"self.method"`, `"foo"`). Skip calls where `render_expr` returns `None` (non-name/attribute callees).
- `root = base.split('.').next()` ; `module = imports.resolve(root).map(str::to_string)`.
- **`qualified = module.is_some()`** — the Python rule: a reference is a qualified outward reference iff its leading name resolves to an import (`os`, `requests`, `from x import foo`). Bare locals and `self.`/receiver methods (root not imported) → `qualified = false`.
- `kind`: `RefKind::Method` if `base.contains('.')` AND `module.is_none()` (a receiver attribute/method like `self.foo`/`x.bar`); else `RefKind::Free`.
- `line`/`col`: from the `leftmost_name` anchor (`name_line(anchor, span)` for line; the anchor's column via the span — mirror how calls.rs / functions.rs get col, e.g. `span.line_col(...)`; if only line is readily available, use the unit's mechanism and set col best-effort — col is for the site key, line is the important one).

- [ ] **Step 1: Failing test** — parse `import os\nfrom sub import run\ndef f():\n    os.getcwd()\n    run()\n    self.foo()\n    bare()` (as a module; grab `f`'s unit). Assert refs include: `base "os.getcwd"`, `module Some("os")`, `qualified true`; `base "run"`, `module Some("sub.run")`, `qualified true`; `base "self.foo"`, `module None`, `qualified false`, `kind Method`; `base "bare"`, `module None`, `qualified false`.
- [ ] **Step 2: Run** `cargo test -p fxrank-lang-python` → FAIL (no `refs`).
- [ ] **Step 3: Implement** the visitor in `refs.rs` mirroring `calls.rs::on_call`; add `pub mod refs;`.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-python` → PASS.
- [ ] **Step 5: Commit** `feat(python): detect::refs — extract outgoing call references`

---

### Task 4: Python `build_record` + emit records

**Files:** Modify `crates/fxrank-lang-python/src/detect/mod.rs` (`build_record`); `crates/fxrank-lang-python/src/lib.rs` (analyze loop).

**Interfaces:** `pub fn build_record(unit: &FnUnit, path: &str, imports: &Imports, module_bindings: &HashSet<String>, span: &SpanIndex) -> fxrank_core::record::UnitRecord`. The analyze loop pushes one record per scored unit (1:1 with the Hotspot).

- Reuse the SAME gather `analyze_unit` uses (`calls::detect` + `mutation::detect` + `risk::detect` + `count_awaits`) so own effects/risks/async match the Hotspot exactly. `refs = refs::extract(unit, imports, span)`. `unit_id = format!("{}:{}:{}:{}", path, unit.line, unit.col, unit.symbol)` (same as the Hotspot id). `language: Language::Python`. `is_root: false`, `export: None`. `async_boundary`/`await_count` as in `analyze_unit`.
- **Note on `contained`:** keep the detectors' current `contained` values (Python mutation detector tracks containment via tuples; the `Effect.contained` field is stubbed like Rust until phase 3 — keep behavior consistent with the Hotspot path).

- [ ] **Step 1: Failing test** — parse `import os\ndef writer():\n    os.getcwd()`; build the record; assert symbol `"writer"`, a ref `base "os.getcwd"` with `qualified true`, effects non-empty (os.getcwd → an effect, or at least the unit builds), `unit_id` ends `:writer`, `language == Python`, `!is_root`.
- [ ] **Step 2: Run** → FAIL (`build_record` missing).
- [ ] **Step 3: Implement** `build_record`; in `lib.rs::analyze`, after each `output.functions.push(detect::analyze_unit(...))` push `output.records.push(detect::build_record(unit, &file.path, &imports, &module_bindings, &span))` for the SAME units (skipped test units contribute neither).
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-python` → PASS; existing Python snapshots unchanged (records additive).
- [ ] **Step 5: Commit** `feat(python): emit UnitRecords alongside Hotspots`

---

### Task 5: Python propagation fixture + dogfood

**Files:** a `run_scan`-level test (gated `feature="python"`) and a manual dogfood.

- [ ] **Step 1: Failing/regression test** — temp `.py` file: `def outer():\n    inner()\ndef inner():\n    import os\n    os.getcwd()` (or use a clearer IO: `open("p")`). Scan (no_resolve=false). Assert `inner` has own effect (e.g. its own_max_class > 0 from the IO), `outer` own max_class lower but `propagated_max_class` reflects inner's effect (intra-file resolved edge `inner`), `outer.inherited` non-empty. Plus a `--no-resolve` assertion (propagated==own).
- [ ] **Step 2: Run** → should pass once Tasks 3-4 landed (propagation already wired by the driver). If RED, fix.
- [ ] **Step 3: Dogfood (record, don't assert)** — `cargo run -q -p fxrank --no-default-features --features python -- scan /home/caasi/GitHub/django/django/shortcuts.py 2>/dev/null | jq '.summary, (.hotspots[0:3] | map({symbol, own:.max_class, prop:.propagated_max_class})), (.scope.external_reaches[0:8] | map(.specifier))'` (or a smaller django module). Confirm: propagated scores appear; external reaches are meaningful imported/dotted calls (`os.*`, `django.*`), not bare-name noise; provenance chains present. Record in the report. Also `--no-resolve` collapses to own.
- [ ] **Step 4: Commit** the fixture test: `test(python): intra-file propagation fixture + dogfood notes`

---

### Task 6: Phase-2b gate

- [ ] **Step 1: fmt** `cargo fmt --all` then `--check` → clean.
- [ ] **Step 2: clippy** `cargo clippy --workspace --all-targets -- -D warnings` → clean.
- [ ] **Step 3: test** `cargo test --workspace` → 0 failed; Python snapshots unchanged.
- [ ] **Step 4: slim builds** `--features python`, `--features rust`, `--features ts`, no-features all compile.
- [ ] **Step 5: Commit** if fmt/clippy touched anything.

---

## Self-Review

**Spec coverage (2b):** per-language pooling prerequisite (Task 1-2, closes the final-review seam #4); Python call-site extraction (Task 3); Python records (Task 4); dogfoodable (Task 5). **Deferred to 2c:** TS frontend records (+ `.ts`/`.tsx` pooling, React retrofit). **Deferred to phase 3:** Python roots, module-init units, real `contained`, precise (module-path) resolution.

**Placeholder scan:** the libcst refs visitor (Task 3) points at the concrete `calls.rs`/`expr.rs` pattern — real transcription, not a TODO. Col extraction is "best-effort via the span mechanism" — the implementer reads how functions.rs gets col.

**Type consistency:** `UnitRecord.language: Language` (Task 1) used by the driver partition (Task 2); `build_record -> UnitRecord` (Task 4) sets `language: Python`; `refs::extract -> Vec<CallSiteRef>` (Task 3) with `qualified = module.is_some()`. `unit_id` format matches the Hotspot id (both `path:line:col:symbol`).

## Notes for 2c (TS)
- swc call extraction → `CallSiteRef`; `qualified` = import-specifier-resolved (relative/aliased/workspace, or bare package from the import table). Pool `.ts`/`.tsx` together (both `Language::Ts` → one partition group, dialect dissolves). Retrofit React inheritance onto the shared fold. The driver + per-language partition (this plan) need no further change.
