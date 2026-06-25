# Scoring-table rebaseline (logging↓, time/random=world) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lower `EffectKind::Logging` from class 4 to 2, drop `Logging` from the TS render-phase risk set, and codify `TimeRead`/`Random` as escaping world effects — per spec `docs/superpowers/specs/028-scoring-table-rebaseline.md`.

**Architecture:** A central `EffectKind → class` table change in `fxrank-core` (one line) that all three frontends inherit, plus a TS-only render-risk tweak and a codification test. Cross-language behavioral re-weight; intentional output change.

**Tech Stack:** Rust. Touches `crates/fxrank-core/src/effect.rs`, `crates/fxrank-lang-ts/src/detect/mod.rs`, and existing tests/snapshots.

## Global Constraints

- **Centralization:** the class lives ONLY in `EffectKind::base_class`; no frontend hardcodes a logging class literal in production code (verified). Do not add per-frontend class literals.
- **Intentional output change:** this rebaselines scores; snapshots/asserts that encode the old class must be updated, not worked around.
- **Lands in the same PR as spec 027's implementation** (branch `feat/027-react-effect-scoring`); closes #37 together.
- CI gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Run cargo from the worktree.

## File Structure

- `crates/fxrank-core/src/effect.rs` — **modify** `base_class` (move `Logging` to the class-2 arm) + its unit test.
- `crates/fxrank-lang-python/src/detect/calls.rs` — **modify** the will-fail assert (`(Logging, 4)` → `(Logging, 2)`).
- `crates/fxrank-lang-python/src/detect/mod.rs`, `crates/fxrank-cli/src/main.rs` — **modify** stale class-4 comments (doc only).
- `crates/fxrank-lang-rust/tests/snapshots/snapshots__logging_soup_and_one_io.snap` — **re-accept** (class 4→2).
- `crates/fxrank-lang-ts/src/detect/mod.rs` — **modify** `world_effect` (remove `Logging`) + a test.
- A frontend detect test — **add** the `TimeRead`/`Random` escaping codification assert.

---

### Task 1: `Logging` class 4 → 2 (core + all coupled fixups)

Changing `base_class` immediately breaks a Python test assert and a Rust snapshot, so they land together to keep `cargo test --workspace` green.

**Files:** `crates/fxrank-core/src/effect.rs`; `crates/fxrank-lang-python/src/detect/calls.rs`; `crates/fxrank-lang-python/src/detect/mod.rs`; `crates/fxrank-cli/src/main.rs`; `crates/fxrank-lang-rust/tests/snapshots/snapshots__logging_soup_and_one_io.snap`.

- [ ] **Step 1: Write the failing core test**

In `effect.rs`'s `#[cfg(test)] mod tests`, add to `kind_and_risk_metadata` (after the `UnknownMacro` assert):
```rust
        assert_eq!(EffectKind::Logging.base_class(), 2);
```

- [ ] **Step 2: Run to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p fxrank-core kind_and_risk_metadata 2>&1 | tail -10`
Expected: FAIL — `Logging.base_class()` is 4, not 2.

- [ ] **Step 3: Change the table**

In `EffectKind::base_class`, move `Logging` from the class-4 arm to the class-2 arm:
```rust
            EnvRead | Panic => 4,
            ...
            AmbientRead | UnknownMacro | ExternalUnresolved | Logging => 2,
```
(i.e. remove `Logging` from `EnvRead | Logging | Panic => 4,` and add it to the class-2 line. Leave `EnvRead` and `Panic` at 4.)

- [ ] **Step 4: Fix the coupled Python test assert**

In `crates/fxrank-lang-python/src/detect/calls.rs` (the `detects_world_effects` test, ~line 394):
```rust
        assert!(io.contains(&(Logging, 2))); // logging.info
```
(was `(Logging, 4)`.)

- [ ] **Step 5: Correct stale class-4 comments (doc only)**

`crates/fxrank-lang-python/src/detect/mod.rs` (~line 1424) and `crates/fxrank-cli/src/main.rs` (~lines 1648 AND 1665-1667): update any comment text that says logging is class 4 to class 2. There are two stale-comment sites in `main.rs`: the doc comment at ~1648 (`logging.basicConfig` → "Logging class 4") and the inline comments at ~1665-1667 ("root 'logging' → Logging class 4"). Both must be corrected. (The asserts in those tests do not depend on logging's class — verify by reading them; only the comment wording changes.)

- [ ] **Step 6: Run tests; re-accept the Rust snapshot**

Run: `cargo test -p fxrank-core kind_and_risk_metadata` (passes), then `cargo test --workspace 2>&1 | tail -20`. The Rust snapshot test `logging_soup_and_one_io` now fails (class 4→2). Re-accept it:
```bash
cargo insta accept --workspace
```
Then re-run `cargo test --workspace` (all green). Inspect the snapshot diff (`git diff crates/fxrank-lang-rust/tests/snapshots/snapshots__logging_soup_and_one_io.snap`) to confirm the ONLY change is the four logging entries dropping class 4→2 (and `max_class`/`own_score` re-baselining accordingly) — no unrelated drift.

- [ ] **Step 7: fmt/clippy + commit**

Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`.
```bash
git -C /dev/shm/fxrank/37 add -A
git -C /dev/shm/fxrank/37 commit -m "feat(core): Logging class 4 → 2 (benign output, not mutation/input) (#37, spec 028)

Centralized in EffectKind::base_class; all frontends inherit. Coupled fixups:
Python (Logging,4)→(Logging,2) assert; stale class-4 comments; re-accept the one
Rust logging snapshot. A function no longer ranks as a hotspot for logging alone."
```

---

### Task 2: Drop `Logging` from the TS render-phase risk set

**Files:** `crates/fxrank-lang-ts/src/detect/mod.rs` (`world_effect`) + a test.

**Interfaces:** `world_effect(kind: EffectKind) -> bool` — the explicit kind-match gating `EffectInRender`.

- [ ] **Step 1: Write the failing test**

Add a TS detect test asserting a render-phase `console.*` does NOT raise `EffectInRender` (mirror the existing react `effects` fixture style). Concretely (adapt to the existing test harness in `detect/mod.rs` or `lib.rs` react tests): a component whose render body / a `useMemo` callback contains only `console.warn(...)` must have **no** `EffectInRender` risk. Assert the risk list contains no `RiskKind::EffectInRender`.

- [ ] **Step 2: Run to verify it fails**

Expected: FAIL — `Logging` is in `world_effect`, so `console.warn` in render currently raises `EffectInRender`.

- [ ] **Step 3: Implement**

In `world_effect`, remove the `| EffectKind::Logging` arm:
```rust
fn world_effect(kind: EffectKind) -> bool {
    matches!(
        kind,
        EffectKind::NetFsDb
            | EffectKind::ProcessControl
            | EffectKind::EnvWrite
            | EffectKind::Concurrency
            | EffectKind::TimeRead
            | EffectKind::Random
            | EffectKind::EnvRead
            | EffectKind::Panic
    )
}
```
Update the doc comment above it (it currently says "`env.read` / `logging` / `panic` are class 4" — logging is now class 2 and excluded; reword to keep it accurate). `TimeRead`/`Random` STAY (nondeterminism in render is a legitimate concern — spec 028 §2.3).

- [ ] **Step 4: Run to verify it passes**

`cargo test -p fxrank-lang-ts` (the new test passes; existing react tests still green — confirm none asserted a logging-driven `EffectInRender`). fmt/clippy.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank/37 add crates/fxrank-lang-ts/src/detect/mod.rs
git -C /dev/shm/fxrank/37 commit -m "feat(ts): logging is not a render-phase risk (drop from EffectInRender set) (#37, spec 028)

A benign console.* in render isn't the wrong-place IO that effect.in.render flags
(unlike fetch). TimeRead/Random stay (nondeterminism in render is legitimate)."
```

---

### Task 3: Codify `TimeRead`/`Random` as escaping (regression pin)

`TimeRead`/`Random` are already emitted `contained: false` in all three frontends (call-effects are never contained). This task adds the test that pins it so a future change can't silently flip it.

**Files:** a frontend detect test (use the one whose fixture already emits a time/random effect — e.g. Python's `env_and_rng` fixture in `calls.rs`, or add a `random.random()` / `time.time()` line if absent).

- [ ] **Step 1: Write the test**

Assert that an emitted `TimeRead` (or `Random`) effect **escapes**: its `contained` is `false` (equivalently `effect.escapes()` is true). If the chosen fixture doesn't already produce one, add a `random.random()` (Python) call to the fixture and assert the resulting `Random` effect has `contained == false`. (Pick whichever frontend test gives the cleanest access to the raw `Effect` with its `contained` flag.)

**Important:** the Python `analyze_fixture` helper (`calls.rs` ~line 370) returns `HashMap<String, Vec<(EffectKind, u8)>>` — it drops the `contained` field. A `contained == false` codification assert CANNOT reuse this helper. The implementer must either add a raw-`Effect` accessor that returns the full `Effect` struct (including `contained`), or assert inline directly against `detect(unit, …)`'s raw effect output — do NOT bolt this assert onto the lossy `(EffectKind, u8)` helper.

- [ ] **Step 2: Run** — passes immediately (codification of existing behavior). Confirm it would FAIL if `contained` were set true (reason about it; optionally flip locally to verify, then revert).

- [ ] **Step 3: Commit**

```bash
git -C /dev/shm/fxrank/37 add -A
git -C /dev/shm/fxrank/37 commit -m "test: pin TimeRead/Random as escaping (contained=false) (#37, spec 028 §2.3)

Codifies the anti-Goodhart invariant (time/random are world effects like fetch,
never containment-discounted) so a future change can't silently flip it."
```

---

### Task 4: Dogfood verification (before/after)

**Files:** none (verification only).

- [ ] **Step 1: Full gate** — `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` all green.

- [ ] **Step 2: Dogfood the rebaseline**

```bash
cd /dev/shm/fxrank/37 && export PATH="$HOME/.cargo/bin:$PATH" && cargo build -q -p fxrank
BIN=target/debug/fxrank
APP=/home/caasi/GitLab/omni/114-kg-frontend
# omni: components/hotspots whose only effects were env.read+logging should drop out of class 4
"$BIN" scan "$APP/src" --project "$APP" | jq '[.hotspots[] | select(.max_class>=4)] | length'
# Rust self-scan: println!/log re-weight sanely, real IO unchanged
"$BIN" scan crates/ | jq '[.hotspots[] | select((.effects//[]) | any(.kind=="logging"))] | .[0]'
```
Confirm: the omni "class ≥4 from only log/env" cohort shrinks (was 6 components / 48 hotspots — log-only ones drop, env-only stay at 4); a Rust function whose only effect is `println!` now scores class 2 not 4; real IO (`net.fs.db`) hotspots unchanged. Record before/after numbers in the report. (Note: this is 028-alone; the full React win needs 027.)

- [ ] **Step 3: Record results** in the task report (no commit unless a fixture was added).

---

## Self-Review

**Spec coverage:** §2.1 logging↓ → Task 1; §2.2 render-risk → Task 2; §2.3 time/random escaping → Task 3; §3/§5 dogfood + test/snapshot fixups → Tasks 1,4. ✓
**Placeholder scan:** the only soft spots are "adapt to the existing test harness" (Task 2/3) — concrete locations given; the implementer matches the real harness. ✓
**Type consistency:** `EffectKind::base_class` returns `u8`; `(Logging, 2)` tuple matches the Python test's `(EffectKind, u8)` shape; `world_effect(EffectKind) -> bool` unchanged signature. ✓
**Scope:** core one-line + TS render-set + tests/snapshot. No structural/fold change. Sequence: this plan (028) runs BEFORE plan 027's containment work re-baselines the dogfood (027 composes on these base classes).

## Execution Handoff

Small, mechanical. Run before plan 027 (027's conditionality discount and dogfood assume these base classes). Subagent-driven, per-task review, then proceed to 027. Both land in one PR.
