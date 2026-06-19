# FxRank Rust Effect Scanner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Milestone A of FxRank — a `syn`-based Rust analyzer that scores each function's own effect cost, ranks hotspots, and emits compact JSON to stdout for coding agents.

**Architecture:** A Cargo workspace with three crates. `fxrank-core` holds the language-neutral scoring model (effect vocabulary, Fibonacci weights, containment discount, aggregation/rank, confidence, JSON model, `Frontend` trait) and depends on no parser. `fxrank-lang-rust` implements `Frontend` over `syn`. `fxrank-cli` (package `fxrank`) parses args, discovers `.rs` files, dispatches by extension, and prints compact JSON. Build the pure core first (fully unit-testable without parsing), then the frontend, then the CLI.

**Tech Stack:** Rust edition 2024; `serde`/`serde_json` (core model + compact output); `syn` + `proc-macro2` with `span-locations` (Rust frontend, line numbers); `clap` (CLI args); `insta` (snapshot tests) and `assert_cmd`/`predicates` (CLI integration tests).

Spec: `specs/001-fxrank-rust-effect-scanner.md` — the source of truth for every score, class, discount, and schema field. When this plan and the spec disagree, the spec wins; fix the plan.

---

## Conventions for every task

- **TDD**: write the failing test, run it red, implement minimally, run it green, commit.
- **One logical change per commit**; conventional-commit messages (`feat:`/`test:`/`chore:`).
- After any change, the full gate must pass before the task's final commit:
  `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`.
- Edition 2024, `rustfmt.toml` and `[lints.clippy] all = "warn"` from spec 000 apply to **every** crate (the workspace inherits them — see Task 0).
- Implementation work happens on a **feature branch in a worktree** (see Execution Handoff), never on `main`.

## File Structure

```text
fxrank/
  Cargo.toml                         # [workspace] members + shared lints/edition
  crates/
    fxrank-core/
      Cargo.toml                     # serde, serde_json
      src/lib.rs                     # re-exports
      src/effect.rs                  # EffectKind, Tier, Effect, RiskFeature
      src/score.rs                   # class->weight, discount, own_score, max_class, rank key
      src/confidence.rs              # tier bases + penalties + min
      src/model.rs                   # Function, Hotspot, Scope, Summary, Diagnostic, Report
      src/frontend.rs                # Frontend trait, SourceFile, FrontendOutput
    fxrank-lang-rust/
      Cargo.toml                     # syn (full), proc-macro2 (span-locations), fxrank-core
      src/lib.rs                     # RustFrontend: impl Frontend
      src/functions.rs              # collect function units + ids from a syn::File
      src/imports.rs                 # use-statement import table
      src/detect/mod.rs             # effect detection orchestration per function
      src/detect/calls.rs           # path/method-call effects (io, time, env, ...)
      src/detect/macros.rs          # macro effects + unknown.macro + whitelist
      src/detect/mutation.rs        # local/param/hidden/global mutation + discount
      src/detect/risk.rs            # risk_features + module-level risk
      tests/fixtures/               # .rs fixtures (one per spec case)
      tests/rust_frontend.rs        # snapshot tests over fixtures
    fxrank-cli/
      Cargo.toml                     # clap, serde_json, fxrank-core, fxrank-lang-rust
      src/main.rs                    # arg parse, file discovery, dispatch, print
      tests/cli.rs                   # assert_cmd integration + dogfood smoke
```

Each `src/*.rs` has one responsibility; detection is split by effect family so each file stays small and individually testable.

---

## Phase 0 — Workspace

### Task 0: Convert the scaffold into a workspace

**Files:**
- Modify: `Cargo.toml` (root → workspace)
- Create: `crates/fxrank-core/Cargo.toml`, `crates/fxrank-core/src/lib.rs`
- Create: `crates/fxrank-lang-rust/Cargo.toml`, `crates/fxrank-lang-rust/src/lib.rs`
- Create: `crates/fxrank-cli/Cargo.toml`, `crates/fxrank-cli/src/main.rs`
- Delete: root `src/main.rs` (its greeting/test move into the CLI crate as a placeholder)

- [ ] **Step 1: Write the root workspace manifest**

```toml
# Cargo.toml
[workspace]
resolver = "3"
members = ["crates/fxrank-core", "crates/fxrank-lang-rust", "crates/fxrank-cli"]

[workspace.package]
edition = "2024"
version = "0.1.0"

[workspace.lints.clippy]
all = "warn"
```

- [ ] **Step 2: Create the three crate manifests**

```toml
# crates/fxrank-core/Cargo.toml
[package]
name = "fxrank-core"
edition.workspace = true
version.workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[lints]
workspace = true
```

```toml
# crates/fxrank-lang-rust/Cargo.toml
[package]
name = "fxrank-lang-rust"
edition.workspace = true
version.workspace = true

[dependencies]
fxrank-core = { path = "../fxrank-core" }
syn = { version = "2", features = ["full", "extra-traits"] }
proc-macro2 = { version = "1", features = ["span-locations"] }

[lints]
workspace = true
```

```toml
# crates/fxrank-cli/Cargo.toml
[package]
name = "fxrank"
edition.workspace = true
version.workspace = true

[[bin]]
name = "fxrank"
path = "src/main.rs"

[dependencies]
fxrank-core = { path = "../fxrank-core" }
fxrank-lang-rust = { path = "../fxrank-lang-rust" }
clap = { version = "4", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
assert_cmd = "2"
predicates = "3"

[lints]
workspace = true
```

> `proc-macro2`'s `span-locations` feature is what makes `span.start().line` return real line numbers outside a proc-macro context. Without it, every line is `0`. This is load-bearing for the whole `line` field.

- [ ] **Step 3: Minimal crate roots so the workspace builds**

```rust
// crates/fxrank-core/src/lib.rs
//! FxRank core: language-neutral effect scoring model.
```

```rust
// crates/fxrank-lang-rust/src/lib.rs
//! FxRank Rust frontend (syn-based).
```

```rust
// crates/fxrank-cli/src/main.rs
fn main() {
    println!("fxrank");
}
```

- [ ] **Step 4: Move the rustfmt/toolchain files (already at root — they apply workspace-wide). Delete the old `src/`.**

Run: `git rm src/main.rs`

- [ ] **Step 5: Verify the gate passes**

Run: `cargo build && cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`
Expected: builds; no tests yet (0 passed); fmt clean; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: restructure fxrank into a cargo workspace"
```

---

## Phase 1 — fxrank-core (pure scoring model, no parsing)

> Everything here is unit-testable without `syn`. Each task adds one focused module.

### Task 1: Severity class → Fibonacci weight

**Files:**
- Create: `crates/fxrank-core/src/score.rs`
- Modify: `crates/fxrank-core/src/lib.rs` (add `pub mod score;`)

- [ ] **Step 1: Failing test**

```rust
// in src/score.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_map_is_fibonacci() {
        let expected = [0, 1, 2, 3, 5, 8, 13, 21, 34];
        for (class, w) in expected.iter().enumerate() {
            assert_eq!(weight_for_class(class as u8), *w);
        }
    }
}
```

- [ ] **Step 2: Run red** — `cargo test -p fxrank-core weight_map` → FAIL (`weight_for_class` undefined).

- [ ] **Step 3: Implement**

```rust
// src/score.rs
/// Severity classes are 0..=8. The convex weight makes one severe effect
/// dominate a pile of trivial ones (spec: Scoring Model).
pub const CLASS_WEIGHTS: [u32; 9] = [0, 1, 2, 3, 5, 8, 13, 21, 34];

/// Weight for a severity class, clamped to the valid range.
pub fn weight_for_class(class: u8) -> u32 {
    CLASS_WEIGHTS[(class as usize).min(8)]
}
```

- [ ] **Step 4: Run green** — `cargo test -p fxrank-core weight_map` → PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(core): fibonacci class-weight map"`

### Task 2: Effect kinds, tiers, and the Effect/RiskFeature types

**Files:**
- Create: `crates/fxrank-core/src/effect.rs`
- Modify: `crates/fxrank-core/src/lib.rs` (`pub mod effect;`)

- [ ] **Step 1: Failing test** — assert serde emits the wire names and the base class for a couple of kinds.

```rust
// src/effect.rs tests
#[test]
fn kind_wire_names_and_base_class() {
    assert_eq!(EffectKind::NetFsDb.wire(), "net.fs.db");
    assert_eq!(EffectKind::NetFsDb.base_class(), 7);
    assert_eq!(EffectKind::LocalMutation.base_class(), 1);
    assert_eq!(EffectKind::UnknownMacro.base_class(), 2);
}
```

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** — the full catalog vocabulary from the spec's *Effect Catalog* table, plus tier and the effect record.

```rust
// src/effect.rs
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier { Exact, Path, Heuristic }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    NetFsDb, ProcessControl, EnvWrite, Concurrency, TimeRead, Random, EnvRead,
    Logging, Panic, GlobalMutation, HiddenMutation, ParamMutation, AmbientRead,
    LocalMutation, UnknownMacro,
}

impl EffectKind {
    /// Wire name used in JSON `kind`.
    pub fn wire(self) -> &'static str {
        use EffectKind::*;
        match self {
            NetFsDb => "net.fs.db", ProcessControl => "process.control",
            EnvWrite => "env.write", Concurrency => "concurrency", TimeRead => "time.read",
            Random => "random", EnvRead => "env.read", Logging => "logging",
            Panic => "panic", GlobalMutation => "global.mutation",
            HiddenMutation => "hidden.mutation", ParamMutation => "param.mutation",
            AmbientRead => "ambient.read", LocalMutation => "local.mutation",
            UnknownMacro => "unknown.macro",
        }
    }

    /// Base severity class before any discount (spec catalog).
    pub fn base_class(self) -> u8 {
        use EffectKind::*;
        match self {
            NetFsDb => 7,
            ProcessControl | EnvWrite | Concurrency => 6,
            TimeRead | Random => 5,
            EnvRead | Logging | Panic => 4,
            GlobalMutation | HiddenMutation | ParamMutation => 3,
            AmbientRead | UnknownMacro => 2,
            LocalMutation => 1,
        }
    }
}
```

```rust
// src/effect.rs (cont.)
use crate::score::weight_for_class;

#[derive(Debug, Clone, Serialize)]
pub struct Effect {
    #[serde(rename = "kind", serialize_with = "ser_kind")]
    pub kind: EffectKind,
    pub class: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discounted_to: Option<u8>,
    pub weight: u32,
    pub line: usize,
    pub tier: Tier,
    pub confidence: f64,
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount: Option<String>,
}

impl Effect {
    /// The class that actually scores (post-discount when present).
    pub fn effective_class(&self) -> u8 { self.discounted_to.unwrap_or(self.class) }
    /// Recompute `weight` from the effective class. Call after setting a discount.
    pub fn sync_weight(&mut self) { self.weight = weight_for_class(self.effective_class()); }
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskFeature {
    pub kind: String,   // e.g. "transmute", "mem.forget", "impl.drop"
    pub class: u8,
    pub weight: u32,
    pub line: usize,
    pub tier: Tier,
    pub evidence: String,
}

fn is_false(b: &bool) -> bool { !*b }
fn ser_kind<S: serde::Serializer>(k: &EffectKind, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(k.wire())
}
```

- [ ] **Step 4: Run green.**
- [ ] **Step 5: Commit** — `git commit -am "feat(core): effect kinds, tiers, Effect/RiskFeature"`

### Task 3: Containment discount (class down-shift, clamp, unsafe-cancel)

**Files:** Modify `crates/fxrank-core/src/score.rs`

- [ ] **Step 1: Failing tests** — encode the spec's discount rules exactly.

```rust
#[test]
fn discounts_shift_classes_and_clamp() {
    // &mut param: class 3 -> down 2 -> class 1
    assert_eq!(apply_discount(3, Discount::MutParam, false), 1);
    // &mut self: class 3 -> down 1 -> class 2
    assert_eq!(apply_discount(3, Discount::MutSelf, false), 2);
    // externally-observable never below class 1
    assert_eq!(apply_discount(1, Discount::MutParam, false), 1);
    // unsafe-enclosed mutation: discount cancelled, stays at base
    assert_eq!(apply_discount(3, Discount::MutParam, true), 3);
    // no discount channel: unchanged
    assert_eq!(apply_discount(3, Discount::None, false), 3);
}
```

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discount { None, MutParam, MutSelf }

/// Apply a containment discount as a class down-shift. `unsafe_enclosed` cancels
/// the discount (spec: Containment discount). The floor is class 1 for
/// externally-observable effects (all discountable mutation channels are).
pub fn apply_discount(base_class: u8, discount: Discount, unsafe_enclosed: bool) -> u8 {
    if unsafe_enclosed || discount == Discount::None {
        return base_class;
    }
    let shift = match discount { Discount::MutParam => 2, Discount::MutSelf => 1, Discount::None => 0 };
    base_class.saturating_sub(shift).max(1)
}
```

- [ ] **Step 4: Run green.** **Step 5: Commit** — `git commit -am "feat(core): containment discount as class down-shift"`

### Task 4: own_score, risk_class/risk_weight, max_class

**Files:** Modify `crates/fxrank-core/src/score.rs`

- [ ] **Step 1: Failing tests** — the aggregation formulas from the spec.

```rust
#[test]
fn own_score_damps_non_max_weights() {
    // weights 21, 8, 1 -> 21 + 0.5*(8+1) = 25.5
    assert_eq!(own_score(&[21, 8, 1]), 25.5);
    assert_eq!(own_score(&[]), 0.0);
    assert_eq!(own_score(&[5]), 5.0);
}

#[test]
fn max_class_includes_risk() {
    // effects max class 0, risk class 4 -> max_class 4
    assert_eq!(max_class(&[0], 4), 4);
    assert_eq!(max_class(&[7], 0), 7);
}
```

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement**

```rust
/// own_score = max_weight + 0.5 * sum(other weights). Effects only.
pub fn own_score(weights: &[u32]) -> f64 {
    let max = weights.iter().copied().max().unwrap_or(0);
    let rest: u32 = weights.iter().copied().sum::<u32>() - max;
    max as f64 + 0.5 * rest as f64
}

/// Highest class across effects and the function's risk_class.
pub fn max_class(effect_classes: &[u8], risk_class: u8) -> u8 {
    effect_classes.iter().copied().max().unwrap_or(0).max(risk_class)
}
```

- [ ] **Step 4: Run green.** **Step 5: Commit** — `git commit -am "feat(core): own_score, max_class with risk"`

### Task 5: Rank key (scaled-integer ordering)

**Files:** Modify `crates/fxrank-core/src/score.rs`

- [ ] **Step 1: Failing tests** — the two guarantees from the spec.

```rust
#[test]
fn rank_key_orders_by_max_class_first() {
    // logging soup (own_score 27.5, class 4) must rank BELOW one IO (21, class 7)
    let soup = rank_key(4, 27.5, 0, 0.9);
    let io = rank_key(7, 21.0, 0, 0.9);
    assert!(io > soup);
}

#[test]
fn risk_only_outranks_class_zero() {
    let risk_only = rank_key(4, 0.0, 5, 1.0);  // mem::forget => risk_class 4
    let pure = rank_key(0, 0.0, 0, 1.0);
    assert!(risk_only > pure);
}
```

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** — return a totally-ordered integer tuple (no `f64: Ord`).

```rust
/// Deterministic rank key: (max_class, own_score*2, risk_weight, confidence*100),
/// all integers, descending = "more severe". Spec: Aggregation and rank key.
pub fn rank_key(max_class: u8, own_score: f64, risk_weight: u32, confidence: f64) -> (u8, u64, u32, u32) {
    (
        max_class,
        (own_score * 2.0).round() as u64,
        risk_weight,
        (confidence * 100.0).round() as u32,
    )
}
```

> Sorting hotspots: sort by `rank_key(...)` descending; final tiebreak on `id` (stable) — applied in Task 7 where the model lives.

- [ ] **Step 4: Run green.** **Step 5: Commit** — `git commit -am "feat(core): integer rank key"`

### Task 6: Confidence (tier bases + penalties + min)

**Files:** Create `crates/fxrank-core/src/confidence.rs`; `pub mod confidence;` in lib.rs

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn tier_bases_and_penalties() {
    assert_eq!(tier_base(Tier::Exact), 1.0);
    assert_eq!(tier_base(Tier::Path), 0.9);
    assert_eq!(tier_base(Tier::Heuristic), 0.6);
    // unresolved call penalty x0.8
    assert!((detection_confidence(Tier::Path, true, false) - 0.72).abs() < 1e-9);
    // alias-shadowed path x0.9
    assert!((detection_confidence(Tier::Path, false, true) - 0.81).abs() < 1e-9);
}

#[test]
fn function_confidence_is_min() {
    assert_eq!(function_confidence(&[1.0, 0.6, 0.9]), 0.6);
    assert_eq!(function_confidence(&[]), 1.0);  // zero effects => fully confident
}
```

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** (use `crate::effect::Tier`).

```rust
use crate::effect::Tier;

pub fn tier_base(t: Tier) -> f64 {
    match t { Tier::Exact => 1.0, Tier::Path => 0.9, Tier::Heuristic => 0.6 }
}

/// Per-detection confidence: tier base x penalties (unresolved call, shadowed path).
pub fn detection_confidence(t: Tier, unresolved_call: bool, shadowed_path: bool) -> f64 {
    let mut c = tier_base(t);
    if unresolved_call { c *= 0.8; }
    if shadowed_path { c *= 0.9; }
    c
}

/// Function confidence = min over effect/evidence confidences; 1.0 if none.
pub fn function_confidence(detections: &[f64]) -> f64 {
    detections.iter().copied().fold(1.0, f64::min)
}
```

> `unknown.macro` contributes a detection confidence of `0.4` (spec) — produced by the frontend (Task 12), consumed here via the same `function_confidence` min.

- [ ] **Step 4: Run green.** **Step 5: Commit** — `git commit -am "feat(core): confidence model"`

### Task 7: JSON model (Function/Hotspot/Scope/Summary/Diagnostic/Report) + roll-ups

**Files:** Create `crates/fxrank-core/src/model.rs`; `pub mod model;`

- [ ] **Step 1: Failing tests** — build a Report from one hotspot matching the spec's worked example, assert compact JSON fields and summary roll-ups, plus the zero-hotspot rule.

```rust
#[test]
fn summary_rollups_and_compact_json() {
    let h = Hotspot { /* save_user: effects 21/8/1, risk 0, confidence 0.6, max_class 7 */ };
    let report = Report::build(
        Scope { input: "stdin".into(), files: 2, parsed: 1, functions: 4, risk_features: vec![] },
        vec![h],
        vec![Diagnostic { path: "src/broken.rs".into(), parsed: false, error: "expected `;`, line 8".into() }],
        None,  // --limit
    );
    assert_eq!(report.summary.own_score, 25.5);
    assert_eq!(report.summary.max_class, 7);
    assert_eq!(report.summary.confidence, 0.6);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains('\n'));            // compact
    assert!(json.contains("\"own_score\":25.5"));
}

#[test]
fn zero_hotspots_defaults() {
    let report = Report::build(Scope::empty("stdin"), vec![], vec![], None);
    assert_eq!(report.summary.own_score, 0.0);
    assert_eq!(report.summary.confidence, 1.0);
    assert_eq!(report.summary.max_class, 0);
}
```

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** the structs (`#[derive(Serialize)]`), `Report::build` (sorts hotspots by `rank_key` desc then `id`; computes summary as max own_score / max class / max risk_weight over hotspots **and** `scope.risk_features` / min confidence; applies `--limit` to the hotspots vec only; zero-hotspot defaults per spec). Use `serde_json::to_string` (compact) for output. Include `Scope::empty`, `Scope.risk_features: Vec<RiskFeature>`, `Hotspot { id, symbol, path, line, max_class, own_score, risk_weight, confidence, async_boundary, effects: Vec<Effect>, risk_features: Vec<RiskFeature> }`.

- [ ] **Step 4: Run green.** **Step 5: Commit** — `git commit -am "feat(core): JSON report model + summary roll-ups"`

### Task 8: Frontend trait + SourceFile/FrontendOutput

**Files:** Create `crates/fxrank-core/src/frontend.rs`; `pub mod frontend;`

- [ ] **Step 1: Failing test** — a trivial stub frontend implementing the trait compiles and returns an output shape.

```rust
#[test]
fn frontend_trait_object_safe() {
    struct Stub;
    impl Frontend for Stub {
        fn language(&self) -> Language { Language::Rust }
        fn analyze(&self, _f: &[SourceFile]) -> FrontendOutput { FrontendOutput::default() }
    }
    let f: &dyn Frontend = &Stub;
    assert_eq!(f.language(), Language::Rust);
}
```

- [ ] **Step 2: Run red. Step 3: Implement**

```rust
use crate::model::Diagnostic;
use crate::{effect::RiskFeature};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language { Rust }

pub struct SourceFile { pub path: String, pub text: String }

#[derive(Default)]
pub struct FrontendOutput {
    pub functions: Vec<crate::model::Hotspot>,   // scored functions (pre-ranking)
    pub module_risks: Vec<RiskFeature>,          // module-level (impl Drop, extern)
    pub diagnostics: Vec<Diagnostic>,
}

pub trait Frontend {
    fn language(&self) -> Language;
    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput;
}
```

- [ ] **Step 4: Run green. Step 5: Commit** — `git commit -am "feat(core): Frontend trait + source/output types"`

---

## Phase 2 — fxrank-lang-rust (syn frontend)

> Each detection task ships its own fixture(s) and a test asserting the produced effects. Use `syn::parse_file` + `proc-macro2` spans for line numbers.

### Task 9: Collect function units + ids from a parsed file

**Files:** Create `src/functions.rs`, `src/lib.rs` (RustFrontend skeleton); fixtures dir.

- [ ] **Step 1: Fixture + failing test**

```rust
// tests/fixtures/functions.rs
fn free_fn() {}
struct S;
impl S { fn method(&self) {} }
trait T { fn defaulted(&self) {} fn required(&self); }
impl T for S { fn defaulted(&self) {} fn required(&self) {} }
```

```rust
// tests/rust_frontend.rs
#[test]
fn collects_expected_function_units() {
    let out = analyze_fixture("functions.rs");
    let ids: Vec<_> = out.functions.iter().map(|f| f.symbol.clone()).collect();
    assert!(ids.contains(&"free_fn".to_string()));
    assert!(ids.contains(&"S::method".to_string()));
    assert!(ids.contains(&"<S as T>::defaulted".to_string()));
    // trait *signature* with no body is NOT a unit:
    assert!(!ids.iter().any(|s| s.contains("required") && !s.contains("as T")));
}
```

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** `functions.rs`: walk `syn::Item::{Fn, Impl, Trait}`; emit a unit for free fns, inherent/trait-impl methods, and trait **default** bodies; skip bodyless trait sigs and `extern` decls. Build `symbol` (`Type::method`, `<Type as Trait>::method`) and `id = path:line:symbol` using `item.sig.ident.span().start().line`. Closures/`async` blocks are visited later as part of their enclosing fn (Task 13/15), not as separate units. Provide a test helper `analyze_fixture` in the test module.

- [ ] **Step 4: Run green. Step 5: Commit** — `git commit -am "feat(rust): collect function units + ids"`

### Task 10: Import table from `use`

**Files:** Create `src/imports.rs`

- [ ] **Step 1: Fixture + failing test** — a file with `use std::fs;`, `use std::fs as filesystem;`, `use std::io::*;`; assert the table resolves `filesystem` → `std::fs` and flags the glob as shadow-risk.

- [ ] **Step 2: Run red. Step 3: Implement** a per-file map from local name → full path, recording aliases and a `has_glob` flag (used to set `shadowed_path` for the `×0.9` confidence penalty). **Step 4: green. Step 5: Commit** — `git commit -am "feat(rust): use-statement import table"`

### Task 11: Path/method-call effects (io, time, random, env, process, concurrency, logging)

**Files:** Create `src/detect/mod.rs`, `src/detect/calls.rs`

- [ ] **Step 1: Fixtures + failing tests** — one fixture per family asserting the kind, class, tier, and evidence. e.g. `Instant::now()` → `time.read` class 5 tier `path`; `std::fs::write(..)` → `net.fs.db` class 7; a `.send()` method call → `concurrency` tier `heuristic`; `Command::new(..)` alone → **no** effect, but `.spawn()` → `process.control`.

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** a `syn::visit::Visit` walker over a function body. For `ExprCall` with a path, resolve through the import table and match against the spec's path lists → `path`-tier effect. For `ExprMethodCall`, match method names from the heuristic lists (`send`/`recv`/`lock`/`store`/`load`/…) → `heuristic`-tier effect (with the confidence penalty). Constructors (`Command::new`, `OpenOptions::new`) are **not** effects; only the terminal effectful calls are. Each detection records `line` from the call's span and an `evidence` string (the called path/method).

- [ ] **Step 4: Run green. Step 5: Commit** — `git commit -am "feat(rust): path/method call effect detection"`

### Task 12: Macro effects + unknown.macro + whitelist

**Files:** Create `src/detect/macros.rs`

- [ ] **Step 1: Fixtures + failing tests** — `println!` → `logging` exact; `panic!`/`assert!` → `panic` exact; `vec!`/`format!` → no effect (whitelist); `my_macro!` → `unknown.macro` class 2 confidence 0.4 tier heuristic.

- [ ] **Step 2: Run red. Step 3: Implement** a `Macro` visitor: classify by `mac.path` ident against the spec's exact macro lists; exempt the whitelist (`vec!`, `format!`, `matches!`, `concat!`, `stringify!`, `cfg!`, `line!`, `column!`, `file!`); everything else → `unknown.macro`. **Step 4: green. Step 5: Commit** — `git commit -am "feat(rust): macro + unknown.macro detection"`

### Task 13: Mutation detection (local / param / hidden / global) + discount

**Files:** Create `src/detect/mutation.rs`

- [ ] **Step 1: Fixtures + failing tests** — the spec's flagship cases:
  - `fn set(&mut self, n: String){ self.name = n; }` → `param.mutation` base 3 `discounted_to` 2 (`&mut self`).
  - `fn fill(b: &mut Vec<u8>){ b.push(1); }` → `param.mutation` `discounted_to` 1 (`&mut param`).
  - `fn set(&self, n: String){ *self.name.borrow_mut() = n; }` → `hidden.mutation` class 3, **no** discount, `hidden: true`, and it scores **higher** than the `&mut self` case.
  - `fn f(c: &Context){ c.cell.set(1); }` → `hidden.mutation` (shared-ref interior mutation, not just `&self`).
  - `let mut x = 0; x += 1;` → `local.mutation` at the write site, class 1.
  - a `static mut` write → `global.mutation` default class 6.
  - `unsafe { *p = 1 }` where `p: &mut T` → discount **cancelled** (stays class 3).

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement**: track `&mut` parameter/receiver bindings and `let mut` locals via within-function lexical scope; detect writes (`ExprAssign`, compound assign, `&mut` borrow, known mutating methods). Classify: write to a `&mut` binding → `param.mutation` (+ `Discount::MutParam`/`MutSelf`); interior-mut method (`borrow_mut`/`set`/`store`/guard write) on a shared `&` reference → `hidden.mutation` (`hidden: true`, heuristic, no discount); write to a local mut → `local.mutation`; write to a `static`/`static mut` → `global.mutation` (default class 6, drop to 4 only when the static is private with no public mutating accessor — when unsure, stay 6). Apply `apply_discount` with `unsafe_enclosed` from a lexical `unsafe`-block / `unsafe fn` check; call `Effect::sync_weight()` after.

- [ ] **Step 4: Run green. Step 5: Commit** — `git commit -am "feat(rust): mutation detection + containment discount"`

### Task 14: risk_features + module-level risk

**Files:** Create `src/detect/risk.rs`

- [ ] **Step 1: Fixtures + failing tests** — `unsafe{}`→ risk class 5; `transmute`/raw-ptr-deref/`get_unchecked`→ class 7; `mem::forget`/`Box::leak`→ class 4; module-level `impl Drop for T`→ `scope.risk_features` entry class 2 (not attached to any function). Assert a risk-only function gets `max_class = risk_class` (e.g. `mem::forget` → 4).

- [ ] **Step 2: Run red. Step 3: Implement** detection of the spec's `risk_features` list with their classes (Task 3 table); function-body risks attach to the function and feed its `risk_class`/`risk_weight` (= weight of the max risk class); item-level `impl Drop` / `extern` blocks go to `FrontendOutput.module_risks`. **Step 4: green. Step 5: Commit** — `git commit -am "feat(rust): risk_features + module-level risk"`

### Task 15: async_boundary + per-effect confidence wiring

**Files:** Modify `src/detect/mod.rs`, `src/lib.rs`

- [ ] **Step 1: Fixtures + failing tests** — `async fn` with two `.await`s → `async_boundary: true`, `await_count: 2`; an unresolved awaited call lowers the function confidence by `×0.8`; a function with a heuristic effect has confidence `0.6`.

- [ ] **Step 2: Run red. Step 3: Implement**: set `async_boundary`/`await_count` from `sig.asyncness` and `.await` count; compute each effect's `confidence` via `detection_confidence` (tier + unresolved/shadowed penalties) and roll up the function `confidence` via `function_confidence` (min, including `unknown.macro` 0.4 and unresolved-await detections). Assemble each function into a `Hotspot` (compute `own_score`, `max_class`, `risk_weight`). **Step 4: green. Step 5: Commit** — `git commit -am "feat(rust): async boundary + confidence wiring"`

### Task 16: RustFrontend::analyze end-to-end + parse diagnostics

**Files:** Modify `src/lib.rs`

- [ ] **Step 1: Failing test** — `analyze` over a good file + an un-parseable file returns scored functions for the good one and a `Diagnostic { parsed: false, .. }` for the bad one (no panic).

- [ ] **Step 2: Run red. Step 3: Implement** `impl Frontend for RustFrontend`: for each `SourceFile`, `syn::parse_file`; on `Err`, push a `Diagnostic` with the message+line; on `Ok`, run Tasks 9–15 and collect functions + module risks. **Step 4: green. Step 5: Commit** — `git commit -am "feat(rust): RustFrontend::analyze with parse diagnostics"`

---

## Phase 3 — CLI + end-to-end

### Task 17: `fxrank scan` — args, file discovery, dispatch, compact JSON

**Files:** Modify `crates/fxrank-cli/src/main.rs`; create `tests/cli.rs`

- [ ] **Step 1: Failing integration tests** (`assert_cmd`):
  - `echo "<rust>" | fxrank scan` → stdout is one-line JSON with a `hotspots` array.
  - `fxrank scan <dir>` recurses `.rs` files; a non-existent path → non-zero exit + JSON error object.
  - `--limit 1` truncates `hotspots` to one but leaves `summary` over all functions.

- [ ] **Step 2: Run red.**

- [ ] **Step 3: Implement** with `clap` derive: subcommand `scan { path: Option<PathBuf>, #[arg(long)] limit: Option<usize> }`. No path → read stdin into one `SourceFile { path: "stdin" }`. Path → walk for `*.rs` (std `read_dir` recursion; record IO errors as diagnostics; nonexistent root → error object + exit 1). Dispatch by extension to `RustFrontend` (static match). Build the `Report` (core) from the frontend output + scope counts; `println!("{}", serde_json::to_string(&report)?)`.

- [ ] **Step 4: Run green. Step 5: Commit** — `git commit -am "feat(cli): fxrank scan with compact JSON output"`

### Task 18: Snapshot fixtures + dogfood smoke

**Files:** `crates/fxrank-lang-rust/tests/` (insta snapshots), `crates/fxrank-cli/tests/cli.rs`

- [ ] **Step 1: Add `insta` dev-dep to fxrank-lang-rust; write snapshot tests** over the spec's worked cases (save_user-like; logging-soup vs one IO; `&mut self` vs `&self`+RefCell; pure fn; risk-only `mem::forget`; `Result`/`?` pure; async shell; unsafe discount-cancel; `Command::new` then `.spawn()`). Assert serialized JSON via `insta::assert_json_snapshot!`.

- [ ] **Step 2: Run `cargo test`, review and accept snapshots** (`cargo insta review`). Verify the ranking guarantees hold (IO > logging soup; risk-only > pure).

- [ ] **Step 3: Dogfood smoke test** in `cli.rs`: run `fxrank scan crates/` over the project itself; assert exit 0 and that stdout parses as JSON with a non-empty `hotspots`.

- [ ] **Step 4: Full gate** — `cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 5: Commit** — `git commit -am "test: snapshot fixtures + dogfood smoke"`

---

## Verification (Milestone A done)

All must pass on the feature branch and in CI:

- [ ] `cargo build`
- [ ] `cargo test` (core unit + rust-frontend snapshots + cli integration + dogfood)
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `fxrank scan crates/` emits valid compact JSON whose hotspots/evidence match the spec's worked examples (manual calibration check of weights/discounts).

Then open a PR linking issue caasi/dong3#51.
