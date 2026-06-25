# Spec 028 — Cross-language scoring-table rebaseline (logging↓, time/random = world)

**Status:** draft v2 (2026-06-25, review-looped: Codex doc-clean + opus vs-code — centralization & codification confirmed; §3/§5 accuracy fixed) · **Issue:** #37 (companion to Spec 027; **lands in the same PR**) · **Release-blocking** (part of "the current scoring is wrong"). **Precedence:** spec 001 owns the `EffectKind → class` table; this spec rebaselines **two** entries and codifies one invariant. Cross-language by nature — touches Rust/Python/TS scoring, snapshots, and rank order. Code-vs-spec disagreements resolve to the spec.

## 1. Why (separated from 027 on purpose)

The React redesign (spec 027) fixes **attribution** (who owns an effect). It does **not** fix two **base-class** problems the omni dogfood surfaced, because those live in the central `EffectKind → class` table in `fxrank-core` and affect **all three frontends**, not just React:

- **Logging is over-weighted.** `Logging` is class **4** (tied with `EnvRead`/`Panic`). Dogfood: **48 hotspots whose only effects are `env.read` + `logging`** rank class 4; components rank class 4 purely on an env-guarded `console.warn`. Logging is **neither a mutation nor a world-input** — it is a benign output stream write. It should not drown real signal.
- **time/random must be world/escaping (anti-Goodhart).** `TimeRead`/`Random` are class 5 but the model must guarantee they are treated like `fetch`: **escaping world effects, never eligible for the containment discount** (a `new Date()` / `Math.random()` is observable nondeterminism regardless of how the enclosing function is typed). Spec 027's containment classifier relies on this (a `new Date()` in a `useCallback` is escaping, not contained).

Keeping these in the React spec would couple a TS-frontend redesign with a cross-language table change (Rust `println!`, Python `logging`, snapshots) — different blast radius, different tests. Hence a separate spec, **same PR**.

## 2. Changes

### 2.1 `Logging`: class 4 → **class 2** (decision: lower)
Move `Logging` from the class-4 group to class **2** (joining `AmbientRead`/`UnknownMacro`/`ExternalUnresolved`). Rationale: logging is a real but **minor, benign observable output** — not a mutation (no state change), not a world-input (no nondeterminism/data read), not a risky IO boundary. Class 2 keeps it *visible but non-dominating*: a function does not become a hotspot for logging alone, but a log line still isn't "free" (it is an external write).
- **Single source of truth:** change only `EffectKind::base_class` in `fxrank-core/src/effect.rs`. All three frontends inherit it (no per-frontend class literals — that's the centralization invariant).
- **Decision (confirmed): class 2.** Logging is an external write, not a contained-local — it stays *visible but minor*. (Class 1, "near-free", was considered and rejected as too low for a real output stream write.)

### 2.2 `Logging` removed from the render-phase risk set
`EffectInRender` currently fires for any `world_effect`, which includes `Logging` (`fxrank-lang-ts/src/detect/mod.rs`). A `console.log` during render is **not** the "wrong-place IO" that `effect.in.render` is meant to flag (unlike a `fetch` in render). Remove `Logging` from the `world_effect` set that triggers `EffectInRender` (TS), so a benign render-phase log no longer raises a class-4 render risk. (This is the TS-side companion to 2.1; without it, lowering the base class still leaves logging inflating components via the render risk — codex M11.)

### 2.3 `TimeRead` / `Random`: codify as world/escaping (decision 丙)
Keep class **5**, but make the **escaping/world** treatment explicit and uniform across frontends:
- `contained` is **always false** for `TimeRead`/`Random` (they are world effects — `escapes()` true), so they are **never** reduced by the spec-003 containment discount, exactly like `NetFsDb`. (They arise from calls — `new Date()`, `Math.random()`, `time.time()`, `random.*`, `SystemTime::now()`, `rand::*` — and call-effects are already "never contained"; this spec **codifies** the invariant and adds a test so a future change can't silently make them contained.)
- They **may** carry `EffectInRender` (nondeterminism during render is a legitimate render-phase concern — unlike logging). No change there.

### 2.4 Explicit non-changes
- **`EnvRead` stays class 4.** Reading the environment **is** a world-input (config/nondeterminism from outside), so the "not an input" rationale for lowering logging does **not** apply. The env.read+logging over-count is fixed by lowering logging alone; `EnvRead` is deliberately untouched. (Reconsidering `EnvRead` is a possible future refinement, out of scope.)
- **Env-guarded / dev-only logging** (e.g. `if (process.env.NODE_ENV === 'development') console.error(...)`) is **not** specially discounted here — a finer "dead in prod" distinction is deferred. Class-2 logging is low enough that this is not pressing.

## 3. Cross-language scope & impact

`Logging` is detected per-frontend; lowering its class re-weights all of them:
- **Rust:** `println!`/`eprintln!`/`print!`/`eprint!`/`dbg!`/`log::{info,warn,error,debug,trace}!`/**`tracing::*`** (all map to `EffectKind::Logging`).
- **Python:** `print`, `logging.*`.
- **TS/JS:** `console.*`.

**Expected output changes (intended):** functions/components whose dominant effect was logging drop ~2 classes; rank order shifts (real IO rises relative to log-heavy code); the 48 omni log-only hotspots fall out of the class-4 band. **Snapshot churn is narrow** (verified): exactly **one** committed snapshot is logging-driven — `fxrank-lang-rust/tests/snapshots/snapshots__logging_soup_and_one_io.snap` (rebaselines to class 2); the TS/Python snapshot suites contain **no** logging-dominated entries (the React `effects.snap` `effect.in.render` is `fetch`-driven, so §2.2 does not move it). The behavioral re-weighting is genuinely cross-language (all detectors read `base_class`), but **confirm it via dogfood, not snapshot diffs** for TS/Python. 028 is an intentional rebaseline, not byte-stable.

## 4. Invariants

- **Only two table entries change** (`Logging` class; the `EffectInRender` logging trigger) **+ one codified invariant** (time/random escaping). No structural, fold, or resolution change.
- **Centralization preserved:** class lives only in `EffectKind::base_class`; no frontend hardcodes it.
- **Never-guess / core neutrality** unaffected.
- **Interaction with 027:** 027's containment classifier + conditionality discount compose on top of these base classes. E.g. `DateRangeInput`'s `new Date()` in a `useCallback`: base class 5 (time, escaping per 2.3) → 027 event-phase conditionality discount (−1, floor 1) → class 4, still visible but no longer dominating — and never containment-discounted. A `console.warn` in the same handler: base class 2 (2.1) → not a render risk (2.2) → no longer inflates the component.

## 5. Testing

- **Core unit:** update `base_class` asserts (`Logging == 2`); add an assert that `TimeRead`/`Random` effects are escaping (`contained == false` / `escapes()`) — the only thing pinning the 2.3 invariant.
- **Required test fixup (will FAIL otherwise):** `crates/fxrank-lang-python/src/detect/calls.rs:394` hardcodes `assert!(io.contains(&(Logging, 4)))` — change to `(Logging, 2)`. Also correct stale class-4 mentions in **comments** (`fxrank-lang-python/src/detect/mod.rs:1424`, `fxrank-cli/src/main.rs:1648` — doc only, asserts there don't depend on logging's class).
- **Snapshots:** re-accept the **one** Rust logging snapshot (`snapshots__logging_soup_and_one_io.snap`) via `cargo insta review`. TS/Python snapshot suites have **no** logging entries — do not chase non-existent diffs there; verify them via dogfood.
- **Dogfood (before/after, the [[dogfood-repos]]):** the omni log-only class-4 hotspots drop; spot-check Rust (`fxrank scan crates/`) and a Python repo to confirm `println!`/`logging` re-weight sanely and nothing real regressed. Record the before/after of the omni "class ≥4 from only log/env" count (was 6 components / 48 hotspots).

## 6. Out of scope

- **Spec 027** — React attribution (the companion; same PR).
- **#46** — deterministic output ordering.
- `EnvRead` reclassification; env-guarded-logging "dead in prod" discount; any new `EffectKind`.

## 7. Decomposition

Small. A paired plan (`plans/028-scoring-table-rebaseline.md`) sequences: (1) `base_class` change + core asserts + time/random escaping assert; (2) remove `Logging` from the TS `EffectInRender` world set; (3) re-accept snapshots across frontends; (4) dogfood before/after. Lands in the **same PR** as spec 027's implementation, closing #37 and lifting the release gate.
