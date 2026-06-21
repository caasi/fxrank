# React-aware Effect Detectors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `fxrank` score React function components by their own held effects and untraced state, so an agent can see which components are impure and push effects toward parents.

**Architecture:** All work lives in `fxrank-lang-ts` plus two small vocabulary additions to `fxrank-core`. A component (a `FnUnit` returning JSX) **inherits** the raw detected effects of the inline arrows it passes to built-in hooks (single-hop, by phase), and those inherited arrow units are suppressed. `useRef().current` writes, `useState`/`useReducer` declarations, and `useContext` reads are new per-unit signals.

**Tech Stack:** Rust, swc (`swc_ecma_ast`, `swc_ecma_visit`), `insta` snapshots, the existing `fxrank-core` scoring model.

## Global Constraints

- `fxrank-core` **must not** depend on any parser (`swc` must never leak in). New vocabulary is language-neutral.
- New effect kinds go in `EffectKind` with `wire()` + `base_class()`; new risk kinds in `RiskKind` with `wire()` + `class()`. Never hand-write wire strings at call sites.
- All React/inherited effects are emitted `contained = false` (not boundary-discountable).
- Every React signal is `Tier::Heuristic` and uses `detection_confidence(...)` for its confidence.
- CI gates (run before every push): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Source of truth: `specs/007-react-aware-effect-detectors.md`. When code and spec disagree, the spec wins.
- Detectors stay pure (return `Vec<Effect>` / `Vec<(Effect, bool)>`); assembly lives in `detect::analyze_unit` and the new inheritance pass.

---

## File Structure

- **Create** `crates/fxrank-lang-ts/src/react.rs` — React-specific syntax recognition: JSX detection, component detection (incl. `memo`/`forwardRef`), `useRef`-binding collection, `useState`/`useReducer` declaration → `StateTransition`, `useContext` → `AmbientRead`, and the hook-callback **inheritance map** (which arrow ids a component inherits, and the phase).
- **Modify** `crates/fxrank-core/src/effect.rs` — add `EffectKind::StateTransition`, `RiskKind::EffectInRender`, and the optional `Effect.subreason` field.
- **Modify** `crates/fxrank-lang-ts/src/detect/mutation.rs` — `useRef`-cell write classification (ref-binding set checked before `locals`).
- **Modify** `crates/fxrank-lang-ts/src/detect/mod.rs` — accept ref-bindings + React signals when assembling a `Hotspot`; mark world effects in a component render body with `EffectInRender`.
- **Modify** `crates/fxrank-lang-ts/src/lib.rs` — the two-pass inheritance assembly in `TsFrontend::analyze`.
- **Modify** `crates/fxrank-lang-ts/src/functions.rs` — name the inner function of `memo(...)`/`forwardRef(...)` after the outer binding.
- **Create** `crates/fxrank-lang-ts/tests/fixtures/react/*.tsx` + `crates/fxrank-lang-ts/tests/react.rs` — acceptance fixtures.

The inheritance map keys on a `FnUnit.id` (`path:line:col:symbol`): the component-body walk computes each inline hook arrow's `(line,col)` via `SpanLines`, which matches the arrow unit's own id (same span). That is the linkage between a component and its hook-callback arrow units — no parent pointers needed.

---

## Task 1: Core vocabulary — `StateTransition` kind + `EffectInRender` risk

**Files:**
- Modify: `crates/fxrank-core/src/effect.rs`
- Test: `crates/fxrank-core/src/effect.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces: `EffectKind::StateTransition` (wire `"state.transition"`, `base_class()` = 1); `RiskKind::EffectInRender` (wire `"effect.in.render"`, `class()` = 4).

- [ ] **Step 1: Write the failing test** — add to the `tests` mod in `effect.rs`:

```rust
#[test]
fn react_vocabulary_metadata() {
    assert_eq!(EffectKind::StateTransition.wire(), "state.transition");
    assert_eq!(EffectKind::StateTransition.base_class(), 1);
    assert_eq!(RiskKind::EffectInRender.wire(), "effect.in.render");
    assert_eq!(RiskKind::EffectInRender.class(), 4);
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-core react_vocabulary_metadata`
Expected: FAIL — `no variant named StateTransition`.

- [ ] **Step 3: Implement** — in `effect.rs`:
  - add `StateTransition` to `enum EffectKind`;
  - in `wire()`: `StateTransition => "state.transition",`;
  - in `base_class()`: add `StateTransition` to the `LocalMutation => 1` arm, i.e. `LocalMutation | StateTransition => 1,`;
  - add `EffectInRender` to `enum RiskKind`;
  - in `RiskKind::wire()`: `EffectInRender => "effect.in.render",`;
  - in `RiskKind::class()`: add `EffectInRender` to the class-4 arm: `BoxLeak | MemForget | ManuallyDrop | ProtoPollution | EffectInRender => 4,`.

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-core react_vocabulary_metadata`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/effect.rs
git commit -m "feat(core): add StateTransition kind and EffectInRender risk"
```

---

## Task 2: Core — optional `Effect.subreason` field

**Files:**
- Modify: `crates/fxrank-core/src/effect.rs`
- Test: `crates/fxrank-core/src/effect.rs`

**Interfaces:**
- Produces: `Effect.subreason: Option<String>` — serialized as `subreason`, skipped when `None`. Every existing `Effect { … }` literal must add `subreason: None`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn subreason_serializes_only_when_present() {
    let mut e = Effect {
        kind: EffectKind::HiddenMutation, class: 3, discounted_to: None, weight: 3,
        line: 1, tier: Tier::Heuristic, hidden: true, evidence: "x".into(),
        discount: None, confidence: 1.0, subreason: Some("ref-cell-write".into()),
    };
    let j = serde_json::to_string(&e).unwrap();
    assert!(j.contains("\"subreason\":\"ref-cell-write\""));
    e.subreason = None;
    assert!(!serde_json::to_string(&e).unwrap().contains("subreason"));
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-core subreason_serializes_only_when_present`
Expected: FAIL — `missing field subreason` / no such field.

- [ ] **Step 3: Implement** — add to `struct Effect`, after `discount`:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subreason: Option<String>,
```

Then fix every `Effect { … }` construction site to add `subreason: None` (compiler lists them): `detect/mutation.rs` (`record_write`), `detect/calls.rs` (`push`), and the Rust/Python frontends' effect constructors. (Search: `cargo build -p fxrank-core` first, then `cargo build --workspace` surfaces each call site.)

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-core subreason_serializes_only_when_present && cargo build --workspace`
Expected: PASS, workspace builds.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(core): add optional Effect.subreason for structured evidence"
```

---

## Task 3: React JSX + component detection (`react.rs`)

**Files:**
- Create: `crates/fxrank-lang-ts/src/react.rs`
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (add `pub mod react;`)
- Test: `crates/fxrank-lang-ts/src/react.rs`

**Interfaces:**
- Produces: `pub fn returns_jsx(body: &FnBodyOwned) -> bool` — true if any return path (or a bare expr body) yields a `JSXElement`/`JSXFragment`, descent stopping at nested functions/arrows.

- [ ] **Step 1: Write the failing test** — in `react.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::parse_and_collect;
    use crate::source::Lang;

    fn unit(src: &str, symbol: &str) -> crate::functions::FnUnit {
        parse_and_collect(src, "t.tsx", Lang::Tsx).unwrap()
            .into_iter().find(|u| u.symbol == symbol).expect("unit")
    }

    #[test]
    fn detects_jsx_return() {
        assert!(returns_jsx(&unit("function C(){ return <div/>; }", "C").body));
        assert!(returns_jsx(&unit("const C = () => <div/>;", "C").body));
        assert!(returns_jsx(&unit("function C(){ if (x) return null; return <p/>; }", "C").body));
        assert!(!returns_jsx(&unit("function f(){ return 1; }", "f").body));
        // nested JSX inside a callback does not make the OUTER a component:
        assert!(!returns_jsx(&unit("function f(){ items.map(() => <li/>); return 1; }", "f").body));
    }
}
```

(Confirm `Lang::Tsx` exists in `source.rs`; if the variant is spelled differently, use that.)

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts detects_jsx_return`
Expected: FAIL — `returns_jsx` not found.

- [ ] **Step 3: Implement** — in `react.rs`:

```rust
//! React-specific syntax recognition for the TS frontend.

use swc_ecma_ast::{Expr, ReturnStmt};
use swc_ecma_visit::{Visit, VisitWith};

use crate::functions::FnBodyOwned;

/// True if the function body yields JSX on at least one return path (or as a
/// bare arrow expression body). Descent stops at nested functions/arrows, so a
/// JSX-returning callback does not make its enclosing function a component.
pub fn returns_jsx(body: &FnBodyOwned) -> bool {
    match body {
        FnBodyOwned::Expr(e) => expr_is_jsx(e),
        FnBodyOwned::Block(_) => {
            let mut v = JsxReturnFinder { found: false };
            body.walk_with(&mut v);
            v.found
        }
    }
}

fn expr_is_jsx(e: &Expr) -> bool {
    matches!(e, Expr::JSXElement(_) | Expr::JSXFragment(_))
        || matches!(e, Expr::Paren(p) if expr_is_jsx(&p.expr))
        // `cond ? <a/> : <b/>` and `x && <a/>` are common JSX return shapes.
        || matches!(e, Expr::Cond(c) if expr_is_jsx(&c.cons) || expr_is_jsx(&c.alt))
        // only logical `&&` / `||` JSX shapes, not arbitrary binary exprs:
        || matches!(e, Expr::Bin(b)
            if matches!(b.op, swc_ecma_ast::BinaryOp::LogicalAnd | swc_ecma_ast::BinaryOp::LogicalOr)
            && expr_is_jsx(&b.right))
}

struct JsxReturnFinder { found: bool }

impl Visit for JsxReturnFinder {
    fn visit_return_stmt(&mut self, n: &ReturnStmt) {
        if let Some(arg) = &n.arg {
            if expr_is_jsx(arg) { self.found = true; }
        }
        // do not recurse further; returns inside nested fns are stopped below.
    }
    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}
```

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts detects_jsx_return`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/react.rs crates/fxrank-lang-ts/src/lib.rs
git commit -m "feat(ts): React JSX-return component detection"
```

---

## Task 4: `memo`/`forwardRef` outer-name attribution

**Files:**
- Modify: `crates/fxrank-lang-ts/src/functions.rs:309-327` (`visit_var_declarator`)
- Test: `crates/fxrank-lang-ts/src/functions.rs`

**Interfaces:**
- Produces: a unit named after the outer binding when its init is `memo(fn)` / `forwardRef(fn)` (so `const C = forwardRef(function(){return <i/>})` reports as `C`, not `<fn@…>`).
- Produces: a new `FnUnit.col: usize` field (set in `Collector::push` from the `(line, col)` it already computes) — Task 9's inheritance linkage needs `unit.col` directly and must **never** parse `col` out of `id` (both `path` and `symbol` can contain `:`).

- [ ] **Step 1: Write the failing test** — in the `functions.rs` tests:

```rust
#[test]
fn memo_forwardref_take_outer_name() {
    let names: Vec<_> = parse_and_collect(
        "const C = memo(function () { return null; }); \
         const D = forwardRef((props, ref) => <input ref={ref}/>);",
        "t.tsx", crate::source::Lang::Tsx,
    ).unwrap().into_iter().map(|u| u.symbol).collect();
    assert!(names.contains(&"C".to_string()), "got {names:?}");
    assert!(names.contains(&"D".to_string()), "got {names:?}");
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts memo_forwardref_take_outer_name`
Expected: FAIL — names are `<fn@…>` / `<arrow@…>`.

- [ ] **Step 3: Implement** — in `visit_var_declarator`, extend the `directly_callable` detection so a `memo(...)`/`forwardRef(...)` call whose first arg is an arrow/fn also passes `pending_name` to that inner arrow/fn. Add a helper and set `pending_name` accordingly:

```rust
fn react_wrapped_inner(init: Option<&Expr>) -> bool {
    let Some(Expr::Call(call)) = init else { return false };
    let callee_name = match &call.callee {
        swc_ecma_ast::Callee::Expr(e) => match e.as_ref() {
            Expr::Ident(i) => Some(i.sym.to_string()),
            // React.memo / React.forwardRef
            Expr::Member(m) => match &m.prop {
                swc_ecma_ast::MemberProp::Ident(i) => Some(i.sym.to_string()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    };
    matches!(callee_name.as_deref(), Some("memo") | Some("forwardRef"))
        && matches!(call.args.first().map(|a| a.expr.as_ref()), Some(Expr::Arrow(_)) | Some(Expr::Fn(_)))
}
```

Then in `visit_var_declarator`: `let directly_callable = matches!(...) || react_wrapped_inner(node.init.as_deref());`. Because `pending_name` is consumed by the **next** `visit_arrow_expr`/`visit_fn_expr`, and the wrapper call's argument arrow/fn is the next such node visited during `node.visit_children_with(self)`, the inner function picks up the outer name. The `react_wrapped_inner` guard (arg 0 must be the arrow/fn) is **load-bearing, not optional** — `forwardRef((props, ref) => …)` is arg 0, but a stray `memo(x, () => …)` must not mis-bind. Nested wrappers (`memo(forwardRef(fn))`) are a **documented miss** for Milestone-A (add a fixture noting it).

Also add `pub col: usize` to `struct FnUnit` (functions.rs:84) and set it in `Collector::push` (functions.rs:186) from the `(line, col)` tuple it already receives — Task 9 reads `unit.col` directly because the `id` cannot be safely split for `col`.

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts memo_forwardref_take_outer_name`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/functions.rs
git commit -m "feat(ts): name memo/forwardRef inner component after outer binding"
```

---

## Task 5: `useRef().current` write → `HiddenMutation`

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/mutation.rs`
- Test: `crates/fxrank-lang-ts/src/detect/mutation.rs`

**Interfaces:**
- Consumes: the existing `MutationWalker`.
- Produces: a write whose base ident is bound to `useRef(...)` classifies as `HiddenMutation` (class 3, `subreason: Some("ref-cell-write")`, `contained = false`), **before** the `locals.contains` arm.

- [ ] **Step 1: Write the failing test** — add to mutation.rs tests (use the existing fixture/parse helper there; if the helper is named differently, match it):

```rust
#[test]
fn useref_current_write_is_hidden_mutation() {
    // a component body: const r = useRef(0); r.current = 5;
    let effects = detect_in("function C(){ const r = useRef(0); r.current = 5; return null; }");
    let e = effects.iter().map(|(e,_)| e).find(|e| e.kind == EffectKind::HiddenMutation)
        .expect("hidden mutation");
    assert_eq!(e.effective_class(), 3);
    assert_eq!(e.subreason.as_deref(), Some("ref-cell-write"));
    // and it must NOT be classified as a contained local:
    assert!(effects.iter().all(|(e,contained)| !(*contained && e.kind == EffectKind::HiddenMutation)));
}
```

(`mutation.rs` has **no** `#[cfg(test)]` module yet — create one. There is nothing in this file to "mirror"; model the helper on `calls.rs`'s `kinds` helper (`calls.rs:254`): parse via `functions::parse_module`, build `SpanLines::new(cm)` and `ImportTable::from_module`, `functions::collect`, take the `C` unit, then call the real 5-arg signature `detect(&unit.body, &unit.sig, unit.is_constructor, &lines, &imports) -> Vec<(Effect, bool)>`.)

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts useref_current_write_is_hidden_mutation`
Expected: FAIL — currently `r` is a local → `LocalMutation` class 1.

- [ ] **Step 3: Implement** —
  - add a `ref_bindings: HashSet<String>` to `MutationWalker`, seeded in `visit_var_declarator` when the declarator init is a `useRef(...)` call (`const r = useRef(...)`): match `Expr::Call` whose callee renders to `useRef`, then insert the bound ident;
  - in `classify`, add a branch **before** `self.locals.contains(base)`:

```rust
} else if self.ref_bindings.contains(base) {
    Classification::new(HiddenMutation, 3, false, true, Tier::Heuristic, "ref cell")
        .with_subreason("ref-cell-write")
```

  - add a `subreason: Option<&'static str>` field to `struct Classification` (default `None` in `Classification::new`) plus a builder `fn with_subreason(mut self, s: &'static str) -> Self { self.subreason = Some(s); self }` (the `.with_subreason("ref-cell-write")` call above needs it defined); thread it into the emitted `Effect` as `subreason: c.subreason.map(String::from)`. Only `.current`-targeted writes should qualify: in `record_write`, when the base is a ref binding, confirm the place expression's member chain includes `.current` (guard so a non-`.current` write to the ref binding itself, rare, doesn't misfire). Reads are NOT handled (mutation walker only visits write sites — correct per spec).

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts useref_current_write_is_hidden_mutation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/detect/mutation.rs crates/fxrank-core/src/effect.rs
git commit -m "feat(ts): classify useRef().current writes as hidden mutation"
```

---

## Task 6: `useState`/`useReducer` declaration → `StateTransition`

**Files:**
- Modify: `crates/fxrank-lang-ts/src/react.rs`
- Test: `crates/fxrank-lang-ts/src/react.rs`

**Interfaces:**
- Produces: `pub fn state_transitions(body: &FnBodyOwned, lines: &SpanLines) -> Vec<Effect>` — one `StateTransition` (class 1, `contained` handled by caller as `false`, `subreason: "useState"`/`"useReducer"`) per literal `const [_, set] = useState(…)` / `useReducer(…)` declarator in the body (descent stopping at nested fns).

- [ ] **Step 1: Write the failing test:**

```rust
// parse_and_collect DROPS the SourceMap, so this helper must use parse_module
// (which returns the cm) + collect — it cannot wrap parse_and_collect.
fn unit_with_lines(src: &str, symbol: &str) -> (crate::functions::FnUnit, SpanLines) {
    use crate::functions::{collect, parse_module};
    let (module, cm) = parse_module(src, "t.tsx", Lang::Tsx).unwrap();
    let lines = SpanLines::new(cm);
    let u = collect(&module, "t.tsx", &lines).into_iter()
        .find(|u| u.symbol == symbol).expect("unit");
    (u, lines)
}

#[test]
fn usestate_decl_emits_state_transition() {
    let (u, lines) = unit_with_lines("function C(){ const [v,setV]=useState(0); return <i/>; }", "C");
    let effs = state_transitions(&u.body, &lines);
    assert_eq!(effs.len(), 1);
    assert_eq!(effs[0].kind, EffectKind::StateTransition);
    assert_eq!(effs[0].class, 1);
}
```

(`SpanLines::new(cm)` and `line_col` both exist in `source.rs`. This single `unit_with_lines` helper serves Tasks 6, 7, and 8.)

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts usestate_decl_emits_state_transition`
Expected: FAIL — `state_transitions` not found.

- [ ] **Step 3: Implement** — a visitor over the body that, for each `VarDeclarator` whose `init` is a `CallExpr` to ident `useState` or `useReducer`, pushes one `StateTransition` effect at the declarator's line. Stop at nested fns/arrows. Recognize literal `useState`/`useReducer` only (alias via custom hook is an accepted miss). Build the `Effect` with `tier: Tier::Heuristic`, `confidence: detection_confidence(Tier::Heuristic, false, false)`, `subreason: Some("useState")` (or `"useReducer"`), `hidden: false`, `discounted_to: None`, weight via `weight_for_class(1)`.

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts usestate_decl_emits_state_transition`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/react.rs
git commit -m "feat(ts): useState/useReducer declaration emits StateTransition"
```

---

## Task 7: `useContext(x)` → `AmbientRead`

**Files:**
- Modify: `crates/fxrank-lang-ts/src/react.rs`
- Test: `crates/fxrank-lang-ts/src/react.rs`

**Interfaces:**
- Produces: `pub fn context_reads(body: &FnBodyOwned, lines: &SpanLines) -> Vec<Effect>` — one `AmbientRead` (class 2, `subreason: "useContext-read"`) per `useContext(…)` call (descent stops at nested fns).

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn usecontext_emits_ambient_read() {
    let (u, lines) = unit_with_lines("function C(){ const t = useContext(Theme); return <i/>; }", "C");
    let effs = context_reads(&u.body, &lines);
    assert_eq!(effs.len(), 1);
    assert_eq!(effs[0].kind, EffectKind::AmbientRead);
    assert_eq!(effs[0].class, 2);
    assert_eq!(effs[0].subreason.as_deref(), Some("useContext-read"));
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts usecontext_emits_ambient_read`
Expected: FAIL.

- [ ] **Step 3: Implement** — a visitor matching `CallExpr` whose callee is ident `useContext`; push one `AmbientRead` per call at its line, stop at nested fns. `origin: unconfirmed` is implicit (no cross-file resolution — #25); record nothing extra.

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts usecontext_emits_ambient_read`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/react.rs
git commit -m "feat(ts): useContext read emits class-2 AmbientRead"
```

---

## Task 8: Hook-callback inheritance map

**Files:**
- Modify: `crates/fxrank-lang-ts/src/react.rs`
- Test: `crates/fxrank-lang-ts/src/react.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookPhase { Effect, Render }
/// arrow `(line,col)` -> phase, for every inline arrow passed directly to a
/// recognized built-in hook in `body`. Stops at nested fns (single-hop).
pub fn inherited_callbacks(body: &FnBodyOwned, lines: &SpanLines) -> std::collections::HashMap<(usize,usize), HookPhase>;
```
The `(line,col)` matches the inline arrow's `FnUnit` (same span as its id).

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn maps_inline_hook_callbacks_by_phase() {
    let (u, lines) = unit_with_lines(
        "function C(){ useEffect(() => { fetch('/a'); }, []); \
         const m = useMemo(() => fetch('/b'), []); return <i/>; }", "C");
    let map = inherited_callbacks(&u.body, &lines);
    let phases: Vec<_> = map.values().copied().collect();
    assert!(phases.contains(&HookPhase::Effect));
    assert!(phases.contains(&HookPhase::Render));
    assert_eq!(map.len(), 2);
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts maps_inline_hook_callbacks_by_phase`
Expected: FAIL.

- [ ] **Step 3: Implement** — a visitor over the component body. For each `CallExpr` whose callee ident is a known hook, if `args[0].expr` (and for `useState`/`useReducer` the lazy-init arg) is `Expr::Arrow`, record `(arrow.span → (line,col))` with the phase:
  - `useEffect`, `useLayoutEffect` → `HookPhase::Effect`;
  - `useMemo`, `useCallback` → `HookPhase::Render`;
  - `useState` → `HookPhase::Render` only for the **lazy-initializer arrow** (`args[0]` when it is an arrow);
  - `useReducer` → `HookPhase::Render` only for the **lazy `init` arrow**, which is the **third** argument: `useReducer(reducer, initialArg, init)` → `args[2]`. Do **not** inherit `useReducer`'s reducer (`args[0]`) or initial value (`args[1]`).
  Stop at nested fns/arrows so only **direct** hook-argument arrows are mapped (single-hop). Do not record event-handler arrows (they are JSX attributes, never hook call arguments).

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts maps_inline_hook_callbacks_by_phase`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/react.rs
git commit -m "feat(ts): build inline hook-callback inheritance map by phase"
```

---

## Task 9: Inheritance + suppression in `analyze`

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs` (expose raw effects + accept React signals)
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`TsFrontend::analyze`)
- Test: `crates/fxrank-lang-ts/tests/react.rs` (integration; create)

**Interfaces:**
- Consumes: `returns_jsx`, `inherited_callbacks`, `state_transitions`, `context_reads`, the ref-binding mutation change, the `HookPhase`.
- Produces: a component `Hotspot` whose effects include (a) its own-body effects, (b) `StateTransition`/`AmbientRead`/`useRef` signals, and (c) **inherited** raw effects from its hook-callback arrows (`contained = false`; `Render`-phase world effects gain an `EffectInRender` risk); the inherited arrow units are **dropped** from the output.

- [ ] **Step 1: Write the failing integration test** — `crates/fxrank-lang-ts/tests/react.rs`:

```rust
// Helper: analyze a .tsx source and return the report's hotspots.
mod util; // small parse+analyze wrapper, or inline the TsFrontend call

#[test]
fn useeffect_fetch_inherits_to_component_no_duplicate() {
    let hs = util::analyze_tsx(
        "function C(){ useEffect(() => { fetch('/api'); }, []); return <div/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").expect("component C");
    assert_eq!(c.max_class, 7, "C inherits the fetch at class 7");
    assert!(c.risk_features.iter().all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
            "effect-phase: no EffectInRender");
    // the inline arrow must NOT also appear as a separate hotspot:
    assert!(hs.iter().all(|h| !h.symbol.starts_with("<arrow@")),
            "inherited arrow suppressed");
}

#[test]
fn fetch_in_usememo_is_effect_in_render() {
    let hs = util::analyze_tsx(
        "function C(){ const x = useMemo(() => fetch('/b'), []); return <div/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
    assert!(c.risk_features.iter().any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender));
}

#[test]
fn useref_write_outranks_setter() {
    let hs = util::analyze_tsx(
        "function R(){ const r = useRef(0); r.current = 1; return <i/>; } \
         function S(){ const [v,setV]=useState(0); return <i onClick={()=>setV(1)}/>; }");
    let r = hs.iter().find(|h| h.symbol == "R").unwrap();
    let s = hs.iter().find(|h| h.symbol == "S").unwrap();
    assert!(r.max_class > s.max_class, "hidden ref ({}) > traced setter ({})", r.max_class, s.max_class);
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts --test react`
Expected: FAIL — components don't inherit; arrows still present.

- [ ] **Step 3: Implement** the two-pass in `TsFrontend::analyze`. **Preserve the existing `is_test_file` skip + `skipped_tests` tally (lib.rs:63–71)** — run this pass only on non-test files. Replace the per-unit scoring loop:

```rust
use crate::react;
use std::collections::{HashMap, HashSet};

// 1. Components, their ref-binding sets, and the inherited arrows they own.
let components: Vec<&FnUnit> = units.iter().filter(|u| react::returns_jsx(&u.body)).collect();
let comp_refs: HashMap<String, HashSet<String>> =
    components.iter().map(|c| (c.id.clone(), react::ref_bindings(&c.body))).collect();
let mut inherited: HashMap<(usize, usize), (String, react::HookPhase)> = HashMap::new();
for c in &components {
    for ((l, col), phase) in react::inherited_callbacks(&c.body, &lines) {
        inherited.insert((l, col), (c.id.clone(), phase));
    }
}

// 2. Score each unit, routing inherited arrows into their owning component.
let mut by_id: HashMap<String, Hotspot> = HashMap::new();
let mut order: Vec<String> = Vec::new();
let mut pending: HashMap<String, Vec<(react::HookPhase, detect::RawSignals)>> = HashMap::new();
for unit in &units {
    let key = (unit.line, unit.col); // unit.col is a real field (Task 4) — NEVER parse it from `id`
    if let Some((comp_id, phase)) = inherited.get(&key).cloned() {
        // Suppress this arrow as a standalone hotspot; stash its raw signals.
        // Pass the owning component's ref-binding set so a `r.current = …` write inside
        // this callback still classifies as ref-cell-write (the arrow alone can't know `r`
        // is a useRef binding declared in the component).
        let refs = comp_refs.get(&comp_id).cloned().unwrap_or_default();
        let raw = detect::raw_signals(unit, &imports, &lines, &refs);
        pending.entry(comp_id).or_default().push((phase, raw));
        continue;
    }
    let mut h = detect::analyze_unit(unit, &imports, &lines);
    if react::returns_jsx(&unit.body) {
        detect::augment_component(&mut h, unit, &lines);
    }
    by_id.insert(unit.id.clone(), h);
    order.push(unit.id.clone());
}
// 3. Fold inherited raw signals into each component, then recompute.
for (comp_id, raws) in pending {
    if let Some(h) = by_id.get_mut(&comp_id) {
        detect::absorb_inherited(h, raws);
    }
}
for id in order { output.functions.push(by_id.remove(&id).unwrap()); }
```

New `react.rs` helper this uses:
- `pub fn ref_bindings(body: &FnBodyOwned) -> HashSet<String>` — names bound by `const x = useRef(…)`; factor the Task-5 ref-binding scan so the mutation walker and this share one collector.

Add to `detect/mod.rs` (these own `Hotspot` scoring internals, so they live **here**, not in `react.rs`):
- `pub struct RawSignals { pub effects: Vec<Effect>, pub risks: Vec<RiskFeature> }`.
- `pub fn raw_signals(unit: &FnUnit, imports: &ImportTable, lines: &SpanLines, ref_bindings: &HashSet<String>) -> RawSignals` — run `gather` (refactor it to take the extra `ref_bindings` set, so `useRef` writes through an inherited callback get `subreason: ref-cell-write`) **plus** the coverage-owned `type.escape` risk (`coverage::analyze(...).has_any`) **and** `risk::detect`, returning the **pre-discount** effects + risks. Does not build a `Hotspot`.
- `pub fn augment_component(h: &mut Hotspot, unit: &FnUnit, lines: &SpanLines)` — push `react::state_transitions` + `react::context_reads` effects (all `contained = false`); for each of `h`'s own **world** effects (predicate below) add an `EffectInRender` risk; then `recompute(h)`.
- `pub fn absorb_inherited(h: &mut Hotspot, raws: Vec<(HookPhase, RawSignals)>)` — extend `h.effects` with each raw effect (force `contained = false`, `discounted_to = None`), extend `h.risk_features` with the raw risks; for `HookPhase::Render`, add one `EffectInRender` risk per **world** effect; then `recompute(h)`.
- `fn world_effect(kind: EffectKind) -> bool` — an **explicit kind match**, NOT a class threshold (`env.read`/`logging`/`panic` are class 4, so `>= 5` would miss them):

```rust
matches!(kind,
    EffectKind::NetFsDb | EffectKind::ProcessControl | EffectKind::EnvWrite
  | EffectKind::Concurrency | EffectKind::TimeRead | EffectKind::Random
  | EffectKind::EnvRead | EffectKind::Logging | EffectKind::Panic)
// NOT AmbientRead — useContext reuses it and must never trigger EffectInRender.
```

- `fn recompute(h: &mut Hotspot)` — recompute `own_score` (`own_score(&weights)`), `max_class` (`max_class(&classes, risk_class)`), `risk_weight` (`weight_for_class(risk_class)`, or 0 when no risks), and `confidence` (`function_confidence(&confs)`) from `h.effects`/`h.risk_features`. Call it from both `augment_component` and `absorb_inherited` (DRY).

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts --test react`
Expected: PASS (all three tests).

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/detect/mod.rs crates/fxrank-lang-ts/src/lib.rs crates/fxrank-lang-ts/src/react.rs crates/fxrank-lang-ts/tests/react.rs
git commit -m "feat(ts): component score inheritance from inline hook callbacks"
```

---

## Task 10: `EffectInRender` for render-body world effects + lifting fixture

**Files:**
- Modify: `crates/fxrank-lang-ts/src/react.rs` (`augment_component` render-body detection, if not finished in Task 9)
- Test: `crates/fxrank-lang-ts/tests/react.rs`

**Interfaces:**
- Consumes: `augment_component`.
- Produces: a world effect in a component's own render body carries `EffectInRender`; a presentational child with only props scores ~0; the parent declaring `useState` carries the `StateTransition`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn fetch_in_render_body_is_effect_in_render() {
    let hs = util::analyze_tsx("function C(){ fetch('/x'); return <div/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
    assert!(c.risk_features.iter().any(|r| r.kind == fxrank_core::effect::RiskKind::EffectInRender));
}

#[test]
fn lifting_makes_child_pure_parent_holds_state() {
    let hs = util::analyze_tsx(
        "function Parent(){ const [v,setV]=useState(0); return <Child value={v} onChange={setV}/>; } \
         function Child({value,onChange}){ return <input value={value} onChange={onChange}/>; }");
    let parent = hs.iter().find(|h| h.symbol == "Parent").unwrap();
    let child = hs.iter().find(|h| h.symbol == "Child").unwrap();
    assert!(parent.effects.iter().any(|e| e.kind == fxrank_core::effect::EffectKind::StateTransition));
    assert_eq!(child.max_class, 0, "presentational child is pure");
}

#[test]
fn onclick_handler_is_not_effect_in_render() {
    let hs = util::analyze_tsx("function C(){ return <button onClick={() => fetch('/x')}/>; }");
    let c = hs.iter().find(|h| h.symbol == "C").unwrap();
    assert!(c.risk_features.iter().all(|r| r.kind != fxrank_core::effect::RiskKind::EffectInRender),
            "event handler is not render-time");
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test -p fxrank-lang-ts --test react`
Expected: the new three fail until render-body / handler discrimination is exact.

- [ ] **Step 3: Implement / refine** `augment_component`: the EffectInRender pass must consider only world effects detected in the component's **own** statements (the `analyze_unit` call effects already stop at nested arrows, so `onClick` handler effects are not in `h.effects` — they live in the handler's own unit, which is NOT a hook callback and so is NOT suppressed and NOT inherited). Therefore "world effect present on the component's own Hotspot, not from inheritance" ⇒ render-body ⇒ `EffectInRender`. Ensure inherited effects are tagged so the render-body pass doesn't double-flag effect-phase inherited effects. (Concretely: run the render-body EffectInRender pass on own-body effects in `augment_component` BEFORE `absorb_inherited` adds inherited ones, and let `absorb_inherited` own the render-phase tagging for inherited effects.)

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test -p fxrank-lang-ts --test react`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/react.rs crates/fxrank-lang-ts/tests/react.rs
git commit -m "feat(ts): EffectInRender for render-body effects; lifting + handler fixtures"
```

---

## Task 11: Snapshot fixtures + dogfood note + CI gates

**Files:**
- Create: `crates/fxrank-lang-ts/tests/fixtures/react/{counter.tsx,effects.tsx,uncontrolled_cell.tsx}`
- Modify: `crates/fxrank-lang-ts/tests/react.rs` (insta snapshots)
- Modify: `CLAUDE.md` (a short "Dogfooding the React signals" note)

- [ ] **Step 1: Add fixtures** covering: a controlled counter (`useState` + setter handler), a component with `useEffect(fetch)` and a `useMemo(fetch)`, and a `useRef().current` cell write. Add one `insta` snapshot test asserting the full report shape per fixture.

```rust
#[test]
fn snapshot_react_fixtures() {
    for name in ["counter", "effects", "uncontrolled_cell"] {
        let src = std::fs::read_to_string(format!("tests/fixtures/react/{name}.tsx")).unwrap();
        let hs = util::analyze_tsx(&src);
        // dynamic snapshot name via suffix (the 2-arg assert_json_snapshot! form is version-sensitive):
        insta::with_settings!({ snapshot_suffix => name }, {
            insta::assert_json_snapshot!(hs);
        });
    }
}
```

- [ ] **Step 2: Generate + review snapshots**

Run: `cargo test -p fxrank-lang-ts --test react` then `cargo insta review`
Expected: snapshots created; review that components inherit/flag as designed, no stray `<arrow@…>` duplicates.

- [ ] **Step 3: Add a CLAUDE.md note** — a short paragraph under the TS dogfooding section describing the React signals (inheritance, `EffectInRender`, `useRef` cell, `StateTransition`) and the documented misses (single-hop nested arrows, custom-hook callbacks → #25, all-null components).

- [ ] **Step 4: Run the full CI gate locally**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/tests CLAUDE.md
git commit -m "test(ts): React acceptance fixtures + snapshots; dogfood note"
```

---

## Self-Review

**Spec coverage:**
- §3 gradient: StateTransition (Task 1/6), useContext AmbientRead (Task 7), useEffect baseline + render-phase (Task 8/9), useRef HiddenMutation (Task 5), EffectInRender (Task 1/9/10). ✓
- §4 inheritance (single-hop, suppress, raw-recompute, contained=false): Task 8/9. ✓
- §5 dispositions: useRef writes-only (Task 5), StateTransition at declaration (Task 6), useContext (Task 7); cuts are no-ops (nothing emits them). ✓
- §6 EffectInRender invariant (own body + render-phase; not event handlers): Task 9/10. ✓
- §8 detection (JSX/ext, component, memo/forwardRef): Task 3/4. File-level `.tsx`/JSX gate is honored because `returns_jsx` is the scoring gate; a non-JSX `.ts` simply has no components. ✓
- §10 acceptance bullets: Task 9/10/11 map 1:1. ✓

**Placeholder scan:** no "TBD"/"handle edge cases" — each step has code or an exact command. The few "match the existing helper" notes point at concrete, already-existing test helpers.

**Type consistency:** `HookPhase`, `inherited_callbacks`, `state_transitions`, `context_reads`, `raw_signals`, `augment_component`, `absorb_inherited`, `recompute` are used consistently across Tasks 8–10. `subreason` field added in Task 2 before first use in Task 5.

**Note for the executor:** Tasks 9–10 are the architecturally heavy ones; if `analyze_unit` cannot cleanly expose raw pre-discount effects, factor `gather` (already separate in `detect/mod.rs`) into the `raw_signals` entry point rather than duplicating walkers.

**Corrections applied after plan review (Claude + Codex):**
- `FnUnit` gains a `col: usize` field (Task 4); Task 9 uses `unit.col` — never parse `col` from `id` (path/symbol can contain `:`).
- `useReducer` lazy init is `args[2]`, not `args[0]` (Task 8).
- `augment_component`/`absorb_inherited`/`raw_signals`/`RawSignals` live in `detect/mod.rs` and are called as `detect::…`; `raw_signals` takes the owning component's ref-binding set and includes the coverage-owned `type.escape` risk (Task 9).
- `world_effect` is an explicit `EffectKind` match (not `class >= 5`) and excludes `AmbientRead` (Task 9).
- `Classification` gains a `subreason` field + `with_subreason` builder (Task 5); `mutation.rs` has no test module — create one modeled on `calls.rs`'s `kinds` helper.
- Test helper `unit_with_lines` uses `parse_module` + `collect` (not `parse_and_collect`, which drops the `SourceMap`) — shared by Tasks 6–8.
- `expr_is_jsx` restricts the binary-expr arm to logical `&&`/`||`; `memo(forwardRef(fn))` nesting is a documented miss.
- Task 9 must preserve the `is_test_file` / `skipped_tests` skip; Task 11 uses `with_settings!{ snapshot_suffix }` for dynamic snapshot names.
- Confirmed correct (no change): all `fxrank-core` APIs (`Effect` fields, `EffectKind`/`RiskKind` arms, `weight_for_class`/`own_score`/`max_class`/`function_confidence`/`detection_confidence`), all swc AST (`Expr::JSXElement`/`JSXFragment`/`Cond`/`Bin`/`Paren`), `Lang::Tsx`, and `Hotspot` fields.
