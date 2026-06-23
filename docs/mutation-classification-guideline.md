# Mutation-classification guideline (descriptive)

How FxRank's language frontends classify a write site into a mutation effect. This
is a **descriptive reference** — it documents the shared model and the honest
per-language differences. It is not a contract; nothing is required to conform.

## Shared model

Each frontend (`crates/fxrank-lang-{rust,ts,python}/src/detect/mutation.rs`) reduces
a write to a **base name**, classifies it against **flat per-unit binding sets** via a
**fixed priority cascade**, and emits an `EffectKind` (`fxrank-core`). No lexical scope
stack; an *unresolved base* is the proxy for "captured/module". TS/Python stop at nested
functions; Rust's mutation walker descends into closures and block-local `fn` items.

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
  pre-scan of recognized local targets). In
  **Python**, a `global x` *rebind* already escalates via the explicit `global`-decl arm
  (Exact); the module-binding set adds the **content-mutation without `global`** case
  (`_cache["k"]=1`, `shared.append(1)` — Heuristic). TS has no such keyword, so all module
  writes go through the set.
- **Destructuring-target writes** — dropped in all three (accepted limitation).
- **Constructor breadth** — only a *direct* `this.x=`/`self.attr=` field-init is contained-local;
  a method/subscript write on the receiver escapes (TS aligned to Python's rule).

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
- **Python** — `global`/`nonlocal` pre-scanned; comprehension scopes unmodeled; **module
  top-level bindings** (module-level assign targets + `def`/`class` names) whose contents are
  mutated → global; a genuinely captured enclosing-function local → hidden.
