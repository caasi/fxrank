# Cross-Language Module-Binding → `global.mutation` Alignment — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A write whose base is a **module top-level binding** classifies as `global.mutation` (class 6) **consistently across all three frontends**, while a genuinely captured *enclosing-function* local stays `hidden.mutation` (class 3). Today only Rust does this (spec 008 **F2**, the real-`static` set); TS and Python drop module-level writes into the `hidden.mutation`/`captured-binding` catch-all. This plan makes the canonical model explicit (guideline + spec 008) and brings TS and Python into conformance.

**Architecture:** Each frontend reduces a write to a base name and runs a fixed priority cascade over flat per-unit binding sets. The shared rule: collect the module's **top-level binding names** into a `module_bindings: HashSet<String>` set, thread it through that frontend's mutation cascade, and add **one arm** that escalates a write whose base is a module binding — placed **after** locals/params (so a shadowing function-scoped binding still wins) and **before** the `hidden.mutation` catch-all. Per language:
- **Rust** — already conforms: the real `static`/`static mut`/atomic-static name set is threaded and escalated (F2). Rust has no mutable module-level `let`, so `static` *is* its module-binding case. **No code change** — Task 2 verifies it.
- **TS/JS** — collect module `const`/`let`/`var`/`function`/`class` (incl. `export` + named default); add the arm. This is issue **#29**.
- **Python** — collect module top-level assignment targets + `def`/`class` names; add the arm. This catches the **content-mutation of a module-level container without `global`** case (`_cache["k"]=1`, `shared.append(1)`). The explicit-`global` rebind case already escalates via the `global`-decl arm (Python's `global` keyword makes it precise), so the change is **purely additive** — no existing Python test breaks.

**Tech Stack:** Rust, swc (`swc_ecma_ast`/`swc_ecma_visit`, TS), libcst (`libcst_native`, Python), `syn` (Rust frontend), `fxrank-core` effect vocabulary, `cargo test`/`fmt`/`clippy`, `insta` snapshots.

**Source / context:** issue [#29](https://github.com/caasi/fxrank/issues/29) (the TS case) generalized to all frontends per the spec-008 cross-language-alignment thesis ("same write concept → same `EffectKind`/class across frontends"). The Python gap was found by reading `crates/fxrank-lang-python/src/detect/mutation.rs` (the F1 catch-all comment at lines ~477-483 explicitly conflates "captured outer binding **or a module-level name**"). The descriptive source of truth is `docs/mutation-classification-guideline.md`.

## Global Constraints

- **Canonical rule (the alignment target):** a write whose base resolves to a **module top-level binding** (and is not a local/param/declared-capture/import that wins earlier in the cascade) → `global.mutation` (class 6), `contained:false`, `Tier::Heuristic`. A genuinely captured *enclosing-function* local stays `hidden.mutation` (class 3, `subreason:"captured-binding"`).
- **Cascade order (all frontends):** the new `module_bindings` arm goes **after** `this`/`self` + declared-capture (`global`/`nonlocal`) + params + locals + imports, and **before** the `hidden.mutation`/`captured-binding` catch-all. The shared rule is **locals/params win before module bindings**, so a function-scoped binding that shadows a module name resolves to local/param. This is a **flat-scope syntactic approximation** (spec 003 *Deferred #3*); the *order* in which the local set is built is a per-frontend nuance: **TS** populates `locals` in **traversal order** (a `var` written before its declarator can slip through), while **Python** **pre-scans the whole function body** for local targets — the prescan recurses through tuple/list/starred destructuring and handles augmented-assign targets, so shadow wins regardless of position and assignment form. Do **not** attempt real lexical-scope tracking — the shared scope-resolver path was retired with #31.
- **Centralize vocabulary:** reuse `EffectKind::GlobalMutation` / `HiddenMutation`; never hand-write wire strings.
- **The containment discount touches only the mutation channel; never sibling effects.** This change adds *no* new discount; it only re-routes the classification of a write's base.
- **CI gates (run before pushing):** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Clippy denies warnings — a threaded-but-unread field/param fails it, so each frontend's param and its first use land in the **same** commit.
- **No committed snapshots shift.** Verify per frontend (TS Task 4 Step 8; Python Task 6). If any `.snap.new` appears, stop and investigate — never blind-`cargo insta accept`.
- **Per-frontend native walk stays.** No shared classifier crate (spec 008 invariant). Each frontend keeps its own `module_bindings` collector + cascade; the alignment is behavioral + descriptive (guideline), not a code-sharing refactor.

**Branch / worktree:** code change → **feature branch**, not `main`. Execute in a git worktree on the RAM disk (`superpowers:using-git-worktrees`); plain-git fallback `git switch -c feat/009-cross-language-module-binding-global`. Commit this plan as the first commit on that branch (spec/plan artifacts ride the feature branch per the repo's git conventions).

**Sequencing rationale:** Task 1 (guideline) first establishes the canonical target; Task 2 confirms Rust already meets it; Tasks 3–4 align TS; Tasks 5–6 align Python; Task 7 finalizes cross-cutting docs + dogfood. Tasks 3–4 and 5–6 are independent and may run in parallel after Task 1.

---

### Task 1: Canonical model — guideline + spec 008

Make the "module top-level binding → `global.mutation`" rule explicit in the descriptive source of truth (`docs/mutation-classification-guideline.md`) and note it in spec 008. Doc-only; no code. Done first so it is the conformance target for Tasks 2–6.

**Files:**
- Modify: `docs/mutation-classification-guideline.md` (canonical mapping + honest differences + per-frontend realization)
- Modify: `docs/superpowers/specs/008-cross-language-mutation-alignment.md` (one-line traceability note)

- [ ] **Step 1: Add the canonical-mapping row**

In `docs/mutation-classification-guideline.md`, under `## Canonical mapping`, the table currently has `| real global / static | global.mutation | 6 | ... |`. Add a row directly below it for the generalized module-binding case:

```markdown
| write to a module top-level binding | global.mutation | 6 | no | no | Heuristic |
```

And update the `captured-enclosing / unresolved base` row's intent by leaving it as-is (it already maps to `hidden.mutation`/3) — the distinction is now: *module-level* base → global; *enclosing-function* local → hidden.

- [ ] **Step 2: Record the honest per-language nuance**

Under `## Honest per-language differences (intentional — not aligned)`, add a bullet:

```markdown
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
```

- [ ] **Step 3: Update per-frontend realization**

Under `## Per-frontend realization`, replace the TS and Python bullets so they describe the post-alignment behavior (read the current bullets first and mirror their terse style):

```markdown
- **TS** — `var` hoist vs `let`/`const` TDZ unmodeled; `useRef().current` → ref-cell hidden;
  imports + `globalThis`/`window` + **module top-level bindings** (`const`/`let`/`var`/`fn`/`class`,
  incl. `export`/named-default) → global; captured enclosing-function local → hidden.
- **Python** — `global`/`nonlocal` pre-scanned; comprehension scopes unmodeled; **module
  top-level bindings** (module-level assign targets + `def`/`class` names) whose contents are
  mutated → global; a genuinely captured enclosing-function local → hidden.
```

Leave the Rust bullet as-is (it already states the `static`-set realization, which is the canonical rule for Rust).

- [ ] **Step 4: Note it in spec 008**

In `docs/superpowers/specs/008-cross-language-mutation-alignment.md`, add a short note (in the F2 discussion, or a "follow-ups landed" location consistent with the doc's structure) that **F2 (module-binding → `global.mutation`) is generalized across frontends**: Rust via the `static` set (already), TS via #29, Python via the module-level-name set (content-mutation case). Keep it descriptive; do not rewrite F2's Rust content.

- [ ] **Step 5: Commit**

```bash
git add docs/mutation-classification-guideline.md docs/superpowers/specs/008-cross-language-mutation-alignment.md
git commit -m "docs: make module-binding -> global.mutation a canonical cross-language rule (#29)

Generalize spec 008's F2 (Rust static-set escalation) into a canonical rule that
applies to all frontends: a write to a module top-level binding is global.mutation
(class 6). Records the per-language realization (Rust static set; TS const/let/var/
fn/class; Python module-level names) and the honest nuance (Python's explicit
global-rebind vs content-mutation-without-global). Conformance lands in later tasks."
```

---

### Task 2: Verify Rust already conforms (no code change)

Rust already escalates writes to real `static`s to `global.mutation`/6 (spec 008 F2). Confirm this with a runnable check so the cross-language claim is evidence-backed, and only add a test if a gap is found.

**Files:**
- (Verify only) `crates/fxrank-lang-rust/src/detect/mutation.rs`
- (If a gap is found) add a unit test in its `#[cfg(test)] mod tests`

- [ ] **Step 1: Confirm the static arm + an existing test**

Run:

```bash
rg -n -m 20 'static|GlobalMutation' crates/fxrank-lang-rust/src/detect/mutation.rs   # -m 20, not `| head` (pipefail+SIGPIPE)
cargo test -p fxrank-lang-rust mutation    # run separately; don't pipe to tail (masks the test exit code)
```

Expected: the classify cascade has a `self.statics.contains(&base) → GlobalMutation (class 6)` arm (around mutation.rs:198 and the method-receiver variant ~358), and mutation tests pass. A write to a `static`/`static mut`/atomic static already classifies as `global.mutation`.

- [ ] **Step 2: Spot-check with a fragment**

```bash
set -o pipefail
printf 'static mut S: i32 = 0;\nfn f() { unsafe { S += 1; } }\n' | cargo run -p fxrank -- scan --lang rust - | jq '.hotspots[].effects[] | select(.kind=="global.mutation")'
```

Expected: the write to `S` surfaces as `global.mutation` (class 6). If it does **not**, stop — that is an unexpected Rust gap; add a failing test and a fix arm mirroring the TS/Python work before proceeding. (Expected outcome: it already works; no code change.)

- [ ] **Step 3: (No commit unless a gap was found.)** If Step 2 confirmed conformance, record "Rust verified conformant, no change" in the task notes / PR description and move on. Only commit if a test or fix was actually added.

---

### Task 3: TS — module top-level binding collector

Add `imports::module_bindings(&Module) -> HashSet<String>` — the data source the walker consults. It collects names introduced by **top-level** declarations only (bare and `export`ed): `const`/`let`/`var` declarators (incl. destructuring), `function`, and `class`. Names declared inside function bodies are *not* collected, so a write to one of these from inside a function is a write to module-shared state.

**Files:**
- Modify: `crates/fxrank-lang-ts/src/imports.rs` (add `use` of `HashSet` + `collect_pat_bindings`, add the function)
- Test: `crates/fxrank-lang-ts/src/imports.rs` (add unit test in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::detect::mutation::collect_pat_bindings(&Pat, &mut HashSet<String>)` — already `pub(crate)` (mutation.rs:417), handles destructuring/rest/assign patterns.
- Produces: `pub fn module_bindings(module: &Module) -> HashSet<String>` — consumed by Task 4 (`lib.rs`, `ts_frontend.rs` helpers).

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/fxrank-lang-ts/src/imports.rs` (the block already has `use super::*;`):

```rust
#[test]
fn module_bindings_collects_top_level_only() {
    use crate::functions;
    use crate::source::Lang;
    let src = "\
import x from 'm';\n\
const sharedMap = new Map();\n\
let counter = 0;\n\
var legacy;\n\
const { a, b } = obj;\n\
function helper() {}\n\
class Box {}\n\
export const exported = 1;\n\
export function exportedFn() {}\n\
export class ExportedClass {}\n\
export default function namedDefault() {}\n\
function withLocals() { const innerLocal = 1; let p = 2; return innerLocal + p; }\n";
    let (module, _cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
    let mb = module_bindings(&module);
    // Top-level declarations are collected — bare, exported, and named default.
    // Note `withLocals` itself IS a top-level function, so its NAME is collected;
    // only the bindings *inside* its body are not (asserted below).
    for name in [
        "sharedMap", "counter", "legacy", "a", "b", "helper", "Box",
        "exported", "exportedFn", "ExportedClass", "namedDefault", "withLocals",
    ] {
        assert!(mb.contains(name), "expected module binding `{name}`, got {mb:?}");
    }
    // Function-body locals are NOT collected:
    assert!(!mb.contains("innerLocal"), "function-body local leaked into module_bindings");
    assert!(!mb.contains("p"), "function-body local leaked into module_bindings");
    // Imported names are NOT module-owned declarations (they live in the ImportTable):
    assert!(!mb.contains("x"), "imported name leaked into module_bindings");

    // A module allows only one `export default`, so the named-default CLASS branch
    // needs its own parse (the fixture above exercised the default FUNCTION branch):
    let (m2, _cm2) =
        functions::parse_module("export default class NamedDefaultClass {}", "t2.ts", Lang::Ts)
            .expect("parse");
    assert!(
        module_bindings(&m2).contains("NamedDefaultClass"),
        "named `export default class` should be collected"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-ts --lib module_bindings_collects_top_level_only`
Expected: FAIL to compile — `cannot find function module_bindings in this scope`.

- [ ] **Step 3: Add the imports**

In `crates/fxrank-lang-ts/src/imports.rs`, change the std import and add `DefaultDecl` to the existing `use swc_ecma_ast::{...};` list (it currently lists `CallExpr, Callee, Decl, Expr, ExprStmt, Lit, Module, ModuleDecl, ModuleItem, Pat, Stmt, VarDecl`):

```rust
use std::collections::{HashMap, HashSet};
```

```rust
use swc_ecma_ast::{
    CallExpr, Callee, Decl, DefaultDecl, Expr, ExprStmt, Lit, Module, ModuleDecl, ModuleItem, Pat,
    Stmt, VarDecl,
};
```

Add after the existing `use swc_ecma_visit::{...};` line (we accept the dependency on the mutation module's `pub(crate)` pattern-binding helper):

```rust
use crate::detect::mutation::collect_pat_bindings;
```

- [ ] **Step 4: Implement the collector**

Add this function to `crates/fxrank-lang-ts/src/imports.rs` (above `pub struct ImportTable`):

```rust
/// Collect the names introduced by **top-level** declarations of `module`.
///
/// These are the module's own shared bindings: top-level `const`/`let`/`var`
/// declarators (including destructuring patterns), `function` declarations, and
/// `class` declarations — bare, `export`ed (`export const`/`export function`/
/// `export class`), or a **named** default (`export default function f(){}` /
/// `export default class C{}`, which binds `f`/`C`). Only the module body is
/// scanned; names introduced inside function bodies are NOT collected, so a
/// write to one of these names from inside a function is a write to
/// module-shared state (the "module var used for cross-component communication"
/// anti-pattern), which the mutation walker escalates to `global.mutation`
/// (issue #29).
///
/// **Not** collected: export specifiers / re-exports (`export { foo }`,
/// `export { foo as bar }`, `export * from "x"`) introduce no local declaration;
/// anonymous default exports have no name to bind; TS-only forms
/// (`interface`/`type`) have no runtime binding. `enum`/`namespace` DO bind at
/// runtime but mutating module-shared enum/namespace state is an accepted miss
/// for this pass (revisit if dogfooding surfaces it). Likewise, only **direct**
/// module-body declaration items are scanned: a `var` hoisted out of a top-level
/// `if`/`for` block (`if (c) { var shared = 0; }`) is an accepted miss.
///
/// A mutated module `const`'s contents (`sharedMap.set(...)`, `arr.push(...)`)
/// already registers as a write on the base ident, so collecting the `const`
/// name is enough — no `const`-vs-`let` special-casing is needed.
pub fn module_bindings(module: &Module) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &module.body {
        // Bare top-level declarations, `export`ed ones, and NAMED default
        // exports contribute. Export specifiers / re-exports and anonymous
        // defaults do not (see doc above).
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                match &export.decl {
                    DefaultDecl::Fn(f) => {
                        if let Some(ident) = &f.ident {
                            out.insert(ident.sym.to_string());
                        }
                    }
                    DefaultDecl::Class(c) => {
                        if let Some(ident) = &c.ident {
                            out.insert(ident.sym.to_string());
                        }
                    }
                    DefaultDecl::TsInterfaceDecl(_) => {}
                }
                continue;
            }
            _ => continue,
        };
        match decl {
            Decl::Var(var) => {
                for d in &var.decls {
                    collect_pat_bindings(&d.name, &mut out);
                }
            }
            Decl::Fn(f) => {
                out.insert(f.ident.sym.to_string());
            }
            Decl::Class(c) => {
                out.insert(c.ident.sym.to_string());
            }
            // TS-only / enum / namespace forms: see doc above (accepted misses).
            _ => {}
        }
    }
    out
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p fxrank-lang-ts --lib module_bindings_collects_top_level_only`
Expected: PASS.

- [ ] **Step 6: fmt + clippy**

Run: `cargo fmt -p fxrank-lang-ts && cargo clippy -p fxrank-lang-ts --all-targets -- -D warnings`
Expected: clean (no warnings).

- [ ] **Step 7: Commit**

```bash
git add crates/fxrank-lang-ts/src/imports.rs
git commit -m "feat(ts): collect module top-level bindings (imports::module_bindings) for #29"
```

---

### Task 4: TS — thread `module_bindings` into the walker and escalate in `classify`

The TS behavioral delta. Thread the set through the cascade and add one arm to `classify`. This change reclassifies a module-level write from `hidden.mutation`→`global.mutation`, which **breaks exactly two existing tests** that used a module-level binding as a stand-in for "captured":
1. the unit test `captured_binding_write_has_captured_subreason` (`mutation.rs:817`) — fixture `let counter = 0; function C(){ counter += 1; }` (refit in Step 6);
2. the integration test `classifies_mutation_by_escape` (`ts_frontend.rs:164`) — its `viaClosure` assertion (refit in Step 7).

Both fixtures are updated here (a *labelled intentional delta* per spec 008's phased-migration principle, **not** a regression).

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/mutation.rs` (module-doc table; `detect`/`detect_with_refs`/`MutationWalker` field/`seed`/`classify`; new unit tests; fix the F3 test fixture; update the two test helpers)
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs` (`gather`, `analyze_unit`, `raw_signals` signatures + bodies)
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`Frontend::analyze` computes the set; `analyze_units` threads it)
- Modify: `crates/fxrank-lang-ts/tests/ts_frontend.rs` (update call-site helpers; rename fixture refs; new assertions)
- Modify: `crates/fxrank-lang-ts/tests/fixtures/mutation.ts` (rename `viaClosure`→`viaModuleVar`; add `viaEnclosing`/`inner`)

**Interfaces:**
- Consumes: `imports::module_bindings(&Module) -> HashSet<String>` (Task 3).
- Produces (new signatures all later call sites depend on):
  - `mutation::detect(body, sig, is_constructor, lines, imports, module_bindings)`
  - `mutation::detect_with_refs(body, sig, is_constructor, lines, imports, module_bindings, extra_refs)`
  - `detect::analyze_unit(unit, imports, lines, module_bindings) -> Hotspot`
  - `detect::raw_signals(unit, imports, lines, module_bindings, ref_bindings) -> RawSignals`
  - (`detect::gather(unit, imports, lines, module_bindings, extra_refs)` — private)
  - `lib::analyze_units(units, imports, module_bindings, lines, out)` — private

- [ ] **Step 1: Write the failing unit tests**

Append these four tests to the `#[cfg(test)] mod tests` block in `crates/fxrank-lang-ts/src/detect/mutation.rs` (after the existing mutation tests). They use the existing `detect_in_fn` helper — its signature is unchanged at this step; it gains `module_bindings` wiring in Step 3d:

```rust
// ── issue #29: module-level binding mutation escalates to global.mutation ──

#[test]
fn module_level_let_mutation_is_global() {
    // A write to a module-level `let shared` from inside a function is a write
    // to module-shared state — global.mutation (class 6), NOT hidden.mutation.
    let effects = detect_in_fn("let shared = {}; function f(){ shared.x = 1; }", "f");
    // Exactly one global.mutation, naming the var — guards against an accidental
    // pass if some unrelated global write is ever introduced.
    let globals: Vec<_> = effects
        .iter()
        .map(|(e, _)| e)
        .filter(|e| e.kind == EffectKind::GlobalMutation && e.evidence.contains("shared"))
        .collect();
    assert_eq!(globals.len(), 1, "expected exactly one global.mutation naming `shared`");
    assert_eq!(globals[0].effective_class(), 6);
    assert!(
        effects.iter().all(|(e, _)| e.kind != EffectKind::HiddenMutation),
        "module-level write wrongly classified as hidden.mutation"
    );
}

#[test]
fn module_level_const_map_mutation_is_global() {
    // Mutating a module `const`'s contents (`m.set(...)`) registers as a write
    // on the base ident `m`, a module binding -> global.mutation.
    let effects = detect_in_fn("const m = new Map(); function f(){ m.set('k', 1); }", "f");
    let e = effects
        .iter()
        .map(|(e, _)| e)
        .find(|e| e.kind == EffectKind::GlobalMutation)
        .expect("expected a global.mutation for .set on module-level `m`");
    assert_eq!(e.effective_class(), 6);
}

#[test]
fn captured_enclosing_local_stays_hidden() {
    // A captured ENCLOSING-FUNCTION local (declared in an outer function,
    // mutated in a nested function) is NOT module-level — it must stay
    // hidden.mutation (class 3). Guards that we only escalated module bindings.
    // From `inner`'s perspective `acc` is a captured outer binding (not its
    // param/own-local, not a module binding).
    let effects = detect_in_fn(
        "function outer(){ let acc = {}; function inner(){ acc.x = 1; } return inner; }",
        "inner",
    );
    let e = effects
        .iter()
        .map(|(e, _)| e)
        .find(|e| e.kind == EffectKind::HiddenMutation)
        .expect("expected hidden.mutation for captured enclosing-function local `acc`");
    assert_eq!(e.effective_class(), 3);
    assert_eq!(e.subreason.as_deref(), Some("captured-binding"));
    assert!(
        effects.iter().all(|(e, _)| e.kind != EffectKind::GlobalMutation),
        "captured enclosing-function local wrongly escalated to global.mutation"
    );
}

#[test]
fn function_local_shadowing_module_binding_is_local() {
    // A module `let shared` AND a function that declares its OWN `let shared`
    // then writes it -> local.mutation (class 1). The shadow wins because
    // locals are checked before the global arm (the flat-scope approximation).
    let effects = detect_in_fn(
        "let shared = {}; function f(){ let shared = {}; shared.x = 1; }",
        "f",
    );
    let e = effects
        .iter()
        .map(|(e, _)| e)
        .find(|e| e.kind == EffectKind::LocalMutation)
        .expect("expected local.mutation — function-scoped `shared` shadows the module binding");
    assert_eq!(e.effective_class(), 1);
    assert!(
        effects.iter().all(|(e, _)| e.kind != EffectKind::GlobalMutation),
        "shadowing local wrongly escalated to global.mutation"
    );
}
```

- [ ] **Step 2: Run to verify it fails at runtime**

Run: `cargo test -p fxrank-lang-ts --lib module_level_let_mutation_is_global`
Expected: the test **compiles and FAILS at runtime** — it calls the existing (unchanged) `detect_in_fn` signature, so it builds, but `classify` has no module-binding arm yet, so it panics with `expected exactly one global.mutation naming \`shared\``. (Runtime assertion failure is the expected "red" here — *not* a compile error; the classify arm in Step 4 is what turns it green.)

- [ ] **Step 3: Thread `module_bindings` through the cascade**

**3a. `crates/fxrank-lang-ts/src/detect/mutation.rs` — add the field, signatures, and seed wiring.**

Add the field to `struct MutationWalker<'a>` (after `imports: &'a ImportTable,`):

```rust
    /// Names introduced by the module's top-level declarations. A captured
    /// write whose base is one of these is module-shared state, escalated to
    /// `global.mutation` (class 6) rather than the `hidden.mutation` catch-all.
    module_bindings: &'a HashSet<String>,
```

Update `fn seed` to take and store it:

```rust
    fn seed(
        sig: &FnSig,
        is_constructor: bool,
        lines: &'a SpanLines,
        imports: &'a ImportTable,
        module_bindings: &'a HashSet<String>,
    ) -> Self {
        let mut params = HashSet::new();
        for pat in &sig.params {
            collect_pat_bindings(pat, &mut params);
        }
        MutationWalker {
            params,
            locals: HashSet::new(),
            ref_bindings: HashSet::new(),
            is_constructor,
            lines,
            imports,
            module_bindings,
            effects: Vec::new(),
        }
    }
```

Update `detect` and `detect_with_refs`:

```rust
pub fn detect(
    body: &FnBodyOwned,
    sig: &FnSig,
    is_constructor: bool,
    lines: &SpanLines,
    imports: &ImportTable,
    module_bindings: &HashSet<String>,
) -> Vec<(Effect, bool)> {
    detect_with_refs(
        body,
        sig,
        is_constructor,
        lines,
        imports,
        module_bindings,
        &HashSet::new(),
    )
}
```

```rust
pub fn detect_with_refs(
    body: &FnBodyOwned,
    sig: &FnSig,
    is_constructor: bool,
    lines: &SpanLines,
    imports: &ImportTable,
    module_bindings: &HashSet<String>,
    extra_refs: &HashSet<String>,
) -> Vec<(Effect, bool)> {
    let mut walker = MutationWalker::seed(sig, is_constructor, lines, imports, module_bindings);
    walker.ref_bindings.extend(extra_refs.iter().cloned());
    body.walk_with(&mut walker);
    walker.effects
}
```

**3b. `crates/fxrank-lang-ts/src/detect/mod.rs` — thread through `gather`, `analyze_unit`, `raw_signals`.**

`analyze_unit` signature + first line:

```rust
pub fn analyze_unit(
    unit: &FnUnit,
    imports: &ImportTable,
    lines: &SpanLines,
    module_bindings: &HashSet<String>,
) -> Hotspot {
    let gathered: Vec<(Effect, bool)> = gather(unit, imports, lines, module_bindings, &HashSet::new());
```

`gather` signature + the `mutation::detect_with_refs` call:

```rust
fn gather(
    unit: &FnUnit,
    imports: &ImportTable,
    lines: &SpanLines,
    module_bindings: &HashSet<String>,
    extra_refs: &HashSet<String>,
) -> Vec<(Effect, bool)> {
```

```rust
    effects.extend(mutation::detect_with_refs(
        &unit.body,
        &unit.sig,
        unit.is_constructor,
        lines,
        imports,
        module_bindings,
        extra_refs,
    ));
```

`raw_signals` signature + its `gather` call:

```rust
pub fn raw_signals(
    unit: &FnUnit,
    imports: &ImportTable,
    lines: &SpanLines,
    module_bindings: &HashSet<String>,
    ref_bindings: &HashSet<String>,
) -> RawSignals {
    let effects: Vec<Effect> = gather(unit, imports, lines, module_bindings, ref_bindings)
        .into_iter()
        .map(|(e, _contained)| e)
        .collect();
```

**3c. `crates/fxrank-lang-ts/src/lib.rs` — compute the set per module and thread it.**

In `Frontend::analyze`, after `let imports = ImportTable::from_module(&module);` (lib.rs:65), add:

```rust
                    let module_bindings = imports::module_bindings(&module);
```

Update the `analyze_units` call (lib.rs:70):

```rust
                        analyze_units(&units, &imports, &module_bindings, &lines, &mut output.functions);
```

Update `fn analyze_units` signature (lib.rs:93):

```rust
fn analyze_units(
    units: &[FnUnit],
    imports: &ImportTable,
    module_bindings: &HashSet<String>,
    lines: &SpanLines,
    out: &mut Vec<Hotspot>,
) {
```

Update the two inner call sites (lib.rs:132, :136):

```rust
            let raw = detect::raw_signals(unit, imports, lines, module_bindings, refs);
```
```rust
        let mut h = detect::analyze_unit(unit, imports, lines, module_bindings);
```

(**Do not** change the `use crate::imports::ImportTable;` line in lib.rs. `lib.rs` is the crate root and already declares `pub mod imports;` (lib.rs:6), so `imports::module_bindings(&module)` already resolves — adding `use crate::imports::{self, …}` would conflict with the module item ("`imports` defined multiple times"). `HashSet` is already imported in lib.rs:10 and detect/mod.rs:25, so no new `use` is needed there. The `{self, ImportTable}` form *is* correct in the external integration test `ts_frontend.rs` (Step 7), which has no `mod imports;`.)

**3d. `crates/fxrank-lang-ts/src/detect/mutation.rs` test helpers — compute and pass the set.**

Update `detect_in_fn` (mutation.rs:520) to compute and pass `module_bindings`:

```rust
    fn detect_in_fn(src: &str, fn_name: &str) -> Vec<(Effect, bool)> {
        let (module, cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        detect(
            &unit.body,
            &unit.sig,
            unit.is_constructor,
            &lines,
            &imports,
            &module_bindings,
        )
    }
```

Update `detect_with_refs_in_fn` (mutation.rs:619) the same way — compute `module_bindings` and add it to the `detect_with_refs(...)` call (the `module_bindings` arg goes immediately before `extra_refs`).

**3e. Sweep `src` for any remaining call site** (belt-and-suspenders):

```bash
rg -n 'mutation::detect\(|detect_with_refs\(|detect::analyze_unit\(|detect::raw_signals\(|analyze_units\(' crates/fxrank-lang-ts/src
```

At each hit: a **definition** must declare the new `module_bindings` parameter; a **call site** must pass it. Expected: only the sites updated in 3a–3c. Then confirm the library compiles:

```bash
cargo build -p fxrank-lang-ts        # builds the lib only (not test targets)
```

Expected: compiles. rustc **may emit a `dead_code` warning** that the `MutationWalker.module_bindings` field is never read — expected at this step (stored by `seed`, consulted only in Step 4's `classify`), resolved by Step 4 before clippy runs in Step 9; `cargo build` does not deny warnings. (The `tests/` integration call sites in `ts_frontend.rs` are updated in Step 7; they are not built by `cargo build -p` or `cargo test --lib`. The new unit tests still fail at runtime until Step 4.)

- [ ] **Step 4: Add the escalation arm to `classify`**

In `crates/fxrank-lang-ts/src/detect/mutation.rs::classify` (mutation.rs:221), extend the `global` arm condition. **Keep the `hidden.mutation` tail's `.with_subreason("captured-binding")` exactly as-is:**

```rust
        } else if base == "globalThis"
            || base == "window"
            || self.imports.resolve(base).is_some()
            || self.module_bindings.contains(base)
        {
            // A host global (`globalThis`/`window`), an imported binding, or a
            // write to a MODULE top-level binding — module-shared state used for
            // cross-component communication (issue #29). Checked AFTER
            // locals/params, so a function-scoped binding that shadows a module
            // name still wins (the flat-scope syntactic approximation).
            Classification::new(GlobalMutation, 6, false, false, Tier::Heuristic, "global")
        } else {
            // Captured enclosing-function local — hidden from the signature, but
            // NOT module-shared (not in `module_bindings`), so it stays class 3.
            Classification::new(HiddenMutation, 3, false, true, Tier::Heuristic, "captured")
                .with_subreason("captured-binding")
        }
```

- [ ] **Step 5: Run the new unit tests — expect PASS**

```bash
cargo test -p fxrank-lang-ts --lib module_level_
cargo test -p fxrank-lang-ts --lib captured_enclosing_local_stays_hidden
cargo test -p fxrank-lang-ts --lib function_local_shadowing_module_binding_is_local
```

Expected: all four PASS.

- [ ] **Step 6: Fix the breaking F3 unit test fixture**

Change `captured_binding_write_has_captured_subreason` (mutation.rs:817) to a genuinely captured *enclosing-function* local:

```rust
    #[test]
    fn captured_binding_write_has_captured_subreason() {
        // F3: a captured ENCLOSING-FUNCTION local (`counter`, declared in `outer`,
        // mutated in nested `C`) is hidden.mutation/3 with subreason
        // "captured-binding". (Pre-#29 this used a module-level `counter`; that now
        // escalates to global.mutation, so the fixture nests the binding.)
        let effects = detect_in_fn(
            "function outer(){ let counter = 0; function C(){ counter += 1; } return C; }",
            "C",
        );
        let e = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::HiddenMutation)
            .expect("captured write must be hidden.mutation");
        assert_eq!(e.0.effective_class(), 3);
        assert!(e.0.hidden, "captured write stays hidden == true");
        assert!(!e.1, "captured write stays contained == false");
        assert_eq!(e.0.subreason.as_deref(), Some("captured-binding"));
        assert_ne!(e.0.subreason.as_deref(), Some("ref-cell-write"));
    }
```

- [ ] **Step 7: Update the integration fixture + assertions**

In `crates/fxrank-lang-ts/tests/fixtures/mutation.ts`, replace the `viaClosure` block (mutation.ts:7-8) with:

```ts
// A module-level binding mutated from inside a function: cross-component shared
// state -> global.mutation (class 6), per issue #29.
let counter = 0;
function viaModuleVar(): void { counter += 1; }
// A captured ENCLOSING-FUNCTION local (not module-level): the nested `inner`
// writes `acc`, a local of `viaEnclosing` -> hidden.mutation (class 3).
function viaEnclosing(): () => void {
  let acc = 0;
  function inner(): void { acc += 1; }
  return inner;
}
```

In `crates/fxrank-lang-ts/tests/ts_frontend.rs`, change the import (ts_frontend.rs:8) to:

```rust
use fxrank_lang_ts::imports::{self, ImportTable};
```

Update the helpers `analyze_fixture_unit` (ts_frontend.rs:17), `mutation_effects` (ts_frontend.rs:133), `analyze_unit_pure_fn_scores_zero` (ts_frontend.rs:304), and `analyze_inline_units` (ts_frontend.rs:402) to compute `let module_bindings = imports::module_bindings(&module);` after each `ImportTable::from_module(&module)` and pass `&module_bindings` as the new arg to `detect::analyze_unit(...)` / `mutation::detect(...)`.

Then update the `classifies_mutation_by_escape` assertions (ts_frontend.rs:158) — replace the `viaClosure` line with:

```rust
    // issue #29: a write to a MODULE-level binding (`counter`) escalates to
    // global.mutation (class 6), not hidden.mutation.
    assert!(mutation_kinds("mutation.ts", "viaModuleVar").contains(&"global.mutation".into()));
    // A captured ENCLOSING-FUNCTION local stays hidden.mutation (class 3).
    assert!(mutation_kinds("mutation.ts", "inner").contains(&"hidden.mutation".into()));
```

- [ ] **Step 8: Run the full workspace test suite + verify snapshots unchanged**

Run: `cargo test --workspace`
Expected: PASS. The `insta` snapshot tests (`react.rs`, `snapshots.rs`) pass **without** any `.snap.new` — module bindings don't occur in those fixtures.
Run: `git status --porcelain crates/fxrank-lang-ts/tests/snapshots/`
Expected: **empty**. If a `.snap.new` exists, stop and investigate — do not accept it.

- [ ] **Step 9: fmt + clippy**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 10: Update the `mutation.rs` module-doc table**

Update the doc comment in `crates/fxrank-lang-ts/src/detect/mutation.rs` (the table at lines ~19-32):

```rust
//! | `globalThis`/`window`/imported/module binding | `global.mutation`| 6  | no     | no     |
//! | otherwise (captured enclosing-function local) | `hidden.mutation`| 3 | no    | **yes**|
//!
//! Note: `globalThis`, `window`, imported bindings, and **module top-level
//! bindings** are recognised as `global.mutation`; other host globals
//! (`document`, `navigator`, …) currently fall through to `hidden.mutation`
//! (full DOM coverage is a deferred Milestone-B item).
//!
//! The `contained` bool is returned alongside each `Effect`; the boundary
//! discount is its sole consumer. Per spec 003 Deferred #3 (issue #29) a write
//! whose base is a **module top-level binding** (`module_bindings`) is escalated
//! to `global.mutation` (class 6) — the "module var used for cross-component
//! communication" anti-pattern — while a genuinely captured enclosing-function
//! local stays `hidden.mutation` (class 3). The distinction is syntactic/
//! best-effort (the flat-scope approximation): a local/param that shadows a
//! module binding still wins as local/param **when declared before the write**
//! (the walker collects `locals` in traversal order — see the `locals` field
//! doc), since those are checked first.
```

Then re-verify (this doc-comment edit lands after Steps 8–9):

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p fxrank-lang-ts`
Expected: all clean.

- [ ] **Step 11: Commit**

```bash
git add crates/fxrank-lang-ts/src/detect/mutation.rs \
        crates/fxrank-lang-ts/src/detect/mod.rs \
        crates/fxrank-lang-ts/src/lib.rs \
        crates/fxrank-lang-ts/tests/ts_frontend.rs \
        crates/fxrank-lang-ts/tests/fixtures/mutation.ts
git commit -m "feat(ts): escalate module-level binding mutation to global.mutation (#29)

A TS/JS write to a module top-level binding (const/let/var/function/class)
now classifies as global.mutation (class 6) instead of the hidden.mutation
catch-all — the 'module var for cross-component communication' anti-pattern.
A captured enclosing-function local stays hidden.mutation (class 3). This is
the direct TS analog of spec 008's F2 (real-static-set threading).

Locals/params are checked before the new arm, so a function-scoped binding
already discovered before the write still wins (the flat-scope traversal-order
approximation, spec 003 Deferred #3). The spec-008 F3 test fixture used a
module-level binding as its 'captured' stand-in; it now nests the binding in an
enclosing function (labelled intentional delta, not a regression)."
```

---

### Task 5: Python — module top-level binding collector

Add `imports::module_bindings(&Module) -> HashSet<String>` for the Python frontend: module top-level **assignment targets** — bare `Name` plus destructured tuple/list/starred targets (`a, b = …`, `[x, y] = …`, `*rest, last = …`) of `Assign`/`AnnAssign` — plus `def` names and `class` names. Symmetric to the TS collector (which collects destructured bindings via `collect_pat_bindings`).

**Files:**
- Modify: `crates/fxrank-lang-python/src/imports.rs` (add the function + `HashSet` use if absent)
- Test: `crates/fxrank-lang-python/src/imports.rs` (unit test in the existing `#[cfg(test)] mod tests`, or add one)

**Interfaces:**
- Produces: `pub fn module_bindings(module: &Module) -> HashSet<String>` — consumed by Task 6 (`lib.rs`, `mutation.rs`/`mod.rs` test helpers). The libcst types `Statement`, `SmallStatement`, `CompoundStatement`, `AssignTargetExpression`, `Module`, `Expression` are already imported in imports.rs (imports.rs:33-35); **add `Element`** (needed for destructuring elements).

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/fxrank-lang-python/src/imports.rs` (mirror the file's existing test setup — it parses with `libcst_native::parse_module`). The collector recurses into tuple/list destructuring (Step 3), so `A`/`B`/`x`/`y` are collected:

```rust
#[test]
fn module_bindings_collects_top_level_only() {
    let src = "\
import config\n\
_counter = 0\n\
shared_map = {}\n\
A, B = 1, 2\n\
[x, y] = [3, 4]\n\
def helper():\n    inner_local = 1\n    return inner_local\n\
class Box:\n    pass\n";
    let module = libcst_native::parse_module(src, None).unwrap();
    let mb = module_bindings(&module);
    // Bare names, destructured tuple/list targets, def + class names all collected:
    for name in ["_counter", "shared_map", "A", "B", "x", "y", "helper", "Box"] {
        assert!(mb.contains(name), "expected module binding `{name}`, got {mb:?}");
    }
    // Function-body locals are NOT collected:
    assert!(!mb.contains("inner_local"), "function-body local leaked into module_bindings");
    // Imported names live in the Imports table, not here:
    assert!(!mb.contains("config"), "imported name leaked into module_bindings");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p fxrank-lang-python --lib module_bindings_collects_top_level_only`
Expected: FAIL to compile — `cannot find function module_bindings`.

- [ ] **Step 3: Implement the collector**

Add to `crates/fxrank-lang-python/src/imports.rs`. Ensure `use std::collections::HashSet;` is present (the file currently imports only `HashMap` — add `HashSet`), and add `Element` to the `libcst_native::{…}` import list (imports.rs:33-35). The recursive target helpers mirror the existing `walk_assign_target_subexprs`/`walk_target_element`/`walk_target_value` trio in `detect/mod.rs:294-361` (same enum shapes), but collect names instead of walking sub-exprs:

```rust
/// Collect the names introduced by **module top-level** statements of `module`:
/// assignment targets (`x = …`, `x: T = …`, and destructured `a, b = …` /
/// `[x, y] = …` / `*rest, last = …`), `def` names, and `class` names. Only the
/// module body is scanned; names bound inside function bodies are not collected.
/// A write whose root is one of these — when it is not a local/param/`global`-
/// declared/import in the writing function — is a write to module-shared state,
/// escalated to `global.mutation` (the Python analog of #29).
///
/// Not collected (accepted misses, consistent with the syntactic flat-scope
/// approximation in the other frontends): import names (handled by the F5 import
/// arm via the `Imports` table); subscript/attribute assignment targets (not new
/// bindings); names bound by module top-level `for`/`with … as`/`except … as`/
/// `match` patterns; names bound only inside nested blocks/comprehensions. Edge:
/// the per-function `prescan` collects *locals* from bare-`Name` targets only
/// (it does not recurse into tuple targets), so a function that tuple-rebinds and
/// then content-mutates a name that is *also* a module tuple-binding can
/// mis-escalate — the same pre-existing limitation the import (F5) arm has; rare,
/// accepted.
pub fn module_bindings(module: &Module) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in &module.body {
        match stmt {
            Statement::Simple(line) => {
                for small in &line.body {
                    match small {
                        SmallStatement::Assign(a) => {
                            for target in &a.targets {
                                collect_target_names(&target.target, &mut out);
                            }
                        }
                        SmallStatement::AnnAssign(a) => {
                            collect_target_names(&a.target, &mut out);
                        }
                        _ => {}
                    }
                }
            }
            Statement::Compound(c) => match c {
                CompoundStatement::FunctionDef(f) => {
                    out.insert(f.name.value.to_owned());
                }
                CompoundStatement::ClassDef(c) => {
                    out.insert(c.name.value.to_owned());
                }
                _ => {}
            },
        }
    }
    out
}

/// Collect bound names from an assignment target, recursing into destructuring.
/// Attribute/Subscript targets bind no new name. Mirrors
/// `detect::walk_assign_target_subexprs`'s enum shape.
fn collect_target_names(target: &AssignTargetExpression, out: &mut HashSet<String>) {
    match target {
        AssignTargetExpression::Name(n) => {
            out.insert(n.value.to_owned());
        }
        AssignTargetExpression::Tuple(t) => {
            for el in &t.elements {
                collect_element_names(el, out);
            }
        }
        AssignTargetExpression::List(l) => {
            for el in &l.elements {
                collect_element_names(el, out);
            }
        }
        AssignTargetExpression::StarredElement(s) => collect_expr_target_names(&s.value, out),
        AssignTargetExpression::Attribute(_) | AssignTargetExpression::Subscript(_) => {}
    }
}

/// A destructuring element (`(a, *rest) = …`). Mirrors `detect::walk_target_element`.
fn collect_element_names(el: &Element, out: &mut HashSet<String>) {
    match el {
        Element::Simple { value, .. } => collect_expr_target_names(value, out),
        Element::Starred(s) => collect_expr_target_names(&s.value, out),
    }
}

/// Destructuring elements are typed as `Expression`. Mirrors `detect::walk_target_value`.
fn collect_expr_target_names(expr: &Expression, out: &mut HashSet<String>) {
    match expr {
        Expression::Name(n) => {
            out.insert(n.value.to_owned());
        }
        Expression::Tuple(t) => {
            for el in &t.elements {
                collect_element_names(el, out);
            }
        }
        Expression::List(l) => {
            for el in &l.elements {
                collect_element_names(el, out);
            }
        }
        Expression::StarredElement(s) => collect_expr_target_names(&s.value, out),
        _ => {}
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p fxrank-lang-python --lib module_bindings_collects_top_level_only`
Expected: PASS.

- [ ] **Step 5: fmt + clippy**

Run: `cargo fmt -p fxrank-lang-python && cargo clippy -p fxrank-lang-python --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-python/src/imports.rs
git commit -m "feat(python): collect module top-level bindings (imports::module_bindings) for #29 analog"
```

---

### Task 6: Python — thread `module_bindings` and add the `global.mutation` arm

The Python behavioral delta. Thread the set into `MutSink` and add one arm in the classify cascade, **after** the F5 import arm and **before** the F1 `captured-binding` catch-all. **Purely additive** — no existing test breaks (the `mutation.py` captured case `inner`/`outer` is an enclosing-function local, not module-level; all module-level writes in the fixture use explicit `global`, which hits the earlier `globals` arm).

**Files:**
- Modify: `crates/fxrank-lang-python/src/detect/mutation.rs` (`MutSink` field + construction; `detect` signature; the classify arm; module-doc; new tests; thread test helpers + inline `MutSink`/`detect` call sites in tests)
- Modify: `crates/fxrank-lang-python/src/detect/mod.rs` (`analyze_unit` signature + the `mutation::detect` call; thread test call sites)
- Modify: `crates/fxrank-lang-python/src/lib.rs` (compute the set; thread into `analyze_unit`)

(The new behavioral tests use **inline source**, not the shared `tests/fixtures/mutation.py` — that fixture is scanned by the `snapshots__dogfood_report` insta snapshot, so editing it would force a snapshot regen. Inline source keeps this change snapshot-neutral.)

**Interfaces:**
- Consumes: `imports::module_bindings(&Module)` (Task 5).
- Produces:
  - `mutation::detect(unit, imports, module_bindings, span)` — new `module_bindings: &HashSet<String>` arg after `imports`.
  - `detect::analyze_unit(unit, path, imports, module_bindings, span)` — same insertion point.

- [ ] **Step 1: Add an inline-source test helper + the failing tests**

Add to the `#[cfg(test)] mod tests` in `mutation.rs` a small helper that parses inline source, collects, and runs `detect` with the new `module_bindings` arg (mirrors the existing inline `detect_accepts_imports_param` test at mutation.rs:781). Its `detect(...)` call uses the **new** signature, so it doubles as the first call site of the threaded API:

```rust
fn detect_src(src: &str, fn_name: &str) -> Vec<(Effect, bool)> {
    let module = libcst_native::parse_module(src, None).unwrap();
    let imports = crate::imports::Imports::build(&module);
    let module_bindings = crate::imports::module_bindings(&module);
    let span = crate::source::SpanIndex::new(src);
    let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
    let (units, _) = functions::collect(&module, src, &span, &anchors);
    let unit = units.iter().find(|u| u.symbol == fn_name).expect("unit not found");
    detect(unit, &imports, &module_bindings, &span)
}

#[test]
fn module_level_content_mutation_is_global() {
    // A module-level dict mutated by content (no `global` decl) is module-shared
    // state -> global.mutation (class 6), not the hidden captured-binding fallback.
    let src = "_cache = {}\ndef f():\n    _cache['k'] = 1\n";
    let pairs = detect_src(src, "f");
    assert!(
        pairs.iter().any(|(e, c)| e.kind == GlobalMutation && !*c),
        "module-level `_cache['k']=1` (no `global`) must be GlobalMutation(false), got: {:?}",
        pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
    );
    assert!(
        !pairs.iter().any(|(e, _)| e.kind == HiddenMutation),
        "module-level content mutation must not be hidden.mutation"
    );
}

#[test]
fn local_shadowing_module_binding_is_local() {
    // A bare local rebind shadows the module name (Python creates a local) ->
    // local.mutation; the shadow wins because locals are checked before the
    // module-binding arm.
    let src = "_cache = {}\ndef f():\n    _cache = {}\n    _cache['k'] = 1\n";
    let pairs = detect_src(src, "f");
    assert!(
        pairs.iter().any(|(e, c)| e.kind == LocalMutation && *c),
        "shadowing local `_cache` must be LocalMutation(true), got: {:?}",
        pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
    );
    assert!(
        !pairs.iter().any(|(e, _)| e.kind == GlobalMutation),
        "shadowing local must not escalate to GlobalMutation"
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p fxrank-lang-python --lib module_level_content_mutation_is_global`
Expected: **compile error** at this step. Task 5 already landed (Task 6 consumes it), so `crate::imports::module_bindings` resolves; the red is the `detect` **arity mismatch** — `detect_src` calls `detect(unit, &imports, &module_bindings, &span)` (the new 4-arg signature) but `detect` is still 3-arg until Step 3 threads it. The threading in Step 3 makes it compile; then `module_level_content_mutation_is_global` FAILS at runtime until the Step 4 classify arm. `local_shadowing_module_binding_is_local` passes once it compiles (it's a local today).

- [ ] **Step 3: Thread `module_bindings` through the cascade**

**3a. `mutation.rs` — `MutSink` field + construction + `detect` signature.**

Add to `struct MutSink<'a>` (mutation.rs:287, after the `imports` field):

```rust
    /// Module top-level binding names (assign targets + def/class). A write whose
    /// root is one of these — and is not a local/param/global-decl/import — is
    /// module-shared state, escalated to `global.mutation` (the #29 analog).
    module_bindings: &'a HashSet<String>,
```

Update `fn detect` (mutation.rs:57) signature and the `MutSink` construction (mutation.rs:69):

```rust
pub fn detect(
    unit: &FnUnit,
    imports: &Imports,
    module_bindings: &HashSet<String>,
    span: &SpanIndex,
) -> Vec<(Effect, bool)> {
```

```rust
    let mut sink = MutSink {
        params: &params,
        globals: &globals,
        nonlocals: &nonlocals,
        locals: &locals,
        imports,
        module_bindings,
        is_init,
        span,
        effects: Vec::new(),
    };
```

**3b. `detect/mod.rs` — `analyze_unit` signature + the `mutation::detect` call.**

Update `analyze_unit` (mod.rs:905) to take `module_bindings` (after `imports`):

```rust
pub fn analyze_unit(
    unit: &FnUnit,
    path: &str,
    imports: &Imports,
    module_bindings: &HashSet<String>,
    span: &SpanIndex,
) -> Hotspot {
```

Update the `mutation::detect` call (mod.rs:922):

```rust
    let mut_pairs = mutation::detect(unit, imports, module_bindings, span);
```

Ensure `use std::collections::HashSet;` is in scope in mod.rs (add if absent).

**3c. `lib.rs` — compute the set and thread it.**

In `analyze` (lib.rs), after `let imports = imports::Imports::build(&module);` (lib.rs:49), add — **use the fully-qualified path**, because the local `imports` value shadows the `imports` module name:

```rust
                    let module_bindings = crate::imports::module_bindings(&module);
```

Update the `analyze_unit` call (lib.rs:123):

```rust
                                    .push(detect::analyze_unit(unit, &file.path, &imports, &module_bindings, &span));
```

(Do **not** add `use std::collections::HashSet;` to lib.rs — it only binds the local `module_bindings` by inference and passes `&module_bindings`; it never *names* `HashSet`, so an import would be unused and fail `-D warnings`. `HashSet` is named only in the `mod.rs` `analyze_unit` signature and the `mutation.rs` `MutSink` field / `detect` signature — see 3a/3b.)

**3d. Thread the existing test call sites.** The new arg breaks every existing `detect(...)` / `analyze_unit(...)` / inline `MutSink {…}` call in tests. The new `detect_src` helper (Step 1) already uses the new signature. The remaining sites — **exhaustively enumerated** (verified against source); the `rg` below is a safety net, not the source of truth:

```bash
rg -n 'mutation::detect\(|[^_a-z]detect\(|analyze_unit\(|MutSink \{' crates/fxrank-lang-python/src crates/fxrank-lang-python/tests
```

- **Two fixture-parsing helpers in `mutation.rs`** — `mutation_effects` (calls `detect` at mutation.rs:614) **and `mutation_evidence`** (calls `detect` at mutation.rs:633). Both parse a fixture; in each, add `let module_bindings = crate::imports::module_bindings(&module);` after the `Imports::build`, and pass `&module_bindings` to the `detect(...)` call.
- **Two inline `detect(unit, &imports, &span)` test bodies** — `detect_accepts_imports_param` (mutation.rs:789) and `captured_binding_subreason_is_set` (mutation.rs:823) → `detect(unit, &imports, &module_bindings, &span)` (compute `module_bindings` from the `module` parsed just above each).
- **One inline `MutSink { … }` literal** (mutation.rs:758, the `push_hidden`-direct test) → add `module_bindings: &HashSet::new(),` after the `imports` field (that test calls `push_hidden` directly, bypassing the cascade, so an empty set keeps it isolated).
- **Exactly six `detect::analyze_unit(...)` test call sites in `mod.rs`** — mod.rs:1028, 1140, 1161, 1404, 1423, 1446. Each has its own `module` parsed locally in scope; in each, add `let module_bindings = crate::imports::module_bindings(&module);` and pass `&module_bindings`. (Where two share a helper, thread the helper once — confirm by the `rg` count that all six are covered and no seventh appears.)

`HashSet` is already imported in `mutation.rs` (mutation.rs:34); the inline `MutSink` literal's `&HashSet::new()` therefore needs no new import.

Then build the lib:

```bash
cargo build -p fxrank-lang-python
```

Expected: compiles (a `dead_code` warning on the unread `module_bindings` field is expected until Step 4; `cargo build` doesn't deny warnings).

- [ ] **Step 4: Add the escalation arm to the classify cascade**

In `mutation.rs`, insert the new arm **between** the F5 import arm (ends ~mutation.rs:475) and the F1 `push_hidden` catch-all (mutation.rs:483):

```rust
        // F2 analog (Python #29): root is a MODULE top-level binding (a
        // module-level name / def / class) whose contents are mutated
        // (subscript/attr/method) — module-shared state used for cross-function /
        // cross-module communication → global.mutation (class 6, Heuristic). A
        // bare rebind without `global` is a LOCAL (Python semantics) and already
        // won above; an explicit `global x` rebind already hit the globals arm.
        // So this catches exactly the content-mutation-of-module-container case.
        if self.module_bindings.contains(&root) {
            self.push(
                EffectKind::GlobalMutation,
                Tier::Heuristic,
                line,
                format!("{evidence} (module-level `{root}`)"),
                false,
            );
            return;
        }
```

- [ ] **Step 5: Run the new tests + the existing captured/global tests**

```bash
cargo test -p fxrank-lang-python --lib mutation
```

Expected: PASS, including the new `module_level_content_mutation_is_global` / `local_shadowing_module_binding_is_local`, **and** the unchanged `captured_binding_subreason_is_set` (its `inner`/`outer` enclosing-local case is unaffected — `outer` is not a module binding), `import_rooted_write_is_global_mutation`, `uses_global`, `plain_global_rebind`.

- [ ] **Step 6: Update the `mutation.rs` module-doc + F1 comment**

In `crates/fxrank-lang-python/src/detect/mutation.rs`: (a) add a row to the `## Escape classification table` (doc header ~line 11):

```rust
//! | module top-level binding, content-mutated (no `global`) | global.mutation | 6 | false |
```

(b) Update the F1 catch-all comment (mutation.rs ~477-483) so it no longer claims "or a module-level name" — that case now escalates above; the catch-all is now only a genuinely captured enclosing-function/opaque binding.

- [ ] **Step 7: Full suite + snapshots + gates**

```bash
cargo test --workspace
git status --porcelain crates/fxrank-lang-python/tests/snapshots/    # expect empty
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS/clean, and **`git status --porcelain crates/fxrank-lang-python/tests/snapshots/` is empty** — the new tests use inline source and touch no fixture, so `snapshots__dogfood_report.snap` must stay byte-identical. If a `.snap.new` appears, stop and investigate; do not accept it.

- [ ] **Step 8: Commit**

```bash
git add crates/fxrank-lang-python/src/detect/mutation.rs \
        crates/fxrank-lang-python/src/detect/mod.rs \
        crates/fxrank-lang-python/src/lib.rs
git commit -m "feat(python): escalate module-level binding content-mutation to global.mutation (#29 analog)

A Python write that mutates the CONTENTS of a module top-level binding without a
`global` declaration (`_cache[\"k\"]=1`, `shared.append(1)`) now classifies as
global.mutation (class 6) instead of the hidden.mutation/captured-binding
catch-all — the Python analog of #29 and the cross-language generalization of
spec 008's F2. The explicit `global x` rebind case already escalated via the
globals arm (unchanged); a genuinely captured enclosing-function local still
falls to captured-binding. A bare local rebind shadowing a module name wins as
local (locals checked first). Existing test helpers/call sites are threaded with
the new arg, but no existing fixture, snapshot, or behavioral expectation changed."
```

---

### Task 7: Cross-cutting docs + dogfood

Finalize the docs that span frontends and validate both TS and Python end-to-end.

**Files:**
- Modify: `CLAUDE.md` (resolve the *Cross-language mutation alignment → Known remaining gap → issue #29* paragraph; note Python alignment)

- [ ] **Step 1: Resolve the CLAUDE.md known-gap note**

In `CLAUDE.md`, the *Cross-language mutation alignment (spec 008)* section ends with **"Known remaining gap → issue #29"** (TS module-binding miss). Update it to state the gap is **resolved cross-language**: a module top-level binding write now classifies as `global.mutation`/6 in all three frontends — Rust via the `static` set (F2, pre-existing), TS via the `module_bindings` set (#29), Python via the module-level-name set for the content-mutation case (the explicit-`global` rebind already escalated). Note the residual heuristic limit: a function-scoped binding shadowing a module name resolves to local — flat syntactic binding sets (TS traversal-order; Python whole-function local pre-scan), both still short of full lexical-scope modeling.

- [ ] **Step 2: Dogfood verification (manual — acceptance; run before the Step 3 commit)**

**Prerequisites:** local checkouts of the `dogfood-repos` (memory) + `jq`. If unavailable, record "manual dogfood skipped (repo/jq unavailable)" in the PR and rely on the committed tests.

TS (issue #29's concrete case) — `set -o pipefail` so a scan failure isn't masked by `jq`:

```bash
set -o pipefail
cargo run -p fxrank -- scan <path-to>/114-kg-frontend/src/hooks/use-api-handler.ts | jq '.hotspots[].effects[] | select(.kind=="global.mutation")'
```

Expected: `globalActiveRequests` / `globalPendingRequests` `.set`/`.delete` writes appear as `global.mutation` (class 6), evidence naming the vars.

Python (find a module-level mutable container mutated from a function in a dogfood repo, e.g. Django/PyTorch):

```bash
set -o pipefail
cargo run -p fxrank -- scan <path-to>/django/django/ | jq '[.hotspots[].effects[] | select(.kind=="global.mutation")] | length'
```

Expected: a non-zero count where module-level caches/registries are content-mutated; spot-check a couple for plausibility (signal, not gospel). Record observations in the PR.

- [ ] **Step 3: Commit the doc** (only after Step 2 dogfood — so the "resolved" claim is evidence-backed; if dogfood reveals a miss, fix the code first and commit this last)

```bash
git add CLAUDE.md
git commit -m "docs: resolve #29 cross-language — module-binding -> global.mutation in all frontends"
```

---

## Self-Review

**1. Spec coverage:**
- Canonical rule in the descriptive SoT → Task 1 (guideline + spec 008). ✓
- Rust conformance (F2) → Task 2 (verify; no change expected). ✓
- TS module-binding → global (#29), captured-local stays hidden, shadow wins, evidence names var, snapshots stable, breaking F3/integration tests refit → Tasks 3–4. ✓
- Python module-level content-mutation → global; explicit-`global` rebind unchanged; captured enclosing-local stays hidden; shadow wins; additive (no break) → Tasks 5–6. ✓
- CLAUDE.md known-gap resolved cross-language; dogfood TS+Python → Task 7. ✓

**2. Placeholder scan:** every code step has complete code; commands have expected output. The Python tuple/list/starred destructuring case is now *implemented* (recursive collector, Task 5) — no longer a judgement call. The Python test call-site threading is exhaustively enumerated (Task 6 Step 3d), not left to an `rg` sweep. No TBD/TODO/ellipsis-as-placeholder.

**3. Type consistency:** `module_bindings: &HashSet<String>` is the threaded type at every hop in both frontends; `imports::module_bindings(&Module) -> HashSet<String>` is the single producer per frontend. TS arg position: after `imports`, before `extra_refs`/`ref_bindings`. Python arg position: after `imports`, before `span` — consistent within each frontend and matching every enumerated call site. Rust unchanged (already conformant).

**4. Cross-language consistency (the point of this plan):** all three frontends end at "module top-level binding write → `global.mutation`/6, captured enclosing-function local → `hidden.mutation`/3", with documented per-language realizations (Rust static set / TS module decls / Python module-level names + the explicit-`global` nuance). The guideline (Task 1) and CLAUDE.md (Task 7) record it; spec 008 notes it. ✓
