# 007 — React-aware effect detectors in the TS frontend

**Status:** Draft (design; two adversarial review rounds by Claude + Codex; revised twice)
**Issue:** [#19](https://github.com/caasi/fxrank/issues/19)
**Related:** [#25](https://github.com/caasi/fxrank/issues/25) (cross-file resolution — the deferred precision upgrade), #14 (Python receiver-state gradient — shared posture), specs/003 (TS frontend), specs/001 (scoring model + Known Limitations).

> Two review rounds. Round 1 killed the original "own-body + detection-only, for free" thesis.
> Round 2 confirmed the rewrite and refined the core mechanism from a "roll-up re-walk" into
> **score inheritance** from inline hook-callback units. §13 records the full changelog.

---

## 1. Purpose & framing

React is an **effect system implemented without native algebraic effects**: a component is a
*reentrant* computation that `perform`s operations (hooks); the React runtime is the *handler* that
re-invokes it. fxrank's goal is to **measure a component's purity** — does this component hold
effects / untraced hidden state, or has it pushed them up to parents and become a near-pure render
function? — and point an agent toward fxrank's standing refactor: *toward purer cores*, at the
component level.

**The mechanism (corrected over two rounds).** `functions::collect` emits **every nested
arrow/method as its own `FnUnit`**, and the detector walkers stop descent at `visit_arrow_expr`.
So a component's real effects (a `fetch` inside a `useEffect` callback) are scored against an
**anonymous arrow unit**, never the component. fxrank therefore gives a component **score
inheritance** (§4): it absorbs the already-computed scores of the inline arrows it passes to
recognized hooks. This is a **bounded, within-file, single-hop** inheritance — *not* the deferred
general call-graph propagation (`inherited_score` across named calls/files, #25-adjacent).

## 2. Architecture decisions

- **No new crate.** Detectors live in `fxrank-lang-ts`; swc parses JSX/TSX natively.
- **React detection is structural, not import-based.** A file is React when its extension is
  `.jsx`/`.tsx` *or* its body contains JSX syntax (swc-detectable). A **component** is a `FnUnit`
  whose body has **≥1 return path yielding JSX/TSX**, *or* the inner function of a `memo(...)` /
  `forwardRef(...)` wrapper. Fixes type-only-import false positives and the automatic-JSX-runtime
  case.
- **Generic, language-neutral effect kinds + structured `subreason` evidence** (not `ReactX` kinds).
  Reuse `HiddenMutation` for the untraced-state differentiator; add one small generic
  declared-transition kind. A `subreason` (`ref-cell-write`, `useContext-read`, …) keeps a reused
  generic kind legible in the wire output (so a ref cell is distinguishable from a closure capture).
  Shares posture with Python #14.
- **All React/inherited effects are emitted `contained = false`** — they are *not* boundary-
  discountable. (Required: the pipeline's `apply_boundary_discount` would otherwise zero a
  contained class-≤2 effect like `useContext` under a typed signature and invert §10's ordering.)
- **Cross-file resolution deferred to #25.** Milestone-A uses per-file heuristics (option (i)): the
  `useContext(arg)` API contract for context reads. Custom-hook *callback inheritance* needs the
  hook's phase/semantics, which is cross-file → **deferred to #25**; Milestone-A inherits only the
  **built-in hooks** whose phase is known.
- **Performance is out of scope.** Re-render cost / memoization (un-memoized pure components
  re-render more) is a *runtime* concern, not an *effect* concern. A pure component scores ~0
  whether or not it is memoized; fxrank never penalizes missing `memo`/`useMemo`/`useCallback`.
- **Scope split (Q7=a).** Raw DOM/HTML world effects and the `hidden`→`global` module-var refinement
  (spec deferred #3) are base-TS work, out of #19. §10 is demonstrable without them.

## 3. The honesty gradient (low → high), with concrete kinds

| Signal | role | kind / class | notes |
|---|---|---|---|
| pure render (props in, JSX out) | — | — (~0) | the target state |
| `useState`/`useReducer` **declaration** (component holds traced state) | **traced** transition | `StateTransition` (new generic), **class 1** | attributed at the binding; setter calls are corroborating evidence; `contained = false` |
| `useContext(x)` read | contained shared read | reuse `AmbientRead`, **class 2** | flat — no "containment credit"; `contained = false`; `origin: unconfirmed` |
| world effect **inherited** from a `useEffect`/`useLayoutEffect` callback | declared effect (effect-phase) | the effect's own class, **no risk** | honest baseline; *lifting* removes it |
| `useRef().current` **write** (`= …`, `++`, `.current.x = …`) | **untraced** hidden state | reuse `HiddenMutation`, **class 3**, `subreason: ref-cell-write` | the differentiator; above the setter |
| world effect **inherited** from a `useCallback` callback | declared effect (event-phase) | the effect's own class, **no risk** | `useCallback` only memoizes the function ref; the body runs on invocation, not during render — same posture as `useEffect` |
| world effect in the component's **own render body**, or **inherited** from a render-phase hook (`useMemo`/`useState` lazy init) | impure render | the effect's class **+ `EffectInRender` risk** (§6) | the render-path penalty |

Risk channel (participates in `max_class`/`rank_key` — deliberately): existing `html.injection`;
new **`EffectInRender`** (§6).

## 4. Score inheritance from inline hook callbacks (the core mechanism)

A unit that **is a component** inherits the already-computed score (effects + risks) of each
**inline arrow passed directly to a recognized built-in hook** — `useEffect`, `useLayoutEffect`,
`useCallback`, `useMemo`, and the `useState`/`useReducer` lazy initializer — **tagged by the hook's
phase**:

- **effect-phase** (`useEffect`, `useLayoutEffect`, `useCallback`): inherited world effects keep
  their class at **honest baseline** (no `EffectInRender`). Note: `useCallback` is effect-phase
  because React only *memoizes* the function reference; the callback body executes when the handler
  is *invoked* (on an event), never during render. Classifying it as render-phase produces a false
  positive. Caught by dogfooding on real codebases (see §13).
- **render-phase** (`useMemo`, `useState`/`useReducer` lazy initializer): these run
  *during render*, so inherited world effects get **`EffectInRender`** (render-time impurity).

Properties, invariants, and bounds:

- **Inheritance, not re-walk.** The hook-callback arrow is already its own scored `FnUnit`; the
  component absorbs that unit's **raw detected effects/risks** (pre-discount), re-emits them with
  `contained = false`, and recomputes its own `own_score` / `max_class` / `risk_weight` over the
  union. It does **not** copy the arrow's final discounted `own_score` (which may already carry the
  arrow's own boundary coverage). No new selective-descent walker.
- **Once-only attribution (B2, blocking).** An inline arrow that is inherited is **suppressed as a
  standalone hotspot** — its effects are now attributed to the component exactly once. (Only arrows
  inherited this way are suppressed; all other arrows/units are emitted as today.)
- **Single-hop.** Only the *direct* hook-argument arrow is inherited. An effect nested *deeper*
  (e.g. `useEffect(() => setInterval(() => fetch(), 1000))` — the `fetch` is two arrows deep, inside
  a deferred timer callback) stays in its own deeper unit and is **not** inherited. Documented
  limitation with a confidence note; it is genuinely deferred execution.
- **Inline-only.** `useEffect(handleMount, [])` (a named/imported callback) cannot be inherited
  (that is data-flow → #25). `handleMount` is scored as its own unit; documented limitation.
- **Built-in hooks only.** Custom `useFoo(() => …)` callbacks are *not* inherited in Milestone-A
  (unknown phase) → #25. The custom hook's own definition is scored as an ordinary function.
- **Event handlers are never inherited.** A JSX event-handler arrow (`onClick={() => fetch()}`) is
  *not* a hook argument — it stays its own unit (event-time), and is **not** `EffectInRender`.

## 5. Per-signal dispositions

**In scope (Milestone-A core):**
- **`useRef` `.current` writes** → `HiddenMutation` (class 3), `subreason: ref-cell-write`,
  `contained = false`. Requires **ref-binding tracking**: record locals bound to `useRef(...)` and
  check that set **before** the `locals.contains` arm in `classify()` — otherwise `base_ident`
  resolves `ref.current.x = …` to the local `ref` → `LocalMutation` class 1, the opposite of intent.
  Writes only (`.current` reads over-fire on the official "latest ref" pattern).
- **`useState`/`useReducer`** → `StateTransition` (class 1, `contained = false`), attributed at the
  **literal declaration** `const [v, setV] = useState(…)` / `[s, dispatch] = useReducer(…)` in the
  component's own body — the ownership signal (*the component holds traced state*). Setter/dispatch
  call sites are corroborating evidence, **not** the primary trigger; attributing at the declaration
  captures the ubiquitous `onChange={setValue}` / `onChange={(e) => setValue(…)}` case (whose arrow
  is an un-inherited event handler) without needing event-handler inheritance. Alias through a custom
  hook (`useToggle()`) is not recognized — accepted miss.
- **`useEffect`/`useLayoutEffect`** → effect-phase inheritance markers (§4); zero intrinsic cost. An
  effectless one is ignored (rendering-order smell = linter territory).
- **`useMemo`** → the hook runs its factory *during render* to produce the memoized value; an
  effectful factory surfaces as render-phase `EffectInRender` (§4). The hook itself is never scored
  as a cost.
- **`useCallback`** → the hook only *memoizes* the function reference; its body executes on
  invocation (event-time), **not** during render. It is effect-phase (§4): inherited world effects
  keep their honest baseline class with no `EffectInRender`. The hook itself is never scored as a
  cost.
- **`useContext`** → `AmbientRead` (class 2), `subreason: useContext-read`, `contained = false`,
  `origin: unconfirmed`.

**Deferred / cut (documented):** `.current` reads; `use(promise)` (data-flow → #25);
`useSyncExternalStore`, `createPortal` (underspecified — revisit with #25); namespace `React.useX`
calls (member-call shape; documented follow-up); custom-hook callback inheritance (#25).
**Class components** → plain OOP, zero React work (existing `this.mutation`/method machinery); the
class-`setState` claim is **withdrawn** (ordinary method call).

## 6. The `EffectInRender` risk (replaces the rejected Q8 risk)

`RiskKind::EffectInRender` fires for a world effect that executes during render, i.e.:
(a) directly in the **component's own statement body** (descent stops at every nested arrow), or
(b) **inherited from a render-phase hook** callback (§4).
It does **not** fire for effect-phase inherited effects, nor for event-handler arrows (own units).

Justification: unlike the rejected "imperative DOM in declarative component" cleanliness judgment
(which wrongly shared an axis with `transmute`/`html.injection`), an effect during render is a real
correctness/fragility hazard — it re-runs every render and can loop. It belongs on the risk channel
and *should* raise rank. Suggested class **4** (final class set in the plan).

## 7. The exclusion principle

> fxrank scores **world effects** and **untraced hidden state**. A signal that is purely about
> **rendering order / scheduling** and carries **neither a world effect nor a hidden write** is **not
> handled** (linter / rendering-order territory).

Disposes of the *hook recognition* for effectless `useEffect`, `useMemo`/`useCallback` (as hooks),
`useDeferredValue`, `useTransition`, `useId`, and dependency-array correctness. It does **not** drop
effects *inside* those callbacks — those are inherited (§4). `useRef().current` writes are untraced
hidden state → not excluded.

## 8. Detection & confidence

- **React file:** `.jsx`/`.tsx` extension or JSX syntax present. **Component:** `FnUnit` with ≥1
  JSX-returning path, *or* the inner function of `memo(...)` / `forwardRef(...)`.
- **`memo`/`forwardRef` attribution:** recognize the inner function as the component and **report it
  under the outer binding name** (`const C = forwardRef(fn)` → reported as `C`) — forwardRef is
  information hiding; the user reasons about `C` as a native-element-like component. An all-`null`
  component (no JSX path, unwrapped) is a documented miss.
- **Custom hooks / contexts:** `useContext(arg)` by API contract (per-file (i)). Custom-hook
  inheritance and `createContext` confirmation are #25.
- **Tier:** all React signals are `heuristic` with the standard confidence penalty;
  `imports.has_dynamic()` weakens further; single-hop inheritance carries a note for deferred
  nested-arrow effects. No type-checker at any point.

## 9. Out of scope (explicit)

Dependency-array correctness; controlled-vs-uncontrolled classification; **re-render / memoization
performance** (un-memoized pure components); class-component React special-casing; rendering-order
hooks as hooks; `.current` reads; `use(promise)`; `useSyncExternalStore`; `createPortal`; namespace
`React.useX` calls; custom-hook callback inheritance; base-TS raw-DOM world effects and the
`hidden`→`global` refinement; cross-file confirmation (#25).

**Known Limitation (Milestone-A, accepted):** a `useMemo(() => <jsx/>)` arrow that both returns JSX
and is a hook callback is suppressed (the inherited-callback check precedes the component branch), so
its **JSX-returning nature is never evaluated** — it does not receive *component-only* augmentation
(`augment_component`'s own `StateTransition` / `useContext` signals for that arrow). Its world effects
are still absorbed into the owning component and, being render-phase, still earn `EffectInRender` —
only the arrow's own component-level signals are skipped. Deferred to Milestone-B.

## 10. Acceptance sketch (only in-scope demonstrables)

- React files detected by `.tsx`/`.jsx` or JSX syntax; detectors in `fxrank-lang-ts`; no new crate.
- A component's `useRef().current` **write** surfaces as `HiddenMutation` (class 3), ranking **above**
  a `useState` **setter** (`StateTransition`, class 1).
- A `fetch` **directly in a component render body** carries `EffectInRender`; the **same `fetch`
  inside a `useEffect` or `useCallback` callback** is inherited at honest baseline (no risk); a
  `fetch` inside a `useMemo` callback carries `EffectInRender`. (All demonstrable without DOM.) The
  inherited callback arrow does **not** appear as a separate duplicate hotspot.
- **Lifting:** a child taking `value`/`onChange` props scores ~0; the parent declaring `useState`
  absorbs the `StateTransition`.
- A component wrapped in `memo`/`forwardRef` is reported under its outer binding name.
- `useContext` read (class 2) ranks **below** a captured-binding mutation (class 3); it is *not*
  zeroed under a typed signature (because it is `contained = false`).
- Non-React TS/JS output is unchanged when no JSX is present. Existing tests pass.

## 11. Named tasks for the implementation plan

- `rollup`/inheritance module: detect components; collect built-in hook calls; absorb the inline
  hook-argument arrow's scored unit; tag by phase; **suppress** the absorbed arrow's standalone unit.
- Ref-binding tracking in `mutation.rs`, ordered **before** the `locals.contains` arm.
- `StateTransition` kind (new generic, class 1) and `EffectInRender` risk (new, proposed class 4) in
  `fxrank-core`; the `subreason` evidence field/schema.
- Fixtures: ref-cell write vs latest-ref read (cut); render-body vs useEffect vs useMemo `fetch`;
  lifting; `memo`/`forwardRef` outer-name attribution; all-`null` miss; single-hop nested-arrow miss;
  setter-call vs setter-as-prop.

## 12. Risks & open questions

- **Component heuristic edges:** conditional/early returns (rule: ≥1 JSX path), HOCs returning a
  component (the outer fn is correctly *not* a component), `memo`/`forwardRef` (handled), all-`null`
  (documented miss).
- **`StateTransition` over-fire:** every `onChange` calls a setter; class 1 (weight 1) and
  `own_score`'s 0.5× damping keep N setters from out-ranking one class-7 IO — verify on a dogfood
  fixture.
- **Single-hop misses** deeply-nested deferred effects (timer/promise callbacks) — by design;
  confidence-noted.

## 13. Changelog (two adversarial review rounds)

**Round 1** killed the "own-body + detection-only, no new machinery, for free" thesis (per-unit
collection scatters a component's effects across anonymous callback units); replaced import-gating
with structural JSX/extension detection; defined "React component"; removed the Q8 "imperative DOM
in component" risk (it conflated cleanliness with XSS and mis-ranked) in favor of `EffectInRender`;
corrected `useRef().current` (ref-binding tracking, writes-only); cut `use(promise)` /
`useSyncExternalStore` / `createPortal` / `.current` reads / namespace calls; withdrew class-
`setState`; kept `useContext` (flat class 2); assigned concrete kinds; added `subreason`; rewrote
acceptance to drop out-of-scope DOM dependencies.

**Round 2** reframed the roll-up as **score inheritance** from inline hook-callback units
(single-hop, by phase) — dissolving the selective-descent-walker concern; made the three blocking
fixes: `contained = false` for all React effects (else the boundary discount zeroes `useContext`),
the `EffectInRender`-only-on-render-path invariant (else inline `onClick` is mislabeled), and
once-only attribution via suppressing the inherited arrow unit; split hooks into effect-phase vs
render-phase (`useMemo`/`useCallback`/lazy-init effects → `EffectInRender`); supported
`memo`/`forwardRef` with outer-name attribution; declared re-render/memoization performance
explicitly out of scope; restricted `StateTransition` to literal `useState`/`useReducer` call sites;
dropped the now-dead `import type` filter (detection no longer reads imports).

**Note (post-Round 2 correction, caught by dogfooding):** Round 2 placed `useCallback` in the
render-phase alongside `useMemo` — this was a false positive. The distinction is semantic:
`useMemo(() => compute())` calls the factory during render to produce a value; `useCallback(() =>
handler())` only *memoizes* the function reference, whose body executes on invocation (event-time),
exactly like a plain `onClick` handler. Dogfooding on three real codebases surfaced
`fetch`/`setState` calls inside `useCallback` bodies wrongly receiving `EffectInRender`. Fixed:
`useCallback` moved to effect-phase (honest baseline, no `EffectInRender`). Only `useMemo` and lazy
initializers remain render-phase. The spec has been corrected in §3 gradient table, §4 phase rules,
§5 per-signal dispositions, and §10 acceptance sketch.

**Round 3 (convergence)** moved `StateTransition` attribution to the literal `useState`/`useReducer`
**declaration** (the ownership signal), so the ubiquitous `onChange={setValue}` case is captured
without event-handler inheritance; and specified that inheritance absorbs the callback's **raw**
detected effects (re-emitted `contained = false` and recomputed), not its final discounted
`own_score`. Codex confirmed plan-ready after these.
