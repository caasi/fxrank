# Phase 3a — Real `Effect.contained` (de-noise propagation) Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Populate the real `Effect.contained` value in all three frontends (currently stubbed `false`), so the escaping-only fold correctly **drops contained local/param mutations from propagation** — making propagated scores meaningful (today they over-report: contained class-1 mutations climb the graph and inflate scores).

**Architecture:** The frontends already compute containment — TS/Python's mutation detectors return `(Effect, contained)` tuples (used for the boundary discount); Rust knows it by effect kind (`local.mutation` is body-local/contained). Wire that real value onto `Effect.contained` at effect construction. Keep `contained` `#[serde(skip)]` (so own-body `scan` output stays byte-identical — only the propagated channel changes, which the snapshots don't capture). The fold's `Effect::escapes() = ExternalUnresolved || !contained` then does the right thing.

**Tech Stack:** Rust + the three frontend crates; the phase-1/2 core fold (`Effect::escapes`, `apply_fold`).

## Global Constraints

- `fxrank-core` stays parser-free. `Effect.contained` stays `#[serde(skip)]` (do NOT serialize — own-body output must stay byte-identical; existing snapshots must pass UNCHANGED).
- **Own-body `own_score`/`max_class`/`effects` byte-stable** — `contained` does NOT feed `own_score` (weights come from the discount/class). Only the propagated_* fields change (contained mutations stop propagating). Existing snapshots use field projection without propagated fields → must stay green.
- CI gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Slim builds compile.
- Containment canonical model: `docs/mutation-classification-guideline.md` — `local.mutation` (body-local) is `contained: yes`; `param.mutation`/`global.mutation`/`this.mutation`/`hidden.mutation`/IO/`panic` are `contained: no` (escaping). The boundary discount (TS/Python) already keys on the same notion.
- Do NOT git-commit the SDD report file (gitignored scratch) — only code.

---

### Task 1: Rust — set `Effect.contained`

**Files:** `crates/fxrank-lang-rust/src/detect/mutation.rs` (and/or the effect-construction site); test in the same crate.

**Interfaces:** Rust effects carry the real `contained`: `local.mutation` → `contained: true`; all others (`param.mutation`, `global.mutation`, IO `net.fs.db`, `panic`, etc.) → `contained: false`. (Rust uses the discount channel for `&mut`, not `contained` — a `&mut`/`&mut self` write is `param.mutation` and ESCAPES, so it is NOT contained. Only body-local writes are contained.)

- [ ] **Step 1: Failing test** — a `detect`-level test: a fn with a body-local mutation (`let mut x = 0; x = 1;` → `local.mutation`) and an escaping one (a `static`/global write → `global.mutation`). Build the record/effects; assert the `local.mutation` effect has `contained == true` and the `global.mutation` has `contained == false`. (Check `Effect::escapes()`: the local one does NOT escape, the global does.)
- [ ] **Step 2: Run** `cargo test -p fxrank-lang-rust <name>` → FAIL (contained stubbed false → local.mutation.contained == false).
- [ ] **Step 3: Implement** — in the Rust mutation detector (where `local.mutation` Effects are built, `mutation.rs`), set `contained: true` for the body-local `local.mutation` case and `false` for the escaping cases. (Search `EffectKind::LocalMutation` / the `Effect { … }` literals in mutation.rs.) Other detectors (calls/macros/risk) build escaping effects → `contained: false` (already the stub, leave as-is). Net: only `local.mutation` flips to `true`.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-rust` → PASS; existing snapshots UNCHANGED (contained is serde-skip; own_score unaffected).
- [ ] **Step 5: Commit** `feat(rust): set real Effect.contained (local.mutation contained)`

---

### Task 2: Python — set `Effect.contained` from the mutation tuple

**Files:** `crates/fxrank-lang-python/src/detect/mod.rs` (the `gather`/effect-assembly that consumes `mutation::detect`'s `(Effect, contained)` tuples).

**Interfaces:** Python's `mutation::detect` already returns `Vec<(Effect, bool)>` where the bool is containment (used for the boundary discount). Wire it: when assembling the final effects list, set `effect.contained = tuple_bool`.

- [ ] **Step 1: Failing test** — a `detect`-level test: a Python fn with a body-local rebind/mutation that the detector marks contained, and an escaping one (e.g. a module-global write → `global.mutation`, or IO). Assert the contained one has `effect.contained == true`, the escaping one `false`.
- [ ] **Step 2: Run** → FAIL (contained stubbed false).
- [ ] **Step 3: Implement** — in `gather` (the shared helper) / wherever the `(Effect, contained)` tuples are flattened into the effects list, set `effect.contained = contained` (the tuple bool) instead of leaving the stub. Verify both `analyze_unit` and `build_record` paths see it (they share `gather`, so one change suffices).
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-python` → PASS; snapshots UNCHANGED.
- [ ] **Step 5: Commit** `feat(python): set real Effect.contained from the mutation tuple`

---

### Task 3: TS — set `Effect.contained` from the gather tuple

**Files:** `crates/fxrank-lang-ts/src/detect/mod.rs` (the `gather` returns `Vec<(Effect, bool)>`; `analyze_unit` consumes it; `record_from_hotspot` copies the final Hotspot effects).

**Interfaces:** TS's `gather` already returns `(Effect, contained)` tuples (the bool drives the boundary discount at `analyze_unit`). Wire `effect.contained = tuple_bool` when building the final effects. **Because TS records copy from the final Hotspot (`record_from_hotspot`), setting it on the Hotspot's effects automatically flows to the record** — so set it in `analyze_unit` where the gathered tuples become `h.effects`.

- [ ] **Step 1: Failing test** — a `detect`-level test: a TS fn with a contained body-local mutation (`let x = 0; x = 1;` → `local.mutation` contained) and an escaping one (a module-binding write → `global.mutation`, or `this.mutation`). Assert the contained effect on the Hotspot has `contained == true`, the escaping `false`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — in `analyze_unit`, in the loop that maps the gathered `(effect, contained)` tuples into `effects` (where the boundary discount is applied), ALSO set `effect.contained = contained`. (The discount already reads `contained`; now persist it onto the Effect.) This flows to records via `record_from_hotspot` (copies `h.effects`).
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-ts` → PASS; **React snapshots UNCHANGED** (contained serde-skip; own_score/discount unaffected).
- [ ] **Step 5: Commit** `feat(ts): set real Effect.contained from the gather tuple`

---

### Task 4: Verify de-noising + gate

**Files:** none (verification) + optionally tighten a propagation test.

- [ ] **Step 1: Re-dogfood Rust (the noisy case)** — `cargo run -q -p fxrank --no-default-features --features rust -- scan /dev/shm/fxrank-025/crates/fxrank-cli/src 2>/dev/null | jq '.hotspots[] | select(.symbol=="main") | {own:.max_class, prop:.propagated_max_class, own_s:.own_score, prop_s:.propagated_score, n_inherited:(.inherited|length)}'`. BEFORE 3a: `main` had ~109 inherited (mostly class-1 local mutations) + inflated propagated_score (~154). AFTER: `n_inherited` should drop substantially (contained local mutations no longer propagate) and `propagated_score` should fall (the class-7 IO + real escaping effects remain; the class-1 local-mutation noise is gone). Record before/after in the report. (max_class should stay 7 — the real IO still propagates.)
- [ ] **Step 2: Add/strengthen a core fold test** (optional but recommended) — assert that in `A→B` where `B` has a `contained` local.mutation and an escaping IO, `A` inherits ONLY the IO, not the local.mutation. (This may already exist from phase 1's `summary_keeps_only_escaping`; if so, note it covers this.)
- [ ] **Step 3: Gate** — `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (0 failed; snapshots unchanged), slim builds (`rust`/`ts`/`python`/none) compile.
- [ ] **Step 4: Commit** if fmt/clippy touched anything.

---

## Self-Review

**Spec coverage (3a):** real `Effect.contained` in Rust/Python/TS (Tasks 1-3); de-noised propagation verified by dogfood (Task 4). Closes the "contained-stub over-propagation" Known Limitation. **Out of scope (other phase-3 plans):** roots, module-tree/precise resolution, config first/third-party classifier, module-init units, React retrofit.

**Placeholder scan:** each task points at the concrete tuple/effect-construction site; the implementer finds the exact line by reading the named file. No vague placeholders.

**Type consistency:** `Effect.contained` stays `#[serde(skip)] pub contained: bool`; `Effect::escapes()` unchanged (`ExternalUnresolved || !contained`). Each frontend sets it at effect assembly; TS flows to records via `record_from_hotspot`. Own-body (own_score/effects/snapshots) byte-identical; only propagated_* improves.
