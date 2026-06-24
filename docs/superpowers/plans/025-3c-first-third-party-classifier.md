# Phase 3c — First/Third-Party Reach Classifier Plan

> **Post-review note (historical plan):** this plan deferred TS `@/` / `~/` path
> aliases, but they were **added during review** (the implementation in
> `detect/refs.rs` treats `@/`/`~/` as first-party). The current behavior is in the
> guideline + spec; this plan's "deferred" note is superseded.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Classify each opaque external reach as `FirstPartyOutOfScope` (the project's own code that wasn't in the scan batch — e.g. a relative `./util` import) vs `ThirdParty` (a real external dependency — `react`, `std`, `os`). Today `resolve.rs` hardcodes EVERY opaque reach to `ThirdParty`; the `ReachKind::FirstPartyOutOfScope` variant exists but is never emitted. This makes `scope.external_reaches` distinguish "you didn't scan enough" from "genuine outward dependency."

**Architecture:** Mirror the `qualified` pattern — the **frontend tags** each `CallSiteRef` with a `first_party: bool` (it knows the language's first-party shape: TS relative `./`/`../`/`/` specifier; Python leading-dot relative import; Rust `crate::`/`super::`/`self::`), and **core decides** the `ReachKind` in `resolve.rs` (`first_party → FirstPartyOutOfScope else ThirdParty`). Core stays syntax-free. `CallSiteRef` is internal (not serialized), so only `scope.external_reaches[].kind` changes in output.

**Tech Stack:** Rust + the three frontend crates + `fxrank-core` resolve.

## Global Constraints

- `fxrank-core` stays parser-free AND syntax-free in resolution — the first-party judgment is frontend-side (a `bool` on `CallSiteRef`); core only branches on the bool.
- `CallSiteRef` is the internal record format (NOT serialized) — adding `first_party` doesn't change scan output by itself. **Output change is limited to `scope.external_reaches[].kind`** flipping some `ThirdParty`→`FirstPartyOutOfScope`. Snapshots capturing `external_reaches` will churn — re-accept and confirm ONLY `kind` changes on genuinely relative/first-party reaches.
- Own-body scores + propagation UNCHANGED (this only re-labels reach kind).
- CI gates: fmt/clippy/test/slim-builds.
- Default `first_party = false` preserves today's behavior (ThirdParty) for anything not explicitly first-party.
- Do NOT git-commit the SDD report file.

---

### Task 1: Core — `first_party` on `CallSiteRef` + `resolve.rs` picks `ReachKind`

**Files:** `crates/fxrank-core/src/record.rs` (`CallSiteRef`), `crates/fxrank-core/src/resolve.rs` (Opaque-edge construction); tests in core.

**Interfaces:** add `pub first_party: bool` to `CallSiteRef` (with `#[serde(default)]` if the struct derives Deserialize). In `resolve.rs` where the Opaque `ExternalReach` is built (line ~78-80), set `kind: if r.first_party { ReachKind::FirstPartyOutOfScope } else { ReachKind::ThirdParty }`.

- [ ] **Step 1: Failing test** — in `resolve.rs` tests: build a `CallSiteRef { qualified: true, first_party: true, .. }` that doesn't resolve → assert the resulting `Edge::Opaque(reach)` has `reach.kind == ReachKind::FirstPartyOutOfScope`. And one with `first_party: false` → `ThirdParty`.
- [ ] **Step 2: Run** `cargo test -p fxrank-core` → FAIL (field missing / always ThirdParty).
- [ ] **Step 3: Implement** — add the field (update every `CallSiteRef { .. }` literal in core tests/builders to include `first_party: false` or `..Default`); branch the `kind` in `resolve_ref`. Update the `resolve.rs` doc comment (line ~58) to note the first_party→kind mapping.
- [ ] **Step 4: Run** `cargo test -p fxrank-core` → PASS.
- [ ] **Step 5: Commit** `feat(core): resolve picks ReachKind from CallSiteRef.first_party`

---

### Task 2: Rust frontend — tag `first_party` (`crate::`/`super::`/`self::`)

**Files:** `crates/fxrank-lang-rust/src/detect/refs.rs`; test.

**Interfaces:** in the Rust `refs::extract`, set `first_party = base starts with "crate::" | "super::" | "self::"` (the in-crate path roots). `std::`/`core::`/`alloc::` and external crates (`serde::`, `tokio::`) → `first_party = false` (ThirdParty). A bare unqualified name → `first_party = false` (it's not a qualified outward ref anyway).

- [ ] **Step 1: Failing test** — a fn calling `crate::helpers::foo()`, `super::bar()`, `std::fs::write(...)`, `serde::to_string(...)`; assert the refs have `first_party == true` for the `crate::`/`super::` ones, `false` for `std::`/`serde::`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — compute `first_party` from the `base` path prefix; set it on each `CallSiteRef`.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-rust` → PASS.
- [ ] **Step 5: Commit** `feat(rust): tag first_party refs (crate::/super::/self::)`

---

### Task 3: Python frontend — tag `first_party` (relative imports)

**Files:** `crates/fxrank-lang-python/src/detect/refs.rs` + possibly `imports.rs`; test.

**Interfaces:** a call whose `root` resolves through a **relative import** (`from . import x`, `from .mod import y`, `from ..pkg import z` — a leading-dot module) → `first_party = true`. An absolute import (`os`, `django.http`, `numpy`) → `first_party = false`. (Check how `Imports` records relative imports — does `resolve` preserve the leading dot, or is there a relative-ness flag? If relative imports aren't currently distinguishable, add the minimal tracking: the import table notes whether a name came from a leading-dot `from`-import.)

- [ ] **Step 1: Failing test** — a module with `from .utils import helper` and `import os`, calling `helper()` and `os.getcwd()`; assert the `helper` ref `first_party == true`, the `os.getcwd` ref `first_party == false`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — track leading-dot relative imports in `Imports` (a set of locals from relative imports, or a `resolve_relative` predicate); set `first_party` on the ref accordingly.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-python` → PASS.
- [ ] **Step 5: Commit** `feat(python): tag first_party refs (relative imports)`

---

### Task 4: TS frontend — tag `first_party` (relative/absolute-path specifiers)

**Files:** `crates/fxrank-lang-ts/src/detect/refs.rs`; test.

**Interfaces:** a call whose resolved `module` (ES specifier) starts with `.` (`./util`, `../lib`) or `/` (absolute) → `first_party = true`. A bare package (`react`, `node:fs`, `@org/pkg`) → `first_party = false`. (tsconfig path-aliases like `@/` are a config-aware enhancement — DEFER unless trivially available; the `.`/`/` prefix is the tractable syntactic signal.)

- [ ] **Step 1: Failing test** — a module `import {a} from './util'; import {b} from 'react';` calling `a()` and `b()`; assert the `a` ref `first_party == true`, the `b` ref `first_party == false`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — set `first_party = module.map(|m| m.starts_with('.') || m.starts_with('/')).unwrap_or(false)` on each ref.
- [ ] **Step 4: Run** `cargo test -p fxrank-lang-ts` → PASS.
- [ ] **Step 5: Commit** `feat(ts): tag first_party refs (relative specifiers)`

---

### Task 5: Dogfood + gate

- [ ] **Step 1: Dogfood (record)** — show the reach split per frontend:
  - TS: `cargo run -q -p fxrank --no-default-features --features ts -- scan /home/caasi/GitLab/omni/114-kg-frontend/src | jq '.scope.external_reaches | group_by(.kind) | map({kind:.[0].kind, n:length})'` — expect BOTH `FirstPartyOutOfScope` (relative `./` imports to unscanned files) AND `ThirdParty` (react, etc.).
  - Python: `... --features python -- scan <a django subpackage dir with relative imports> | jq '.scope.external_reaches | group_by(.kind) | map({kind:.[0].kind, n:length})'`.
  - Rust: `... --features rust -- scan crates/fxrank-cli/src | jq '.scope.external_reaches | group_by(.kind) | map({kind:.[0].kind, n:length})'` — `crate::` reaches → FirstPartyOutOfScope, `std::`/external → ThirdParty.
  Record observations (the split looks right: relative/in-crate = FirstParty, packages = ThirdParty).
- [ ] **Step 2: Gate** — fmt/clippy/test (0 failed; re-accept snapshots showing ONLY `kind` flips on relative reaches)/slim builds.
- [ ] **Step 3: Commit** snapshot re-accepts + fmt/clippy.

---

## Self-Review

**Spec coverage (3c):** frontend `first_party` tagging (Tasks 2-4) + core ReachKind selection (Task 1) + dogfood (Task 5). Closes the "all reaches are ThirdParty" gap. **Deferred:** tsconfig path-aliases / pnpm-workspace / pyproject names as first-party (config-file parsing — a further enhancement); the syntactic relative/in-crate signal is the tractable core.

**Placeholder scan:** each task names the concrete prefix test (`crate::`/`super::`, leading-dot, `.`/`/` specifier). Python Task 3 flags the "may need to add relative-import tracking to `Imports`" honestly.

**Type consistency:** `CallSiteRef.first_party: bool` (frontend-set, `#[serde(default)]`); `resolve_ref` maps it to `ReachKind`; `ExternalReach.kind` already exists. Default false = today's ThirdParty behavior. Own-body/propagation unchanged; only `external_reaches[].kind` re-labels.
