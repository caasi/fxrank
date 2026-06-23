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
- The base is classified against **flat per-unit binding sets** via a **fixed priority cascade**
  (`record_write`/`classify`/`classify_and_push`). Params are pre-seeded from the signature;
  locals are populated **during the source-order walk** in TS/Rust (with a before-declarator
  caveat — a write before its declarator misclassifies) while Python **pre-scans**
  `global`/`nonlocal`/local-assign into sets first.
- No lexical scope stack, no hoisting/TDZ modeling, no shadowing resolution beyond cascade
  priority. **TS and Python stop descent at nested functions** (each becomes its own `FnUnit`);
  **Rust's mutation walker descends into closures and block-local `fn` items** (it overrides no
  `visit_*` to stop, and `functions.rs` collects only top-level/module/impl/trait fns as units),
  so their writes are attributed to the enclosing unit. This traversal divergence is
  acknowledged, not aligned — it is outside the F1–F5 mutation-classification scope, and means
  Rust's set of bases reaching the F1 fallback differs from TS/Python's (a closure-captured
  enclosing `let mut` is in the parent's `let_mut`, so it stays `local.mutation`/1, not hidden).
- The result is an `EffectKind` with `class`/`tier`; the project's effect/score vocabulary
  (`fxrank-core`) is already shared.

So the architecture is common; only the **origin→effect mapping** and a few **fallbacks**
diverge. That is what this spec aligns.

### 1.1 The canonical mapping (target after alignment)

The agreed origin→effect mapping every frontend should produce (this table is the heart of the
guideline — descriptive, the post-alignment target):

| write case | `EffectKind` | class | contained² | hidden | tier | applies to |
|---|---|:--:|:--:|:--:|:--:|---|
| body-local **place** mutation¹ | `local.mutation` | 1 | yes | no | Exact | all |
| param **place** mutation (no declared channel)¹ | `param.mutation` | 3 | no | no | Heuristic | TS, Python |
| `&mut` param / `&mut self` | `param.mutation` | 3 (`discounted_to` 1/2 when safe) | — | no | Heuristic | **Rust only** (mut-channel, KEEP) |
| receiver field, normal method | `this.mutation` | 3 | no | no | Heuristic | TS, Python |
| **constructor *direct* field-init** | `local.mutation` | 1 | yes | no | Heuristic | TS, Python (**F4**) |
| explicit declared capture (`nonlocal`) | `this.mutation` | 3 | no | no | Exact | **Python only** (KEEP) |
| real global / static | `global.mutation` | 6 | no | no | Exact/Heuristic | all (**F2** for Rust) |
| write to imported binding | `global.mutation` | 6 | no | no | Heuristic | all (**F5**; near-vacuous for Rust) |
| interior-mutability / ref-cell write | `hidden.mutation` | 3 | no | yes | Heuristic | Rust, TS (+ subreason **F3**) |
| **captured-enclosing / unresolved base** | `hidden.mutation` | 3 | no | yes | Heuristic | all (**F1**) |

¹ All three emit `local.mutation`/1 for a *place* mutation of a local (`x.f = …`, `x[i] = …`,
`x += …`, `x.method()`). They **differ on a plain rebind** (`x = …` to a bare local name):
**Python emits nothing** (assignment rebinds the name, not a mutation of prior state —
`python/.../mutation.rs:379`), while **TS and Rust emit `local.mutation`/1** (reassigning a
declared local / `let mut` is a variable mutation — `ts/.../mutation.rs:207-208`,
`rust/.../mutation.rs:168-175`). Honest language difference (Python name-rebinding vs TS/Rust
variable-slot mutation), KEEP — see §2.
² `contained` is **not** a serialized `Effect` field — Rust's `Effect` has no containment
channel at all (`fxrank-core/src/effect.rs`). TS and Python carry it as a **per-frontend
side-channel** (`detect` returns `(Effect, bool)` pairs) consumed by `apply_boundary_discount`
in `analyze_unit`. Rust expresses containment only via `discounted_to` (the mut-channel
discount) and never runs the boundary discount — hence the `—` in the `&mut` row.

## 2. KEEP — honest language differences (out of alignment scope)

These differ because the *languages* differ; aligning them would be wrong. The guideline
documents them as intentional:

- **Rust mut-channel discount** (`apply_discount`: `&mut`→−2, `&mut self`→−1, floor 1, cancelled
  under `unsafe`; `rust/.../mutation.rs:111-132`, `score.rs:74-84`). Rust ownership; TS/Python
  have no `&mut`.
- **TS/Python typed-boundary discount** (`apply_boundary_discount`, floor 0, applied in
  `analyze_unit`; `score.rs:62-72`). Gradual typing; Rust is fully typed and does not apply it.
- **Python `nonlocal` → `this.mutation`/3/Exact/not-hidden** (`python/.../mutation.rs:429-438`).
  An *explicitly declared* capture is visible, so not `hidden`. Note this is the same class (3)
  as F1's implicit-capture `hidden.mutation` but a **distinct kind** — declared/visible
  (`this.mutation`) vs implicit/hidden (`hidden.mutation`); not a contradiction.
- **Tier = Exact when truly known vs Heuristic when inferred** (e.g. Python `global` decl is
  Exact; a type-blind interior-mutator guess is Heuristic). Reflects real confidence.
- **Per-language mutating-method allowlists** — Rust `{push,insert,clear,extend,remove,pop,append,truncate}`
  (`rust/.../mutation.rs:247-252`), TS `{push,pop,shift,unshift,splice,sort,reverse,fill,copyWithin,set,add,delete,clear}`
  (`ts/.../mutation.rs:393-410`), Python `{append,extend,insert,remove,pop,clear,sort,reverse,update,add,discard,setdefault}`
  (`python/.../mutation.rs:505-521`). These are language-appropriate vocabularies and the
  *gate* on whether a method write is even seen — honest, KEEP (not aligned).
- **Plain rebind of a local** (`x = …` to a bare local name) — **Python no-emits** (rebinding
  ≠ mutation; `python/.../mutation.rs:379`); **TS and Rust emit `local.mutation`/1**
  (reassigning a declared local / `let mut` is a variable mutation). Honest semantic difference
  (name-rebinding vs variable-slot mutation), KEEP.
- **Destructuring-target writes are dropped in all three** (TS `assign_target_base`→`None`
  `ts/.../mutation.rs:336`; Python skips Tuple/List/Starred `python/.../mutation.rs:388-389`;
  Rust `base_ident` has no tuple/struct-target support `rust/.../mutation.rs:345`). A shared
  *limitation*, not a parity target — recorded as an accepted gap, not fixed here.

## 3. FIX — the alignment (behavioral parity; snapshots change intentionally)

Each fix is the **same concept** handled incompatibly today. Targets per §1.1.

### F1 — captured-enclosing / unresolved base → `hidden.mutation` everywhere
- **Definition:** F1 is the **final fallback** taken *after* every existing known-origin check
  in each frontend's priority cascade **and** after F2's real-static check and F5's import check
  — i.e. a base that resolves to none of {own-local, param, receiver, declared global/static,
  import}. It is not "any unresolved base in isolation"; it sits at the tail of the cascade.
- **Today:** TS emits `hidden.mutation`/3/hidden at this tail (`ts/.../mutation.rs:213-215`, role
  `"captured"`). Python **emits nothing** (its cascade falls off the end with no `else`,
  `python/.../mutation.rs:457-459`) — silent false-purity. Rust **drops** it (`record_write`
  falls through all branches, `rust/.../mutation.rs:164-189`), *except* an UPPERCASE name which
  the proxy mis-routes to `global.mutation`/6. (A `let mut` binding is body-local, not captured —
  the walker stops at the `FnUnit` boundary, so a genuinely captured binding is never in
  `let_mut`.)
- **Target:** that tail-of-cascade fallback → `hidden.mutation`/3/hidden in all three. **TS is
  the reference** (already correct).
- **Why no scope analysis needed:** the fallback bucket *is* the captured/module/unknown
  approximation — Python emits in its (currently empty) `else`, Rust emits instead of dropping.
  No lexical scope graph; just the existing cascade tail.

### F2 — Rust real static set (retire the UPPERCASE proxy)
- **Today:** Rust uses `is_screaming_snake` as a proxy for statics because the collected static
  names are **not threaded** into `mutation::detect` (`detect/mod.rs:89-93` passes `statics`
  only to `calls::detect`). False-positives on UPPERCASE locals/consts, false-negatives on
  lowercase statics.
- **Target:** thread the real static set; a write to a genuine static → `global.mutation`/6;
  everything else unresolved → F1's `hidden.mutation`. Retire `is_screaming_snake`.
- **Signature change:** `mutation::detect(&block, &sig)` (`rust/.../mutation.rs:35`) gains the
  static set (and the import table, for F5) as parameters; `gather` already holds both and
  passes them to `calls::detect` (`detect/mod.rs:91-93`) — same plumbing.
- **Couples with F1:** once real statics are caught, the remaining unresolved bases are the
  **fallback bucket** F1 routes to `hidden` (the chosen captured/module/unknown approximation —
  not provably "exactly captured/module," but the honest catch-all).

### F3 — consistent `hidden.mutation` subreason
- **Today:** TS ref-cell sets `subreason: "ref-cell-write"`; Rust interior-mut sets **none**;
  the new F1 captured case has none.
- **Target:** a small, documented subreason vocabulary for `hidden.mutation` used consistently
  (e.g. `"ref-cell-write"`, `"interior-mut"`, `"captured-binding"`). Evidence-string / reporting
  only — **no class/ranking change**.

### F4 — constructor breadth parity (TS → Python's stricter rule)
- **Today:** TS collapses **all** of `this.x = …`, `this.x.push()`, and `this[i] = …` in a
  constructor to `local.mutation`/1/contained — because `classify` only sees the base ident
  `"this"` and the constructor branch ignores the place shape (`ts/.../mutation.rs:198-205`;
  the method-call path routes the receiver `this.x` through `record_write`, base→`"this"`,
  `mutation.rs:298-309`). Python treats only a **direct** `self.attr =` in `__init__` as
  contained; `self.x.append()` / `self[i]=` escape to `this.mutation`/3 even in `__init__`
  (`python/.../mutation.rs:397-412`).
- **Target:** only a **direct** field-init (assignment target exactly `this.<ident>` /
  `self.attr`) is the contained `local.mutation`/1 case; a method-call receiver or
  subscript/member-chain write on the receiver inside a constructor escapes to
  `this.mutation`/3. **Python is the reference.** Rust has no constructor concept — n/a.
- **Implementation note:** the place shape is **not** available in TS's `classify(base)` — it is
  in `record_write` (the `place: &Expr`, `mutation.rs:136`) and the assign-target (`Member`
  shape preserved, `mutation.rs:323-338`). So F4 is a `record_write`/place-shape decision made
  *before* delegating to the constructor branch, not a change inside `classify`.

### F5 — write to an imported binding → `global.mutation`
- **Today:** TS classifies a write whose base `imports.resolve(base).is_some()` as
  `global.mutation`/6 (`ts/.../mutation.rs:211`). Python doesn't consult imports → no-emit
  (falls to F1's tail). Rust is **import-blind**: an UPPERCASE imported name may coincidentally
  emit `global` via the proxy, any other imported base drops.
- **Target:** Python and Rust also classify a write whose base resolves through the import table
  as `global.mutation`/6. **TS is the reference.**
- **Concrete API changes:** Python `mutation::detect(unit, span)` →
  `mutation::detect(unit, imports, span)` (`python/.../detect/mod.rs:905,922`); Rust folds the
  import table into the same `mutation::detect` signature change as F2.
- **Rust caveat (accepted asymmetry):** Rust's `ImportTable` resolves `use`-paths (types /
  traits / fns), **not** writable module objects. A Rust write *through* an imported binding is
  almost always to a `static`/`static mut` already caught by **F2**, so F5 is **near-vacuous for
  Rust** — implement it for symmetry, but expect ~zero snapshot impact. The guideline records
  this as an accepted per-language asymmetry, not a behavioral gap.

## 4. Per-frontend alignment plan

| Fix | Rust | TS | Python |
|---|---|---|---|
| F1 | cascade-tail fallback → `hidden.mutation` (was drop, or UPPER→global via proxy) | — *(reference)* | emit `hidden.mutation` in the (empty) `else` branch — **Python gains its first `HiddenMutation`**; needs a `hidden`-aware push (see §4 note) |
| F2 | thread real static set into `mutation::detect`; retire `is_screaming_snake` | n/a | n/a (`global` decl already exact) |
| F3 | add subreason to interior-mut + captured | add subreason to captured (ref-cell already has one) | add subreason to the new captured case |
| F4 | n/a | direct-field-init-only → contained; receiver method/subscript in ctor → `this.mutation`/3 (place-shape decision in `record_write`) | — *(reference)* |
| F5 | fold import table into the same `mutation::detect` signature change (near-vacuous; see §3 F5) | — *(reference)* | thread import table → imported write = `global.mutation` |

> **§4 note — Python `push` cannot emit `hidden` today.** `MutSink::push`
> (`python/.../mutation.rs:462-487`) hardcodes `hidden: false` and `subreason: None`. F1/F3 for
> Python therefore require a `hidden`-aware push (a new parameter or a `push_hidden` sibling) —
> the plan must include this, it is not just a new call site.

**Expected snapshot impact (all intentional, labelled — never "regression"):**
- **Python** — functions mutating captured/module/imported names gain `hidden`/`global`
  effects (were scored pure). First `HiddenMutation` emissions for Python.
- **Rust** — UPPERCASE non-static writes move from `global`/6 to `hidden`/3; real statics now
  caught as `global`/6; some previously dropped unresolved writes now emit `hidden`/3. (F5
  imported-binding → `global`/6 is near-vacuous in Rust — expect ~zero such deltas.)
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

- **Snapshots** — updated for the functions touched by an in-scope fix, each labelled an
  intentional behavioral change. Expect **broad but reviewed churn** for F1: Python today
  no-emits *every* captured/module fallback and Rust drops *every* non-UPPERCASE unresolved
  base, so the fallback fixtures/corpora will move widely — review each diff to confirm it is
  the intended `hidden.mutation`/3, not a misclassification. Outside the fallback, KEEP behavior
  must stay byte-identical (the alignment must not perturb unrelated rankings).
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
2. **Rust patches** — first change `mutation::detect` to receive the static set **and** the
   import table (one signature change, used by F2 and F5); then F2 (real statics, retire proxy)
   + F1 (cascade-tail fallback → hidden) + F5 (imports → global, near-vacuous) + F3
   (subreasons); each its own commit; update Rust snapshots.
3. **Python patches** — add a `hidden`-aware push path (parameter or `push_hidden`); thread the
   import table into `mutation::detect`; then F1 (emit hidden in the `else` branch) + F5
   (imports → global) + F3 (subreason); update Python snapshots.
4. **TS patches** — F4 (constructor: place-shape decision in `record_write`, direct-field-init
   only contained) + F3 (captured subreason); update TS snapshots.
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
