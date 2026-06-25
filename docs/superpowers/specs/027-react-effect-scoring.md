# Spec 027 — React effect-scoring model (definition-site attribution + CPS containment)

**Status:** draft v2 (2026-06-25, review-looped: Codex doc-clean + opus vs-code; C1 floor-rule/spec-003 reconciliation + 7 more fixed) · **Issue:** #37 (umbrella; absorbs the original "3f fold consolidation") · **Companion:** Spec 028 (cross-language scoring-table rebaseline — logging↓, time/random=world; separate because cross-language) · **Related:** #46 (deterministic output ordering). **Release-blocking** — the current React scoring is wrong; shipping it is not meaningful.

**Precedence:** spec 001 governs base scoring/classes; spec 003 the containment discount; spec 025 the shared cross-file fold; spec 008 mutation classification. This spec defines the **TS-frontend React attribution layer** on top of them. `fxrank-core` stays language-neutral and parser-free — this spec adds **no React concept to core**. Code-vs-spec disagreements resolve to the spec.

## 1. Problem & evidence

fxrank scores a function by the effects in its body. React breaks this: a component's *real* lifecycle effects live in hooks and handlers it defines, not in its render-return expression. The current bespoke "React two-pass" only absorbs an **allowlist** of inline hook-callback arrows (`useEffect/useMemo/useCallback/useLayoutEffect/useState`), single-hop, into the component's own-body.

**Dogfood (omni/114-kg-frontend, 277 components):**
- **84% score own=0** (effect-blind) while **55 IO-bearing handlers float as orphan hotspots** — named handlers, inline `onClick` arrows, `useMutation`/`useQuery`, `return null` components, and nested callbacks all escape attribution.
- **Over-counts:** components ranked class 4 purely by env-guarded `console.*`; a form input at class 5 for `new Date()` in a callback.

The model is wrong in both directions. This spec replaces it.

## 2. The model — a component as CPS

View a component as `(props, context) → (JSX, effects)`: `value`-props are inputs, `onXxx`/JSX are outputs. Four principles:

### 2.1 Definition-site attribution (no allowlist)
A component **owns every effect lexically defined within its function body** — the render body, **all** hooks (including `useMutation`/`useQuery`/custom hooks — not an allowlist), and **all** handlers it defines (inline event arrows like `onClick={() => …}`, named local handlers, nested callbacks at **any** depth), **including `return null` components** — **minus** effects it hands across the component boundary (§2.3). The mechanism flips from "enumerate hooks to absorb" to "**default-own the lexical subtree, subtract what's handed out**".

### 2.2 CPS containment discount
An effect that touches only the component's **own internal** state/ref/memo (`useState` set/read, a `useRef().current` used as private storage, a pure `useMemo`) is **contained** → discounts low (class ~1, the React analog of Rust `&mut self`). An effect that **reaches the world** (`fetch`/net/fs/db, global/module mutation, the real DOM outside React's managed tree, **time, random**) is **escaping** → full class. The discount always carries a recorded rationale.

> **Implementation note — `ref-cell-write` is conservatively escaping.** Private-ref containment is the *intent* above (a `useRef().current` used purely as private storage should be contained), but the current implementation cannot distinguish private storage from a `ref` that is DOM-attached or forwarded to a consumer (`ref={domNode}`, `forwardRef`, passed to a child) **without ref-forwarding-chain analysis** (deferred — see §6, "ref-forwarding chains"). So a `ref-cell-write` (`HiddenMutation`, `subreason: "ref-cell-write"`) is classified **escaping** by default — the conservative choice (under-discount, never a false purity). This is a deliberate decision, not a bug; the `adopted_ref_cell_write_is_escaping_conservative` test pins it.

### 2.3 Cross-boundary = consumer's responsibility (onXxx and ref, one rule)
Control/cells handed across the component boundary are the **consumer's** responsibility, not the definer's-callee's:
- **Calling a *received* `onXxx`/prop/param callback → NOT charged.** The effect belongs to whoever defined and passed it.
- **Defining a handler with effects and passing it down → charged to the PASSER** (definition site).
- **A `ref` handed to a consumer** (`ref={domNode}`, passed to a child, `forwardRef`) → the consumer owns its use; the creator isn't charged for the consumer's use. Using `.current` to touch the **real DOM** is escaping, charged where that code is written; a `ref` used purely as private storage is contained (§2.2).

### 2.4 Phase = certainty-graded discount, recorded
*Whether* an effect counts never depends on phase; *how much* does:
- **render phase** (runs every render) → full weight **+ an `effect.in.render` risk** for world effects (wrong place to do IO).
- **mount/effect phase** (`useEffect`/`useLayoutEffect`, runs on mount+update) → ≈ full.
- **event phase** (handlers, conditional on interaction) → a **small conditionality discount with a recorded rationale** (e.g. `subreason: "phase:event — conditional on interaction"`).

**Conditionality is a SEPARATE axis from containment (escaping), and this discount is a NEW TS mechanism — it does NOT reuse and must NOT contradict spec 003.** Spec 003's anti-Goodhart invariant ("a *world* effect like `fetch` is observable regardless of typing, and is **never discounted by containment**") stays intact: the containment discount still never touches an escaping effect. The conditionality discount is orthogonal — it reflects *will this run* (always vs maybe), not *does this escape*. It is **not** a Goodhart hole because it rewards a **genuine** improvement (deferring IO out of render really is better), not a syntactic trick. Rules:
- **Mechanism:** the TS frontend writes `discounted_to` / `discount` / `subreason` **directly** on the effect (NOT via `apply_discount`, which is Rust-mutation-only, nor `apply_boundary_discount`, which refuses escaping effects). `effective_class()` honors `discounted_to` everywhere, including the fold.
- **Capped & floored:** at most a **1-class** downshift, floored at **class 1** — a conditional world effect can be nudged down one notch but never erased.
- **Recorded:** always carries `discount`/`subreason` so the user sees why.
- This spec **amends spec 003's wording** to scope "never discounted" to the *containment/typing* axis, leaving the orthogonal conditionality axis to this spec.

## 3. Architecture — core stays neutral, TS materializes

`fxrank-core` already carries the exact neutral primitives this needs — **no new core React concept, and ideally no new core field**:

| core field (`Effect`) | role here | serialized? |
|---|---|---|
| `contained: bool` | §2.2 — drives `escapes()` (= `!contained`) so the **shared fold** keeps contained in `own` and propagates only escaping; also gates the spec-003 containment discount (contained-only, floor 0) | **internal** (`#[serde(skip)]`) |
| `discounted_to: Option<u8>` | post-discount effective class (`effective_class()` honors it everywhere incl. the fold) — the channel the §2.4 conditionality discount writes | yes |
| `discount: Option<String>` | discount rationale | yes |
| `subreason: Option<String>` | classification/discount subreason (e.g. `phase:event`, `ref-cell-private`) | yes |
| `confidence: f64` | lowered for heuristic provenance / unknown-hook calls — affects the **function-level** min only | **internal** (`#[serde(skip)]`); surfaces as `hotspots[].confidence` (the min) |
| `RiskKind::EffectInRender` | §2.4 render-phase risk (already exists) | yes (risk) |

**The TS frontend** computes phase, hook semantics, JSX, component identity, and provenance, and **materializes** them into these neutral fields **before** records enter the fold. Core never learns what a hook, phase, or component is. **No new core field is required.** Two distinct, orthogonal discount axes apply: (1) spec-003 **containment** discount — `apply_boundary_discount`, contained-only, floor 0, never touches an escaping effect; (2) this spec's **conditionality** discount (§2.4) — a new TS-side write of `discounted_to`/`discount`/`subreason`, 1-class cap, floor 1, for event-phase effects. (Note: `apply_discount` in `score.rs` is the **Rust** mutation-channel discount keyed on `&mut`/`&self` — it has no React analog and is NOT used here.)

## 4. Mechanisms (TS frontend)

### 4.1 Component recognizer (beyond `returns_jsx`)
Today component detection is `returns_jsx`, which misses `return null` components. Recognize a component as a **PascalCase function (hard gate)** that ALSO shows at least one of these signals: returns JSX (strong → confidence 1.0); is in a `.tsx`/`.jsx`-dialect file (weak); calls a React hook (weak, bare-ident `use[A-Z]…`). Two-or-more weak signals → 1.0; exactly one weak → 0.8. TS-only heuristic, carries `confidence` (lower when only one weak signal holds). *Shipped subset:* the implementation recognizes the above; the additional signals "used as a JSX element" and "default export of a component file" are **deferred** (the shipped signals already catch the dogfood cases incl. `return null` via PascalCase+hooks). Non-components keep today's per-function scoring. **Recognition does NOT change the `<module>` synthetic unit's scope** (`module_init_unit` still scores only top-level statements): a hook-calling helper or a default-exported component must not have its effects counted both in the component AND in `<module>` — re-parenting (§4.2) assigns each effect exactly one owner, and a component's body effects never belong to `<module>`.

### 4.2 Lexical ownership via re-parenting (NOT copying)
A component **adopts** its owned nested units: each adopted handler/arrow is **suppressed as a standalone hotspot** and its effects + outgoing refs are **moved** to the component's record (re-parented), so an effect appears on exactly one owner — **no double-count**. A function that is **not** owned (handed out / escaped, §4.3) stays a standalone callable unit and is reached via a **graph edge**, never copied. The ownership-assignment pass is **tree-aware (any depth)**: it walks the full lexical-ownership tree (component → nested fn → nested-nested handler), unlike today's `inherited_callbacks`, which is **single-hop** (`functions::collect` already yields all nested units as a flat list, so depth is reachable — the pass just must traverse it, not stop at one hop). (Contrast: today only allowlisted inline hook arrows are suppressed; nested/named/onClick handlers stay independent → the orphan-hotspot problem.)

### 4.3 Function-value provenance lattice
Resolve, per function value the component references, one of:
- **`OwnedImmediate`** — defined by the component and known to run as part of component-owned evaluation (render body, render-phase hook callback). Effects adopted (§4.2), render-phase weighting.
- **`OwnedDeferred`** — defined by the component, invoked later by React/an event (event handlers, effect-phase callbacks, callbacks to **unknown** hooks). **Still charged to the component**, with the event/conditional discount (§2.4) and — for unknown hooks — lowered `confidence` + an "unknown callback schedule" rationale.
- **`EscapedValue`** — returned/exported, stored in module/global/context/ref, or passed as a call argument. **Not** adopted; left standalone + graph edge. *(Implementation note: the shipped escape rule is conservative — a value passed as a call **argument to any callee** (not only an opaque/unknown one) is treated as escaped, since the callee owns its invocation and we don't trace it interprocedurally. This under-attributes rather than over-attributes — the safe direction — and is broader than "unknown callee only". Hook-shaped calls are handled separately as ownership, §4.5/§6.)*
- **`ReceivedValue`** — a param/prop/destructured-prop callback. **Invoking it is not charged** (§2.3).

A **local provenance pass** classifies each binding: function params / destructured props → `Received`; ES imports → `Imported`; local `function`/arrow decls → `LocalDefined`; simple aliases/destructuring carry provenance through; unknown spreads/computed access **downgrade confidence** rather than guessing. Then: invoking a `Received` value is not charged; invoking `Imported`/`LocalDefined` is charged/resolved/propagated as normal.

**Lattice precedence (fixed order)** — a value can match more than one class (e.g. a received prop that is then passed onward is both `Received` and "passed to a callee"): check **`ReceivedValue` first** (origin wins — a received callback is never charged here regardless of where it then flows), then `EscapedValue`, then `OwnedImmediate`/`OwnedDeferred`. Origin (where the value came from) dominates destination (where it goes).

### 4.4 Containment classifier (React)
Before adoption, classify each adopted effect `contained` vs escaping per §2.2: `useState` setters/reads, `nonlocal`-free internal `ref.current` private storage, pure `useMemo` → **contained**; `fetch`/net/fs/db, global/module mutation, real-DOM access, `time`/`random`, a ref forwarded/attached to a DOM node then used → **escaping**. This **fixes the current code's loss of containment**: today `raw_signals` drops the `(Effect, contained)` tuple, `absorb_inherited` forces inherited effects non-contained (and `discounted_to = None`), and `augment_component` hardcodes `contained: false` — all must preserve/assign the real `contained` flag. **Ripple:** `record_from_hotspot` copies `h.effects` verbatim into the record, so the corrected `contained` flags flow into the fold (the desired path). The plan must sequence §4.4 (and §4.2) **before** re-capturing the omni dogfood baseline, since every existing React fixture's own-body output changes.

### 4.5 JSX-prop & hook-arg function-value edges
Passing a function **value** (not calling it) — `<Button onClick={handleClick}>`, `useX(cb)` — is invisible to today's call-only `refs::extract` (it visits `visit_call_expr` only and stops at `visit_arrow_expr`/`visit_function`). This needs a **new walker** (JSX-attribute + call-argument visitor that distinguishes a function *value* from a *call* and routes by provenance §4.3) — a substantial addition, not a tweak; the plan budgets it as its own task. It extracts:
- an **inline** arrow/handler with effects → owned (§4.2), per its provenance class.
- a **`LocalDefined`** named handler passed as a prop → owned by the component (it defined it).
- an **`Imported`** handler passed as a prop → **not copied**; a graph edge so its effects **propagate** (the imported definition owns them); first-party resolves, third-party stays opaque (class-2 external).

### 4.6 Fold integration (delivers the "3f" consolidation)
With effects correctly tagged `contained`/escaping and re-parented, the **existing shared fold** does the rest: contained effects stay in `own` and don't propagate; escaping effects propagate to callers; the spec-003 discount and the §2.4 phase discount are already materialized on the effects. The bespoke own-body absorption is replaced by re-parenting + the shared fold → **TS carries one fold implementation** (closes the original 3f goal as a side effect).

## 5. Invariants & expected output changes

- **Core neutrality preserved** — no React/JS/phase concept in `fxrank-core` (compiler-enforced: no parser dep; this spec adds none).
- **Never-guess preserved** — provenance/ownership resolve only from syntax + the import table; unknowns **downgrade confidence**, never fabricate ownership or a target.
- **No double-count** — re-parenting gives each effect exactly one owner (§4.2).
- **Output CHANGES (intended, not byte-stable):** the 84%-own=0 components gain real scores; orphan IO handlers fold into their components; internal-only callbacks discount down; event-phase effects discount with rationale. A **dedicated fixture suite** (not whole-report byte-equality) asserts each principle.
- **027-alone vs 028 — what each moves (don't assert 028 behavior under 027):** 027 ALONE moves the **attribution** metrics — own=0 component count ↓ sharply, orphan-handler count ↓, `propagated ≥ own` holds. The **over-count class reductions** in §1's evidence (`console.*` at class 4, `new Date()` at class 5) are NOT fixable by 027 — `world_effect` still includes `Logging`/`TimeRead`/`Random` at their current classes; those need **spec 028**. 027's fixtures must assert attribution + containment + conditionality, NOT the logging/time class numbers (028's job).

## 6. Known limits / deferred (honest)

- **Type-info-dependent cases** stay heuristic (no type checker): is `obj.fn` a received prop or an import? — provenance from syntax + import table, confidence-tagged. HOCs, ref-forwarding chains, context-provided callbacks → lower confidence.
- **Unknown 3rd-party hook phase** — ownership is certain (`OwnedDeferred`), but the *phase* (render vs effect vs event) is guessed via a small semantics table; unknown → deferred + rationale.
- **Object-literal hook-arg callbacks (`useMutation({ mutationFn })`) — RESOLVED (Task 9).** A function value the component defines and hands to a **hook-shaped call** (`use[A-Z]…`) is `OwnedDeferred` whether it's a direct arg OR a property of an object-literal arg — a structural rule, NO library allowlist (the score is side-effect *risk*, which a `mutationFn` doing `fetch` genuinely carries for the component). Non-function properties are skipped; `Received`/`Imported` properties route per provenance (not-charged / edge). A callback handed to a **non-hook** unknown callee stays `EscapedValue` (the T3 escape rule) — the hook-vs-non-hook boundary is the discriminator. *Still deferred:* callbacks nested deeper (objects-in-objects, arrays of callbacks) beyond the top-level object-arg properties.
- **Aliased React built-ins (deferred, bounded).** Built-in hooks are recognized by exact name (`react::is_builtin_hook`, the full React 16.8/18/19 set), so an *aliased* import — `import { useState as x } from 'react'; x({…})` — reads as a custom hook and its state-data object arg may be descended → a **low-confidence false positive** (custom-hook path ⇒ `Unknown` phase ⇒ 0.8 confidence + conditionality discount, never a full-confidence hotspot). Near-theoretical (React built-ins are essentially never aliased); a real fix needs import-provenance in the hook-classification path. Deferred; the low-confidence penalty bounds the damage ("signal, not gospel").
- **Syntactic-equivalence bound:** inline-local and named-local handlers inside a component should score **identically**; imported/extracted handlers score **through propagation** (may differ from inlined by the cross-file path); factory/unknown carries uncertainty. The spec accepts and documents this bound rather than claiming perfect equivalence.

## 7. Out of scope

- **Spec 028** — the cross-language scoring-table rebaseline (logging↓, time/random=world). Separate spec, lands in the **same PR** as this.
- **#46** — deterministic `external_reaches[]`/`inherited[]` ordering.
- Rust/Python frontends, the `Frontend` trait signature, and the core fold algorithm itself are unchanged.

## 8. Decomposition (plan to follow via writing-plans)

A paired plan (`plans/027-react-effect-scoring.md`) will sequence, roughly: (1) component recognizer; (2) local provenance pass + lattice; (3) re-parenting/adoption transform replacing `absorb_inherited`; (4) React containment classifier (preserve the `contained` tuple); (5) JSX-prop/hook-arg function-value edges; (6) phase-as-discount materialization (+ floor rule); (7) fixtures + omni dogfood. Large, TS-frontend-only. Reviewed via review-loop before execution.
