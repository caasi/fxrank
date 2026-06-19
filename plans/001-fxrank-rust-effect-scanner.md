# FxRank Rust Effect Scanner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Milestone A of FxRank — a `syn`-based Rust analyzer that scores each function's own effect cost, ranks hotspots, and emits compact JSON to stdout for coding agents.

**Architecture:** A Cargo workspace with three crates. `fxrank-core` holds the language-neutral scoring model (effect vocabulary, Fibonacci weights, containment discount, aggregation/rank, confidence, JSON model, `Frontend` trait) and depends on no parser. `fxrank-lang-rust` implements `Frontend` over `syn`. `fxrank-cli` (package `fxrank`) parses args, discovers `.rs` files, dispatches by extension, and prints compact JSON. Frontends are **feature-gated** so a slim single-language build is possible. Build the pure core first (fully unit-testable without parsing), then the frontend, then the CLI.

**Tech Stack:** Rust edition 2024; `serde`/`serde_json`; `syn` 2 (`full`, `visit`, `extra-traits`) + `proc-macro2` (`span-locations`); `clap` 4; `insta` (snapshots) + `assert_cmd`/`predicates` (CLI tests).

Spec: `specs/001-fxrank-rust-effect-scanner.md` — the source of truth for every score, class, discount, and schema field. When this plan and the spec disagree, the spec wins; fix the plan.

---

## Conventions for every task

- **TDD**: write the failing test, run it red, implement minimally, run it green, commit.
- **Commits stage explicitly.** Use `git add <exact paths>` then `git commit -m "…"`. **Never `git commit -am`** — `-a` does not stage new files, and most tasks create new files. Conventional-commit messages.
- After any change, the gate must pass before the task's final commit:
  `cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`.
- Edition 2024; `rustfmt.toml` and the clippy lints apply workspace-wide (Task 0).
- Work on a **feature branch in a worktree** (see Execution Handoff), never on `main`.

## Wire-format decisions (locked, from the spec)

- `own_score` is an `f64`; whole values serialize as `3.0` (not `3`). Snapshots and the spec examples use this form.
- **Per-effect `confidence` is NOT serialized.** Detection computes a confidence per effect, but only the **function-level** min is emitted (`hotspots[].confidence`). `effects[]` carry `kind/class/discounted_to/weight/line/tier/hidden/evidence/discount` — no `confidence`.
- `scope.risk_features[]` and `hotspots[].risk_features[]` entries are `{ kind, class, weight, path, line, evidence, tier }`.
- `diagnostics[]` are `{ path, parsed, error }`; `error` is `format!("{e}")` from `syn` (the spec's `"expected ';', line 8"` is illustrative — `syn`'s real message/line varies, lexer-level failures report line 1).

## File Structure

```text
fxrank/
  Cargo.toml                         # [workspace] members + shared lints/edition
  .github/workflows/ci.yml           # updated for the workspace + slim build + dogfood
  crates/
    fxrank-core/
      Cargo.toml                     # serde, serde_json
      src/lib.rs                     # re-exports
      src/effect.rs                  # EffectKind, Tier, Effect, RiskKind, RiskFeature
      src/score.rs                   # class->weight, discount, own_score, max_class, rank key
      src/confidence.rs              # tier bases + penalties + min
      src/model.rs                   # Function/Hotspot/Scope/Summary/Diagnostic/Report
      src/frontend.rs                # Frontend trait, SourceFile, FrontendOutput
    fxrank-lang-rust/
      Cargo.toml                     # syn (full,visit,extra-traits), proc-macro2, fxrank-core
      src/lib.rs                     # RustFrontend: impl Frontend
      src/functions.rs               # collect function units + ids
      src/imports.rs                 # use-statement import table
      src/detect/mod.rs              # per-function orchestration + Hotspot assembly
      src/detect/calls.rs            # path/method-call effects
      src/detect/macros.rs           # macro effects + unknown.macro + whitelist
      src/detect/mutation.rs         # local/param/hidden/global mutation + discount
      src/detect/risk.rs             # risk_features + module-level risk
      tests/fixtures/*.rs            # fixtures (one per spec case)
      tests/rust_frontend.rs         # unit + insta snapshot tests
    fxrank-cli/
      Cargo.toml                     # clap, serde_json, optional fxrank-lang-rust, [features]
      src/main.rs                    # arg parse, discovery, dispatch, print
      tests/cli.rs                   # assert_cmd integration + dogfood smoke
```

---

## Phase 0 — Workspace

### Task 0: Convert the scaffold into a feature-gated workspace + update CI

**Files:** Modify `Cargo.toml`, `.github/workflows/ci.yml`; create the three crate manifests + roots; `git rm src/main.rs`.

- [ ] **Step 1: Root workspace manifest**

```toml
# Cargo.toml
[workspace]
resolver = "3"   # explicit: a virtual manifest has no edition, so resolver isn't inferred
members = ["crates/fxrank-core", "crates/fxrank-lang-rust", "crates/fxrank-cli"]

[workspace.package]
edition = "2024"
version = "0.1.0"

[workspace.lints.clippy]
all = "warn"
```

- [ ] **Step 2: Crate manifests** — core, lang-rust, and a **feature-gated** cli.

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
syn = { version = "2", features = ["full", "visit", "extra-traits"] }
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
fxrank-lang-rust = { path = "../fxrank-lang-rust", optional = true }
clap = { version = "4", features = ["derive"] }
serde_json = "1"
[features]
default = ["rust"]
rust = ["dep:fxrank-lang-rust"]
[dev-dependencies]
assert_cmd = "2"
predicates = "3"
[lints]
workspace = true
```

> `syn`'s `visit` feature is required for `syn::visit::Visit` (Phase 2). `proc-macro2`'s `span-locations` is what makes `span.start().line` return real line numbers outside a proc-macro context — load-bearing for every `line`. The `rust` feature with an optional `fxrank-lang-rust` dep is the spec's slim-build gate.

- [ ] **Step 3: Minimal crate roots** so the workspace builds: `fxrank-core/src/lib.rs` (doc comment), `fxrank-lang-rust/src/lib.rs` (doc comment), and `fxrank-cli/src/main.rs`:

```rust
fn main() {
    println!("fxrank");
}
```

- [ ] **Step 4: Remove the old root crate source.** Run: `git rm src/main.rs`. `rustfmt.toml` / `rust-toolchain.toml` are already at root and apply workspace-wide — verify, no move needed.

- [ ] **Step 5: Update CI for the workspace + slim build.** Edit `.github/workflows/ci.yml` so the run steps are:

```yaml
      - run: cargo fmt --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
      - run: cargo build -p fxrank --no-default-features --features rust   # slim-build gate compiles
```

(The dogfood `fxrank scan crates/` step is added in Task 18 once `scan` exists.)

- [ ] **Step 6: Gate** — `cargo build && cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`. Expected: builds, 0 tests, clean.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml .github/workflows/ci.yml crates/
git rm --cached src/main.rs 2>/dev/null || true
git commit -m "chore: restructure fxrank into a feature-gated cargo workspace"
```

---

## Phase 1 — fxrank-core (pure scoring model)

### Task 1: Severity class → Fibonacci weight

**Files:** Create `crates/fxrank-core/src/score.rs`; `pub mod score;` in lib.rs.

- [ ] **Step 1: Failing test**

```rust
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

- [ ] **Step 2: Run red** — `cargo test -p fxrank-core weight_map`.
- [ ] **Step 3: Implement**

```rust
pub const CLASS_WEIGHTS: [u32; 9] = [0, 1, 2, 3, 5, 8, 13, 21, 34];
pub fn weight_for_class(class: u8) -> u32 { CLASS_WEIGHTS[(class as usize).min(8)] }
```

- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-core/src/score.rs crates/fxrank-core/src/lib.rs && git commit -m "feat(core): fibonacci class-weight map"`

### Task 2: Effect kinds/tiers, RiskKind, Effect/RiskFeature types

**Files:** Create `crates/fxrank-core/src/effect.rs`; `pub mod effect;`.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn kind_and_risk_metadata() {
    assert_eq!(EffectKind::NetFsDb.wire(), "net.fs.db");
    assert_eq!(EffectKind::NetFsDb.base_class(), 7);
    assert_eq!(EffectKind::GlobalMutation.base_class(), 6);  // spec default, not 3
    assert_eq!(EffectKind::HiddenMutation.base_class(), 3);
    assert_eq!(EffectKind::UnknownMacro.base_class(), 2);
    assert_eq!(RiskKind::Transmute.class(), 7);
    assert_eq!(RiskKind::MemForget.wire(), "mem.forget");
    assert_eq!(RiskKind::ImplDrop.class(), 2);
}
```

- [ ] **Step 2: Run red.**
- [ ] **Step 3: Implement** the full vocabulary. Note `GlobalMutation` is class **6** (spec default; the class-4 module-private downgrade is deferred — Known Limitations). Centralize risk wire-strings + classes in `RiskKind` so Task 14 and the spec can't drift (`mem.forget` not `mem::forget`).

```rust
use serde::Serialize;
use crate::score::weight_for_class;

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
    pub fn wire(self) -> &'static str {
        use EffectKind::*;
        match self {
            NetFsDb => "net.fs.db", ProcessControl => "process.control", EnvWrite => "env.write",
            Concurrency => "concurrency", TimeRead => "time.read", Random => "random",
            EnvRead => "env.read", Logging => "logging", Panic => "panic",
            GlobalMutation => "global.mutation", HiddenMutation => "hidden.mutation",
            ParamMutation => "param.mutation", AmbientRead => "ambient.read",
            LocalMutation => "local.mutation", UnknownMacro => "unknown.macro",
        }
    }
    pub fn base_class(self) -> u8 {
        use EffectKind::*;
        match self {
            NetFsDb => 7,
            ProcessControl | EnvWrite | Concurrency => 6,
            TimeRead | Random => 5,
            EnvRead | Logging | Panic => 4,
            GlobalMutation => 6,                          // spec default
            HiddenMutation | ParamMutation => 3,
            AmbientRead | UnknownMacro => 2,
            LocalMutation => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskKind {
    Transmute, RawPtrDeref, FfiCall, Asm, Volatile, MaybeUninit, FromRaw, GetUnchecked,
    UnsafeBlock, UnsafeFn, UnsafeImpl, BoxLeak, MemForget, ManuallyDrop, ImplDrop,
}
impl RiskKind {
    pub fn wire(self) -> &'static str {
        use RiskKind::*;
        match self {
            Transmute => "transmute", RawPtrDeref => "raw.ptr.deref", FfiCall => "ffi.call",
            Asm => "asm", Volatile => "volatile", MaybeUninit => "maybe.uninit",
            FromRaw => "from.raw", GetUnchecked => "get.unchecked", UnsafeBlock => "unsafe.block",
            UnsafeFn => "unsafe.fn", UnsafeImpl => "unsafe.impl", BoxLeak => "box.leak",
            MemForget => "mem.forget", ManuallyDrop => "manually.drop", ImplDrop => "impl.drop",
        }
    }
    pub fn class(self) -> u8 {
        use RiskKind::*;
        match self {
            Transmute | RawPtrDeref | FfiCall | Asm | Volatile | MaybeUninit | FromRaw
            | GetUnchecked => 7,
            UnsafeBlock | UnsafeFn | UnsafeImpl => 5,
            BoxLeak | MemForget | ManuallyDrop => 4,
            ImplDrop => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Effect {
    #[serde(serialize_with = "ser_kind")]
    pub kind: EffectKind,
    pub class: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discounted_to: Option<u8>,
    pub weight: u32,
    pub line: usize,
    pub tier: Tier,
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount: Option<String>,
    // detection confidence is carried alongside but NOT serialized (wire decision):
    #[serde(skip)]
    pub confidence: f64,
}
impl Effect {
    pub fn effective_class(&self) -> u8 { self.discounted_to.unwrap_or(self.class) }
    pub fn sync_weight(&mut self) { self.weight = weight_for_class(self.effective_class()); }
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskFeature {
    #[serde(serialize_with = "ser_risk")]
    pub kind: RiskKind,
    pub class: u8,
    pub weight: u32,
    pub path: String,   // required for module-level risks (which file)
    pub line: usize,
    pub evidence: String,
    pub tier: Tier,
}

fn is_false(b: &bool) -> bool { !*b }
fn ser_kind<S: serde::Serializer>(k: &EffectKind, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(k.wire()) }
fn ser_risk<S: serde::Serializer>(k: &RiskKind, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(k.wire()) }
```

- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-core/src/effect.rs crates/fxrank-core/src/lib.rs && git commit -m "feat(core): effect kinds, risk kinds, Effect/RiskFeature"`

### Task 3: Containment discount (down-shift, clamp, unsafe-cancel)

**Files:** Modify `score.rs`.

- [ ] **Step 1: Failing test** (as before): `apply_discount(3, MutParam, false)==1`, `(3, MutSelf, false)==2`, `(1, MutParam, false)==1`, `(3, MutParam, true)==3`, `(3, None, false)==3`.
- [ ] **Step 2: Red. Step 3: Implement**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discount { None, MutParam, MutSelf }
pub fn apply_discount(base_class: u8, discount: Discount, unsafe_enclosed: bool) -> u8 {
    if unsafe_enclosed || discount == Discount::None { return base_class; }
    let shift = match discount { Discount::MutParam => 2, Discount::MutSelf => 1, Discount::None => 0 };
    base_class.saturating_sub(shift).max(1)
}
```

- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-core/src/score.rs && git commit -m "feat(core): containment discount as class down-shift"`

### Task 4: own_score, max_class

**Files:** Modify `score.rs`.

- [ ] **Step 1: Failing test** — `own_score(&[21,8,1])==25.5`, `own_score(&[])==0.0`, `own_score(&[5])==5.0`; `max_class(&[0],4)==4`, `max_class(&[7],0)==7`.
- [ ] **Step 2: Red. Step 3: Implement** (harden the subtraction with `saturating_sub`).

```rust
pub fn own_score(weights: &[u32]) -> f64 {
    let max = weights.iter().copied().max().unwrap_or(0);
    let rest: u32 = weights.iter().copied().sum::<u32>().saturating_sub(max);
    max as f64 + 0.5 * rest as f64
}
pub fn max_class(effect_classes: &[u8], risk_class: u8) -> u8 {
    effect_classes.iter().copied().max().unwrap_or(0).max(risk_class)
}
```

- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-core/src/score.rs && git commit -m "feat(core): own_score + max_class with risk"`

### Task 5: Rank key (scaled-integer ordering)

**Files:** Modify `score.rs`. (Unchanged from the reviewed plan.)

- [ ] **Step 1: Failing test** — `rank_key(7,21.0,0,0.9) > rank_key(4,27.5,0,0.9)` (IO over logging-soup); `rank_key(4,0.0,5,1.0) > rank_key(0,0.0,0,1.0)` (risk-only over pure).
- [ ] **Step 2: Red. Step 3: Implement**

```rust
pub fn rank_key(max_class: u8, own_score: f64, risk_weight: u32, confidence: f64) -> (u8, u64, u32, u32) {
    (max_class, (own_score * 2.0).round() as u64, risk_weight, (confidence * 100.0).round() as u32)
}
```

- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-core/src/score.rs && git commit -m "feat(core): integer rank key"`

### Task 6: Confidence (tier bases + penalties + min)

**Files:** Create `confidence.rs`; `pub mod confidence;`.

- [ ] **Step 1: Failing test** — `tier_base(Exact)==1.0`, `Path==0.9`, `Heuristic==0.6`; `detection_confidence(Path,true,false)≈0.72`; `detection_confidence(Path,false,true)≈0.81`; `function_confidence(&[1.0,0.6,0.9])==0.6`; `function_confidence(&[])==1.0`.
- [ ] **Step 2: Red. Step 3: Implement** `tier_base`, `detection_confidence(tier, unresolved_call, shadowed_path)` (×0.8 / ×0.9), `function_confidence(&[f64]) -> min, 1.0 if empty`. `unknown.macro` contributes a detection confidence of `0.4` (produced by the frontend, Task 12).
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-core/src/confidence.rs crates/fxrank-core/src/lib.rs && git commit -m "feat(core): confidence model"`

### Task 7: JSON model + roll-ups (runnable tests)

**Files:** Create `model.rs`; `pub mod model;`.

- [ ] **Step 1: Failing tests** — **fully runnable** (construct complete `Hotspot`s). Prove summary takes the **max** (two hotspots), the compact one-line JSON, integer `own_score` rendering, and zero-hotspot defaults.

```rust
fn hot(id: &str, max_class: u8, own_score: f64, conf: f64) -> Hotspot {
    Hotspot {
        id: id.into(), symbol: id.into(), path: "f.rs".into(), line: 1,
        max_class, own_score, risk_weight: 0, confidence: conf,
        async_boundary: false, await_count: 0, effects: vec![], risk_features: vec![],
    }
}
#[test]
fn summary_takes_max_and_min_over_two_hotspots() {
    let report = Report::build(
        Scope { input: "f.rs".into(), files: 1, parsed: 1, functions: 2, risk_features: vec![] },
        vec![hot("a", 4, 5.0, 0.9), hot("b", 7, 25.5, 0.6)],
        vec![], None,
    );
    assert_eq!(report.summary.own_score, 25.5);   // max, not sum
    assert_eq!(report.summary.max_class, 7);
    assert_eq!(report.summary.confidence, 0.6);   // min
    assert_eq!(report.hotspots[0].id, "b");       // ranked first
}
#[test]
fn whole_own_score_serializes_with_point_zero() {
    let report = Report::build(Scope::empty("f.rs"), vec![hot("x", 3, 3.0, 0.6)], vec![], None);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains('\n'));
    assert!(json.contains("\"own_score\":3.0"));
}
#[test]
fn zero_hotspots_defaults() {
    let report = Report::build(Scope::empty("stdin"), vec![], vec![], None);
    assert_eq!(report.summary.own_score, 0.0);
    assert_eq!(report.summary.confidence, 1.0);
    assert_eq!(report.summary.max_class, 0);
}
```

- [ ] **Step 2: Red.**
- [ ] **Step 3: Implement** the structs (all `#[derive(Serialize)]`): `Scope { input, files, parsed, functions, risk_features: Vec<RiskFeature> }` (+ `Scope::empty(input)`), `Summary { own_score, max_class, risk_weight, confidence }`, `Hotspot { id, symbol, path, line, max_class, own_score, risk_weight, confidence, async_boundary, await_count, effects: Vec<Effect>, risk_features: Vec<RiskFeature> }`, `Diagnostic { path, parsed, error }`, `Report { scope, summary, hotspots, diagnostics }`. `Report::build(scope, hotspots, diagnostics, limit: Option<usize>)`: sort hotspots by `rank_key(...)` descending then `id`; compute summary (`own_score` = max hotspot, `max_class`/`risk_weight` = max over hotspots **and** `scope.risk_features`, `confidence` = min over hotspots; zero-hotspot defaults per spec); truncate the hotspots vec to `limit` **after** computing the summary. Output via `serde_json::to_string` (compact).
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-core/src/model.rs crates/fxrank-core/src/lib.rs && git commit -m "feat(core): JSON report model + summary roll-ups"`

### Task 8: Frontend trait + SourceFile/FrontendOutput

**Files:** Create `frontend.rs`; `pub mod frontend;`.

- [ ] **Step 1–5** as in the reviewed plan: `Language::Rust`, `SourceFile { path, text }`, `FrontendOutput { functions: Vec<Hotspot>, module_risks: Vec<RiskFeature>, diagnostics: Vec<Diagnostic> }` (default), `trait Frontend { language; analyze }`. Test a stub impl. Commit — `git add crates/fxrank-core/src/frontend.rs crates/fxrank-core/src/lib.rs && git commit -m "feat(core): Frontend trait + source/output types"`

---

## Phase 2 — fxrank-lang-rust (syn frontend)

> **syn 2.x notes for this phase:** functions are `Item::Fn`, `ImplItem::Fn`, `TraitItem::Fn` (a `TraitItem::Fn` with `default: Some(block)` is a default body; `None` is a bodyless signature — skip it). Compound assignment is `Expr::Binary` with an assign `BinOp` (e.g. `BinOp::AddAssign`), **not** a standalone node; plain assignment is `Expr::Assign`. For spans on non-ident nodes, `use syn::spanned::Spanned;` and call `.span().start().line`. Walk bodies with a `syn::visit::Visit` visitor.

### Task 9: Collect function units + ids (+ test helper)

**Files:** Create `functions.rs`, `lib.rs` (RustFrontend skeleton), `tests/fixtures/functions.rs`, `tests/rust_frontend.rs`.

- [ ] **Step 1: Fixture + failing test.** Fixture has a free fn, an inherent method, a trait with a default body **and** a bodyless required method, and a trait impl. Define the shared test helper here (every Phase-2 task uses it):

```rust
// tests/rust_frontend.rs
use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_lang_rust::RustFrontend;

fn analyze_fixture(name: &str) -> fxrank_core::frontend::FrontendOutput {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    let text = std::fs::read_to_string(&path).expect("fixture exists");
    RustFrontend.analyze(&[SourceFile { path: name.into(), text }])
}

#[test]
fn collects_expected_function_units() {
    let out = analyze_fixture("functions.rs");
    let syms: Vec<_> = out.functions.iter().map(|f| f.symbol.clone()).collect();
    assert!(syms.contains(&"free_fn".to_string()));
    assert!(syms.contains(&"S::method".to_string()));
    assert!(syms.contains(&"T::defaulted".to_string()));         // trait default BODY is a unit
    assert!(syms.contains(&"<S as T>::required".to_string()));   // impl method
    assert!(!syms.contains(&"T::required".to_string()));         // bodyless sig is NOT a unit
}
```

- [ ] **Step 2: Red.**
- [ ] **Step 3: Implement** `functions.rs`: collect units from `Item::Fn`, inherent `ImplItem::Fn`, trait-impl `ImplItem::Fn` (symbol `<Type as Trait>::method`), and `TraitItem::Fn` **with a default body** (symbol `Trait::method`). Skip bodyless trait sigs and `extern` decls. Build `symbol`/`id = path:line:symbol` via `sig.ident.span().start().line`. `RustFrontend` in `lib.rs` for now just collects units with empty effects (effects arrive in later tasks). `tests/fixtures/*.rs` live in a subdir so cargo does not compile them as test targets (only top-level `tests/*.rs` compile).
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-lang-rust/ && git commit -m "feat(rust): collect function units + ids"`

### Task 10: Import table from `use`

**Files:** Create `imports.rs`, fixture.

- [ ] **Steps** as reviewed: map local name → full path, record `use … as …` aliases and a `has_glob` flag (feeds the `shadowed_path` ×0.9 penalty). Test alias resolution + glob flag. Commit — `git add crates/fxrank-lang-rust/src/imports.rs crates/fxrank-lang-rust/tests/ && git commit -m "feat(rust): use-statement import table"`

### Task 11: Path/method-call effects — with an explicit signal matrix

**Files:** Create `detect/mod.rs`, `detect/calls.rs`, fixtures.

- [ ] **Step 1: Fixtures + failing tests covering the whole catalog's call signals** (don't hand-wave — one assertion per signal group):
  - `net.fs.db` paths: `std::fs::read`, `std::fs::write`, `File::open`, `File::create`, `fs::remove_file`, `fs::rename`, `fs::create_dir_all`, `fs::metadata`, `std::net::TcpStream::connect`, `tokio::fs::read`, `reqwest::get`, a `sqlx` query → class 7; `stdin()/stdout()/stderr()` and `.read_line()`/`.write_all()` → heuristic class 7.
  - `process.control`: `std::process::exit`, `abort`, `Command::new(..).spawn()`/`.status()`/`.output()`, `Child::kill` → class 6; **`Command::new(..)` alone → no effect.**
  - `env.write`: `set_var`, `remove_var`, `set_current_dir` → 6. `env.read`: `var`, `vars`, `args`, `current_dir`, `current_exe`, `temp_dir` → 4.
  - `concurrency`: `thread::spawn`, `tokio::spawn`, `rayon::join`, `JoinSet::spawn`, `thread::sleep`, channel `.send()`/`.recv()` (heuristic) → 6.
  - `time.read`: `Instant::now`, `SystemTime::now` → 5. `random`: `rand::random`, `thread_rng` → 5.
  Each asserts `kind`, `class`, `tier`, and `evidence`.
- [ ] **Step 2: Red.**
- [ ] **Step 3: Implement** a `Visit` walker. `ExprCall` with a path → resolve via the import table, match the path lists → `path`-tier effect. `ExprMethodCall` → match heuristic method names → `heuristic`-tier effect. **Constructors (`Command::new`, `OpenOptions::new`) are not effects** — only terminal effectful calls/methods are. Record `line` + `evidence` (called path/method). Detectors are **pure** (return `Vec<(Effect, /*detection conf inputs*/)>`); `detect/mod.rs` owns assembly (Task 15).
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-lang-rust/src/detect/ crates/fxrank-lang-rust/tests/ && git commit -m "feat(rust): path/method call effect detection"`

### Task 12: Macro effects (logging/panic/write) + unknown.macro

**Files:** Create `detect/macros.rs`, fixtures.

- [ ] **Step 1: Fixtures + failing tests:**
  - logging: `println!`, `eprintln!`, `dbg!` (exact); **`log::info!`, `tracing::warn!`** (qualified macro paths → `logging` **path** tier, **not** `unknown.macro`).
  - panic: `panic!`, `unreachable!`, `todo!`, `unimplemented!`, `assert!`, `assert_eq!`, `assert_ne!` (exact); `debug_assert!` (exact, note cfg).
  - `write!`/`writeln!` → `net.fs.db`, heuristic (target `io::Write` vs `fmt::Write` unknown). **These are macros → handled here, not in calls.rs.**
  - whitelist: `vec!`, `format!`, `matches!`, `concat!`, `stringify!`, `cfg!`, `line!`, `column!`, `file!` → no effect.
  - `my_macro!` → `unknown.macro` class 2, weight 2, tier heuristic, and the function's `confidence == 0.4` (assert the 0.4 flows through to the hotspot).
- [ ] **Step 2: Red. Step 3: Implement** a `Macro` visitor matching the last path segment, but treat a multi-segment macro path (`log::info`, `tracing::warn`) as its mapped kind, not unknown. Emit `unknown.macro` (detection confidence 0.4) for any non-whitelisted, unclassified macro. Macros already classified elsewhere don't double-count.
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-lang-rust/src/detect/macros.rs crates/fxrank-lang-rust/tests/ && git commit -m "feat(rust): macro + unknown.macro detection"`

### Task 13a: param.mutation + containment discount (flagship)

**Files:** Create `detect/mutation.rs`, fixtures.

- [ ] **Step 1: Fixtures + failing tests:**
  - `fn fill(b: &mut Vec<u8>) { b.push(1); }` → `param.mutation` base 3, `discounted_to` 1.
  - `fn set(&mut self, n: String) { self.name = n; }` → `param.mutation` base 3, `discounted_to` 2.
  - **Channel-scoped:** `fn save(u: &mut User) -> std::io::Result<()> { std::fs::write("x", b"")?; u.dirty = true; Ok(()) }` → the `net.fs.db` effect stays class 7 (the discount touches only the mutation).
- [ ] **Step 2: Red. Step 3: Implement** within-function lexical tracking of `&mut` param/receiver bindings; detect writes (`Expr::Assign`, `Expr::Binary` assign binops, `&mut` borrow, known mutating methods like `push`). Classify as `param.mutation` with `Discount::MutParam`/`MutSelf`; call `apply_discount` then `Effect::sync_weight()`; set `discount` evidence string. Only the mutation effect is discounted.
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-lang-rust/src/detect/mutation.rs crates/fxrank-lang-rust/tests/ && git commit -m "feat(rust): param.mutation + containment discount"`

### Task 13b: hidden.mutation (shared-ref interior mutation)

- [ ] **Step 1: Fixtures + failing tests:**
  - `fn set(&self, n: String) { *self.name.borrow_mut() = n; }` → `hidden.mutation` class 3, no discount, `hidden: true`, tier heuristic — and assert it scores **higher** than the `&mut self` case from 13a.
  - `fn bump(c: &Context) { c.counter.set(1); }` (shared `&` param, not `&self`) → `hidden.mutation`.
- [ ] **Step 2: Red. Step 3: Implement** interior-mut method recognition (`borrow_mut`, `set`/`replace`, atomic `store`/`swap`/`fetch_*`, guard writes after `lock()`) on a receiver reached through any shared `&` reference. **Step 4: Green. Step 5: Commit** — `git add … && git commit -m "feat(rust): hidden.mutation via shared-ref interior mutation"`

### Task 13c: local.mutation

- [ ] **Step 1: Fixture + failing test:** `let mut x = 0; x += 1; x = 2;` → two `local.mutation` effects (per write site), class 1; the `let mut` declaration alone produces none.
- [ ] **Step 2: Red. Step 3: Implement** lexical `let mut` binding tracking; count writes to those bindings (assignment/compound/`&mut` borrow). **Step 4: Green. Step 5: Commit** — `git add … && git commit -m "feat(rust): local.mutation write-site detection"`

### Task 13d: global.mutation (always class 6 in Milestone A)

- [ ] **Step 1: Fixture + failing test:** a `static mut COUNT: u32` write, and a mutation of a `static` interior-mut value → `global.mutation` class 6. (The class-4 module-private downgrade is deferred — see spec Known Limitations; do not implement it now.)
- [ ] **Step 2: Red. Step 3: Implement** detection of writes to `static`/`static mut` items; always class 6. **Step 4: Green. Step 5: Commit** — `git add … && git commit -m "feat(rust): global.mutation detection (class 6)"`

### Task 13e: lexical unsafe-cancel

- [ ] **Step 1: Fixtures + failing tests — both directions:**
  - **Cancels:** `fn w(p: &mut u8) { unsafe { *p = 1; } }` → `param.mutation` discount cancelled, stays class 3.
  - **Does NOT cancel:** `fn w(p: &mut Vec<u8>) { unsafe { std::mem::transmute::<u8,i8>(0); } p.push(1); }` → the `&mut` mutation is **outside** the `unsafe` block, so it keeps `discounted_to` 1 (and the transmute is a separate risk feature).
- [ ] **Step 2: Red. Step 3: Implement** lexical enclosure: a mutation is `unsafe_enclosed` iff a `unsafe` block lexically encloses it, or the fn is an `unsafe fn`. Track enclosure during the visit (a depth counter entering/leaving `ExprUnsafe`). **Step 4: Green. Step 5: Commit** — `git add … && git commit -m "feat(rust): lexical unsafe discount cancellation"`

### Task 14: risk_features + module-level risk (full table)

**Files:** Create `detect/risk.rs`, fixtures.

- [ ] **Step 1: Fixtures + failing tests for every `RiskKind`:** `unsafe {}`/`unsafe fn`/`unsafe impl` → class 5; `transmute`, a raw-pointer deref `*p`, `get_unchecked`, `MaybeUninit::uninit`, `*::from_raw`, `asm!`, `ptr::write`/`read`/`copy_nonoverlapping` (volatile) → class 7; `Box::leak`, `mem::forget`, `ManuallyDrop::new` → class 4; module-level `impl Drop for T` and an `extern "C" {}` block → `FrontendOutput.module_risks` (class 2 / per their kind) with `path` set, **not** attached to a function. Assert a function whose only effect is `mem::forget` gets `max_class == 4` (risk feeds max_class).
- [ ] **Step 2: Red. Step 3: Implement** detection per `RiskKind` (Task 2 classes); body risks attach to the function (feed `risk_class`/`risk_weight`); item-level `impl Drop`/`extern` blocks go to `module_risks` with the file `path`.
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-lang-rust/src/detect/risk.rs crates/fxrank-lang-rust/tests/ && git commit -m "feat(rust): risk_features + module-level risk"`

### Task 15: async_boundary + confidence wiring + Hotspot assembly

**Files:** Modify `detect/mod.rs`, `lib.rs`.

- [ ] **Step 1: Fixtures + failing tests:** an `async fn` with two `.await`s → `async_boundary: true`, `await_count: 2`; a function with one heuristic effect → `confidence == 0.6`; an unresolved awaited call applies the `×0.8` penalty.
- [ ] **Step 2: Red. Step 3: Implement** — `detect/mod.rs` is the single owner that turns a function's detected effects + risks into a `Hotspot`: set `async_boundary`/`await_count` from `sig.asyncness` + `.await` count; compute each effect's detection confidence (`detection_confidence(tier, unresolved, shadowed)`), fold the function `confidence` via `function_confidence` (min, including `unknown.macro` 0.4 and unresolved-await items); compute `own_score`, `max_class` (effects + `risk_class`), `risk_weight`. Detectors stay pure; assembly lives here.
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-lang-rust/src/detect/mod.rs crates/fxrank-lang-rust/src/lib.rs crates/fxrank-lang-rust/tests/ && git commit -m "feat(rust): async boundary + confidence + hotspot assembly"`

### Task 16: RustFrontend::analyze end-to-end + parse diagnostics

**Files:** Modify `lib.rs`.

- [ ] **Step 1: Failing test** — `analyze` over a good file + an un-parseable one (`"fn a( {}"`) returns scored functions for the good file and a `Diagnostic { parsed: false, .. }` for the bad one, no panic.
- [ ] **Step 2: Red. Step 3: Implement** `impl Frontend for RustFrontend`: per `SourceFile`, `match syn::parse_file(&f.text)`; `Err(e)` → push `Diagnostic { path: f.path.clone(), parsed: false, error: format!("{e}") }` (the message is whatever `syn` yields; do not fabricate the spec's illustrative `"line 8"`). `Ok(file)` → run Tasks 9–15, collect functions + module risks.
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-lang-rust/src/lib.rs crates/fxrank-lang-rust/tests/ && git commit -m "feat(rust): RustFrontend::analyze with parse diagnostics"`

---

## Phase 3 — CLI + end-to-end

### Task 17: `fxrank scan` — args, discovery, feature-gated dispatch, compact JSON

**Files:** Modify `crates/fxrank-cli/src/main.rs`; create `tests/cli.rs`.

- [ ] **Step 1: Failing integration tests** (`assert_cmd`): stdin pipe → one-line JSON with `hotspots`; `scan <dir>` recurses `.rs`; non-existent path → non-zero exit + JSON error object; `--limit 1` truncates `hotspots` but not `summary`.
- [ ] **Step 2: Red.**
- [ ] **Step 3: Implement** with `clap` derive: `scan { path: Option<PathBuf>, #[arg(long)] limit: Option<usize> }`. No path → read stdin into one `SourceFile { path: "stdin" }`. Path → recurse for `*.rs` (record per-file IO errors as diagnostics; nonexistent root → JSON error object + exit 1). **Feature-gated dispatch:** `#[cfg(feature = "rust")]` selects `RustFrontend` for `.rs`; build the core `Report` from the frontend output + scope counts; `println!("{}", serde_json::to_string(&report)?)`. Without the `rust` feature, an `.rs` input yields a diagnostic "no frontend for .rs".
- [ ] **Step 4: Green. Step 5: Commit** — `git add crates/fxrank-cli/src/main.rs crates/fxrank-cli/tests/cli.rs && git commit -m "feat(cli): fxrank scan with compact JSON output"`

### Task 18: Snapshot fixtures + dogfood + CI dogfood step

**Files:** `crates/fxrank-lang-rust/tests/` (insta), `crates/fxrank-cli/tests/cli.rs`, `.github/workflows/ci.yml`.

- [ ] **Step 1: Add `insta` dev-dep to fxrank-lang-rust; snapshot tests** over the spec's worked cases: save_user-like (DB write + time + `&mut` param), logging-soup vs one `fs::write` (assert IO ranks first), `&mut self` vs `&self`+`RefCell` (assert hidden scores higher), a pure fn (score 0), risk-only `mem::forget` (max_class 4), `Result`/`?` pure, an async shell, the unsafe discount-cancel pair, `Command::new` then `.spawn()`. Use `insta::assert_json_snapshot!`.
- [ ] **Step 2: `cargo insta review`** to accept snapshots; verify the ranking guarantees.
- [ ] **Step 3: Dogfood smoke** in `cli.rs`: run `fxrank scan crates/` over the project; assert exit 0 and stdout parses as JSON with non-empty `hotspots`. Add the same as a CI step in `ci.yml`:

```yaml
      - run: cargo run -p fxrank -- scan crates/ > /dev/null
```

- [ ] **Step 4: Full gate** — `cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `git add crates/ .github/workflows/ci.yml && git commit -m "test: snapshot fixtures + dogfood smoke"`

---

## Verification (Milestone A done)

All must pass on the feature branch and in CI:

- [ ] `cargo build` and `cargo build -p fxrank --no-default-features --features rust` (slim gate)
- [ ] `cargo test --workspace` (core unit + rust-frontend snapshots + cli integration + dogfood)
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `fxrank scan crates/` emits valid compact JSON whose hotspots/evidence match the spec's worked examples (manual calibration check of weights/discounts).

Then open a PR linking issue caasi/dong3#51.
