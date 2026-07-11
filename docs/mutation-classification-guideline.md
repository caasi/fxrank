# Mutation-classification guideline (descriptive)

How FxRank's language frontends classify a write site into a mutation effect. This
is a **descriptive reference** — it documents the shared model and the honest
per-language differences. It is not a contract; nothing is required to conform.

## Shared model

Each frontend (`crates/fxrank-lang-{rust,ts,python,shell}/src/detect/mutation.rs`) reduces
a write to a **base name**, classifies it against **flat per-unit binding sets** via a
**fixed priority cascade**, and emits an `EffectKind` (`fxrank-core`). No lexical scope
stack; an *unresolved base* is the proxy for "captured/module". Like TS/Python, Rust stops at
nested functions and closures — each nested `fn`/method/closure is its own unit, so the
enclosing walker does not descend into it (#83). Shell
has no closures — its cascade is a **declaration-vs-hidden gate**, not a binding-resolution
cascade (see *Per-frontend realization*).

## Canonical mapping

| write case | EffectKind | class | contained | hidden | tier |
|---|---|:--:|:--:|:--:|:--:|
| body-local place mutation | local.mutation | 1 | yes | no | Exact |
| param place mutation (no declared channel) | param.mutation | 3 | no | no | Heuristic |
| `&mut` param / `&mut self` (Rust) | param.mutation | 3 (discounted_to 1/2 when safe) | — | no | Heuristic |
| receiver field, normal method | this.mutation | 3 | no | no | Heuristic |
| constructor *direct* field-init | local.mutation | 1 | yes | no | Heuristic |
| explicit declared capture (`nonlocal`, Python) | this.mutation | 3 | no | no | Exact |
| real global / static | global.mutation | 6 | no | no | Exact/Heuristic |
| write to a module top-level binding | global.mutation | 6 | no | no | Heuristic |
| write to imported binding | global.mutation | 6 | no | no | Heuristic |
| interior-mutability / ref-cell write | hidden.mutation | 3 | no | yes | Heuristic |
| captured-enclosing / unresolved base | hidden.mutation | 3 | no | yes | Heuristic |
| top-level `FOO=bar` declaration (Shell, script scope) | *(no effect)* | — | — | — | — |
| function bare non-local write (Shell, dynamic scoping) | global.mutation | 6 | no | no | Heuristic |
| computed/indirect write target (Shell, `printf -v "$var"`) | global.mutation | 6 | no | **yes** | Heuristic |

`hidden.mutation` subreasons: `"interior-mut"` (Rust interior-mutability), `"ref-cell-write"`
(TS `useRef().current`), `"captured-binding"` (the unresolved fallback, all three).

## Target model — captured-write classification via the capture environment *(not yet implemented — #84)*

The two captured-write rows above (*interior-mutability / ref-cell write* and
*captured-enclosing / unresolved base*) are where #83 currently **guesses**. Once a closure
is its own unit, its write sites are classified with **no** enclosing scope in hand, and the
dogfood (`docs/032-nested-unit-dogfood.md`, Q5) showed that forces a bad choice: Q5(a) emits a
flat `hidden.mutation "captured-binding"` for every captured write (**over-scores** — **this is
what Rust currently ships**); the move-gated interim Q5(c) swung the other way to contained
`local.mutation` for every non-`move` closure (**under-scores** — **implemented then reverted**
after review; see the Q5(c) bullet under *Honest per-language differences*).
Neither can tell a captured **owned local** from a captured **`&mut` param** from a captured
**`&RefCell`**, because the discriminator — the *nature of the captured binding* — lives in the
enclosing scope the isolated closure unit threw away. The target model **stops guessing** by
carrying that scope in.

**The capture environment (context, not effects).** When a frontend recurses into a closure to
analyze it as its own unit, it threads in the **enclosing** unit's binding sets: the
`&mut`-param set, the shared-`&`-ref set, the owned-local (`let`/`let mut`) set, and the module
`static`s. This is the **capture environment** — it supplies classification *context*
downward and carries **no** effects (the closure still owns its own effects, so there is **no
double-count**; the #83 boundary for *effects* is intact). See
`adding-a-language-frontend.md` §3.1 for the cross-language principle.

**The captured-write cascade.** A write whose base is **not** in the closure's OWN sets is
looked up in the capture environment and classified by **what it is there**:

| captured base resolves to (in the enclosing env) | EffectKind | class | contained | escapes? |
|---|---|:--:|:--:|:--:|
| **owned local** (enclosing `let`/`let mut`, **plain in-frame value**) — **Rust-only case** | local.mutation | 1 | yes | no — the borrow can't outlive the enclosing frame, genuinely contained |
| **`&mut` param** (enclosing `mut_params`) | param.mutation-class | 3 | no | **yes** — the referent is caller-owned |
| **`&` / `&RefCell`** (enclosing `shared_refs`) | hidden.mutation (`interior-mut`) | 3 | no | yes — interior mutability, must score high (anti-Goodhart) |
| **`static`** (enclosing module statics) | global.mutation | 6 | no | yes |
| **genuinely unresolved** (in no set, own or captured) | hidden.mutation (`captured-binding`) | 3 | no | yes — the existing fallback, now only the true unknowns |

The owned-local → contained row holds **conditionally, not unconditionally**: only for an owned
local whose *type* is a **plain in-frame value**. An owned local whose type is an **aliasing
handle** (`Arc`/`Mutex`/`Sender`/`RefCell`/raw ptr — see *Known residual*) is **NOT** contained
even though it is an owned `let`, because mutating through it escapes the frame; such ambiguous
owned-handle locals default to `hidden`.

The `&mut`-param case is the crux: the rejected move-gate (Q5(c)) *would have* contained it
because a non-`move` closure borrows, but the *referent* is caller-owned, so the mutation is
**caller-visible / escaping**, not contained. Only the capture environment distinguishes it from a captured owned
local. Escaping captured writes are additionally **re-attributed to the binding's owner** (the
enclosing unit); contained ones stay in the closure's own record — see
`cross-file-resolution-guideline.md`, *Captured-effect re-attribution* (the principled
resolution of the deferred Q5(b)).

**Known residual (honest).** A binding's nature is syntactically visible for **params** (types
are written in the signature: `dst: &mut Vec`) and for **most locals** (`let r = &mut x` shows
the `&mut`; `let v = Vec::new()` is owned). Two residual classes escape a purely syntactic pass,
and **both default conservatively to `hidden`** (prefer over-report to a false-negative):

- A rare **type-inferred reference local** — a `let` whose RHS doesn't syntactically reveal
  `&`/`&mut` but whose *inferred* type is a reference — cannot be told from an owned local by a
  syntactic pass.
- An **owned handle / interior-mutability-carrier local** — an owned local whose *type* aliases
  state **outside the frame**: `Arc`, `Rc`, `Mutex`, `RwLock`, `Cell`, `RefCell`,
  `Sender`/channel handles, atomics-by-value, raw pointers, `&'static mut`. The handle is an
  owned local, but mutating *through* it is **caller-visible / cross-thread** — so it is **NOT
  contained**. Without type inference this is **syntactically indistinguishable** from a plain
  owned value (`let v = Vec::new()`), so an ambiguous owned-handle local defaults **up** to
  `hidden`, not down to contained.

**Why the owned-handle default is load-bearing:** it is what keeps #84 from **under-scoring** the
`spawn(move || arc_mutex.lock()…push())` cross-thread pattern — under-scoring an escaping
cross-thread mutation is exactly the anti-Goodhart false-negative the tool exists to prevent, and
exactly the regression that sank the rejected Q5(c) move-gate interim.

Milestone-A is syntactic; a precise owned-vs-borrowed / owned-vs-aliasing-handle answer needs
type inference and is out of scope.

**Cross-language — an epistemic limit, not an ontological one.** TS/JS and Python nested
functions **do** capture outer variables, exactly like Rust closures — this is not a Rust
peculiarity. And a captured mutation of a **truly-local outer variable consumed in-frame** IS
morally *contained* in those languages too — the very same shape as the Rust FP the capture
environment discounts. The real difference is **not** that GC-language captures are
ontologically external; it is **epistemic**: Rust's ownership/borrow system hands the frontend a
cheap **syntactic proof of containment** (an owned `let` → the borrow can't outlive the frame)
that GC languages lack. Absent that proof they **conservatively default** captured mutations to
external — they *can't cheaply prove* containment, not that there is *nothing to contain*. So:

- **Universal (all frontends).** Absent a containment proof, a captured mutation is treated as
  an **external effect** → it **re-attributes to the binding's owner** (the enclosing unit).
  Threading the enclosing binding environment in as context, and re-attributing the captured
  mutation to the owner, is the **cross-language** core of this model — a TS/Python
  nested-fn/closure mutating a captured outer binding is (conservatively) an external effect its
  enclosing scope's caller should see, same principle.
- **Rust-only ownership-based contained case.** Ownership gives Rust a *second*, **provable**
  case where the GC languages conservatively keep **one**: a captured **owned local** is
  *contained* (the mutation can't escape unless the closure is `move` and itself escapes) — the
  owned-`let` syntax plus the capture environment is the cheap proof, and the capture
  environment is what finds this subset and discounts it. A captured **`&mut` param / `&`-ref**
  stays external and caller-visible, the same as the conservative GC-language default.

There is **also** a **language-neutral** containment signal this model does not yet exploit:
**inline-non-escaping consumption** — the closure is passed **directly to a synchronous
consumer** (`.forEach` / `.map` / `.for_each`) and is **never stored, returned, or spawned**.
That proves containment in **TS/Python too** (not only Rust), and it would additionally catch the
`move` + synchronous residual the move-gate interim would have missed (see *Rust `move`-gated captured-binding*
below). Flag it as a **future cross-language refinement** to the conservative default.

So the shared model is "thread the enclosing binding environment as context; absent a proof of
containment, a captured mutation is external and re-attributes to the owner," and Rust *adds* an
ownership-proved contained-owned-local discount on top — with inline-non-escaping consumption a
still-unexploited proof available to **every** frontend. This supersedes both #83 guesses; it does
**not** claim TS/Python already do it — and it does not frame them as weaker, only as **lacking
Rust's cheap syntactic proof** of containment.

## Honest per-language differences (intentional — not aligned)

- **Rust mut-channel discount** (`apply_discount`, `&mut`→−2/`&mut self`→−1, unsafe-cancel) —
  Rust ownership; TS/Python have no `&mut`.
- **TS/Python typed-boundary discount** (`apply_boundary_discount`, floor 0) — gradual typing;
  Rust is fully typed and does not apply it. Rust's `Effect` has no `contained` field.
- **Python `nonlocal`→this.mutation/Exact/not-hidden** — an explicitly declared capture is
  visible (not hidden); distinct kind from the implicit-capture `hidden.mutation` (same class 3).
- **Rust `move`-gated captured-binding — Q5(c), implemented then reverted (#83 → #84).** A tried
  interim: a **non-`move`** closure that mutates a captured local/param captures it by `&mut`
  **borrow**, which the borrow checker guarantees cannot outlive the enclosing fn, so the move-gate
  classified it as *contained* `local.mutation` (subreason `"captured-borrow"`) rather than the
  shared `hidden.mutation "captured-binding"`, keeping `move` closures and named fns hidden. It was
  **implemented and then reverted after review** as too coarse: keying on "is the closure `move`?"
  instead of "what is the captured binding's nature?" **false-negatives a captured `&mut`-param
  mutation** (the referent is caller-owned → escaping), **inverts** the `&RefCell` interior-mut
  thesis (demotes a class-3 interior-mut write to contained), and **under-scores** an `Arc<Mutex>`
  cross-thread mutation through a non-`move` scoped-thread closure — a net regression vs simply
  shipping Q5(a). **Rust currently ships Q5(a)** — the flat `hidden.mutation "captured-binding"`
  fallback, identical to TS/Python for the same closure shape. The correct fix is the *Target model
  — captured-write classification via the capture environment* above, filed as **#84**, which
  replaces "is the closure `move`?" with "what is the captured binding's nature in the enclosing
  environment?" — the precise discriminator the move-gate only approximated. Evidence in
  `docs/032-nested-unit-dogfood.md`.
- **Plain rebind `x = …`** — Python no-emits (name-rebinding ≠ mutation); TS/Rust emit
  local.mutation/1 (variable-slot reassignment).
- **Per-language mutating-method allowlists** — language-appropriate vocabularies.
- **Module-binding detection is per-frontend & syntactic** — each frontend collects its own
  module top-level binding set (Rust: real `static` set; TS: `const`/`let`/`var`/`fn`/`class`,
  incl. `export` + named default; Python: module-level assign targets + `def`/`class` names).
  Locals/params win before module bindings (a function-scoped binding shadows a module name);
  the local-set construction is frontend-specific (TS traversal-order; Python whole-function
  pre-scan that recurses through tuple/list/starred destructuring and handles augmented-assign
  targets). In
  **Python**, a `global x` *rebind* already escalates via the explicit `global`-decl arm
  (Exact); the module-binding set adds the **content-mutation without `global`** case
  (`_cache["k"]=1`, `shared.append(1)` — Heuristic). TS has no such keyword, so all module
  writes go through the set.
- **Destructuring-target writes** — dropped in all three (accepted limitation).
- **Constructor breadth** — only a *direct* `this.x=`/`self.attr=` field-init is contained-local;
  a method/subscript write on the receiver escapes (TS aligned to Python's rule).
- **Shell's declaration-vs-hidden gate replaces binding-resolution entirely.** The other three
  frontends resolve a base name against local/param/receiver/static/import/module sets; shell has
  none of those channels. Its gate is purely `unit.is_script`: a **top-level** `FOO=bar` is a
  script-scope *declaration* (no effect — shell has no "module" to pollute, the whole script
  *is* the scope), while the identical assignment inside a **function**, to a name the function
  never declared `local`, is a *hidden* write to the caller's dynamic scope — `global.mutation`/6
  (shell's dynamic scoping folds cleanly into the existing global channel; no separate kind
  needed).
- **Subshell containment is a fourth axis, orthogonal to the other three frontends' escape
  logic.** A write inside `$(…)`/`( )`/`&`/a pipeline stage can never mutate the parent shell's
  state (the subshell is a forked copy), so **every** mutation detected inside one is forced
  `contained = true` regardless of what the write would score outside — even a bare non-local
  write that would otherwise be `global.mutation`/6 escaping. No other frontend has an analogous
  "this whole syntactic region cannot escape" rule; Rust/TS/Python contain by declared channel
  (`&mut`, typed boundary), not by execution context.
- **Shell is the first frontend to pair `hidden: true` with `global.mutation`** (indirect/computed
  write targets: `printf -v "$var" value`, where the target name is itself a runtime value, not a
  literal). This is a deliberate departure from the canonical mapping's usual "hidden ⇒
  `hidden.mutation`/3" pairing: routing an indirect write through `hidden.mutation`/3 would score
  it *below* an honest plain named-but-undeclared write (`global.mutation`/6), inverting the
  anti-Goodhart ordering (an indirect/obfuscated target should never score lower than a plain
  one). `hidden` stays evidentiary-only here — it documents the indirection for a human/agent
  reader but is unread by scoring/fold. Do not "fix" this by rerouting it to `hidden.mutation`.
- **`global.mutation` (in-script shared state) and `env.write` (child-process environment) are
  distinct axes in Shell**, both real but orthogonal: `export FOO=bar` / `FOO=bar cmd` write into
  the environment a child process inherits (`env.write`), while a bare `FOO=bar` write to an
  undeclared name in a function mutates the *current shell's* dynamic scope (`global.mutation`).
  A single statement can be both channels at once (`export` on an undeclared name), but they are
  never conflated into one kind — an agent reading effects needs to tell "this leaks to children"
  apart from "this corrupts caller state" even when they co-occur.

## The differentiator (must hold)

The anti-Goodhart inversion: a hidden interior-mutability write (`hidden.mutation`/3, no
discount) scores *higher* than an honest declared `&mut self` write (`param.mutation`/3
discounted to 2). Hidden state scores above declared state.

## Per-frontend realization

- **Rust** — `static`/`static mut`/atomic statics resolve via the real static-name set
  (no casing proxy); interior-mut on shared `&` receivers → hidden; closures share the parent
  unit's sets.
- **TS** — `var` hoist vs `let`/`const` TDZ unmodeled; `useRef().current` → ref-cell hidden;
  imports + `globalThis`/`window` + **module top-level bindings** (`const`/`let`/`var`/`fn`/`class`,
  incl. `export`/named-default) → global; captured enclosing-function local → hidden.
- **Python** — `global`/`nonlocal` pre-scanned; prescan now also collects `for`/`async for`
  loop targets, `with … as`/`async with … as` names, and `except … as`/`except* … as` names
  as function-local bindings (shadowing same-named module-level names); **module top-level
  bindings** (module-level assign targets + `def`/`class` names) whose contents are mutated
  → global; a genuinely captured enclosing-function local → hidden.
  **Residual accepted limits:** `match` pattern captures, comprehension-scope targets
  (Python 3 gives them their own scope), and walrus (`:=`) operator targets.
- **Shell** — no binding-resolution cascade; a `unit.is_script` gate (top-level = declaration,
  no effect; function body = dynamic-scope write, `global.mutation`/6 unless the name is
  `local`-declared) plus a `subshell` flag that forces `contained = true` on every write inside
  `$(…)`/`( )`/`&`/a pipeline stage. `declare`/`readonly`/`let`/`export`/`unset`/`read`/
  `mapfile`/`readarray`/`getopts`/`printf -v`/assignment prefixes (`VAR=v cmd`) are each their
  own detector; `local`-declared names stay `local.mutation`/1 contained; `export`/env-prefix
  writes are `env.write`/6 (a separate axis, see above); indirect/computed targets
  (`printf -v "$var"`) are `global.mutation`/6 with `hidden: true` (the one deliberate exception
  to "hidden ⇒ `hidden.mutation`", see above).
