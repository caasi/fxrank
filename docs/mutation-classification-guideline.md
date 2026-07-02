# Mutation-classification guideline (descriptive)

How FxRank's language frontends classify a write site into a mutation effect. This
is a **descriptive reference** — it documents the shared model and the honest
per-language differences. It is not a contract; nothing is required to conform.

## Shared model

Each frontend (`crates/fxrank-lang-{rust,ts,python,shell}/src/detect/mutation.rs`) reduces
a write to a **base name**, classifies it against **flat per-unit binding sets** via a
**fixed priority cascade**, and emits an `EffectKind` (`fxrank-core`). No lexical scope
stack; an *unresolved base* is the proxy for "captured/module". TS/Python stop at nested
functions; Rust's mutation walker descends into closures and block-local `fn` items. Shell
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

## Honest per-language differences (intentional — not aligned)

- **Rust mut-channel discount** (`apply_discount`, `&mut`→−2/`&mut self`→−1, unsafe-cancel) —
  Rust ownership; TS/Python have no `&mut`.
- **TS/Python typed-boundary discount** (`apply_boundary_discount`, floor 0) — gradual typing;
  Rust is fully typed and does not apply it. Rust's `Effect` has no `contained` field.
- **Python `nonlocal`→this.mutation/Exact/not-hidden** — an explicitly declared capture is
  visible (not hidden); distinct kind from the implicit-capture `hidden.mutation` (same class 3).
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
