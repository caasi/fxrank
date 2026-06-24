# Cross-file Resolution — Phase 1: Core Fold Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the language-neutral fold machinery in `fxrank-core` — the `UnitRecord`
intermediate format, the escaping-only transitive join fixpoint, and the schema/ranking
changes — fully unit-tested against synthetic records, with no frontend or CLI changes yet.

**Architecture:** A new parser-free `fxrank-core` layer. Frontends will (in phase 2) emit
`UnitRecord`s; here we define that type and the `fold` that consumes a `CallGraph` of them,
producing per-unit propagated effects/risks (escaping-only), bounded provenance, and recorded
external reaches. Scoring/ranking moves to the propagated aggregates. Everything is tested with
hand-built records — no parser is touched.

**Tech Stack:** Rust, Cargo workspace, `serde` (wire), `insta` (snapshots, already a dev-dep).

## Global Constraints

- `fxrank-core` MUST remain parser-free — no `syn`/`swc`/`libcst` dependency may appear (the
  compiler enforces this; do not add one).
- Wire conventions (spec 001) preserved: `own_score` is `f64` rendering whole values as `3.0`;
  per-effect `confidence` is NOT serialized (`#[serde(skip)]`).
- CI gates that must stay green: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Effect/risk class vocabulary is centralized in `EffectKind`/`RiskKind` (`base_class`/`class`); never hand-write wire strings or class numbers at call sites.
- Site key is `(unit_id, line, col, kind)`; `unit_id` encodes the path — do not add a `path` field to `Effect`.
- Escaping rule: an **effect** escapes iff `contained == false` (plus `ExternalUnresolved` always escapes); a **risk** escapes per the `RiskKind::escapes()` table.

---

### Task 1: Add `col` to `Effect` and `RiskFeature`

**Files:**
- Modify: `crates/fxrank-core/src/effect.rs` (struct `Effect`, struct `RiskFeature`)
- Test: `crates/fxrank-core/src/effect.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `Effect.col: usize` and `RiskFeature.col: usize` (1-based character column of the
  effect/risk anchor, mirroring `Hotspot` line/col). Both serialize after `line`.

- [ ] **Step 1: Write the failing test**

Add to `effect.rs` tests:

```rust
#[test]
fn effect_and_risk_carry_col() {
    let e = Effect {
        kind: EffectKind::NetFsDb, class: 7, discounted_to: None, weight: 21,
        line: 4, col: 9, tier: Tier::Path, hidden: false,
        evidence: "fetch(x)".into(), discount: None, subreason: None, confidence: 1.0,
    };
    assert_eq!(e.col, 9);
    let j = serde_json::to_string(&e).unwrap();
    assert!(j.contains("\"line\":4,\"col\":9"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-core effect_and_risk_carry_col`
Expected: FAIL — `Effect` has no field `col`.

- [ ] **Step 3: Add the field**

In `effect.rs`, add `pub col: usize,` immediately after `pub line: usize,` in **both** `Effect`
and `RiskFeature`. (Serde emits in declaration order, so `col` follows `line` on the wire.)

- [ ] **Step 4: Fix existing constructors**

Every literal `Effect { … }` / `RiskFeature { … }` in the crate now needs `col`. In `effect.rs`
tests, the existing `subreason_serializes_only_when_present` builds an `Effect` — add `col: 1,`
after its `line: 1,`. Search the crate: `rg "RiskFeature \{|Effect \{" crates/fxrank-core/src` and
add `col: 1,` after each `line:` in those literals (only `model.rs` test helper `risk()` and the
`effect.rs` tests exist in core today).

- [ ] **Step 5: Run tests**

Run: `cargo test -p fxrank-core`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-core/src/effect.rs crates/fxrank-core/src/model.rs
git commit -m "feat(core): Effect/RiskFeature gain col for the cross-file site key"
```

---

### Task 2: Add `EffectKind::ExternalUnresolved` (the opaque-boundary token)

**Files:**
- Modify: `crates/fxrank-core/src/effect.rs` (`EffectKind` enum, `wire()`, `base_class()`)
- Test: `crates/fxrank-core/src/effect.rs` tests

**Interfaces:**
- Produces: `EffectKind::ExternalUnresolved`, `wire() == "external.unresolved"`, `base_class() == 2`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn external_unresolved_is_class_2() {
    assert_eq!(EffectKind::ExternalUnresolved.wire(), "external.unresolved");
    assert_eq!(EffectKind::ExternalUnresolved.base_class(), 2);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core external_unresolved_is_class_2`
Expected: FAIL — no variant `ExternalUnresolved`.

- [ ] **Step 3: Add the variant**

In `effect.rs`: add `ExternalUnresolved,` to `EffectKind`; in `wire()` add
`ExternalUnresolved => "external.unresolved",`; in `base_class()` add `ExternalUnresolved` to the
class-2 arm (`AmbientRead | UnknownMacro | ExternalUnresolved => 2,`).

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core external_unresolved_is_class_2`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/effect.rs
git commit -m "feat(core): EffectKind::ExternalUnresolved (class 2) opaque-boundary token"
```

---

### Task 3: `RiskKind::escapes()` predicate

**Files:**
- Modify: `crates/fxrank-core/src/effect.rs` (`impl RiskKind`)
- Test: `crates/fxrank-core/src/effect.rs` tests

**Interfaces:**
- Produces: `RiskKind::escapes(self) -> bool`. Capability risks escape; encapsulated risks do not.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn risk_escaping_predicate() {
    // capability risks the caller transitively triggers -> escape
    assert!(RiskKind::DynamicCode.escapes());
    assert!(RiskKind::FfiCall.escapes());
    assert!(RiskKind::HtmlInjection.escapes());
    assert!(RiskKind::ProtoPollution.escapes());
    assert!(RiskKind::EffectInRender.escapes());
    // encapsulated risks the callee owns -> do not escape
    assert!(!RiskKind::UnsafeBlock.escapes());
    assert!(!RiskKind::Transmute.escapes());
    assert!(!RiskKind::RawPtrDeref.escapes());
    assert!(!RiskKind::MemForget.escapes());
    assert!(!RiskKind::ImplDrop.escapes());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core risk_escaping_predicate`
Expected: FAIL — no method `escapes`.

- [ ] **Step 3: Implement**

In `impl RiskKind`, add:

```rust
/// Whether this risk propagates to a caller (capability) or is encapsulated by
/// the callee. Spec 025 sec 7 / sec 15.7 — a judgment table, change here if dogfooding shifts it.
pub fn escapes(self) -> bool {
    use RiskKind::*;
    matches!(self, DynamicCode | FfiCall | HtmlInjection | ProtoPollution | EffectInRender)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core risk_escaping_predicate`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/effect.rs
git commit -m "feat(core): RiskKind::escapes() per-kind propagation predicate"
```

---

### Task 4: `Effect::escapes()` helper

**Files:**
- Modify: `crates/fxrank-core/src/effect.rs` (`impl Effect`)
- Test: `crates/fxrank-core/src/effect.rs` tests

**Interfaces:**
- Consumes: `Effect` carries no `contained` field today (only TS/Python frontends track it on
  their side). For the language-neutral core, escaping is decided by **kind + a `contained` flag
  added to `Effect`**. Add `pub contained: bool` to `Effect` (default `false` for IO/global/etc.;
  `true` for body-local/param-channel mutations). Frontends set it in phase 2; core tests set it.
- Produces: `Effect::escapes(&self) -> bool` = `kind == ExternalUnresolved || !self.contained`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn effect_escapes_unless_contained() {
    let mut e = Effect {
        kind: EffectKind::LocalMutation, class: 1, discounted_to: None, weight: 1,
        line: 1, col: 1, tier: Tier::Exact, hidden: false, contained: true,
        evidence: "s = 1".into(), discount: None, subreason: None, confidence: 1.0,
    };
    assert!(!e.escapes());           // contained local mutation stays put
    e.contained = false;
    assert!(e.escapes());            // escaping mutation propagates
    e.kind = EffectKind::ExternalUnresolved; e.contained = true;
    assert!(e.escapes());            // external.unresolved always escapes
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core effect_escapes_unless_contained`
Expected: FAIL — no field `contained` / no method `escapes`.

- [ ] **Step 3: Implement**

Add `pub contained: bool,` to `Effect` after `pub hidden: bool,` (serialize it; it is meaningful
output). Add to `impl Effect`:

```rust
pub fn escapes(&self) -> bool {
    matches!(self.kind, EffectKind::ExternalUnresolved) || !self.contained
}
```

Add `contained: false,` (or `true` where semantically a contained mutation) to every existing
`Effect { … }` literal in the crate (the `effect.rs` tests).

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/effect.rs
git commit -m "feat(core): Effect.contained + Effect::escapes() (escaping = !contained)"
```

---

### Task 5: The `record` module — `SiteKey`, `CallSiteRef`, `UnitRecord`, `ExternalReach`

**Files:**
- Create: `crates/fxrank-core/src/record.rs`
- Modify: `crates/fxrank-core/src/lib.rs` (add `pub mod record;`)
- Test: `crates/fxrank-core/src/record.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (consumed by Task 6–9 and, in phase 2, by every frontend):

```rust
pub type UnitId = String;                 // "path:line:col:symbol"

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SiteKey { pub unit: UnitId, pub line: usize, pub col: usize, pub kind: String }

#[derive(Debug, Clone)]
pub enum RefKind { Free, Ctor, Method, Member, ModuleInit }

#[derive(Debug, Clone)]
pub struct CallSiteRef {
    pub kind: RefKind,
    pub base: String,             // the resolvable local name / receiver path
    pub module: Option<String>,   // import module string if the base is imported
    pub line: usize, pub col: usize,
}

#[derive(Debug, Clone)]
pub struct UnitRecord {
    pub unit_id: UnitId,
    pub path: String, pub line: usize, pub col: usize, pub symbol: String,
    pub is_root: bool,
    pub export: Option<(String, String)>,   // (module_id, exported name)
    pub effects: Vec<crate::effect::Effect>,
    pub risks: Vec<crate::effect::RiskFeature>,
    pub refs: Vec<CallSiteRef>,
    pub async_boundary: bool, pub await_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum ReachKind { ThirdParty, FirstPartyOutOfScope, Dynamic, Ambiguous }

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExternalReach { pub specifier: String, pub kind: ReachKind, pub site: String }
```

- [ ] **Step 1: Write the failing test**

Create `record.rs` ending with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_record_holds_effects_and_refs() {
        let r = UnitRecord {
            unit_id: "a.rs:1:1:f".into(), path: "a.rs".into(), line: 1, col: 1,
            symbol: "f".into(), is_root: true, export: None,
            effects: vec![], risks: vec![],
            refs: vec![CallSiteRef { kind: RefKind::Free, base: "g".into(), module: None, line: 2, col: 3 }],
            async_boundary: false, await_count: 0,
        };
        assert_eq!(r.refs.len(), 1);
        assert!(r.is_root);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core unit_record_holds_effects_and_refs`
Expected: FAIL — module `record` does not exist.

- [ ] **Step 3: Implement**

Write the type definitions above into `record.rs` (above the test module), and add
`pub mod record;` to `lib.rs`. Derive `Debug, Clone` everywhere; `Serialize` only on the wire
types (`ReachKind`, `ExternalReach`). `SiteKey` derives `PartialEq, Eq, Hash` for dedup.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core unit_record_holds_effects_and_refs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/record.rs crates/fxrank-core/src/lib.rs
git commit -m "feat(core): record module — UnitRecord/CallSiteRef/SiteKey/ExternalReach"
```

---

### Task 6: `CallGraph` — nodes + resolved/opaque edges

**Files:**
- Create: `crates/fxrank-core/src/graph.rs`
- Modify: `crates/fxrank-core/src/lib.rs` (`pub mod graph;`)
- Test: `crates/fxrank-core/src/graph.rs` tests

**Interfaces:**
- Consumes: `UnitRecord`, `UnitId`, `ExternalReach` (Task 5).
- Produces:

```rust
pub enum Edge { Resolved(UnitId), Opaque(crate::record::ExternalReach) }

pub struct CallGraph {
    pub nodes: std::collections::HashMap<UnitId, crate::record::UnitRecord>,
    pub edges: std::collections::HashMap<UnitId, Vec<Edge>>,
}
impl CallGraph {
    pub fn from_records(records: Vec<UnitRecord>, resolve: impl Fn(&CallSiteRef, &HashMap<UnitId,UnitRecord>) -> Edge) -> Self
    pub fn roots(&self) -> impl Iterator<Item = &UnitId>
}
```

The `resolve` closure is supplied by the caller (phase 2 frontends); for phase-1 tests we pass a
trivial resolver. This keeps `graph.rs` parser-free and resolution-policy-free.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::*;
    fn rec(id: &str, refs: Vec<&str>) -> UnitRecord {
        UnitRecord { unit_id: id.into(), path: id.into(), line:1, col:1, symbol: id.into(),
            is_root: id == "root", export: None, effects: vec![], risks: vec![],
            refs: refs.into_iter().map(|b| CallSiteRef{kind:RefKind::Free, base:b.into(), module:None, line:1, col:1}).collect(),
            async_boundary:false, await_count:0 }
    }
    #[test]
    fn builds_resolved_edges_by_base_name() {
        let recs = vec![rec("root", vec!["b"]), rec("b", vec![])];
        let g = CallGraph::from_records(recs, |r, nodes| {
            match nodes.keys().find(|k| **k == r.base) {
                Some(id) => Edge::Resolved(id.clone()),
                None => Edge::Opaque(ExternalReach{ specifier: r.base.clone(), kind: ReachKind::ThirdParty, site: "x".into() }),
            }
        });
        assert!(matches!(g.edges["root"][0], Edge::Resolved(ref id) if id == "b"));
        assert_eq!(g.roots().count(), 1);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core builds_resolved_edges_by_base_name`
Expected: FAIL — module `graph` missing.

- [ ] **Step 3: Implement**

Write `graph.rs` with the types above. `from_records` indexes nodes by `unit_id`, then for each
node maps its `refs` through `resolve`. `roots()` filters `nodes.values().filter(|r| r.is_root)`.
Add `pub mod graph;` to `lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core builds_resolved_edges_by_base_name`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/graph.rs crates/fxrank-core/src/lib.rs
git commit -m "feat(core): CallGraph with resolved/opaque edges from UnitRecords"
```

---

### Task 7: `fold` — single-hop escaping boundary summary

**Files:**
- Create: `crates/fxrank-core/src/fold.rs`
- Modify: `crates/fxrank-core/src/lib.rs` (`pub mod fold;`)
- Test: `crates/fxrank-core/src/fold.rs` tests

**Interfaces:**
- Consumes: `CallGraph`, `Edge` (Task 6); `Effect::escapes`, `RiskKind::escapes` (Tasks 3–4).
- Produces:

```rust
pub struct Propagated {
    pub effects: Vec<crate::effect::Effect>,       // own ∪ inherited (full, for ranking)
    pub risks: Vec<crate::effect::RiskFeature>,
    pub inherited: Vec<Inherited>,                  // folded-in signals w/ provenance
    pub external_reaches: Vec<crate::record::ExternalReach>,
}
pub struct Inherited { pub effect: Option<Effect>, pub risk: Option<RiskFeature>, pub from: UnitId, pub via: String }
pub fn boundary_summary(unit: &UnitId, g: &CallGraph) -> (Vec<Effect>, Vec<RiskFeature>)  // escaping subset, single hop
```

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // helper fns to build a graph with an escaping IO effect on "b" and a contained one ...
    #[test]
    fn summary_keeps_only_escaping() {
        // b has: net.fs.db (escaping) + local.mutation contained
        // boundary_summary(b) must contain net.fs.db and NOT the contained local.mutation
        let g = /* build via Task-6 helpers */ ;
        let (eff, _risk) = boundary_summary(&"b".into(), &g);
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].kind, EffectKind::NetFsDb);
    }
}
```

(Write the graph-builder helper inline, reusing Task 6's `rec` shape but giving `b` two effects.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core summary_keeps_only_escaping`
Expected: FAIL — `fold` module / `boundary_summary` missing.

- [ ] **Step 3: Implement single-hop summary**

In `fold.rs`: `boundary_summary(unit, g)` reads `g.nodes[unit]`, returns
`(effects.filter(Effect::escapes), risks.filter(|r| r.kind.escapes()))`. Add `pub mod fold;`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core summary_keeps_only_escaping`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/fold.rs crates/fxrank-core/src/lib.rs
git commit -m "feat(core): fold::boundary_summary — single-hop escaping filter"
```

---

### Task 8: `fold` — transitive memoized fixpoint with SCC + bounded provenance

**Files:**
- Modify: `crates/fxrank-core/src/fold.rs`
- Test: `crates/fxrank-core/src/fold.rs` tests

**Interfaces:**
- Produces: `pub fn fold(g: &CallGraph) -> HashMap<UnitId, Propagated>`. Transitive: `summary(u) =
  escaping(own) ∪ ⋃ summary(resolved callee)`, deduped by `SiteKey`; cycles handled via memoized
  fixpoint (compute on a visiting stack; a back-edge into an in-progress node contributes its
  current partial set; iterate to a fixed point so an SCC converges to one set). Provenance is
  **exemplar**: keep the first (shortest-discovered) path per inherited `SiteKey`, never all paths.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn transitive_io_reaches_root_diamond_counts_once() {
    // root -> a -> d(io);  root -> b -> d(io)  (diamond)
    // root.propagated effects contain exactly ONE net.fs.db (deduped by site)
}
#[test]
fn cycle_terminates_one_summary_per_scc() {
    // a -> b -> a, and b -> c(io). fold() returns; a.summary == b.summary and both contain c's io.
}
#[test]
fn provenance_is_bounded_on_cycle() {
    // same cycle; the inherited io on `a` carries exactly one `via` path, not unbounded.
}
```

(Fill the graph builders inline using Task 6 helpers; give `d`/`c` an escaping `net.fs.db`.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p fxrank-core -- fold`
Expected: FAIL — `fold` fn missing / wrong.

- [ ] **Step 3: Implement the fixpoint**

`fold(g)`: for each node compute `summary` via DFS with a `memo: HashMap<UnitId, Vec<(Effect|Risk, SiteKey)>>`
and an `on_stack` set; on a back-edge, fold in the callee's current memo (partial) instead of
recursing; after the DFS, run a second pass over each SCC member unioning the SCC's sets so all
members converge. Dedup every union by `SiteKey`. Record provenance only the first time a
`SiteKey` is inserted (`from = origin unit`, `via = "caller → … → origin"`, truncated to the
discovery path). Build `Propagated { effects: own ∪ inherited-effects, risks: own ∪ inherited-risks,
inherited, external_reaches: own-opaque ∪ inherited-opaque }`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core -- fold`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/fold.rs
git commit -m "feat(core): fold transitive fixpoint — SCC convergence + bounded provenance"
```

---

### Task 9: `fold` — collect external reaches along the fold

**Files:**
- Modify: `crates/fxrank-core/src/fold.rs`
- Test: `crates/fxrank-core/src/fold.rs` tests

**Interfaces:**
- Produces: an `Opaque` edge contributes (a) an `ExternalUnresolved`/class-2 escaping `Effect` to
  the referencing unit's summary (so it propagates up), and (b) an `ExternalReach` entry that
  propagates onto every unit that transitively reaches it.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn opaque_edge_becomes_external_reach_and_propagates() {
    // a -> opaque("analytics-sdk");  root -> a
    let out = fold(&g);
    assert!(out[&"a".into()].external_reaches.iter().any(|r| r.specifier == "analytics-sdk"));
    assert!(out[&"root".into()].external_reaches.iter().any(|r| r.specifier == "analytics-sdk"));
    // and an external.unresolved/2 effect rode up to root
    assert!(out[&"root".into()].effects.iter().any(|e| e.kind == EffectKind::ExternalUnresolved && e.class == 2));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core opaque_edge_becomes_external_reach_and_propagates`
Expected: FAIL.

- [ ] **Step 3: Implement**

When folding a node's edges, for each `Edge::Opaque(reach)`: push `reach.clone()` to the node's
reach set, and synthesize an `Effect { kind: ExternalUnresolved, class: 2, contained: false,
tier: Heuristic, line/col from the reach site, evidence: reach.specifier, confidence: penalty }`
into its escaping summary. Both flow up through the same `SiteKey`-deduped union.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core opaque_edge_becomes_external_reach_and_propagates`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/fold.rs
git commit -m "feat(core): fold records external reaches + external.unresolved propagation"
```

---

### Task 10: `Hotspot`/`Summary` propagated fields + `Report::build` ranking

**Files:**
- Modify: `crates/fxrank-core/src/model.rs` (`Hotspot`, `Summary`, `Scope`, `Report::build`)
- Modify: `crates/fxrank-core/src/score.rs` (`rank_key` — already takes the values it needs)
- Test: `crates/fxrank-core/src/model.rs` tests

**Interfaces:**
- Consumes: `Propagated` (Task 8), `ExternalReach` (Task 5).
- Produces: `Hotspot` gains `propagated_score: f64`, `propagated_max_class: u8`, `root: bool`,
  `inherited: Vec<Inherited wire form>`, `external_reaches: Vec<ExternalReach>`. `Scope`/`Summary`
  gain `external_reaches: Vec<ExternalReach>` and propagated aggregates. `Report::build` sorts by
  `rank_key(propagated_max_class, propagated_score, risk_weight, confidence)` and computes the
  propagated summary aggregates; own-body `own_score`/`max_class`/`effects` unchanged.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn ranks_by_propagated_not_own() {
    // h_own0: own_score 0 / max_class 0 BUT propagated 28.5 / class 7
    // h_own5: own_score 5 / max_class 4, propagated == own (no callees)
    // ranked first must be h_own0 (propagated class 7 > 4)
    let report = Report::build(Scope::empty("x"),
        vec![hot_prop("a", 0,0.0, 7,28.5, 0.9), hot_prop("b", 4,5.0, 4,5.0, 0.9)], vec![], None);
    assert_eq!(report.hotspots[0].id, "a");
    assert_eq!(report.summary.propagated_max_class, 7);
}
```

(Add a `hot_prop(...)` test helper extending the existing `hot(...)` with propagated fields.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core ranks_by_propagated_not_own`
Expected: FAIL — fields/helper missing.

- [ ] **Step 3: Implement**

Add the new fields to `Hotspot`/`Scope`/`Summary` (serialize after the own-body ones). In
`Report::build`, change the sort closure to `rank_key(h.propagated_max_class, h.propagated_score,
h.risk_weight, h.confidence)`; compute `summary.propagated_max_class` (max over hotspots +
`scope.risk_features`), `summary.propagated_score` (max over hotspots), and
`summary.external_reaches` (dedup union). Keep the existing own-body summary computations as-is.
Update the existing `hot(...)` helper and the prior summary tests to set the new fields
(propagated == own for those legacy hotspots so their assertions still hold).

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core`
Expected: PASS (legacy tests still pass because propagated defaults equal own there).

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/model.rs crates/fxrank-core/src/score.rs
git commit -m "feat(core): Hotspot/Summary propagated fields; rank by propagated aggregates"
```

---

### Task 11: End-to-end fold→report assembly helper + integration test

**Files:**
- Modify: `crates/fxrank-core/src/fold.rs` (add `pub fn to_hotspots(g, fold_out) -> Vec<Hotspot>`)
- Test: `crates/fxrank-core/src/fold.rs` tests

**Interfaces:**
- Produces: `pub fn to_hotspots(g: &CallGraph, folded: &HashMap<UnitId, Propagated>) -> Vec<Hotspot>`
  — assembles each `UnitRecord` + its `Propagated` into a `Hotspot` (own from the record, propagated
  from the fold, `root` from `is_root`, `external_reaches`, `inherited` wire form). This is the
  seam phase 2's frontends call after building their graph.

- [ ] **Step 1: Write the failing integration test**

```rust
#[test]
fn dashboard_scenario_root_blast_radius() {
    // root(no own effects) -> useStats -> fetchStats(net.fs.db escaping + opaque "analytics-sdk")
    let g = build_dashboard_graph();
    let folded = fold(&g);
    let hotspots = to_hotspots(&g, &folded);
    let root = hotspots.iter().find(|h| h.id.contains("root")).unwrap();
    assert_eq!(root.own_score, 0.0);                 // looks pure
    assert_eq!(root.propagated_max_class, 7);         // blast radius
    assert!(root.external_reaches.iter().any(|r| r.specifier == "analytics-sdk"));
    assert!(root.root);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-core dashboard_scenario_root_blast_radius`
Expected: FAIL — `to_hotspots` missing.

- [ ] **Step 3: Implement `to_hotspots`**

Map each `(unit_id, record)` to a `Hotspot`: own fields from the record (`own_score` via
`score::own_score` over own escaping+contained effect weights, `max_class` via `score::max_class`),
propagated fields from `folded[unit_id]` (`propagated_score` via `own_score` over the propagated
effect weights, `propagated_max_class` via `max_class`), `root = record.is_root`, `inherited` and
`external_reaches` from `Propagated`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p fxrank-core dashboard_scenario_root_blast_radius`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/fold.rs
git commit -m "feat(core): to_hotspots assembly + dashboard-scenario integration test"
```

---

### Task 12: Phase-1 gate — fmt, clippy, full workspace test

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all` then `cargo fmt --check`
Expected: clean.

- [ ] **Step 2: Clippy (warnings are errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. Fix any in the new modules.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: PASS — existing ~285 tests plus the new core tests; no frontend/CLI behavior changed
(frontends do not yet emit `UnitRecord`s, so `fxrank scan` output is byte-identical to today).

- [ ] **Step 4: Commit (if fmt/clippy touched anything)**

```bash
git add -A
git commit -m "chore(core): phase-1 fmt/clippy clean"
```

---

## Self-Review

**Spec coverage (phase-1 slice of spec 025):**
- §5 `UnitRecord`/`CallSiteRef`/`SiteKey` → Task 5. `ExternalUnresolved` → Task 2. `col` → Task 1.
- §7 escaping-only join, SCC fixpoint, bounded provenance, risk predicate → Tasks 3,4,7,8.
- §8 external reaches recorded + class-2 propagation → Task 9.
- §9 `Hotspot`/`Summary` propagated fields + `Report::build` ranking → Task 10.
- §11.3/§11.11 transitive/SCC/diamond + own-body byte-stable → Tasks 8, 11, 12.
- **Deferred to phase 2/3 (not this plan):** call-site *extraction* in frontends, module→file
  resolution, the pass-1/pass-2 driver + pooling, `--no-resolve`, roots, module-init, config
  classifier. Phase 1 ships the core engine tested against synthetic records only.

**Placeholder scan:** test bodies marked "build via Task-6 helpers" (Tasks 7–9) require the
implementer to write the inline graph-builder; this is intentional (the helper shape is given in
Task 6) and is real work, not a TODO. No "TBD"/"handle edge cases" placeholders remain.

**Type consistency:** `UnitId`, `SiteKey`, `CallSiteRef`, `UnitRecord`, `Edge`, `CallGraph`,
`Propagated`, `Inherited`, `ExternalReach`, `to_hotspots`, `fold`, `boundary_summary` are used
with the same signatures across Tasks 5–11. `Effect.contained`/`Effect::escapes`/`RiskKind::escapes`
consistent Tasks 3–9.

---

## Notes for phases 2 and 3 (to be written as their own plans after phase 1 lands)

- **Phase 2:** add `Frontend::scan(&[SourceFile]) -> Vec<UnitRecord>` (call-site extraction per
  frontend, syntactic only); a driver that pools records per language (dissolving `.ts`/`.tsx`)
  and runs `CallGraph::from_records` + `fold` + `to_hotspots`; `--no-resolve` (emit pass-1 records);
  the export index + module→file resolver per frontend. Rust + Python first (two reference
  consumers), then TS-React retrofit onto the shared fold.
- **Phase 3:** roots (Rust Cargo/module-tree; TS framework files/barrels; Python tiered `__all__`),
  the first-party/third-party classifier (read `tsconfig`/`pnpm-workspace`/`pyproject`/`Cargo.toml`),
  and module-init synthetic units (incl. `module_init→module_init` import edges).
