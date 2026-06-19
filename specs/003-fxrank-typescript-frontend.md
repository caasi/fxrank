# 003 — TypeScript / JavaScript Frontend

## Goal

Add a JS/TS language frontend so `fxrank scan` can profile the **own-body effect
cost** of TypeScript and JavaScript functions, alongside the existing Rust frontend.

The differentiator carries over from Rust and sharpens into a thesis: **types lower
the score by bounding the function's space of variation.** Untyped JS is structurally
the same situation as Rust's `&self` + interior mutability — every value is a shared
mutable reference, nothing is declared, so every effect is *hidden*. TypeScript's
explicit annotations (a fully-typed in/out boundary, `readonly`, `as const`) are the
closest thing JS has to Rust's ownership declarations: they make some effects
*declared and bounded*, so the same effect scores *lower*. Conversely `any` is the
JS analog of `unsafe` — the escape hatch that re-opens the space of variation; it
*cancels* discounts and is itself a risk flag.

Like Rust (Milestone A), this frontend is **primarily syntactic**: it parses with
[`swc`](https://github.com/swc-project/swc) and runs **no type checker** (`tsc` is
never invoked). Type-dependent signals are heuristic and carry a confidence penalty.
A direct, load-bearing consequence: **only explicitly-written annotations are
visible** to a syntactic instrument — inferred types are invisible. This is not a
limitation to apologize for; it is the honest boundary of the instrument, and it
means FxRank rewards type-first / FP style and is blind to (does not credit)
inference-stripped style. See *The boundary-containment discount*.

## Scope

In scope (Milestone A, JS/TS):

- A new `fxrank-lang-ts` crate (feature `ts`), mirroring `fxrank-lang-rust`'s
  structure, parsing `.ts` / `.tsx` / `.js` / `.jsx` / `.mjs` / `.cjs`.
- The full effect/risk vocabulary mapping below (*Effect vocabulary*, *Risk
  vocabulary*).
- The **boundary-containment discount** in `fxrank-core`: a graduated, coverage-based
  containment discount keyed on how much of a function's signature is explicitly typed
  (*The boundary-containment discount*). This is a language-neutral scoring addition;
  Rust does not invoke it this milestone (see *Deferred*).
- Syntactic **escape analysis** for mutation (local / param / `this` / closure-capture
  / module-global), since "contained vs escaping" is what the discount turns on.
- **Fragment analysis as a first-class entry point**: scanning a code snippet from
  stdin (`fxrank scan --lang {ts,tsx,js,jsx} -`), not only files/dirs. Agents split
  work into fragments; the tool must score a single function in isolation.
- Reuse the existing wire format unchanged except for the new effect/risk **kind**
  strings; `async` rides the existing `async_boundary` / `await_count` fields.

Out of scope (deferred — see *Deferred / Future work* for the full list):

- Any type resolution / `tsc` integration / borrow-style aliasing proof.
- JSDoc comment types (`/** @type {…} */`): treated as **untyped**. Comment-embedded
  types are not AST type nodes; we do not parse comment strings.
- Call-graph propagation / `inherited_score` (already deferred in 001). The symmetric
  payoff of `readonly` *on parameters passed to callees* cashes out here and stays
  deferred.
- The full DOM / browser effect catalog (only the obvious sinks are caught now).
- Revisiting the Rust frontend's `unsafe`-cancels-discount rule under the
  boundary-containment principle.

## The core thesis: types bound the space of variation

The containment discount in 001 rewards effects the Rust type system makes *declared
and bounded* (`&mut`, ownership). The unifying principle behind it — made explicit
here — is:

> A type narrows the set of possible behaviors. A bounded behavior set is a lower
> effect cost. **`any` re-opens the set (possibility explosion); a fully-typed
> boundary closes it.**

This gives a principled test for *which* TS constructs earn a discount: any construct
that **provably shrinks the set of reachable effects**, observed *syntactically*.

### Types contain *state*, not *world*

The decisive sharpening. A type-sound in/out boundary makes a function an opaque
typed transformation: given typed input it yields typed output, and the *observable*
behavior space is bounded by the types. But this only holds for effects whose
footprint stays **inside** the boundary:

- **Boundary-containable (state / memory effects):** local mutation, and other
  mutations that do **not escape** the function. A sound boundary genuinely bounds
  these — they are observationally part of computing the typed result (the `ST`-monad
  intuition: arbitrary local mutation behind a pure typed signature is observationally
  pure).
- **Boundary-escaping (world effects):** `net.fs.db`, `time.read`, `random`,
  `env.read` / `env.write`, `logging`, `process.control`, `concurrency`. These are
  observable to the world regardless of how the signature is typed — a `fetch` is seen
  by everyone no matter how pretty the types. **Types never contain world effects.**
- **Escaping state effects:** mutation of a parameter (the caller's object), a
  captured closure variable, or a module-global. The in/out types do **not** bound
  these — they leak past the boundary — so they are **not** discounted.

This line is non-negotiable: discounting *all* interior mutation behind a typed
boundary would re-hide exactly the hidden mutation this tool exists to surface (001's
anti-Goodhart property). **A typed boundary discounts only interior effects that are
syntactically proven not to escape.** The boundary is the *gate*; escape analysis is
the *discriminator*.

```ts
// (1) typed boundary, effect stays internal → contained → discounts toward 0
function parseConfig(raw: string): Config {
  const acc = {} as Config;
  for (…) { acc.x = …; acc.items.push(…); }   // local.mutation, never escapes
  return acc;
}
// (2) identical-looking boundary, but writes module state → escapes → NOT discounted
function parseConfig(raw: string): Config {
  globalCache.push(raw);                       // hidden/global mutation, world sees it
  …
}
// (3) typed boundary, but does IO → world effect → keeps full class
async function parseConfig(raw: string): Promise<Config> {
  await fetch(…);                              // escapes any boundary
}
```

## Effect vocabulary

Reuse 001's `EffectKind` wherever the semantics match; add **one** new kind
(`this.mutation`). Wire strings are produced by `EffectKind::wire()` — never
hand-written at call sites.

### World effects (escape any boundary; never discounted by typing)

| kind | class | JS/TS signal | tier |
| --- | --- | --- | --- |
| `net.fs.db` | 7 | `fetch`, `XMLHttpRequest`, `WebSocket`, `EventSource`, `navigator.sendBeacon`; node `fs` / `fs/promises`; `localStorage` / `sessionStorage` / `indexedDB`; DB client `.query` / `.execute` | path / heuristic |
| `process.control` | 6 | `process.exit` / `kill` / `abort`; `child_process` exec/spawn/fork | path |
| `env.write` | 6 | `process.env.X = …`; `Deno.env.set` | heuristic |
| `concurrency` | 6 | `new Worker`, `worker_threads`, `postMessage`, `SharedArrayBuffer` + `Atomics.*` | heuristic |
| `time.read` | 5 | `Date.now()`, `new Date()` (no args), `performance.now()` | heuristic |
| `random` | 5 | `Math.random()`, `crypto.getRandomValues` / `randomUUID` / `randomBytes` | path |
| `env.read` | 4 | `process.env.X` (read), `import.meta.env`, `process.argv` / `platform` | heuristic |
| `logging` | 4 | `console.*`, `process.stdout` / `stderr.write` | path |
| `panic` | 4 | `throw` statements; `node:assert` | exact |

### State effects (boundary-containable)

| kind | class | JS/TS signal | escapes? |
| --- | --- | --- | --- |
| `local.mutation` | 1 | assign / `++` / `delete` / array mutators (`push`/`splice`/`sort`/…) / `Map.set` / `Set.add` / `Object.assign` on a **locally-created** binding | no → contained, discounts to **0** |
| `param.mutation` | 3 | same mutators targeting a **parameter** | yes → not discounted, **not** flagged `hidden` |
| `this.mutation` (new) | 3 | `this.x = …` in a **non-constructor** method | yes → declared (not hidden), not discounted |
| `hidden.mutation` | 3 (flag) | mutation through a **captured closure variable** or **imported binding** | yes → the Goodhart case, never discounted |
| `global.mutation` | 6 | `globalThis` / `window.x = …`, mutation of a module-level export | yes → escapes by definition |
| `ambient.read` | 2 | reads of `window.location` / `document` / `navigator`, module-level `let`, global config | — |

`this.mutation` is the honest `&mut self` analog: the class declares the field, so it
is *declared* (not `hidden`) — but a method mutates an already-shared instance, so it
**escapes** and is not boundary-discounted. **Constructor** field initialization is
classified as `local.mutation` (it builds the value that is returned — contained), so
it is boundary-discountable.

## Risk vocabulary

New `RiskKind`s (the danger channel, separate from effect cost). Wire via
`RiskKind::wire()`.

| kind | class | signal |
| --- | --- | --- |
| `type.escape` | 3 | `any` (explicit), `as any`, `as unknown as T`, `@ts-ignore`, `@ts-expect-error`, non-null `!` |
| `dynamic.code` | 7 | `eval`, `new Function(…)`, `with` (arbitrary code execution — the real "unsafe" of JS) |
| `proto.pollution` | 4 | `__proto__` assignment, `Object.setPrototypeOf` |
| `html.injection` | 5 | `innerHTML` / `outerHTML` / `insertAdjacentHTML` / `document.write` |

`type.escape` is deliberately **gentle** (class 3, weight 3): it must be *visible*
without shadowing a real IO effect (class 7, weight 21). It is the `any ≈ unsafe`
flag — surfaced because `any` is where an agent should look (the space of variation is
out of control), but not loud enough to dominate the ranking.

## The boundary-containment discount

A **language-neutral** addition to `fxrank-core`'s scoring model, parallel to the
existing containment discount (`apply_discount` / `Discount`). It is a class
down-shift (never point subtraction), applied **only to contained (non-escaping)
state effects**, with depth keyed on how much of the signature is explicitly typed.

### Signature coverage

Let a function's signature have `S` **slots** = (one per parameter) + (one return
slot). For a constructor there is no return slot (`S` = parameter count). A slot is
**typed** iff it carries an *explicit* annotation whose top-level type is not the
`any` keyword. Inferred / omitted annotations are **not** typed (invisible to a
syntactic instrument). Let `t` = number of typed slots; coverage `c = t / S`.

### The `any` poison rule

If **any** `any`-family token appears in the function — in a signature slot *or* the
body (`any`, `as any`, `as unknown as`, `@ts-ignore`, `@ts-expect-error`) — the
boundary-containment discount is **voided entirely** (shift 0) and a `type.escape`
risk is emitted. `any` re-opens the space of variation; a boundary that casts its way
out cannot be trusted to contain anything. (Non-null `!` is a `type.escape` risk but
is **not** a discount-voider — it asserts non-null within otherwise-typed flow; it
does not turn anything into `any`. `unknown` is the *safe* top type and never voids;
generics, unions, optionals are fully typed.)

### Discount depth (graduated)

When the function is `any`-free, the class shift applied to each **contained** effect:

| coverage | tier | shift |
| --- | --- | --- |
| `c = 1` (whole boundary explicit) | Full | down 2 |
| `0 < c < 1` (partial — e.g. params typed, return inferred) | Partial | down 1 |
| `c = 0` (all inferred / unannotated) | None | down 0 |

The graduation honors "some typing beats none": every pinned slot removes a dimension
of variation. The return slot may be weighted above parameters in a later milestone
(see *Deferred*).

### Floor 0 for contained effects

001's `apply_discount` floors observable effects at **class 1** ("an externally
observable effect never discounts below class 1"). A **contained** effect is by
definition *not* observable, so the boundary-containment discount floors at **class 0**
(weight 0 — genuinely free). Consequences:

- `local.mutation` (class 1): `c = 0` → stays 1; any `c > 0` → 0 (free). A single
  typed slot is enough to confirm the boundary contains it.
- constructor `this`-init classified as `local.mutation`: same as above.
- A general-method `this.mutation` / `param.mutation` / `hidden.mutation` (class 3) is
  **escaping**, so it receives **no** boundary shift regardless of coverage — only the
  world-vs-state and existing-discount machinery apply.

**The graduated depth is latent in this milestone.** Every contained effect in JS/TS
Milestone A is `local.mutation` (class 1), and class 1 with a floor of 0 saturates at
the first step: both **Partial** (down 1 → 0) and **Full** (down 2 → 0) coverage floor
it to class 0, so they are **observationally identical here** — only `c = 0` vs `c > 0`
is distinguishable. The Partial-vs-Full distinction becomes visible only on a contained
effect of **class ≥ 2**, of which this milestone has none. The graduated model is kept
in `fxrank-core` (language-neutral) for future contained effects and for the deferred
Rust application; JS/TS Milestone A simply does not exercise the second step. (So "some
typing beats none" — the requirement that drove graduation — is fully delivered; the
finer depth is reserved, not removed.)

This is what makes "I don't care how much mutation is inside, as long as the boundary
types are right" *measurable*: contained interior mutation goes to 0 under a typed
boundary, while escaping mutation and world effects stay fully visible.

## Architecture (mirrors `fxrank-lang-rust`)

- **New crate `fxrank-lang-ts`**, behind feature `ts`. It **depends on no parser type
  in `fxrank-core`** — `swc` must never leak into core, exactly as `syn` must not.
  The compiler enforces this.
- **Modules mirror the Rust frontend:**
  - `functions` — collect `FnUnit`s from the swc AST. JS function forms collected as
    units: function declarations, function expressions, arrow functions, class
    methods, getters/setters, object methods, generators (and their `async`
    variants). Anonymous arrows get a synthesized symbol `<arrow@L{line}>`. The
    own-body model holds: **nested functions are their own units** (no call-graph
    roll-up; deferred). Trivially-pure callbacks score 0 and fall off via
    `Report::build`'s limit, so collecting every form does not flood the output.
  - `imports` — an ES `import` + `require` table (the swc analog of the Rust `use`
    table) for call resolution / tier.
  - `detect/{calls, mutation, risk}` (+ a coverage helper) — `syn::visit`-style
    walkers over the swc AST via `swc_ecma_visit::Visit`, each following the
    `classify_* → push` shape and always calling the default `visit_*` so nested
    expressions are still visited.
  - `detect::analyze_unit` — **the single owner** of turning a function's effects /
    risks / signature-coverage into a scored `Hotspot`. Detectors stay pure (return
    `Vec<Effect>` / risks / a per-mutation `contained` flag); assembly, coverage
    computation, and the boundary-containment shift live here.
- **swc syntax config from the source kind:** `.tsx` → TSX, `.ts` → TS, `.jsx` / `.js`
  / `.mjs` / `.cjs` → JS with JSX enabled. Extension determines it for files; `--lang`
  determines it for stdin. (TSX-vs-TS resolves the `<T>` ambiguity deterministically.)
- **`async` reuses existing fields:** `analyze_unit` sets `async_boundary` from
  `async`-ness or any `await`, and `await_count` from awaited expressions — identical
  semantics to the Rust frontend. `async` / `await` / `Promise` / `.then` are **not**
  effects themselves.
- **Owned AST:** swc's AST is owned (no arena lifetime), so `FnUnit` retains the
  function body the same way it retains a `syn::Block` today — no lifetime threading.

### CLI

- File discovery gains the JS/TS extensions; dispatch is feature-gated on `ts`
  exactly as Rust is on `rust` (an unknown extension built without its feature emits a
  "no frontend" diagnostic, as today).
- **Fragment mode:** a path of `-` reads source from stdin; `--lang {ts,tsx,js,jsx}`
  is **required** for stdin (no extension to infer from) and selects the swc syntax
  config. For file/dir paths the extension wins and `--lang` is ignored. (Requiring
  `--lang` for *files* was rejected: real trees mix `.ts`/`.tsx`/`.js`, which a single
  `--lang` cannot serve.)

## Output schema

No structural change to `Report` / `Scope` / `Hotspot` / `Summary`. The only
additions are new **kind strings** in `effects[].kind` and `risk_features[].kind`
(`this.mutation`, `type.escape`, `dynamic.code`, `proto.pollution`, `html.injection`),
plus the existing `discounted_to` / `discount` fields carrying boundary-containment
rationale, e.g.:

```json
{ "kind": "local.mutation", "class": 1, "discounted_to": 0, "weight": 0,
  "line": 7, "tier": "heuristic", "evidence": "acc.items.push(item)",
  "discount": "contained by fully-typed boundary (coverage 3/3)" }
```

`async_boundary` / `await_count` are populated as in 001. Per-effect `confidence` is
still **not** serialized (function-level only).

## Error handling

Mirrors 001/002: an un-parseable file or fragment becomes a `diagnostic`, never a
panic. A stdin fragment that is not a valid module still parses as a sequence of
items/statements where possible; if swc cannot parse it, it is one `diagnostic` with
`parsed: false`. `--lang` is a plain enum flag.

## Detectability & confidence

Most type-dependent JS/TS signals are `heuristic` (no `tsc`):

- `exact`: `throw`, syntactic `any` / `as any` / `@ts-ignore` presence, `eval` /
  `new Function`, `with` (the `with` statement is syntactically unambiguous — it is
  always `dynamic.code`, no import resolution needed).
- `path`: a call resolved through the `import` table to a known module/member
  (`console.log`, `crypto.randomUUID`).
- `heuristic`: method-name signals (`.query`, `.set`), `process.env` access, the
  boundary-containment discount itself (we **trust** the annotations; if they lie, or
  `tsc` was never run, the discount is unearned — hence the heuristic penalty), and
  any signal needing type info we do not have.

A fragment scored in isolation has no surrounding `import` table, so call resolution
degrades to `heuristic` and confidence drops — consistent with "we report only what is
syntactically visible," not a bug.

## Testing strategy

Mirrors the Rust frontend: `tests/fixtures/*.{ts,tsx,js}` read by a shared
`analyze_fixture(name)` helper (a subdir cargo does not compile as test targets);
`insta` snapshots for whole-report shape. Coverage must include:

- **World vs state:** a function doing `fetch` keeps `net.fs.db` class 7 even with a
  fully-typed boundary; a function with only local mutation behind a fully-typed
  boundary scores 0.
- **Graduated coverage:** the same locally-mutating function at `c = 0` (contained
  `local.mutation` stays class 1), at partial `c` (floors to class 0), and at `c = 1`
  (floors to class 0). Since every contained effect in this milestone is class 1,
  partial and full coverage are **observationally identical** here (both → 0) — the
  fixtures assert exactly that. The latent Partial=down-1 vs Full=down-2 distinction is
  covered by a **`fxrank-core` unit test** on a synthetic class-≥2 contained input,
  since no JS/TS fixture can exercise it.
- **`any` poison:** a fully-typed boundary with one `as any` in the body → discount
  voided + `type.escape` risk.
- **Escape discrimination:** local mutation (contained, discounts) vs param /
  closure-capture / module-global mutation (escaping, no discount; closure-capture
  flagged `hidden`).
- **Risk kinds:** `eval` → `dynamic.code`; `el.innerHTML =` → `html.injection`;
  `Object.setPrototypeOf` → `proto.pollution`.
- **Function forms:** arrow / method / getter collected as units; a pure
  `x => x*2` callback scores 0; anonymous arrow gets `<arrow@L…>`.
- **async:** `async`/`await` sets `async_boundary` / `await_count`, emits no effect of
  its own.
- **Fragment mode:** `echo '<fn>' | fxrank scan --lang ts -` scores a single function;
  lower confidence than the same function in-file.
- **Slim builds:** `--no-default-features --features ts` and `--features rust` both
  compile (feature-gate hygiene), matching CI's existing slim-build gates.

## Verification

- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check` green.
- `cargo build -p fxrank --no-default-features --features ts` (slim TS build) and
  `--features rust` both compile.
- Dogfood: scan a small typed-TS fixture tree and confirm world-effect hotspots
  surface while contained-mutation functions sink, and that `any`-bearing functions
  carry a `type.escape` risk.

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| What "types lower the score" means | Containment discount (not a blanket typedness bonus) | Stays true to "measuring instrument reports facts." A typed fn that still does IO keeps full IO class. |
| Unifying principle | Types bound the space of variation; `any` re-opens it | Justifies *which* constructs earn a discount: those that provably shrink the reachable-effect set, syntactically. |
| What a boundary contains | State (and memory), not world | Effect-system truth: a sound boundary encapsulates `ST`-style local state, never `IO`. User's own framing: "mutation or unsafe", not IO. |
| Discount eligibility | Only contained (non-escaping) effects, behind a typed boundary | Discounting escaping mutation would re-hide the hidden mutation this tool exists to surface (001 anti-Goodhart). |
| Boundary gate | Graduated by signature coverage `c = t/S` (Full down 2 / Partial down 1 / None) | "Some typing beats none" — each pinned slot removes a dimension of variation. Depth is latent in M-A (every contained effect is class 1, so Partial=Full=0); kept in core for future/Rust. |
| `any` handling | Poison: voids the discount anywhere it appears (sig or body) + gentle `type.escape` risk (class 3) | `any ≈ unsafe`; surfaced for agents without shadowing real IO. |
| Contained-effect floor | Class 0 (not 1) | A contained effect is not observable, so it may discount to truly free — makes "I don't care about internal mutation" measurable. |
| Explicit vs inferred annotations | Only explicit annotations are credited; inferred are invisible | Honest boundary of a syntactic instrument; rewards type-first/FP style. Two runtime-identical fns (one annotated) get different scores — intended. |
| JSDoc comment types | Out of scope (treated as untyped) | Comment-embedded types are not AST type nodes; we do not parse comment strings. |
| Parser | `swc` | Owned AST fits the FnUnit retain-the-body pattern (no arena lifetime, unlike oxc); `swc_ecma_visit::Visit` is a 1:1 parallel to `syn::visit`; first-class TS type nodes for cheap `any`/`readonly` detection; heavily maintained. |
| Speed vs swc/oxc | swc; optimize startup + incremental, not the parse constant | Agent calls are high-frequency but small; process startup + re-parsing unchanged files dominate, not the parser's 2–3× constant. |
| Fragment entry | First-class stdin mode; `--lang` for stdin, extension for files | Agents split work into fragments; files mix dialects so a single `--lang` cannot serve directory scans. |
| `param.mutation` in JS | Class 3, escaping (no boundary discount), not flagged `hidden` | JS has no `&mut` call-site visibility (Rust's reason for the big mut-param discount); but a named param is more visible than a closure capture. |
| async | Flag only (`async_boundary` / `await_count`), never an effect | Identical to the Rust frontend; effects come from what the body actually does. |
| DOM depth | Only obvious sinks now (innerHTML → risk, storage → `net.fs.db`, document reads → `ambient.read`) | Full DOM catalog is a large surface; defer until real scans justify it. |

## Deferred / Future work

1. **Revisit Rust's `unsafe`-cancels-discount rule** under the boundary-containment
   principle (contained unsafe behind a sound boundary). Not touched this milestone;
   changing 001's shipped scoring requires enumerating consumers and confirming
   source-of-truth first.
2. **JSDoc comment types** — credit `/** @type … */` annotations (treat well-typed
   JSDoc-JS as typed). Requires parsing comment trivia.
3. **Closure-capture mutation tiering** — distinguish mutating an *enclosing-function
   local* (lexically bounded within the activation, lighter) from a *module / imported*
   binding (global escape, heavier). Milestone A classifies both as
   `hidden.mutation` class 3.
4. **Full DOM / browser effect catalog** — events, storage variants, navigation,
   workers, the rest.
5. **Call-graph propagation / `inherited_score`** (already deferred in 001) — this is
   where `readonly` *on parameters passed to callees* finally pays off (caller knows
   the callee will not mutate). Within-function `readonly` has little to discount
   because the instrument already observes the body.
6. **Weighting the return slot** above parameters in coverage `c` — the return type
   bounds the output space most directly.
7. **Scheduling effects** (`setTimeout` / `setInterval` / `queueMicrotask` /
   `requestAnimationFrame`) — whether and at what class to score deferral; left
   undetected in Milestone A rather than mis-weighted.
8. **Namespace-import member calls** — `import * as fs from 'node:fs'; fs.readFile()` is
   not resolved through the import table. The import table records `fs → node:fs`, but
   the call detector does not currently walk through the namespace alias to look up the
   member. Only bare single-ident imported names (named/default imports) are resolved;
   namespace members are currently classified by member-name heuristic alone.
9. **`render_expr` / `render_member` duplication** — these helpers exist independently in
   `detect/calls.rs` and `detect/risk.rs` (a Milestone-A copy to keep each detector
   self-contained). Extract a shared helper in `detect/` in Milestone B. Note:
   `mutation.rs`'s `base_ident` serves a distinct semantic (identifying a write target's
   root binding) and must not be merged in.
10. **`as unknown as T` double-assertions and `@ts-ignore` / `@ts-expect-error` comment
    directives** are not yet detected as `any`-family. swc places comment directives in a
    side-table not currently threaded to the detectors. Milestone A detects `as any` and
    `: any` via AST nodes only; comment-directive detection and double-assertion unwinding
    are Milestone-B items.

## CLI / behavior notes

- **`--lang` is OPTIONAL for stdin, not required in the strict sense of "absent → error"
  for all callers.** The flag defaults to Rust (backward compatibility — existing Rust
  stdin usage (`fxrank scan -`) is preserved unchanged). `--lang ts` / `tsx` / `js` /
  `jsx` selects the swc frontend. This decision is deliberate: new TS usage gains an
  explicit `--lang ts`; old Rust usage requires no change.
- **`--lang` is rejected when combined with a file or directory path.** For file/dir
  inputs the file extension determines the language unambiguously; `--lang` is an error
  there (a directory tree can mix `.ts` / `.tsx` / `.js` and a single `--lang` flag
  cannot serve it).

## Open questions

- Exact `type.escape` class (3 chosen as "gentle"); tune only with real scans, not
  speculation.
- Nested `any` (`Foo<any>`, `any[]`) — Milestone A checks only a slot's **top-level**
  `any`; generalize if real code shows it matters.
- DB / ORM detection is method-name heuristic (`.query` / `.execute`); revisit with a
  small allowlist of known clients if false positives appear.
