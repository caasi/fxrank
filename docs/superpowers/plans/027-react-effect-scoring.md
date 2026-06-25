# React effect-scoring (definition-site attribution + CPS containment) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Source of truth is `docs/superpowers/specs/027-react-effect-scoring.md` — when code and spec disagree, the spec wins.

**Goal:** Replace the bespoke allowlist-based React "two-pass" with a definition-site-attribution model: a component **owns every effect lexically defined in its body** (render, all hooks, all handlers at any depth) **minus** effects it hands across the component boundary (received callbacks, refs/handlers passed to consumers). Adopted effects are re-parented (moved, not copied) onto the component with their real `contained` flags preserved, so the shared cross-file fold keeps internal-only effects in `own` and propagates only escaping ones. Event-phase deferred effects get a new, capped, recorded conditionality discount. This closes the orphan-handler problem (55 IO handlers float free today) and the 84%-own=0 effect-blindness, and delivers the long-deferred "one fold" (old 3f).

**Architecture:** TS-frontend-only. `fxrank-core` stays language-neutral and parser-free — **no new core field, no React concept in core**. The frontend computes phase / hook semantics / JSX / component identity / provenance and **materializes** them into existing neutral `Effect` fields (`contained`, `discounted_to`, `discount`, `subreason`, `confidence`) before records enter the fold. The current `analyze_units` (`lib.rs`) keyed-by-`(line,col)` single-hop suppression is replaced by a tree-aware ownership pass; `detect::{raw_signals, adopt_effects, augment_component}` are rewritten so they no longer drop/force the `contained` tuple.

**Tech Stack:** Rust (edition 2024 workspace), swc (`swc_ecma_ast` / `swc_ecma_visit`) for the TS AST, `insta` for snapshots. Touches only `crates/fxrank-lang-ts/` (+ its fixtures/snapshots). Core is read-only here except where 028 (a separate plan, same PR) touches it.

## Global Constraints

Copied verbatim from spec 027 §5 (invariants) and the precedence/mechanism notes:

- **Core neutrality preserved** — no React/JS/phase concept in `fxrank-core` (compiler-enforced: no parser dep; this plan adds none). No new core field.
- **Never-guess preserved** — provenance/ownership resolve only from syntax + the import table; unknowns **downgrade confidence**, never fabricate ownership or a target.
- **No double-count** — re-parenting gives each effect exactly one owner (§4.2). A component's body effects never also belong to `<module>` (§4.1).
- **Output CHANGES (intended, not byte-stable):** the 84%-own=0 components gain real scores; orphan IO handlers fold into their components; internal-only callbacks discount down; event-phase effects discount with rationale. A **dedicated fixture suite** (not whole-report byte-equality) asserts each principle. Existing React snapshots WILL move and must be re-accepted, not worked around.
- **027-alone vs 028 — do NOT assert 028 behavior under 027:** 027 moves the **attribution** metrics (own=0 component count ↓ sharply, orphan-handler count ↓, `propagated ≥ own` holds). The **over-count class reductions** (`console.*` at class 4, `new Date()` at class 5) are NOT fixable by 027 — `world_effect` still includes `Logging`/`TimeRead`/`Random` at their current classes; those need **spec 028**. 027's fixtures must assert attribution + containment + conditionality, NOT the logging/time class numbers.
- **Two orthogonal discount axes:** (1) spec-003 **containment** discount — `apply_boundary_discount`, contained-only, floor 0, never touches an escaping effect (unchanged, already wired in `analyze_unit`). (2) this spec's **conditionality** discount (§2.4) — a NEW TS-side direct write of `discounted_to`/`discount`/`subreason`, **1-class cap, floor 1**, for event-phase effects; NOT `apply_discount` (Rust-only) and NOT `apply_boundary_discount` (refuses escaping). The conditionality discount must never erase an escaping effect (floor 1).
- **Lands in the same PR as spec 028's implementation** (branch `feat/027-react-effect-scoring`); closes #37 together.
- **CI gates per commit:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. **Run cargo from the worktree `/dev/shm/fxrank/37`** (`cargo -C` is not a thing — `cd /dev/shm/fxrank/37 &&` or run from that cwd).

## File Structure

| File | Action |
|---|---|
| `crates/fxrank-lang-ts/src/react.rs` | **modify** — add `is_component` recognizer (§4.1), `HookPhase::Event` + `Unknown`, broaden hook phase table (§2.4); keep `returns_jsx`/`state_transitions`/`context_reads`/`ref_bindings`. |
| `crates/fxrank-lang-ts/src/provenance.rs` | **create** — local binding provenance pass (`Provenance` lattice: Received/Imported/LocalDefined) + function-value classification (`ValueClass`: OwnedImmediate/OwnedDeferred/EscapedValue/ReceivedValue) with fixed precedence (§4.3). |
| `crates/fxrank-lang-ts/src/ownership.rs` | **create** — tree-aware ownership/adoption transform (§4.2): given a component + the file's flat `FnUnit` list, decide which nested units the component adopts (any depth) and which stay standalone + graph edge. |
| `crates/fxrank-lang-ts/src/detect/fnvalues.rs` | **create** — NEW JSX-attribute + call-argument function-**value** walker (§4.5): finds function values passed (not called), routes by provenance to owned / graph-edge. |
| `crates/fxrank-lang-ts/src/detect/mod.rs` | **modify** — `raw_signals` (preserve `(Effect, contained)` tuple, §4.4), `absorb_inherited` → `adopt_effects` (re-parent with containment + conditionality discount, §4.2/§4.4/§2.4), `augment_component` (assign real `contained` instead of hardcoding `false`), `world_effect` (unchanged for 027; note: plan 028 runs BEFORE 027 and removes `Logging` from this set — 027 builds on the post-028 state where `Logging` is already absent; do NOT re-add it). |
| `crates/fxrank-lang-ts/src/lib.rs` | **modify** — `analyze_units` rewritten: component recognition via `react::is_component`, tree-aware adoption via `ownership`, fnvalue edges via `detect::fnvalues`, removal of single-hop `(line,col)` suppression machinery. `<module>` emission unchanged. |
| `crates/fxrank-lang-ts/src/lib.rs` (`mod` decls) | **modify** — add `pub mod provenance; pub mod ownership;`. |
| `crates/fxrank-lang-ts/tests/fixtures/react/*.tsx` | **create** — new per-principle fixtures (attribution, containment, consumer-responsibility, phase). |
| `crates/fxrank-lang-ts/tests/react.rs` | **modify** — replace obsolete single-hop tests, add principle assertions; re-accept the existing 3 snapshots (output moves). |
| `crates/fxrank-lang-ts/tests/snapshots/react__*.snap` | **re-accept** via `cargo insta review` (output intentionally changes). |

---

### Task 1: Component recognizer beyond `returns_jsx` (§4.1)

Today the only component test is `react::returns_jsx`, which misses `return null` components and presentational helpers that call hooks. Add a broader recognizer that is **TS-only heuristic** and carries a confidence signal, WITHOUT changing the `<module>` synthetic unit's scope.

**Files:** `crates/fxrank-lang-ts/src/react.rs`.

**Interfaces:**
- **Consumes:** `&FnUnit` (for `symbol`, `path`, `body`), and the file dialect knowledge (PascalCase + `.tsx`/`.jsx` is a stronger signal). The existing `returns_jsx(&FnBodyOwned) -> bool` stays.
- **Produces:** a new
  ```rust
  /// Component-recognition outcome. `is` gates React treatment; `confidence`
  /// lowers the component's function-level confidence when only one weak signal
  /// fired (e.g. PascalCase alone, no JSX, no hooks).
  pub struct ComponentSignal {
      pub is: bool,
      pub confidence: f64,
  }
  pub fn is_component(unit: &FnUnit, path: &str) -> ComponentSignal
  ```
  Recognition rule: a function is a component ONLY when its `symbol` is **PascalCase** (first char `char::is_uppercase`) AND at least one of: (a) `returns_jsx(&unit.body)` (strong); (b) `path` ends in `.tsx`/`.jsx` (medium); (c) the body calls at least one React hook — a bare-ident callee matching `/^use[A-Z]/` (medium, corroborating only, never sufficient on its own without PascalCase). A lowercase name (`helper`, `useThing`, etc.) is **never** a component regardless of hook calls. Confidence: `1.0` when `returns_jsx` holds or ≥2 signals fire; `0.8` when exactly one of the medium signals fires alone (heuristic, may be a non-component PascalCase factory). `is = false` ⇒ `confidence` is irrelevant (caller ignores it).
  - **Helper (private):** `fn calls_a_hook(body: &FnBodyOwned) -> bool` — a `Visit` walker that returns true on the first bare-ident `CallExpr` callee matching `use[A-Z]…`; stop descent at nested `visit_arrow_expr`/`visit_function` (own-body only — hooks are called in the component body, not in nested callbacks). Reuse the `hook_callee_name`-style match already in `react.rs`.

**`<module>` non-interference (the no-double-count guard):** `is_component` does NOT touch `module_init_unit` (in `functions.rs`), which still collects only top-level statements and skips `Decl::Fn` bodies. A default-exported component (`export default function App(){…}`) is a `Decl::Fn` → its body is already excluded from `<module>`. The Task 3 ownership pass assigns each effect exactly one owner. **This task adds an explicit test that a hook-calling default-exported component contributes nothing to `<module>`.**

**TDD steps:**
- [ ] **Step 1 — failing tests** in `react.rs` `#[cfg(test)] mod tests`:
  ```rust
  #[test]
  fn recognizes_return_null_component() {
      let u = unit("function Widget(){ useState(0); return null; }", "Widget");
      let s = super::is_component(&u, "src/Widget.tsx");
      assert!(s.is, "PascalCase + hook call ⇒ component even when it returns null");
  }
  #[test]
  fn pascalcase_tsx_alone_is_lower_confidence_component() {
      let u = unit("function Box(){ return null; }", "Box");
      let s = super::is_component(&u, "src/Box.tsx");
      assert!(s.is && s.confidence < 1.0, "single weak signal ⇒ lower confidence");
  }
  #[test]
  fn lowercase_helper_is_not_component() {
      let u = unit("function helper(){ useThing(); return null; }", "helper");
      assert!(!super::is_component(&u, "src/h.tsx").is, "lowercase name ⇒ not a component");
  }
  #[test]
  fn jsx_returning_component_is_full_confidence() {
      let u = unit("function C(){ return <div/>; }", "C");
      let s = super::is_component(&u, "src/C.tsx");
      assert!(s.is && (s.confidence - 1.0).abs() < f64::EPSILON);
  }
  ```
  (the `unit(src, symbol)` helper already exists in `react.rs` tests and parses as Tsx.)
- [ ] **Step 2 — run-fail:** `cargo test -p fxrank-lang-ts is_component 2>&1` then by the test names above; expect compile error (no `is_component`).
- [ ] **Step 3 — implement** `ComponentSignal`, `is_component`, `calls_a_hook` per the interface.
- [ ] **Step 4 — `<module>` non-interference test** in `lib.rs` tests (uses `analyze_src`):
  ```rust
  #[test]
  fn default_export_component_body_not_in_module_unit() {
      let src = "import React,{useState} from 'react';\n\
                 export default function App(){ const [v,setV]=useState(0); fetch('/a'); return null; }\n";
      let (out, _records) = analyze_src(src);
      // App is recognized + owns the fetch; there must be NO <module> hotspot
      // carrying that fetch (function-decl bodies never enter module_init_unit).
      assert!(out.iter().any(|h| h.symbol == "App"));
      assert!(!out.iter().any(|h| h.symbol == "<module>"),
          "App's body effects must not also appear as a <module> unit");
  }
  ```
- [ ] **Step 5 — run-pass:** `cargo test -p fxrank-lang-ts` (this test depends on Task 3 wiring `is_component` into `analyze_units`; if Task 3 not yet done, mark `#[ignore]` with a `// un-ignore after Task 3` note and un-ignore in Task 3). Run `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] **Step 6 — commit:** `feat(ts): component recognizer beyond returns_jsx (spec 027 §4.1)`.

---

### Task 2: Local provenance pass + function-value lattice (§4.3)

Resolve, per binding the component references, whether it is `Received` (param/destructured prop), `Imported` (ES import), or `LocalDefined` (local fn/arrow decl); and per function-**value** site, its `ValueClass`. This is pure analysis (no scoring); Tasks 3 and 5 consume it.

**Files:** `crates/fxrank-lang-ts/src/provenance.rs` (new); register `pub mod provenance;` in `lib.rs`.

**Interfaces:**
- **Consumes:** `&FnUnit` (the component — for `sig.params` and `body`), `&ImportTable` (for `resolve`/`import_target`).
- **Produces:**
  ```rust
  /// Where a name visible inside a component came from.
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum Provenance { Received, Imported, LocalDefined, Unknown }

  /// Classification of a function VALUE the component references.
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum ValueClass { OwnedImmediate, OwnedDeferred, EscapedValue, ReceivedValue }

  /// Per-component binding → provenance map, built once.
  pub struct ProvenanceTable { /* name -> Provenance */ }

  impl ProvenanceTable {
      /// Build from the component's params (Received), the file imports (Imported),
      /// and the component body's local `function`/arrow declarators (LocalDefined).
      /// Simple aliases/destructuring carry provenance through; unknown
      /// spreads/computed access are recorded as `Unknown` (caller downgrades
      /// confidence, never guesses).
      pub fn build(component: &FnUnit, imports: &ImportTable) -> Self;
      pub fn get(&self, name: &str) -> Provenance; // Unknown if absent
  }
  ```
- **Binding sources (build rules):**
  - Component `sig.params` → walk each `Pat` with `collect_pat_bindings` (already `pub(crate)` in `detect::mutation`) → each bound name `Provenance::Received`. (Destructured props `{onChange}` are exactly this case.)
  - `imports.resolve(name).is_some()` → `Provenance::Imported`. (Check imports AFTER params so a shadowing param wins — origin dominates; matches the residual-limit note in CLAUDE.md that shadowing is heuristic, but here Received must win per §4.3 precedence.)
  - Component body top-level `function f(){…}` decls and `const f = () => …` / `const f = function(){…}` declarators → `Provenance::LocalDefined`. A small `Visit` walker over the component body, stop at nested fn scopes (own-body only).
  - **Important:** `LocalDefined` records the binding name → provenance, but does NOT imply the binding holds a function. A `useState` setter (`const [v, setV] = useState(0)` → `setV` is `LocalDefined`) is a `LocalDefined` binding whose value is NOT a `FnUnit`. The ownership pass (Task 3) and the fnvalues walker (Task 5) must handle this: when a `LocalDefined` binding's `(line,col)` does not match any `FnUnit`, classify it as "not a function value" and skip — no adoption, no graph edge.
  - Alias carry: `const g = f;` where `f` is known → `g` inherits `f`'s provenance. `const {a} = props` where `props` is `Received` → `a` is `Received`. Computed/spread (`const x = obj[k]`, `const {...rest} = props`) → `Unknown`.
- **`classify_value`:** a free fn (used by Tasks 3 & 5), precedence FIXED per §4.3 — **ReceivedValue first**, then Escaped, then Owned:
  ```rust
  /// Classify a function value at a use-site. `name` is the referenced binding
  /// (None for an inline anonymous arrow). `deferred` = the site schedules the
  /// value for later invocation (event handler / effect-phase / unknown hook)
  /// vs immediate (render body / render-phase hook). `escaped` = the value is
  /// returned/exported/stored in module|global|context|ref or passed to an
  /// unknown callee.
  pub fn classify_value(prov: Provenance, deferred: bool, escaped: bool) -> ValueClass {
      if prov == Provenance::Received { return ValueClass::ReceivedValue; } // origin wins
      if escaped { return ValueClass::EscapedValue; }
      if deferred { ValueClass::OwnedDeferred } else { ValueClass::OwnedImmediate }
  }
  ```

**TDD steps:**
- [ ] **Step 1 — failing tests** in `provenance.rs`:
  ```rust
  #[test]
  fn params_are_received_imports_are_imported_locals_are_localdefined() {
      // component: function C({onChange}){ const local=()=>{}; useImported(); }
      // with `import { useImported } from 'x'`
      let t = build_table_for(
        "import { useImported } from 'x';\n\
         function C({onChange}){ const local = () => {}; return null; }", "C");
      assert_eq!(t.get("onChange"), Provenance::Received);
      assert_eq!(t.get("useImported"), Provenance::Imported);
      assert_eq!(t.get("local"), Provenance::LocalDefined);
      assert_eq!(t.get("nope"), Provenance::Unknown);
  }
  #[test]
  fn alias_carries_provenance() {
      let t = build_table_for(
        "function C({cb}){ const g = cb; return null; }", "C");
      assert_eq!(t.get("g"), Provenance::Received, "alias of a received prop is received");
  }
  #[test]
  fn received_wins_precedence_in_classify() {
      // a received prop that is then passed onward is still ReceivedValue
      assert_eq!(classify_value(Provenance::Received, true, true), ValueClass::ReceivedValue);
      assert_eq!(classify_value(Provenance::LocalDefined, false, false), ValueClass::OwnedImmediate);
      assert_eq!(classify_value(Provenance::LocalDefined, true, false), ValueClass::OwnedDeferred);
      assert_eq!(classify_value(Provenance::LocalDefined, false, true), ValueClass::EscapedValue);
  }
  ```
  (Provide a `build_table_for(src, comp_symbol)` test helper mirroring `react.rs`'s `unit_with_lines` plumbing: parse Tsx, `ImportTable::from_module`, `functions::collect`, find the component, `ProvenanceTable::build`.)
- [ ] **Step 2 — run-fail:** `cargo test -p fxrank-lang-ts provenance` → compile error.
- [ ] **Step 3 — implement** `provenance.rs` + `pub mod provenance;` in `lib.rs`.
- [ ] **Step 4 — run-pass:** `cargo test -p fxrank-lang-ts provenance`; then fmt + clippy.
- [ ] **Step 5 — commit:** `feat(ts): local provenance pass + function-value lattice (spec 027 §4.3)`.

---

### Task 3: Tree-aware ownership/adoption transform replacing `absorb_inherited` (§4.2)

The core mechanism. Replace the single-hop `(line,col)`-keyed suppression in `lib.rs::analyze_units` with a **tree-aware** ownership pass: a component adopts its owned nested units **at any depth**, suppressing each as a standalone hotspot and **moving** (re-parenting) its effects+refs onto the component. Units that are escaped/received stay standalone + reached via a graph edge.

**Files:** `crates/fxrank-lang-ts/src/ownership.rs` (new), `crates/fxrank-lang-ts/src/lib.rs` (rewrite `analyze_units`), `crates/fxrank-lang-ts/src/detect/mod.rs` (rename `absorb_inherited` → `adopt_effects`; see Task 4 for the containment-preserving body).

**Why tree-aware is reachable:** `functions::collect` already yields **every** nested fn/arrow as a flat `FnUnit` (it recurses via `visit_children_with`, see `functions.rs` doc and `visit_arrow_expr`). So the file's `units: &[FnUnit]` already contains the whole lexical subtree; the ownership pass must *partition* it by lexical containment, not stop at one hop.

**Interfaces:**
- **Consumes:** `units: &[FnUnit]` (the file's flat unit list), each component `&FnUnit`, the per-component `ProvenanceTable` (Task 2), `&SpanLines`, the hook-phase map (Task 6's broadened `react::hook_callbacks`).
- **Produces (in `ownership.rs`):**
  ```rust
  /// The lexical-ownership decision for one file's units relative to one component.
  pub struct Adoption {
      /// unit_id → the deferral/phase context under which the component owns it.
      /// Present ⇒ the unit is ADOPTED (suppressed + re-parented).
      pub owned: std::collections::HashMap<String, OwnedContext>,
  }
  #[derive(Clone, Copy)]
  pub struct OwnedContext {
      pub phase: crate::react::HookPhase, // Render | Effect | Event | Unknown
  }
  /// Walk the component's lexical subtree (any depth) and decide adoption.
  /// `units` is the whole file unit list; lexical containment is determined by
  /// SPAN RANGE (a nested unit's (line,col) falls within the component's source
  /// extent) AND by walking ownership edges (a unit owned by an owned unit is
  /// owned).
  pub fn resolve_ownership(
      component: &FnUnit,
      units: &[FnUnit],
      prov: &ProvenanceTable,
      lines: &SpanLines,
  ) -> Adoption;
  ```
- **Containment determination (any depth):** A nested unit `n` is a candidate for component `C` when `n` lexically nests inside `C`. Determine lexical nesting from spans: collect, per component, the set of `(line,col)` of every arrow/fn passed to a hook OR a JSX prop OR defined as a local handler in `C`'s body, then transitively follow those owned units' own hook/JSX/handler arrows. **Concretely (avoid span-range guessing):** drive it off the *value-flow* the §4.5 walker (Task 5) and the hook map (Task 6) report — start from `C`'s body, gather every function value it owns (`OwnedImmediate`/`OwnedDeferred` per `classify_value`), map each to the `FnUnit` with the matching `(line,col)` (the spec-005 anchor; `FnUnit.col` is a real field — never split `id`), and recurse into each owned unit's body to gather its owned function values too. The frontier stops at `EscapedValue`/`ReceivedValue` (not adopted).
- **Adoption effect on `analyze_units` (lib.rs rewrite):**
  1. recognize components via `react::is_component` (Task 1) instead of `returns_jsx`.
  2. for each component, `resolve_ownership` → the owned-unit-id set.
  3. a unit whose id is owned by **some** component is suppressed (not emitted as a standalone hotspot, no standalone record) — exactly like today's `continue`, but driven by the ownership set, not the `(line,col)` hook map.
  4. for each owned unit, harvest its `(Effect, contained)` tuples via `detect::raw_signals` (Task 4 preserves the tuple) + its outgoing refs via `refs::extract`, keyed by owning component id, then `detect::adopt_effects(component_hotspot, harvested)` re-parents them.
  5. emit non-owned units (components and free functions) as today, building records from the final hotspot.
- **No double-count:** an effect is harvested from an owned unit and pushed onto exactly one component; the owned unit produces no hotspot/record. A unit owned by two components is impossible (single lexical parent) — assert it in a debug check or just rely on `HashMap` last-write (document the invariant).

**TDD steps:**
- [ ] **Step 1 — failing tests** in `react.rs` (integration via `util::analyze_tsx`):
  ```rust
  #[test]
  fn nested_handler_any_depth_is_adopted() {
      // fetch lives TWO hops down: component -> useEffect arrow -> inner arrow.
      let hs = util::analyze_tsx(
        "function C(){ useEffect(() => { const run = () => fetch('/x'); run(); }, []); return <div/>; }");
      let c = hs.iter().find(|h| h.symbol == "C").expect("C");
      assert_eq!(c.max_class, 7, "C owns the depth-2 fetch (tree-aware, not single-hop)");
      assert!(hs.iter().all(|h| !h.symbol.starts_with("<arrow@")),
          "all nested arrows suppressed (none float as orphan hotspots)");
  }
  #[test]
  fn named_local_handler_is_adopted_not_orphaned() {
      // The CURRENT bug: a named handler passed to onClick floats as its own hotspot.
      let hs = util::analyze_tsx(
        "function C(){ function onClick(){ fetch('/x'); } return <button onClick={onClick}/>; }");
      let c = hs.iter().find(|h| h.symbol == "C").expect("C");
      assert_eq!(c.max_class, 7, "C owns its named handler's fetch");
      assert!(!hs.iter().any(|h| h.symbol == "onClick"),
          "named local handler must NOT appear as an orphan hotspot");
  }
  ```
- [ ] **Step 2 — run-fail:** `cargo test -p fxrank-lang-ts nested_handler_any_depth_is_adopted named_local_handler_is_adopted_not_orphaned`. With today's single-hop code, `nested_handler` fails (depth-2 fetch not absorbed) and `named_local_handler` fails (handler floats — confirmed by existing `onclick_handler_is_not_effect_in_render` which ASSERTS the orphan; that test will be replaced in Task 5/8).
- [ ] **Step 3 — implement** `ownership.rs`, rewrite `analyze_units`, rename/forward `absorb_inherited`→`adopt_effects` (signature finalized in Task 4). Remove the now-dead single-hop machinery: the `inherited: HashMap<(usize,usize),…>` map, `pending`/`pending_refs` keyed by `(line,col)`, and the `react::inherited_callbacks` call (its phase data moves into Task 6's `hook_callbacks`). **Stub `owned_value_sites` empty here** (`FnValueSites { owned_value_sites: vec![], … }`) — Task 5 fills it in; this is a soft dependency documented in the Execution Handoff. The ownership frontier will adopt hook-callback arrows (from Task 6's `hook_callbacks`) but not JSX-handler named locals until Task 5 lands.
- [ ] **Step 4 — run-pass + un-ignore Task 1 Step 5** test; fmt + clippy.
- [ ] **Step 5 — commit:** `feat(ts): tree-aware lexical ownership / re-parenting (spec 027 §4.2)`.

---

### Task 4: React containment classifier — preserve the `(Effect, contained)` tuple (§4.4)

Today three sites destroy containment: `detect::raw_signals` drops the tuple (maps `(e,_contained) => e`, mod.rs:241-244), `absorb_inherited` forces `discounted_to = None` on every inherited effect (mod.rs:357), and `augment_component` hardcodes `contained: false` (its pushed effects + the `e.discounted_to = None` lines, mod.rs:321-329). Fix all three so the **real** `contained` flag survives adoption and flows into the fold via `record_from_hotspot` (which copies `h.effects` verbatim, mod.rs:443).

**Files:** `crates/fxrank-lang-ts/src/detect/mod.rs`.

**Interfaces:**
- **`raw_signals`** — change its harvested effects from `Vec<Effect>` to **`Vec<(Effect, bool)>`** (preserve the tuple `gather` already produces). Update the struct:
  ```rust
  pub struct RawSignals {
      pub effects: Vec<(Effect, bool)>, // (effect, contained) — tuple PRESERVED
      pub risks: Vec<RiskFeature>,
      pub await_count: usize,
      pub is_async: bool,
  }
  ```
  Remove the `.map(|(e,_contained)| e)` drop. Do NOT apply the boundary discount here (RAW = undiscounted, the existing contract is right).
- **`adopt_effects`** (replaces `absorb_inherited`) — signature:
  ```rust
  pub fn adopt_effects(h: &mut Hotspot, adopted: Vec<(HookPhase, RawSignals)>)
  ```
  For each `(effect, contained)`:
  1. set `effect.contained = contained` (do NOT force `false`).
  2. apply the **React containment classifier** (§2.2/§4.4): override `contained` for the React-specific kinds the gather step can't see:
     - `EffectKind::StateTransition` → `contained = true` (own internal state).
     - `EffectKind::AmbientRead` (useContext) → keep as gathered (it is class-2 ambient read; spec keeps it out of `world_effect`; treat as **contained = false** — it reaches outside, matches today).
     - a `hidden.mutation` with `subreason == Some("ref-cell-write")` → `contained = true` **only when** the ref is used as private storage; **escaping** (`contained = false`) when the ref is forwarded to the DOM. 027 keeps the conservative current behavior: ref-cell-write stays `contained = false` (escaping) — a precise DOM-forward classifier is a deferred limit (§6 ref-forwarding chains). Document this in a code comment and DO NOT claim DOM precision.
     - world effects (`world_effect(kind) == true`: `NetFsDb`/`Random`/`TimeRead`/`EnvRead`/`Panic`/`Concurrency`/`EnvWrite`/`ProcessControl`) → `contained = false` always (anti-Goodhart: a world effect is never contained). Note: `Logging` is NOT in this list — plan 028 removes it from `world_effect` before 027 runs; do not re-add it here.
  3. recompute `effect.discounted_to`: do NOT blanket-`None`. For a **contained** adopted effect, leave `discounted_to` as the boundary discount would set it (or `None` if no boundary applies — adopted effects have no signature, so default `None`). For an **escaping** adopted effect, `discounted_to = None` unless Task 6's conditionality discount applies.
  4. phase risk: keep the existing rule — `HookPhase::Render && world_effect(e.kind)` ⇒ push `effect_in_render_risk`.
  5. push the effect; fold async metadata as today; `recompute(h)`.
- **`augment_component`** — for `state_transitions` set `contained = true` (not the current implicit false via `discounted_to = None`); for `context_reads` keep `contained = false`. Remove the `e.discounted_to = None` lines (they no longer make sense once containment drives the fold) — instead set `contained` explicitly per kind.

**Ripple:** `record_from_hotspot` already copies `h.effects.clone()` (mod.rs:443), so corrected `contained` flags reach the record → `Effect::escapes()` → the shared fold keeps contained in `own`, propagates escaping. No change needed there.

**TDD steps:**
- [ ] **Step 1 — failing tests** in `mod.rs` tests:
  ```rust
  #[test]
  fn adopted_state_transition_is_contained() {
      // a useState-derived StateTransition adopted onto the component must be contained
      // (own internal state) ⇒ does NOT escape ⇒ stays in own, doesn't propagate.
      // build a component hotspot, adopt a StateTransition raw signal, assert contained.
  }
  #[test]
  fn adopted_fetch_is_escaping_not_contained() {
      // a NetFsDb adopted from a hook callback must be contained=false ⇒ escapes().
  }
  #[test]
  fn raw_signals_preserves_contained_tuple() {
      // a body-local write inside an adopted callback keeps contained=true in RawSignals.
  }
  ```
  (Construct via the existing `unit_and_ctx` helper: parse a callback fn, `raw_signals(...)`, inspect the `(Effect, bool)` tuple; and parse a component, `analyze_unit`, then `adopt_effects` with a synthesized `RawSignals`.)
- [ ] **Step 2 — run-fail.**
- [ ] **Step 3 — implement** the three rewrites. Update Task 3's `analyze_units` to pass `Vec<(HookPhase, RawSignals)>` (now tuple-bearing) into `adopt_effects`.
- [ ] **Step 4 — run-pass.** Existing integration tests that assert component `max_class` (e.g. `useeffect_fetch_inherits_to_component_no_duplicate`) must still pass — fetch stays escaping/class 7. fmt + clippy.
- [ ] **Step 5 — commit:** `fix(ts): preserve real contained flag through adoption (spec 027 §4.4)`.

---

### Task 5: JSX-prop & hook-arg function-value walker (§4.5)

A function passed as a **value** (`<Button onClick={handleClick}>`, `useX(cb)`) is invisible to `refs::extract` (call-only — `visit_call_expr` and stops at `visit_arrow_expr`/`visit_function`, refs.rs:62/123-125). Add a NEW walker that finds function values passed (not called) and routes each by provenance: inline → owned, `LocalDefined`-named → owned, `Imported` → graph edge (propagate, not copy), `Received` → not charged.

**Files:** `crates/fxrank-lang-ts/src/detect/fnvalues.rs` (new); register `pub mod fnvalues;` in `detect/mod.rs`.

**Interfaces:**
- **Consumes:** the component `&FnBodyOwned`, `&ProvenanceTable` (Task 2), `&ImportTable`, `&SpanLines`, the component path + `&TsModuleMap` (for edge resolution, same plumbing as `refs::extract`).
- **Produces:**
  ```rust
  pub struct FnValueSites {
      /// (line,col) of inline/local-named function VALUES the component OWNS —
      /// fed into Task 3's ownership frontier (these become adopted units).
      pub owned_value_sites: Vec<(usize, usize)>,
      /// graph edges for Imported function values passed onward (propagate the
      /// imported definition's effects; first-party resolves, third-party opaque).
      pub edges: Vec<fxrank_core::record::CallSiteRef>,
      /// Received values passed onward — recorded for completeness, NOT charged.
      pub received_passed: Vec<(usize, usize)>,
  }
  pub fn extract_fn_values(
      body: &FnBodyOwned,
      prov: &ProvenanceTable,
      imports: &ImportTable,
      lines: &SpanLines,
      referencing_file: &str,
      module_map: &TsModuleMap,
  ) -> FnValueSites;
  ```
- **Walker shape (`Visit`):** override `visit_jsx_attr` (JSX `onX={value}` props) and `visit_call_expr` (hook/callee args). At each, inspect the **value expression**:
  - `Expr::Arrow(_)` / `Expr::Fn(_)` directly as the prop value or call arg → inline owned → record its `(line,col)` (the same anchor `functions::collect` uses) in `owned_value_sites`.
  - `Expr::Ident(name)` as a value (NOT a call — i.e. not the callee of a `CallExpr`) → look up `prov.get(name)`:
    - `LocalDefined` → owned → find the local decl's `(line,col)` and add to `owned_value_sites`.
    - `Imported` → build a `CallSiteRef` edge (reuse `refs.rs`'s `module`/`resolved_target`/`first_party` logic — factor a shared helper `fn ref_for_ident(name, imports, module_map, …) -> CallSiteRef` so refs.rs and fnvalues.rs agree) and push to `edges`.
    - `Received` → push to `received_passed` (NOT charged — §2.3 consumer responsibility).
    - `Unknown` → skip + the caller downgrades confidence (record via a returned flag or a separate `unknown_count` field; simplest: add `pub unknown_count: usize` to `FnValueSites` and let `analyze_units` lower the component confidence by `unknown_count`).
  - DISTINGUISH value from call: in `visit_call_expr`, the callee ident is a CALL (handled by `refs::extract`), but each ARG ident that is a function value is a value. Do not double-count: this walker reports values; `refs::extract` reports calls.
- **Integration:** Task 3's ownership frontier consumes `owned_value_sites`; `analyze_units` extends the component record's refs with `edges` (so imported handlers propagate); `received_passed` is ignored for scoring.

**TDD steps:**
- [ ] **Step 1 — failing tests** in `react.rs`:
  ```rust
  #[test]
  fn imported_handler_passed_as_prop_is_edge_not_copied() {
      // import { handle } from './h';  <button onClick={handle}/>
      // The component must carry a ref/edge to handle, and NOT inline-copy handle's effects.
      let hs = util::analyze_tsx(
        "import { handle } from './h';\n\
         function C(){ return <button onClick={handle}/>; }");
      let c = hs.iter().find(|h| h.symbol == "C").expect("C");
      // own-body has no fetch effect (it's behind the import edge → propagation, not copy)
      assert!(c.effects.iter().all(|e| e.kind != fxrank_core::effect::EffectKind::NetFsDb),
          "imported handler is propagated via edge, not copied into own-body");
  }
  #[test]
  fn received_handler_passed_onward_is_not_charged() {
      // function C({onSave}){ return <button onClick={onSave}/>; } — onSave is a prop.
      let hs = util::analyze_tsx("function C({onSave}){ return <button onClick={onSave}/>; }");
      let c = hs.iter().find(|h| h.symbol == "C").expect("C");
      assert_eq!(c.max_class, 0, "passing a received callback onward charges nothing");
  }
  ```
  (For the edge test, assert at the record level too — add a records-returning variant or reuse `analyze_src` from `lib.rs` tests — that `C`'s record `refs` contains a ref with `module == Some("./h")`.)
- [ ] **Step 2 — run-fail.**
- [ ] **Step 3 — implement** `fnvalues.rs`, the shared `ref_for_ident` helper (refactor refs.rs to call it so they stay in lockstep), register the module, wire into `analyze_units`.
- [ ] **Step 4 — run-pass; fmt + clippy.** Guard: the following existing tests must still pass (they exercise the LocalDefined-non-function / setter case):
  - `lifting_makes_child_pure_parent_holds_state` (react.rs ~line 94): `setV` is a `LocalDefined` binding (not a function) passed as `onChange={setV}` to `Child` — `Child` must remain class 0 (the setter is not an adoptable function value; no effect edge created).
  - `useref_write_outranks_setter` (react.rs ~line 67): `setV` (setter) and `r.current` (ref-write) coexist; the setter must not be treated as a function value and must not inflate `S`'s score above the ref-write's hidden mutation in `R`.
- [ ] **Step 5 — commit:** `feat(ts): JSX-prop & hook-arg function-value walker (spec 027 §4.5)`.

---

### Task 6: Conditionality (phase) discount materialization (§2.4)

Phase never decides *whether* an effect counts — only *how much*. Render-phase world effects keep full weight + `EffectInRender`; effect-phase ≈ full; **event-phase** `OwnedDeferred` effects get a NEW capped, recorded conditionality discount. Broaden the hook-phase model to recognize event handlers and unknown hooks.

**Files:** `crates/fxrank-lang-ts/src/react.rs` (extend `HookPhase`, broaden the hook table), `crates/fxrank-lang-ts/src/detect/mod.rs` (apply the discount in `adopt_effects`).

**Interfaces:**
- **`HookPhase`** — extend:
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum HookPhase { Render, Effect, Event, Unknown }
  ```
  - `Render`: `useMemo`, `useState`/`useReducer` lazy initializers (as today).
  - `Effect`: `useEffect`, `useLayoutEffect` (as today). `useCallback` MOVES from today's `Effect` to **`Event`** (its body runs on invocation = event-time, which is exactly the conditional-on-interaction case). JSX `onX={…}` inline/named handlers are `Event`.
  - `Event`: handlers (JSX `onClick` etc.) and `useCallback` bodies.
  - `Unknown`: a callback passed to an unrecognized hook (`use[A-Z]…` callee not in the known set) → `OwnedDeferred` + confidence downgrade + an "unknown callback schedule" rationale.
  - Provide `pub fn hook_callbacks(body: &FnBodyOwned, lines: &SpanLines) -> HashMap<(usize,usize), HookPhase>` superseding `inherited_callbacks`: same shape, but ALSO maps `useCallback`→`Event`, unrecognized `use[A-Z]` callbacks→`Unknown`, and (in cooperation with Task 5) JSX handler arrows→`Event`. (JSX-handler phase can be supplied by Task 5's owned-value sites tagged `Event`; keep the source of phase consistent.)
- **Conditionality discount (in `adopt_effects`, §2.4 mechanism — direct write, NOT `apply_*`):** for an `OwnedDeferred` effect whose phase is `Event` (or `Unknown`):
  ```rust
  // 1-class cap, floor 1 — never erase an escaping effect.
  let base = e.effective_class();
  let down = base.saturating_sub(1).max(1);   // cap 1 class, floor 1
  if down < base {
      e.discounted_to = Some(down);
      e.discount = Some("phase:event — conditional on interaction".to_string());
      e.subreason = Some("phase:event".to_string());
      e.sync_weight();
  }
  ```
  Applies to escaping AND contained effects, but since contained effects already don't propagate, the observable change is on escaping ones (e.g. a `fetch` in an `onClick` → class 7 → 6 with rationale). For `Unknown` phase additionally lower the effect's `confidence` (e.g. `e.confidence *= 0.8`) and use `discount`/`subreason` text noting the unknown schedule.
  **Must NOT contradict spec 003:** this is orthogonal to containment — it never touches `contained`, and the containment discount (`apply_boundary_discount`) still refuses escaping effects. Floor 1 guarantees a world effect is "nudged, never erased."

**TDD steps:**
- [ ] **Step 1 — failing tests** in `react.rs`:
  ```rust
  #[test]
  fn event_handler_fetch_gets_conditionality_discount() {
      let hs = util::analyze_tsx("function C(){ return <button onClick={() => fetch('/x')}/>; }");
      let c = hs.iter().find(|h| h.symbol == "C").expect("C");
      let fetch = c.effects.iter().find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb)
          .expect("C owns the onClick fetch");
      assert_eq!(fetch.class, 7, "base class unchanged");
      assert_eq!(fetch.discounted_to, Some(6), "event-phase: down 1, floored at 1");
      assert_eq!(fetch.subreason.as_deref(), Some("phase:event"));
      assert!(fetch.discount.is_some(), "rationale recorded");
  }
  #[test]
  fn event_discount_never_below_floor_one() {
      // a class-1 effect in an event handler floors at 1, not 0.
      // (e.g. a state.transition-ish class-1 owned-deferred effect)
  }
  #[test]
  fn unknown_hook_callback_is_owned_deferred_low_confidence() {
      let hs = util::analyze_tsx(
        "function C(){ useMystery(() => fetch('/x')); return <div/>; }");
      let c = hs.iter().find(|h| h.symbol == "C").expect("C");
      assert!(c.effects.iter().any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
          "ownership is certain even for unknown hook");
      assert!(c.confidence < 1.0, "unknown hook schedule lowers confidence");
  }
  ```
- [ ] **Step 2 — run-fail.**
- [ ] **Step 3 — implement** `HookPhase` extension, `hook_callbacks`, the discount in `adopt_effects`. Update `onclick_handler_is_not_effect_in_render` (it currently asserts the handler floats — replace its body to assert adoption + discount, per Task 8).
- [ ] **Step 4 — run-pass; fmt + clippy.**
- [ ] **Step 5 — commit:** `feat(ts): conditionality (phase) discount, capped+floored (spec 027 §2.4)`.

---

### Task 7: Fold integration — remove the bespoke own-body absorption (§4.6)

With effects re-parented and `contained`-tagged, the existing shared cross-file fold (`fxrank-core::fold`, driven by `Effect::escapes()`) does propagation for free. This task confirms the path end-to-end and deletes any remaining dead two-pass machinery, delivering the "one fold" (closes the original 3f).

**Files:** `crates/fxrank-lang-ts/src/lib.rs` (final cleanup), `crates/fxrank-lang-ts/src/detect/mod.rs` (remove now-unused helpers).

**Interfaces:**
- **Consumes:** the records produced by `record_from_hotspot` (with corrected `contained` flags from Task 4 and conditionality discounts from Task 6). No new code path — the CLI already partitions by language and runs `fold`/`apply_fold` (see CLAUDE.md "how a scan flows"); this task is the assertion + cleanup that the TS side now flows through it correctly.
- **Removals:** delete `react::inherited_callbacks` if fully superseded by `hook_callbacks` (Task 6) and no longer referenced; delete the `RawSignals`-drop comment block; ensure `absorb_inherited` is gone (renamed to `adopt_effects` in Task 4). `grep` for `inherited_callbacks` / `absorb_inherited` to confirm zero remaining references.

**TDD steps:**
- [ ] **Step 1 — failing test** (record-level, in `lib.rs` tests — fold lives in CLI, so assert the pre-condition like the existing `hook_callback_refs_routed_into_component_record`):
  ```rust
  #[test]
  fn contained_state_stays_own_escaping_fetch_propagates_via_record() {
      // Component with useState (contained) + an onClick fetch (escaping).
      let src = "import React,{useState} from 'react';\n\
                 function C(){ const [v,setV]=useState(0); \
                   return <button onClick={() => fetch('/x')}/>; }\n";
      let (out, records) = analyze_src(src);
      let c = out.iter().find(|h| h.symbol == "C").unwrap();
      let rec = records.iter().find(|r| r.unit_id == c.id).unwrap();
      // state.transition is contained → does NOT escape
      let st = rec.effects.iter().find(|e| e.kind == fxrank_core::effect::EffectKind::StateTransition).unwrap();
      assert!(!st.escapes(), "contained state stays in own (won't propagate)");
      // fetch is escaping → WILL propagate (the fold seeds from escapes())
      let f = rec.effects.iter().find(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb).unwrap();
      assert!(f.escapes(), "escaping fetch propagates");
  }
  ```
- [ ] **Step 2 — run-fail** (if Tasks 4/6 left a gap) or run-pass-after-cleanup; either way drive the cleanup.
- [ ] **Step 3 — implement** removals; `grep -rn "inherited_callbacks\|absorb_inherited" crates/fxrank-lang-ts/src` must be empty (except possibly historical doc text — update those too).
- [ ] **Step 4 — run-pass `cargo test --workspace`; fmt + clippy.**
- [ ] **Step 5 — commit:** `refactor(ts): one fold — remove bespoke own-body absorption (spec 027 §4.6, closes 3f)`.

---

### Task 8: Fixtures + omni dogfood + snapshot re-accept (§5)

Add per-principle acceptance fixtures and validate the attribution metrics on omni. Re-accept the three existing React snapshots (output intentionally moves). Replace obsolete single-hop tests.

**Files:** `crates/fxrank-lang-ts/tests/fixtures/react/*.tsx` (new), `crates/fxrank-lang-ts/tests/react.rs` (modify), `crates/fxrank-lang-ts/tests/snapshots/react__*.snap` (re-accept).

**Fixtures (one per principle, §5):**
- `attribution.tsx` — a `return null` component owning a named handler + a depth-2 nested callback with `fetch`; asserts own≠0 and no orphan `<arrow@…>`/named-handler hotspots.
- `containment.tsx` — `useState` + a `useRef` used as private storage; asserts the state.transition + (conservatively escaping) ref-cell effects are present and the contained ones don't dominate.
- `consumer_responsibility.tsx` — a `Parent` passing `setV`/a local handler to a `Child({onChange})`; asserts `Child` (only invoking the received callback) is class 0 and `Parent` holds the state.
- `phase.tsx` — same world effect in `useMemo` (render: `EffectInRender`, full weight) vs `useEffect` (effect: no risk) vs `onClick` (event: `discounted_to` one notch + `phase:event` rationale).

**Test assertions to add in `react.rs`:**
- per-principle assertions reading each new fixture (attribution: own=0 count drops; consumer: child pure; phase: the three weightings differ as specified).
- **Replace** `onclick_handler_is_not_effect_in_render` (react.rs ~line 111 — asserts the handler arrow floats as an orphan hotspot — now wrong) with an adoption+discount assertion.
- **Update** `usecallback_fetch_is_not_effect_in_render` (react.rs ~line 191 — currently passes accidentally because the fetch is inherited and `EffectInRender` is absent, but Task 6 moves `useCallback` from `HookPhase::Effect` to `HookPhase::Event`). The test currently only asserts absence of `EffectInRender`; UPDATE it to ALSO assert the new event-phase `discounted_to` on the fetch effect (`discounted_to == Some(6)` and `subreason == Some("phase:event")`), so it tests the new model rather than an accidental pass.
- **Verify `counter.snap`** (`snapshot_react_fixtures` for fixture `"counter"`, react.rs ~line 276): `counter.tsx`'s named inner `handleClick` is currently its own hotspot (scoring 0.0 with no effects). Under Tasks 3+5, `handleClick` is a `LocalDefined` value passed to `onClick` and will be adopted into `Counter` and suppressed. When re-accepting the snapshot, VERIFY that `handleClick` no longer appears as a standalone hotspot and that `Counter` has folded its effects in — this is a correctness signal, not mere drift.
- Keep `lifting_makes_child_pure_parent_holds_state` (still valid — already asserts the consumer rule).
- Add the snapshot suffix entries for the new fixtures to `snapshot_react_fixtures`'s name list.

**Omni dogfood (manual validation, recorded in commit message, NOT a CI test):**
- [ ] Run `cargo run -p fxrank -- scan <omni-frontend-path> | jq` (dogfood repo from MEMORY "Dogfood repos"). Record: own=0 component count ↓ sharply vs the pre-027 baseline; orphan-handler count ↓; and `propagated >= own` holds for every hotspot (a quick `jq 'map(select(.propagated_score < .own_score)) | length'` must be 0). Treat fxrank output as **signal, not gospel** — sanity-check a handful of components by hand.
- Do NOT assert the 028 over-count class numbers (logging/time) here — those move in 028.

**TDD steps:**
- [ ] **Step 1** — write the four fixtures + their assertions (failing until prior tasks land; they should pass once Tasks 1-7 are in).
- [ ] **Step 2 — run** `cargo test -p fxrank-lang-ts`; iterate.
- [ ] **Step 3 — re-accept snapshots:** `cargo test -p fxrank-lang-ts` then `cargo insta review` (review every diff; the moves are expected per §5 — confirm each reflects correct attribution, not a regression).
- [ ] **Step 4 — dogfood** per above; capture the metrics.
- [ ] **Step 5 — full gate:** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
- [ ] **Step 6 — commit:** `test(ts): per-principle React fixtures + omni dogfood metrics (spec 027 §5)` with the dogfood numbers in the body.

---

### Task 9: Object-literal hook-arg callbacks (`useMutation({ mutationFn })`) — spec §6 RESOLVED

**Why:** the dominant residual own=0 in react-query-heavy apps. The score is side-effect *risk*; a `mutationFn` doing `fetch` is genuinely the component's risk → it must be attributed (definition-site). Today `fnvalues` only inspects DIRECT positional hook args, so callbacks inside an object-literal arg are missed.

**Files:** `crates/fxrank-lang-ts/src/detect/fnvalues.rs` (+ tests/fixtures). Also fix two holistic-review doc Minors: `crates/fxrank-lang-python/src/detect/calls.rs:327` stale `// ── logging (class 4) ──` → class 2; `crates/fxrank-core/src/score.rs:92` rename the `// logging soup` label to a class-neutral one (the test stays; only the misleading label changes).

**Interfaces:** `fnvalues::handle_call_arg` (or the hook-arg path) gains object-literal descent. No new public API; reuses the provenance lattice + the unknown-phase/confidence handling from T2/T5/T6.

**Rule (structural, NO allowlist):** for a **hook-shaped call** (`use[A-Z]…`), in addition to a direct `Arrow`/`Fn` positional arg, descend into an **object-literal arg** and route EACH function-valued property through the existing provenance routing:
- a component-`LocalDefined`/inline function-value property → `OwnedDeferred` (owned), **unknown phase** for non-built-in hooks (→ conditionality discount 1 class, floor 1) + confidence penalty (the `unknown_count`/0.9 path).
- a non-function property (`retry: 3`) → skip.
- a `Received` property → not charged; an `Imported` property → graph edge (propagate), per the lattice.
- a callback handed to a **non-hook** unknown callee stays `EscapedValue` (T3 escape rule — the hook-vs-non-hook boundary is the discriminator; do NOT broaden object-descent to arbitrary calls).
- **Scope cap (deferred):** only TOP-LEVEL object-arg properties; objects-in-objects / arrays-of-callbacks stay deferred (§6).

- [ ] **Step 1 — failing test:** a component with `useMutation({ mutationFn: () => fetch('/x'), onError: () => console.warn('e') })` → the component OWNS the `net.fs.db` (escaping, event-phase discounted 7→6) and the logging (class 2); no orphan `<arrow@…>` for these; confidence lowered (unknown hook schedule). Run → FAILS (callbacks currently missed).
- [ ] **Step 2 — implement** the object-literal descent in `fnvalues` per the rule above.
- [ ] **Step 3 — run-pass** + `cargo test --workspace` (re-accept any authorized snapshot move) + fmt + clippy. Fix the two doc Minors. 
- [ ] **Step 4 — omni dogfood:** re-run the §5 metrics; `PersonalTagCard` (the spot-check that was still own=0) should now have non-zero own; own=0 % should drop further from 82.2%. Record before/after. STOP+report if it doesn't move.
- [ ] **Step 5 — commit:** `feat(ts): attribute object-literal hook-arg callbacks (useMutation/useQuery) (#37, spec 027 §6)`.

---

## Self-Review

**Spec coverage:**
- §2.1 definition-site attribution (default-own the lexical subtree, subtract handed-out) → Tasks 3 (adoption) + 5 (handed-out via received/escaped) + 2 (provenance).
- §2.2 CPS containment discount → Task 4 (containment classifier preserving the tuple).
- §2.3 cross-boundary = consumer's responsibility → Task 2 (`ReceivedValue` precedence) + Task 5 (received passed-onward not charged).
- §2.4 conditionality (phase) discount → Task 6 (capped 1-class, floor 1, recorded, NOT `apply_*`).
- §4.1 component recognizer → Task 1 (+ `<module>` non-interference test).
- §4.2 re-parenting tree-aware → Task 3.
- §4.3 provenance lattice + fixed precedence → Task 2.
- §4.4 containment classifier (fix raw_signals/absorb_inherited/augment_component) → Task 4.
- §4.5 JSX-prop/hook-arg function-value walker → Task 5.
- §4.6 fold integration / one fold → Task 7.
- §5 invariants + fixtures + dogfood; §6 documented limits (ref-DOM precision, unknown-hook phase, shadowing) → Task 8 + inline comments in Tasks 4/6.

**Placeholder scan:** no "TBD"/"similar to Task N"/"add error handling" left; every task has concrete signatures, test code, and commit messages. Two interfaces are pinned by contract+test rather than full body (Task 3 `resolve_ownership`, Task 5 walker) because their transforms are large — each has a pinning test and the exact spec section.

**Type consistency:** `RawSignals.effects` becomes `Vec<(Effect, bool)>` (Task 4) and Task 3's `adopt_effects` consumes `Vec<(HookPhase, RawSignals)>` accordingly — consistent. `ComponentSignal`/`Provenance`/`ValueClass`/`HookPhase` enums are introduced once and reused. `Effect`/`Hotspot`/`CallSiteRef`/`UnitRecord` core types are used as-is (no core field added). `is_use_ref_call`/`collect_pat_bindings` are `pub(crate)` (confirmed mutation.rs:459/513) so cross-module reuse compiles.

## Execution Handoff

- Branch: `feat/027-react-effect-scoring` (worktree `/dev/shm/fxrank/37`); 028's plan lands on the same branch / same PR (closes #37 together).
- Order is dependency-driven: 1 → 2 → 3 (depends on 1,2; rename absorb_inherited) → 4 (finalizes `adopt_effects` tuple) → 5 (feeds 3's ownership frontier; if running strictly sequentially, land 5's walker before 3's frontier consumes it, or stub `owned_value_sites` empty in 3 and fill in 5 — note this in the task as a soft dependency) → 6 → 7 (cleanup) → 8 (fixtures/snapshots/dogfood last, since every React fixture's output changes).
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` from the worktree before each commit.
- Snapshots are re-accepted in Task 8 only — do not accept partial snapshots mid-stream.
- Validate the plan via review-loop (per CLAUDE.md) before executing.
