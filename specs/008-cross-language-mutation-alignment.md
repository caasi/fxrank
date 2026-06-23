# 008 — Cross-language mutation-classification alignment

> **Goal.** Collect the shared mutation-classification knowledge spread across the three
> language frontends (Rust `syn`, TS/JS `swc`, Python `libcst`), then **align all three to
> behavioral parity** on the parts that are genuinely the same concept but have drifted. The
> **output of the implementation phase is two things**: (1) a *descriptive* guideline capturing
> the shared model + the agreed mapping, and (2) **a patch commit (series) per frontend** that
> aligns the code.
>
> **What this is NOT** (deliberately, to keep the solution space controllable):
> - **No shared code layer / crate / module.** Each frontend keeps its own native walk and its
>   own `detect/mutation.rs`. We align *behavior*, not implementation.
> - **No normative contract / governing framework.** The guideline is a *descriptive,
>   advisory* reference, not a spec future frontends MUST conform to.
> - **No scope/capture/hoisting rewrite.** All three already use "base name resolved against
>   flat function-wide sets via a priority cascade." We keep that approximation; an *unresolved
>   base* remains the proxy for "captured/module" (which TS already does). We do not build a
>   lexical scope graph.

## 1. The collected shared model (evidence)

All three frontends already share one architecture — established by reading
`crates/fxrank-lang-{rust,ts,python}/src/detect/mutation.rs`:

- A write site is reduced to a **base name** (`base_ident`/`root_name_of_expr`/`assign_target_base`).
- The base is classified against **flat, function-wide binding sets** (params, locals, …) via a
  **fixed priority cascade** (`record_write`/`classify`/`classify_and_push`).
- No nested-scope stack, no hoisting/TDZ modeling, no shadowing resolution beyond cascade
  priority; descent **stops at nested functions** (each is its own `FnUnit`).
- The result is an `EffectKind` with `class`/`tier`; the project's effect/score vocabulary
  (`fxrank-core`) is already shared.

So the architecture is common; only the **origin→effect mapping** and a few **fallbacks**
diverge. That is what this spec aligns.

### 1.1 The canonical mapping (target after alignment)

The agreed origin→effect mapping every frontend should produce (this table is the heart of the
guideline — descriptive, the post-alignment target):

| write case | `EffectKind` | class | contained | hidden | tier | applies to |
|---|---|:--:|:--:|:--:|:--:|---|
| body-local write | `local.mutation` | 1 | yes | no | Exact | all |
| param write (no declared channel) | `param.mutation` | 3 | no | no | Heuristic | TS, Python |
| `&mut` param / `&mut self` | `param.mutation` | 3→discounted | no | no | Heuristic | **Rust only** (mut-channel, KEEP) |
| receiver field, normal method | `this.mutation` | 3 | no | no | Heuristic | TS, Python |
| **constructor *direct* field-init** | `local.mutation` | 1 | yes | no | Heuristic | TS, Python (**F4**) |
| explicit declared capture (`nonlocal`) | `this.mutation` | 3 | no | no | Exact | **Python only** (KEEP) |
| real global / static | `global.mutation` | 6 | no | no | Exact/Heuristic | all (**F2** for Rust) |
| write to imported binding | `global.mutation` | 6 | no | no | Heuristic | all (**F5**) |
| interior-mutability / ref-cell write | `hidden.mutation` | 3 | no | yes | Heuristic | Rust, TS (+ subreason **F3**) |
| **captured-enclosing / unresolved base** | `hidden.mutation` | 3 | no | yes | Heuristic | all (**F1**) |

## 2. KEEP — honest language differences (out of alignment scope)

These differ because the *languages* differ; aligning them would be wrong. The guideline
documents them as intentional:

- **Rust mut-channel discount** (`apply_discount`: `&mut`→−2, `&mut self`→−1, floor 1, cancelled
  under `unsafe`; `rust/.../mutation.rs:111-132`, `score.rs:74-84`). Rust ownership; TS/Python
  have no `&mut`.
- **TS/Python typed-boundary discount** (`apply_boundary_discount`, floor 0, applied in
  `analyze_unit`; `score.rs:62-72`). Gradual typing; Rust is fully typed and does not apply it.
- **Python `nonlocal` → `this.mutation`/3/Exact/not-hidden** (`python/.../mutation.rs:429-438`).
  An *explicitly declared* capture is visible, so not `hidden` — distinct from an implicit
  capture (which is F1's `hidden`).
- **Tier = Exact when truly known vs Heuristic when inferred** (e.g. Python `global` decl is
  Exact; a type-blind interior-mutator guess is Heuristic). Reflects real confidence.

## 3. FIX — the alignment (behavioral parity; snapshots change intentionally)

Each fix is the **same concept** handled incompatibly today. Targets per §1.1.

### F1 — captured-enclosing / unresolved base → `hidden.mutation` everywhere
- **Today:** TS emits `hidden.mutation`/3/hidden (`ts/.../mutation.rs:213-215`, role `"captured"`).
  Python **emits nothing** (`python/.../mutation.rs:457-459`) — silent false-purity. Rust is
  inconsistent: a captured `let mut`→`local.mutation`/1, an UPPERCASE name→`global.mutation`/6,
  otherwise **dropped**.
- **Target:** an unresolved base (not own-local / param / receiver / known-static / import) →
  `hidden.mutation`/3/hidden in all three. **TS is the reference** (already correct).
- **Why no scope analysis needed:** "unresolved base" is already the operative signal; Python
  just emits in its existing `else` branch, Rust emits instead of proxy/drop.

### F2 — Rust real static set (retire the UPPERCASE proxy)
- **Today:** Rust uses `is_screaming_snake` as a proxy for statics because the collected static
  names are **not threaded** into `mutation::detect` (`detect/mod.rs:89-93` passes `statics`
  only to `calls::detect`). False-positives on UPPERCASE locals/consts, false-negatives on
  lowercase statics.
- **Target:** thread the real static set; a write to a genuine static → `global.mutation`/6;
  everything else unresolved → F1's `hidden.mutation`. Retire `is_screaming_snake`.
- **Couples with F1:** once real statics are caught, the remaining unresolved bases are exactly
  the captured/module case F1 routes to `hidden`.

### F3 — consistent `hidden.mutation` subreason
- **Today:** TS ref-cell sets `subreason: "ref-cell-write"`; Rust interior-mut sets **none**;
  the new F1 captured case has none.
- **Target:** a small, documented subreason vocabulary for `hidden.mutation` used consistently
  (e.g. `"ref-cell-write"`, `"interior-mut"`, `"captured-binding"`). Evidence-string / reporting
  only — **no class/ranking change**.

### F4 — constructor breadth parity (TS → Python's stricter rule)
- **Today:** TS treats **any** `this.*` write in a constructor as `local.mutation`/1/contained
  (`ts/.../mutation.rs:200-203`). Python treats only a **direct** `self.attr =` in `__init__`
  as contained; `self.x.append()` / `self[i]=` escape to `this.mutation`/3 even in `__init__`
  (`python/.../mutation.rs:397-412`).
- **Target:** only a **direct** field-init (`this.x = …` / `self.attr = …`) is the contained
  `local.mutation`/1 case; a method/subscript/augmented write on the receiver inside a
  constructor escapes to `this.mutation`/3. **Python is the reference.** TS changes (needs a
  place-shape check in its constructor branch). Rust has no constructor concept — n/a.

### F5 — write to an imported binding → `global.mutation`
- **Today:** TS classifies a write to an imported binding as `global.mutation`/6
  (`ts/.../mutation.rs:211`). Rust and Python don't consult imports in the mutation path
  (drop / no-emit).
- **Target:** Python and Rust also classify a write whose base resolves through the import table
  as `global.mutation`/6. **TS is the reference.** Each needs the mutation walker to consult its
  existing `ImportTable`.

## 4. Per-frontend alignment plan

| Fix | Rust | TS | Python |
|---|---|---|---|
| F1 | unresolved (non-static, non-import) base → `hidden.mutation` (was drop / UPPER-global / captured-`let mut`-as-local) | — *(reference)* | emit `hidden.mutation` in the `else` branch (was no-emit) — **Python gains `HiddenMutation`** |
| F2 | thread real static set; retire `is_screaming_snake` | n/a | n/a (`global` decl already exact) |
| F3 | add subreason to interior-mut + captured | add subreason to captured (ref-cell already has one) | add subreason to the new captured case |
| F4 | n/a | direct-field-init-only → contained; receiver method/subscript in ctor → `this.mutation`/3 | — *(reference)* |
| F5 | consult import table → imported write = `global.mutation` | — *(reference)* | consult import table → imported write = `global.mutation` |

**Expected snapshot impact (all intentional, labelled — never "regression"):**
- **Python** — functions mutating captured/module/imported names gain `hidden`/`global`
  effects (were scored pure). First `HiddenMutation` emissions for Python.
- **Rust** — UPPERCASE non-static writes move from `global`/6 to `hidden`/3; real statics now
  caught as `global`/6; imported-binding writes → `global`/6; some previously dropped writes
  now emit.
- **TS** — constructor method/subscript writes move from `local`/1 to `this.mutation`/3;
  captured-case evidence gains a subreason (no class change).

## 5. Outputs (the deliverables)

1. **Guideline** — `docs/mutation-classification-guideline.md`, *descriptive*: the shared model
   (§1), the canonical mapping (§1.1), the KEEP honest-differences (§2) with rationale, and a
   short per-frontend "how this language realizes the mapping + its accepted limitations" note.
   Advisory reference; **not** a governing contract. A future frontend MAY read it; nothing
   MUST conform.
2. **Patch commits per frontend** — one focused series each for Rust, TS, Python implementing
   §4, with the targeted snapshot updates. Each fix in its own commit (per the repo's
   one-issue-per-commit convention) so the behavioral deltas are reviewable in isolation.

## 6. Validation

- **Snapshots** — updated only for the functions touched by an in-scope fix, each labelled an
  intentional behavioral change. Everything else stays byte-identical (the alignment must not
  perturb unrelated rankings).
- **Targeted assertions** (reason-level, not snapshots alone): a captured-enclosing write emits
  `hidden.mutation`/3 in all three (F1); a Rust real-`static` write is `global`/6 and an
  UPPERCASE local is **not** (F2); a TS ctor `this.x.push()` is `this.mutation`/3 while
  `this.x = …` stays `local`/1 (F4); an imported-binding write is `global`/6 in each frontend
  (F5). The **anti-Goodhart canary stays intact** (hidden interior-mut still out-scores a
  discounted `&mut self`).
- **React internals un-regressed** — component score-inheritance, `EffectInRender`, ref-cell
  inheritance (spec 007) unchanged; F1/F3 must not perturb the absorbed-callback paths.
- **Dogfood** — run `fxrank scan` on the usual dogfood corpora (the local React/TS, Python, and
  Rust repos) before/after to eyeball that the ranking deltas are the intended ones.

## 7. Out of scope

Shared code layer / crate; a normative contract or Contract/Profile framework; a lexical
scope-graph / hoisting / shadowing rewrite; `#28` cross-unit fold; `#4` discount unification.
Honest language differences (§2) are kept, not unified.

## 8. Named tasks for the implementation plan

1. **Write the guideline** (`docs/mutation-classification-guideline.md`) from §1–§2 — the
   collected shared model + canonical mapping + KEEP differences.
2. **Rust patches** — F2 (thread static set, retire proxy) + F1 (unresolved → hidden) + F5
   (imports → global) + F3 (subreasons); each its own commit; update targeted Rust snapshots.
3. **Python patches** — F1 (emit hidden in the `else` branch) + F5 (imports → global) + F3
   (subreason); update targeted Python snapshots.
4. **TS patches** — F4 (constructor direct-field-init only) + F3 (captured subreason); update
   targeted TS snapshots.
5. **Conformance assertions** (§6) across all three frontends + the anti-Goodhart canary + React
   un-regression checks.
6. **Dogfood pass** and a short note recording the observed ranking deltas.

## 9. Risks

- **Unintended snapshot churn** — an alignment perturbs rankings beyond the targeted functions.
  Mitigated by per-fix commits + reviewing each snapshot diff in isolation.
- **Python's first `HiddenMutation`** — ensure the new emission path threads `hidden`/subreason/
  tier correctly and is suppressed inside React-absorbed callbacks where applicable (Python has
  no React path, so low risk, but verify the `contained`/discount interaction in `analyze_unit`).
- **TS constructor place-shape check (F4)** — TS must distinguish a direct `this.x =` from a
  receiver method/subscript write in the constructor branch; verify it doesn't regress the
  React/ref-cell ctor cases.
- **Guideline drifting toward a governing framework** — keep it descriptive; if it starts
  reading like a normative contract, trim it back.

## 10. Relationship to prior issues

- **#31** — this is the *narrow, evidence-grounded* realization of #31's intent (the three
  drifted `detect/mutation.rs`), after a first over-formalized attempt was discarded for making
  the solution space uncontrollable. No shared layer, no IR, no contract.
- **#29** (module-var → global) — partially subsumed: F1+F2 make the captured-vs-global
  distinction consistent via the unresolved-base + real-static signals (not via a scope graph).
  The parked `feat/module-var-global` branch (commit `6092902`) is reference only.
- **#4 / #28** — untouched and orthogonal.
