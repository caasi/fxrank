# 025-3e Plan 1 — Core: path-precise resolver + adoption gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the language-neutral core of spec 025-3e — `canonical_path`/`resolved_target`/`aliases` record fields, a `CanonicalIndex`, and a path-precise `resolve_ref` with the partition-adoption gate — wired into the CLI, shipping with **zero behavior change** (all frontends still emit empty canonical paths, so every partition is non-adopted and the gate runs the existing 025 name-based path verbatim).

**Architecture:** Shape A string-identity (spec 025-3e §3). Frontends will later emit canonical fully-qualified paths; this plan builds the neutral machinery that consumes them and a safe gate so an *unadopted* corpus (the state right after this plan lands) behaves exactly as 025. The dangerous false-resolve is closed by construction: a **qualified** ref never falls back to leaf-name matching.

**Tech Stack:** Rust, Cargo workspace. Crate under change: `fxrank-core` (parser-free). One wiring edit in `fxrank-cli`. No new dependencies.

## Global Constraints

- `fxrank-core` **depends on no parser** — no `syn`/`swc`/`libcst` may be referenced here (compiler-enforced). Verbatim from CLAUDE.md.
- **Own-body output byte-identical** to pre-3e: `UnitRecord` is the pass-1 intermediate and is **never serialized** (no `serde::Serialize` derive); the wire type is `Hotspot`/`Report`. New `UnitRecord`/`CallSiteRef` fields cannot reach the wire. (spec 025-3e §2)
- **`propagated_* ≥ own_*`** invariant (025 §13b F1) must remain green.
- CI gates that must pass for every commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- TDD: failing test first, minimal code, green, commit. Frequent commits.

---

## File Structure

- `crates/fxrank-core/src/record.rs` — **modify**: add `AliasFact`; add `CallSiteRef.resolved_target`; replace `UnitRecord.export` with `canonical_path` + `aliases`. Make `RefKind` derive `Default` (Free) so call sites can later default-build.
- `crates/fxrank-core/src/resolve.rs` — **modify**: keep `SymbolIndex` (becomes the fallback backend); add `CanonicalIndex` (holds a `SymbolIndex`, the primary + alias maps, the `adopted` flag) and `resolve_ref_precise`.
- `crates/fxrank-cli/src/main.rs` — **modify** (lines ~255-258): swap `SymbolIndex::from_records` → `CanonicalIndex::from_records` and the closure to call `resolve_ref_precise`.
- All `UnitRecord { … }` / `CallSiteRef { … }` literal construction sites across `crates/*/src` (compiler-enumerated, ~20 + ~25) — **modify**: set the new fields to non-adopted defaults.

---

### Task 1: Add neutral record fields + `AliasFact`

**Files:**
- Modify: `crates/fxrank-core/src/record.rs`
- Modify (compiler-enumerated sweep): every `UnitRecord { … }` and `CallSiteRef { … }` literal across `crates/*/src`

**Interfaces:**
- Produces:
  - `pub struct AliasFact { pub alias_path: Vec<String>, pub target: Vec<String> }` (derives `Debug, Clone, PartialEq, Eq`)
  - `CallSiteRef.resolved_target: Option<Vec<String>>` — frontend's import-resolved canonical callee path; `None` = not produced.
  - `UnitRecord.canonical_path: Vec<String>` — unit's canonical fully-qualified path; empty = frontend could not assign one.
  - `UnitRecord.aliases: Vec<AliasFact>` — re-export alias facts (replaces the dead `export` field).
  - `RefKind` now derives `Default` with `Free` as default.

- [ ] **Step 1: Write the failing test** (append to `record.rs`'s `#[cfg(test)] mod tests`)

```rust
#[test]
fn new_neutral_fields_default_to_non_adopted() {
    let r = UnitRecord {
        unit_id: "a.rs:1:1:f".into(),
        path: "a.rs".into(),
        line: 1,
        col: 1,
        symbol: "f".into(),
        is_root: false,
        canonical_path: vec![],
        aliases: vec![],
        effects: vec![],
        risks: vec![],
        refs: vec![CallSiteRef {
            kind: RefKind::Free,
            base: "g".into(),
            module: None,
            line: 2,
            col: 3,
            qualified: false,
            first_party: false,
            resolved_target: None,
        }],
        async_boundary: false,
        await_count: 0,
        language: Language::Rust,
    };
    assert!(r.canonical_path.is_empty());
    assert!(r.aliases.is_empty());
    assert_eq!(r.refs[0].resolved_target, None);
    // AliasFact constructs and compares by value.
    let a = AliasFact { alias_path: vec!["m".into(), "x".into()], target: vec!["n".into(), "x".into()] };
    assert_eq!(a.alias_path, vec!["m".to_string(), "x".to_string()]);
    assert_eq!(RefKind::default(), RefKind::Free);
}
```

- [ ] **Step 2: Run test to verify it fails to COMPILE**

Run: `cargo test -p fxrank-core new_neutral_fields_default_to_non_adopted 2>&1 | head -30`
Expected: compile error — `UnitRecord` has no field `canonical_path`/`aliases`, `CallSiteRef` has no field `resolved_target`, `AliasFact` not found, `RefKind: Default` not satisfied.

- [ ] **Step 3: Add the fields and `AliasFact` to `record.rs`**

Add the new struct (place it near `CallSiteRef`):

```rust
/// A re-export alias fact: `alias_path` names the same definition as `target`.
/// Emitted by a frontend per re-export (`pub use`, TS barrel, Python `__init__`);
/// the core indexes it as an extra key in `CanonicalIndex` (spec 025-3e §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasFact {
    pub alias_path: Vec<String>,
    pub target: Vec<String>,
}
```

Add `#[default]` to `RefKind::Free` and `Default` to its derive list:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RefKind {
    #[default]
    Free,
    Ctor,
    Method,
    Member,
    ModuleInit,
}
```

In `CallSiteRef`, add after `first_party`:

```rust
    /// Frontend's import-resolved canonical callee path (spec 025-3e §4.1).
    /// `Some(path)` = resolved against the module tree; `None` = not produced
    /// (frontend not adopted, or attempted-but-out-of-corpus). The
    /// adopted/non-adopted distinction is by the partition gate, not this field.
    pub resolved_target: Option<Vec<String>>,
```

In `UnitRecord`, **replace** the `export` field with the two new fields:

```rust
    /// Unit's canonical fully-qualified path as segments, e.g.
    /// `["crate","helpers","write"]` (spec 025-3e §4.1). Empty ⇒ the frontend
    /// could not assign one (no crate root in scope, cfg/macro module, …) ⇒
    /// the unit participates only via the degradation rules.
    pub canonical_path: Vec<String>,
    /// Re-export alias facts emitted by the frontend (replaces the old, unused
    /// `export` field). One per detected re-export.
    pub aliases: Vec<AliasFact>,
```

- [ ] **Step 4: Sweep all construction sites the compiler flags**

Run `cargo build --workspace 2>&1 | grep -E 'missing field|no field' | sort -u` to enumerate. For **every** `CallSiteRef { … }` literal, add the line `resolved_target: None,`. For **every** `UnitRecord { … }` literal, replace `export: None,` with `canonical_path: vec![],` and `aliases: vec![],`. These sites live in: `crates/fxrank-core/src/{record.rs,graph.rs,resolve.rs,fold.rs}` (tests), `crates/fxrank-lang-rust/src/detect/mod.rs::build_record` (+ its test), `crates/fxrank-lang-ts/src/**` build_record, `crates/fxrank-lang-python/src/**` build_record, and `crates/fxrank-cli/src/main.rs` (tests). Worked example for the Rust frontend's real emitter (`detect/mod.rs::build_record`):

```rust
    fxrank_core::record::UnitRecord {
        unit_id: unit.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,
        canonical_path: vec![], // 025-3e: frontend not yet adopted → non-adopted partition
        aliases: vec![],
        effects,
        risks,
        refs: call_refs,
        async_boundary,
        await_count,
        language: fxrank_core::frontend::Language::Rust,
    }
```

(The `refs` come from `refs::extract`, which builds `CallSiteRef`s — add `resolved_target: None` there too. Same for the TS/Python `refs` builders.)

- [ ] **Step 5: Verify the test passes and the workspace builds green**

Run: `cargo test -p fxrank-core new_neutral_fields_default_to_non_adopted && cargo build --workspace`
Expected: PASS; workspace builds with no `missing field` errors.

- [ ] **Step 6: Confirm own-body output is unchanged (byte-identical gate)**

Run: `cargo run -q -p fxrank -- scan crates/fxrank-core/src/score.rs --no-resolve | head -c 400`
Expected: identical to `main`'s output for the same command (no field added reaches the wire). Spot-check there is no `canonical_path`/`aliases`/`resolved_target` key in the JSON.

- [ ] **Step 7: Commit**

```bash
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(core): add 025-3e neutral fields (canonical_path, resolved_target, aliases)

Replace the dead UnitRecord.export with canonical_path + aliases; add
CallSiteRef.resolved_target and the AliasFact type. All frontends emit the
non-adopted defaults (empty), so behavior is unchanged. RefKind derives Default."
```

---

### Task 2: `CanonicalIndex` (primary + alias maps, adopted flag, holds `SymbolIndex`)

**Files:**
- Modify: `crates/fxrank-core/src/resolve.rs`

**Interfaces:**
- Consumes: `UnitRecord.{canonical_path, aliases, unit_id, refs}`, `AliasFact`, existing `SymbolIndex`.
- Produces:
  - `pub struct CanonicalIndex` with:
    - `pub fn from_records(records: &[UnitRecord]) -> Self`
    - `pub fn adopted(&self) -> bool`
    - internal: `primary: HashMap<Vec<String>, Vec<UnitId>>`, `aliases: HashMap<Vec<String>, Vec<String>>`, `name_idx: SymbolIndex`, `adopted: bool`
    - `fn lookup_canonical(&self, path: &[String]) -> Option<&UnitId>` — unique primary hit, else (ambiguous/none) `None`.
    - `fn follow_alias(&self, path: &[String]) -> Option<&UnitId>` — transitive alias chase to a unique primary hit, cycle-guarded.

- [ ] **Step 1: Write the failing tests** (append to `resolve.rs` tests)

```rust
#[test]
fn canonical_index_adopted_flag() {
    // No canonical paths → not adopted.
    let mut r0 = rec("a.rs:1:1:f", "f");
    r0.canonical_path = vec![];
    let idx0 = CanonicalIndex::from_records(std::slice::from_ref(&r0));
    assert!(!idx0.adopted(), "no canonical_path ⇒ not adopted");

    // ≥1 canonical path → adopted, and unique primary lookup hits.
    let mut r1 = rec("a.rs:1:1:write", "write");
    r1.canonical_path = vec!["crate".into(), "a".into(), "write".into()];
    let idx1 = CanonicalIndex::from_records(std::slice::from_ref(&r1));
    assert!(idx1.adopted(), "a canonical_path ⇒ adopted");
    assert_eq!(
        idx1.lookup_canonical(&["crate".into(), "a".into(), "write".into()]),
        Some(&"a.rs:1:1:write".to_string())
    );
    // Wrong full path ⇒ no hit (this is the false-resolve fix in microcosm).
    assert_eq!(idx1.lookup_canonical(&["std".into(), "fs".into(), "write".into()]), None);
}

#[test]
fn canonical_index_full_path_collision_is_ambiguous() {
    let mut a = rec("a.rs:1:1:dup", "dup");
    a.canonical_path = vec!["crate".into(), "dup".into()];
    let mut b = rec("b.rs:1:1:dup", "dup");
    b.canonical_path = vec!["crate".into(), "dup".into()]; // same full path (cfg dup)
    let idx = CanonicalIndex::from_records(&[a, b]);
    assert_eq!(idx.lookup_canonical(&["crate".into(), "dup".into()]), None, "full-path collision ⇒ ambiguous drop");
}

#[test]
fn canonical_index_alias_hop_and_cycle() {
    let mut def = rec("a.rs:1:1:thing", "thing");
    def.canonical_path = vec!["crate".into(), "internal".into(), "thing".into()];
    // re-export: crate::api::thing → crate::internal::thing
    def.aliases = vec![AliasFact {
        alias_path: vec!["crate".into(), "api".into(), "thing".into()],
        target: vec!["crate".into(), "internal".into(), "thing".into()],
    }];
    let idx = CanonicalIndex::from_records(std::slice::from_ref(&def));
    assert_eq!(
        idx.follow_alias(&["crate".into(), "api".into(), "thing".into()]),
        Some(&"a.rs:1:1:thing".to_string()),
        "alias path resolves to the canonical definition"
    );

    // A self-referential alias cycle must terminate (return None, not loop).
    let mut cyc = rec("c.rs:1:1:x", "x");
    cyc.canonical_path = vec!["crate".into(), "x".into()];
    cyc.aliases = vec![AliasFact {
        alias_path: vec!["crate".into(), "loop".into()],
        target: vec!["crate".into(), "loop".into()], // points at itself
    }];
    let idxc = CanonicalIndex::from_records(std::slice::from_ref(&cyc));
    assert_eq!(idxc.follow_alias(&["crate".into(), "loop".into()]), None, "alias cycle terminates");
}
```

(Note: `rec(...)` is the existing test helper in `resolve.rs`; after Task 1 it sets `canonical_path: vec![]`, `aliases: vec![]`. The tests mutate those fields.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-core canonical_index 2>&1 | head -20`
Expected: compile error — `CanonicalIndex` not found.

- [ ] **Step 3: Implement `CanonicalIndex` in `resolve.rs`**

```rust
/// Path-precise index for spec 025-3e. Holds the 025 `SymbolIndex` as the
/// name-based fallback backend, plus a primary map (canonical path → units),
/// an alias map (re-export path → target path), and the partition-adoption flag.
pub struct CanonicalIndex {
    primary: HashMap<Vec<String>, Vec<UnitId>>,
    aliases: HashMap<Vec<String>, Vec<String>>,
    name_idx: SymbolIndex,
    adopted: bool,
}

impl CanonicalIndex {
    pub fn from_records(records: &[UnitRecord]) -> Self {
        let mut primary: HashMap<Vec<String>, Vec<UnitId>> = HashMap::new();
        let mut aliases: HashMap<Vec<String>, Vec<String>> = HashMap::new();
        let mut adopted = false;
        for rec in records {
            if !rec.canonical_path.is_empty() {
                adopted = true;
                primary
                    .entry(rec.canonical_path.clone())
                    .or_default()
                    .push(rec.unit_id.clone());
            }
            for a in &rec.aliases {
                aliases.insert(a.alias_path.clone(), a.target.clone());
            }
        }
        let name_idx = SymbolIndex::from_records(records);
        Self { primary, aliases, name_idx, adopted }
    }

    pub fn adopted(&self) -> bool {
        self.adopted
    }

    /// Unique primary hit, else None (a multi-unit collision is ambiguous → drop).
    fn lookup_canonical(&self, path: &[String]) -> Option<&UnitId> {
        match self.primary.get(path).map(|v| v.as_slice()) {
            Some([id]) => Some(id),
            _ => None,
        }
    }

    /// Follow the alias map transitively to a unique primary hit, cycle-guarded.
    fn follow_alias(&self, path: &[String]) -> Option<&UnitId> {
        let mut cur = path.to_vec();
        let mut seen = std::collections::HashSet::new();
        loop {
            if let Some(id) = self.lookup_canonical(&cur) {
                return Some(id);
            }
            if !seen.insert(cur.clone()) {
                return None; // cycle
            }
            match self.aliases.get(&cur) {
                Some(next) => cur = next.clone(),
                None => return None,
            }
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-core canonical_index`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(core): add CanonicalIndex (primary/alias maps, adopted flag)

Holds the 025 SymbolIndex as the name-based fallback backend. Unique full-path
lookup, ambiguous-drop on full-path collision, transitive cycle-guarded alias
follow. Pure data; no parser."
```

---

### Task 3: `resolve_ref_precise` — adoption gate + no-leaf-fallback-for-qualified

**Files:**
- Modify: `crates/fxrank-core/src/resolve.rs`

**Interfaces:**
- Consumes: `CanonicalIndex`, `CallSiteRef`, existing `resolve_ref` (the 025 name-based fn) + `Edge`/`ExternalReach`/`ReachKind`.
- Produces: `pub fn resolve_ref_precise(r: &CallSiteRef, idx: &CanonicalIndex, referencing_path: &str) -> Option<Edge>`

- [ ] **Step 1: Write the failing tests** (append to `resolve.rs` tests)

```rust
fn precise_rec(id: &str, canon: &[&str]) -> UnitRecord {
    let mut r = rec(id, id.rsplit([':', '.']).next().unwrap_or(id));
    r.canonical_path = canon.iter().map(|s| s.to_string()).collect();
    r
}

#[test]
fn precise_win_resolves_what_025_had_to_drop() {
    // a::write and b::write are leaf-ambiguous under 025 (both → "write" → dropped).
    let aw = precise_rec("a.rs:1:1:write", &["crate", "a", "write"]);
    let bw = precise_rec("b.rs:1:1:write", &["crate", "b", "write"]);
    let idx = CanonicalIndex::from_records(&[aw, bw]);
    // A call whose frontend resolved the target to crate::a::write resolves to it ONLY.
    let call = CallSiteRef {
        kind: RefKind::Free,
        base: "a::write".into(),
        module: None,
        line: 9,
        col: 1,
        qualified: true,
        first_party: true,
        resolved_target: Some(vec!["crate".into(), "a".into(), "write".into()]),
    };
    assert!(matches!(
        resolve_ref_precise(&call, &idx, "c.rs"),
        Some(Edge::Resolved(ref id)) if id == "a.rs:1:1:write"
    ));
}

#[test]
fn precise_qualified_miss_goes_opaque_never_leaf() {
    // The headline false-resolve: a lone Foo::write in scope, a qualified std::fs::write call.
    let foo = precise_rec("a.rs:1:1:write", &["crate", "Foo", "write"]);
    let idx = CanonicalIndex::from_records(std::slice::from_ref(&foo));
    let stdcall = CallSiteRef {
        kind: RefKind::Free,
        base: "std::fs::write".into(),
        module: Some("std".into()),
        line: 3,
        col: 5,
        qualified: true,
        first_party: false,
        resolved_target: None, // frontend could not resolve (out of corpus)
    };
    // MUST be Opaque(ThirdParty), MUST NOT resolve to Foo::write.
    assert!(matches!(
        resolve_ref_precise(&stdcall, &idx, "b.rs"),
        Some(Edge::Opaque(ref reach)) if matches!(reach.kind, ReachKind::ThirdParty)
    ));
}

#[test]
fn precise_unqualified_miss_may_leaf_fallback_in_adopted() {
    // In an adopted partition, a BARE unqualified call may still leaf-resolve a local.
    let helper = precise_rec("a.rs:1:1:helper", &["crate", "helper"]);
    let idx = CanonicalIndex::from_records(std::slice::from_ref(&helper));
    let bare = CallSiteRef {
        kind: RefKind::Free,
        base: "helper".into(),
        module: None,
        line: 2,
        col: 2,
        qualified: false,
        first_party: false,
        resolved_target: None, // frontend left it uncanonicalized
    };
    assert!(matches!(
        resolve_ref_precise(&bare, &idx, "b.rs"),
        Some(Edge::Resolved(ref id)) if id == "a.rs:1:1:helper"
    ));
}

#[test]
fn precise_non_adopted_delegates_to_025_verbatim() {
    // No canonical paths anywhere → 025 name-based path runs. A qualified call with a
    // lone leaf match leaf-resolves (the 025 behavior we must preserve until adoption).
    let helper = rec("a.rs:1:1:helper", "helper"); // canonical_path empty
    let idx = CanonicalIndex::from_records(std::slice::from_ref(&helper));
    assert!(!idx.adopted());
    let call = CallSiteRef {
        kind: RefKind::Free,
        base: "helper".into(),
        module: None,
        line: 2,
        col: 3,
        qualified: false,
        first_party: false,
        resolved_target: None,
    };
    assert!(matches!(
        resolve_ref_precise(&call, &idx, "b.rs"),
        Some(Edge::Resolved(ref id)) if id == "a.rs:1:1:helper"
    ));
}

#[test]
fn precise_method_kind_drops() {
    let idx = CanonicalIndex::from_records(&[]);
    let m = CallSiteRef {
        kind: RefKind::Method,
        base: "len".into(),
        module: None,
        line: 1,
        col: 1,
        qualified: false,
        first_party: false,
        resolved_target: None,
    };
    assert!(resolve_ref_precise(&m, &idx, "b.rs").is_none());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-core precise_ 2>&1 | head -20`
Expected: compile error — `resolve_ref_precise` not found.

- [ ] **Step 3: Implement `resolve_ref_precise` in `resolve.rs`**

```rust
/// Path-precise resolution with the spec 025-3e adoption gate.
///
/// Step 0 — gate: a non-adopted partition (no unit has a canonical_path) runs the
/// 025 name-based `resolve_ref` verbatim. Steps 1–3 apply only when adopted.
/// Step 1 — `RefKind::Method` drops (no receiver type).
/// Step 2 — `resolved_target` canonical lookup, then transitive alias follow.
/// Step 3 — miss: a QUALIFIED ref goes Opaque (NEVER a leaf lookup — this is the
/// false-resolve fix); an UNqualified bare ref may leaf-fall-back via the 025
/// `SymbolIndex`, else None.
pub fn resolve_ref_precise(
    r: &CallSiteRef,
    idx: &CanonicalIndex,
    referencing_path: &str,
) -> Option<Edge> {
    // Step 0 — adoption gate.
    if !idx.adopted() {
        return resolve_ref(r, &idx.name_idx, referencing_path);
    }
    // Step 1 — method early-drop.
    if r.kind == RefKind::Method {
        return None;
    }
    // Step 2 — canonical resolution.
    if let Some(path) = &r.resolved_target {
        if let Some(id) = idx.lookup_canonical(path).or_else(|| idx.follow_alias(path)) {
            return Some(Edge::Resolved(id.clone()));
        }
    }
    // Step 3 — miss handling.
    let site = format!("{referencing_path}:{}:{}", r.line, r.col);
    if r.qualified {
        // QUALIFIED miss → opaque external reach. NEVER a leaf lookup.
        let kind = if r.first_party {
            ReachKind::FirstPartyOutOfScope
        } else {
            ReachKind::ThirdParty
        };
        Some(Edge::Opaque(ExternalReach {
            specifier: r.module.clone().unwrap_or_else(|| r.base.clone()),
            kind,
            site,
        }))
    } else {
        // UNqualified bare name → 025 leaf fallback (safe: not the import surface).
        // Reuse the 025 resolver, which leaf-looks-up and returns None for bare misses.
        resolve_ref(r, &idx.name_idx, referencing_path)
    }
}
```

Expose `name_idx` to this fn: it is in the same module, so add a private accessor or make the field `pub(crate)`. Simplest — add to `impl CanonicalIndex`:

```rust
    /// The name-based fallback backend (025 SymbolIndex).
    pub(crate) fn name_index(&self) -> &SymbolIndex {
        &self.name_idx
    }
```

…and in `resolve_ref_precise` use `idx.name_index()` instead of `idx.name_idx`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-core precise_`
Expected: 5 tests PASS. Then `cargo test -p fxrank-core` — ALL existing 025 `resolve_ref` tests still green (the gate delegates to them unchanged).

- [ ] **Step 5: clippy + fmt gate**

Run: `cargo fmt -p fxrank-core && cargo clippy -p fxrank-core --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(core): resolve_ref_precise with adoption gate (025-3e §4.3)

Non-adopted partition → 025 name-based resolve verbatim. Adopted: canonical
lookup + alias follow; on miss, a QUALIFIED ref goes opaque (never a leaf
lookup — closes the false-resolve), an unqualified bare ref keeps the 025 leaf
fallback. Method refs drop."
```

---

### Task 4: Wire the CLI to `CanonicalIndex` / `resolve_ref_precise`

**Files:**
- Modify: `crates/fxrank-cli/src/main.rs` (the `partition_by_language` resolve loop, ~lines 251-261) and its `use` imports (line 7).

**Interfaces:**
- Consumes: `CanonicalIndex::from_records`, `resolve_ref_precise`.
- Produces: identical CLI output to pre-3e while no frontend is adopted (every partition non-adopted → gate → 025 path).

- [ ] **Step 1: Capture the golden output BEFORE the change**

Run: `cargo run -q -p fxrank -- scan crates/fxrank-core/ > /tmp/3e-golden-pre.json` (from the worktree)
This is the reference; Step 5 diffs against it.

**Why `crates/fxrank-core/` and NOT `crates/`:** the swap edits `crates/fxrank-cli/src/main.rs`, and fxrank dogfoods its own source — so scanning `crates/` would (correctly) report a changed `run_scan`/`main` because *their source genuinely changed* (they now call `resolve_ref_precise`/`CanonicalIndex`). That is self-reference, not a resolver-behavior change, and it makes a `scan crates/` diff non-empty by construction. To prove the *resolver behavior* is unchanged, the gate must scan a corpus that **excludes the edited file** — `crates/fxrank-core/` contains no edited file, so any diff there would be a real regression. (Also normalize away the known `scope.external_reaches` run-to-run ordering nondeterminism before diffing — see Step 5.)

- [ ] **Step 2: Swap the import and the resolve wiring**

In `main.rs` line ~7, change:

```rust
use fxrank_core::resolve::{SymbolIndex, resolve_ref};
```
to:
```rust
use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
```

In the resolve loop (~lines 255-258), change:

```rust
            let idx = SymbolIndex::from_records(&group);
            let graph = CallGraph::from_records(group, |r, owner, _nodes| {
                resolve_ref(r, &idx, &owner.path)
            });
```
to:
```rust
            let idx = CanonicalIndex::from_records(&group);
            let graph = CallGraph::from_records(group, |r, owner, _nodes| {
                resolve_ref_precise(r, &idx, &owner.path)
            });
```

- [ ] **Step 3: Build the workspace**

Run: `cargo build --workspace`
Expected: builds. (If `SymbolIndex`/`resolve_ref` become unused anywhere else, they are still `pub` and used by tests — no dead-code warning at the crate boundary.)

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --workspace`
Expected: all ~90+ tests PASS, including CLI snapshot/insta tests (output unchanged).

- [ ] **Step 5: Behavior-identical gate (corpus excluding the edited file)**

Run:
```bash
proj() { jq -S '.hotspots | sort_by(.id) | map({id, own_score, propagated_score, propagated_max_class, max_class})'; }
cargo run -q -p fxrank -- scan crates/fxrank-core/ 2>/dev/null | proj > /tmp/3e-golden-post.json
diff <(proj < /tmp/3e-golden-pre.json 2>/dev/null || cat /tmp/3e-golden-pre.json) /tmp/3e-golden-post.json \
  && echo "BEHAVIOR-IDENTICAL ✓"
```
(Capture the Step-1 golden through the same `proj` projection so the comparison ignores the nondeterministic `scope.external_reaches` ordering and compares the deterministic per-hotspot propagation values.)
Expected: `BEHAVIOR-IDENTICAL ✓` (no diff). This proves the resolver landed with zero behavior change — every partition is non-adopted, so the gate runs 025 verbatim. A diff here (on a corpus with no edited file) would be a real regression → STOP and report BLOCKED.

Optional sanity (NOT a gate): `scan crates/` will differ on exactly `run_scan`/`main` by the self-dogfood of the edited `main.rs` (their `propagated_score` rises by one class-2 reach). That is expected and correct — confirm the ONLY changed hotspots are in the edited file, nothing else.

- [ ] **Step 6: clippy + fmt across the workspace**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(cli): resolve via CanonicalIndex/resolve_ref_precise (025-3e core)

The adoption gate makes every partition non-adopted (no frontend emits canonical
paths yet) → 025 name-based resolution runs verbatim → scan output is
byte-identical. Foundation for the per-frontend canonical-path emitters."
```

---

## Self-Review

**Spec coverage (025-3e §4 — the core):**
- §4.1 neutral fields (`canonical_path`, `resolved_target`, `aliases` replacing `export`) → Task 1. ✓
- §4.2 `CanonicalIndex` (primary map, alias map, adopted flag, holds `SymbolIndex`, full-path ambiguous-drop) → Task 2. ✓
- §4.3 gated `resolve_ref` (adoption gate, method-drop, canonical+alias, qualified-opaque-never-leaf, unqualified-leaf) → Task 3. ✓
- CLI wiring + byte-identical floor (§6) → Task 4. ✓
- Interning (§4.1) — **deliberately deferred** as a YAGNI optimization for v1; `Vec<String>` keys are used directly (`Hash + Eq`). Noted here so it's a tracked omission, not a silent gap.
- Per-frontend emitters (§5) — **out of scope for this plan**; they are Plans 2 (Rust), 3 (TS), 4 (Python). This plan ships the neutral core alone, which is self-contained and byte-identical by design.

**Placeholder scan:** no TBD/TODO; every code step shows complete code; the construction-site sweep (Task 1 Step 4) is a compiler-enumerated mechanical rule with a worked example, not a vague instruction.

**Type consistency:** `resolve_ref_precise(&CallSiteRef, &CanonicalIndex, &str) -> Option<Edge>` is used identically in Task 3 (def) and Task 4 (call). `CanonicalIndex::from_records(&[UnitRecord])` matches between Tasks 2 and 4. `AliasFact { alias_path, target }` field names are consistent across Tasks 1–2. `canonical_path`/`resolved_target`/`aliases` names match between Task 1 (def) and Tasks 2–3 (use).

## Execution Handoff

This is **Plan 1 of the 025-3e set** (core). Plans 2–4 (Rust / TS / Python canonical-path emitters) flip partitions to *adopted* and are written next. After this plan: corpus is non-adopted everywhere → output unchanged, but the machinery is live and unit-tested.
