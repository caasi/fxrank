//! React-specific syntax recognition for the TS frontend.

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use std::collections::HashSet;
use swc_ecma_ast::{ArrowExpr, Expr, Pat, ReturnStmt, VarDeclarator};
use swc_ecma_visit::{Visit, VisitWith};

use crate::detect::mutation::{collect_pat_bindings, is_use_ref_call};
use crate::functions::FnBodyOwned;
use crate::source::SpanLines;

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

/// Emit one `AmbientRead` (class 2) effect per `useContext(…)` call in the
/// component body.
///
/// Only bare-ident `useContext` calls in the function's own body are recognized;
/// descent stops at nested functions/arrows so a `useContext` inside a callback
/// does not attribute to the enclosing component. `React.useContext(…)` member
/// forms are an accepted Milestone-A miss — only literal callee idents match.
///
/// The argument is a context by React API contract; we do not resolve what it is
/// (cross-file resolution is issue #25).
pub fn context_reads(body: &FnBodyOwned, lines: &SpanLines) -> Vec<Effect> {
    let mut walker = ContextReadWalker {
        lines,
        effects: Vec::new(),
    };
    body.walk_with(&mut walker);
    walker.effects
}

struct ContextReadWalker<'a> {
    lines: &'a SpanLines,
    effects: Vec<Effect>,
}

impl Visit for ContextReadWalker<'_> {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        // Recognize bare-ident `useContext(…)` calls only.
        let callee_name = match &node.callee {
            swc_ecma_ast::Callee::Expr(e) => match e.as_ref() {
                Expr::Ident(i) => Some(i.sym.as_ref()),
                _ => None,
            },
            _ => None,
        };
        if callee_name == Some("useContext") {
            let (line, col) = self.lines.line_col(node.span);
            let class: u8 = 2;
            let confidence = detection_confidence(Tier::Heuristic, false, false);
            self.effects.push(Effect {
                kind: EffectKind::AmbientRead,
                class,
                discounted_to: None,
                weight: weight_for_class(class),
                line,
                col,
                tier: Tier::Heuristic,
                hidden: false,
                contained: false,
                evidence: "useContext(…)".to_string(),
                discount: None,
                subreason: Some("useContext-read".to_string()),
                confidence,
            });
        }
        // Recurse into the call's arguments (but not into nested fn scopes — those
        // are stopped by the overrides below).
        node.visit_children_with(self);
    }

    // Stop descent at nested function scopes.
    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

/// Emit one `StateTransition` (class 1) effect per literal `useState` /
/// `useReducer` declaration in the component body.
///
/// Only top-level declarations in the function's own body are recognized;
/// descent stops at nested functions/arrows so a `useState` inside a callback
/// does not attribute to the enclosing component. Alias hooks (custom names that
/// wrap `useState`) are an accepted miss — only literal callee idents match.
///
/// Attribution is at the DECLARATION (the `const [v, setV] = useState(…)` line),
/// not at setter call sites. This is the "component holds traced state" signal;
/// `contained` handling belongs to the Task 9 caller.
pub fn state_transitions(body: &FnBodyOwned, lines: &SpanLines) -> Vec<Effect> {
    let mut walker = StateTransitionWalker {
        lines,
        effects: Vec::new(),
    };
    body.walk_with(&mut walker);
    walker.effects
}

struct StateTransitionWalker<'a> {
    lines: &'a SpanLines,
    effects: Vec<Effect>,
}

impl Visit for StateTransitionWalker<'_> {
    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        // Recognize: `const [v, setV] = useState(…)` / `[s, dispatch] = useReducer(…)`
        // The init must be a call expression with a literal `useState`/`useReducer` callee.
        let Some(init) = &node.init else {
            return;
        };
        let Expr::Call(call) = init.as_ref() else {
            return;
        };
        let callee_name = match &call.callee {
            swc_ecma_ast::Callee::Expr(e) => match e.as_ref() {
                Expr::Ident(i) => Some(i.sym.as_ref()),
                _ => None,
            },
            _ => None,
        };
        let subreason = match callee_name {
            Some("useState") => "useState",
            Some("useReducer") => "useReducer",
            _ => return,
        };
        // Only emit for array-destructuring patterns (canonical `[v, setV]` shape).
        // Non-destructured `const state = useState(…)` does not match the hook contract.
        if !matches!(&node.name, Pat::Array(_)) {
            return;
        }
        let (line, col) = self.lines.line_col(node.span);
        let class: u8 = 1;
        let confidence = detection_confidence(Tier::Heuristic, false, false);
        self.effects.push(Effect {
            kind: EffectKind::StateTransition,
            class,
            discounted_to: None,
            weight: weight_for_class(class),
            line,
            col,
            tier: Tier::Heuristic,
            hidden: false,
            contained: false,
            evidence: format!("{subreason}(…)"),
            discount: None,
            subreason: Some(subreason.to_string()),
            confidence,
        });
        // Do NOT recurse into children — this is just a declarator visit.
        // Nested fns/arrows are stopped by the overrides below.
    }

    // Stop descent at nested function scopes so a useState call inside a
    // callback does not attribute to the enclosing component.
    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

/// Collect the names bound by `const x = useRef(…)` declarations in a component
/// body (`x` → in the returned set).
///
/// Only top-level declarations in the function's own body are recognized;
/// descent stops at nested functions/arrows so a `useRef` inside a callback is
/// not attributed to the enclosing component. This shares the `useRef`-call and
/// pattern-binding recognizers with the mutation walker, so the two agree on
/// what counts as a ref binding.
///
/// Task 9 threads this set into the mutation detection of the component's inline
/// hook callbacks: a `r.current = …` write inside an absorbed callback refers to
/// a ref declared here, in the component, not the callback's own scope.
pub fn ref_bindings(body: &FnBodyOwned) -> HashSet<String> {
    let mut walker = RefBindingWalker {
        bindings: HashSet::new(),
    };
    body.walk_with(&mut walker);
    walker.bindings
}

struct RefBindingWalker {
    bindings: HashSet<String>,
}

impl Visit for RefBindingWalker {
    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if let Some(init) = &node.init
            && is_use_ref_call(init)
        {
            collect_pat_bindings(&node.name, &mut self.bindings);
        }
        node.visit_children_with(self);
    }

    // Stop descent at nested function scopes so a useRef inside a callback does
    // not attribute to the enclosing component.
    fn visit_arrow_expr(&mut self, _n: &ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

/// The React lifecycle phase of an inline hook callback or handler.
///
/// Phase never decides *whether* an effect counts — only *how much* (spec 027
/// §2.4). It is an orthogonal axis to spec-003 containment.
///
/// - `Render` — the callback runs during render (`useMemo`, `useState`/
///   `useReducer` lazy initializers) and must be side-effect-free; world effects
///   here keep full weight AND earn an `EffectInRender` risk.
/// - `Effect` — runs after render, outside the render phase (`useEffect`,
///   `useLayoutEffect`). The honest baseline: ≈ full weight, no conditionality
///   discount, no `EffectInRender`.
/// - `Event` — runs only on interaction (`useCallback` bodies; JSX `onX={…}`
///   handlers). Effects here are *conditional on interaction*, so an escaping
///   world effect earns the 1-class conditionality discount (capped 1, floored 1).
/// - `Unknown` — a callback passed to an unrecognized `use[A-Z]…` hook. Ownership
///   is certain (the component hands it over), but the invocation schedule is
///   unknown: treated as event-phase `OwnedDeferred` with a lowered confidence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookPhase {
    Effect,
    Render,
    Event,
    Unknown,
}

/// True if `name` matches the React hook naming convention `/^use[A-Z]/`.
pub(crate) fn is_hook_name(name: &str) -> bool {
    name.starts_with("use")
        && name.len() > 3
        && name.chars().nth(3).map(char::is_uppercase).unwrap_or(false)
}

/// True if `name` is a React built-in hook with a **known signature**.
///
/// Built-in hooks have fixed, documented parameter layouts (callbacks are direct
/// positional args, never wrapped in an options object). An object arg to a
/// built-in hook is therefore **state data / config data**, NOT a callback bag —
/// it must NOT be descended for function-valued properties.
///
/// Custom / library hooks (`useMutation`, `useQuery`, `useForm`, …) are NOT in
/// this set; their object arg IS an opaque options/callback bag and should be
/// descended. Use this predicate to gate `handle_options_object` descent.
pub(crate) fn is_builtin_hook(name: &str) -> bool {
    // Complete set of React built-in hooks, grouped by release. Built-in hooks
    // have KNOWN, fixed parameter signatures: their object args are state data or
    // config, NOT callback bags, so they must NOT have object args descended.
    // Only genuinely-custom / library hooks (useMutation, useQuery, useForm, …)
    // get object-descent (they take opaque options/callback bags).
    matches!(
        name,
        // React 16.8 — core hooks
        "useState"
            | "useEffect"
            | "useContext"
            | "useReducer"
            | "useCallback"
            | "useMemo"
            | "useRef"
            | "useImperativeHandle"
            | "useLayoutEffect"
            | "useDebugValue"
            // React 18 — concurrent + utility hooks
            | "useDeferredValue"
            | "useTransition"
            | "useId"
            | "useSyncExternalStore"
            | "useInsertionEffect"
            // React 19 / react-dom — action + form hooks
            | "useOptimistic"
            | "useActionState"
            | "useFormStatus"
    )
}

/// Component-recognition outcome. `is` gates React treatment; `confidence`
/// lowers the component's function-level confidence when only one weak signal
/// fired (e.g. PascalCase alone, no JSX, no hooks).
pub struct ComponentSignal {
    pub is: bool,
    pub confidence: f64,
}

/// Decide whether `unit` in file `path` is a React component.
///
/// Recognition rule:
/// 1. The `symbol` must be **PascalCase** (first char `char::is_uppercase`). A
///    lowercase name (`helper`, `useThing`, etc.) is **never** a component
///    regardless of hook calls or JSX.
/// 2. At least one of:
///    - (a) `returns_jsx(&unit.body)` — strong signal.
///    - (b) `path` ends in `.tsx` or `.jsx` — medium signal.
///    - (c) the body calls at least one React hook (bare-ident callee matching
///      `/^use[A-Z]/`) — medium, corroborating only, never sufficient alone.
///
/// Confidence: `1.0` when `returns_jsx` holds or ≥2 weak signals fire;
/// `0.8` when exactly one medium signal fires alone. `is = false` ⇒
/// `confidence` is irrelevant (callers ignore it).
pub fn is_component(unit: &crate::functions::FnUnit, path: &str) -> ComponentSignal {
    // Gate 1: PascalCase symbol (first char uppercase).
    let first_is_upper = unit
        .symbol
        .chars()
        .next()
        .map(char::is_uppercase)
        .unwrap_or(false);
    if !first_is_upper {
        return ComponentSignal {
            is: false,
            confidence: 0.0,
        };
    }

    // Evaluate signals.
    let has_jsx = returns_jsx(&unit.body);
    let tsx_jsx_file = path.ends_with(".tsx") || path.ends_with(".jsx");
    let has_hooks = calls_a_hook(&unit.body);

    // Strong signal: returns JSX.
    if has_jsx {
        return ComponentSignal {
            is: true,
            confidence: 1.0,
        };
    }

    // Weak signals (medium): .tsx/.jsx file, or hook calls.
    let weak_count = usize::from(tsx_jsx_file) + usize::from(has_hooks);

    if weak_count >= 2 {
        // Two or more weak signals together → confident.
        ComponentSignal {
            is: true,
            confidence: 1.0,
        }
    } else if weak_count == 1 {
        // Exactly one weak signal → component, but lower confidence.
        ComponentSignal {
            is: true,
            confidence: 0.8,
        }
    } else {
        // PascalCase alone is insufficient.
        ComponentSignal {
            is: false,
            confidence: 0.0,
        }
    }
}

/// Return `true` if the function body calls at least one React hook — a
/// bare-ident callee matching `/^use[A-Z]/` — in the function's own body.
///
/// Descent stops at nested `visit_arrow_expr`/`visit_function` so only
/// direct (own-body) hook calls are counted. Namespace forms
/// (`React.useEffect(…)`) are an accepted miss (only bare idents match).
fn calls_a_hook(body: &FnBodyOwned) -> bool {
    let mut walker = HookCallFinder { found: false };
    body.walk_with(&mut walker);
    walker.found
}

struct HookCallFinder {
    found: bool,
}

impl Visit for HookCallFinder {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if self.found {
            return;
        }
        let is_hook = match &node.callee {
            swc_ecma_ast::Callee::Expr(e) => match e.as_ref() {
                Expr::Ident(i) => is_hook_name(i.sym.as_ref()),
                _ => false,
            },
            _ => false,
        };
        if is_hook {
            self.found = true;
            return;
        }
        node.visit_children_with(self);
    }

    // Stop descent at nested function scopes (own-body only).
    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
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

struct JsxReturnFinder {
    found: bool,
}

impl Visit for JsxReturnFinder {
    fn visit_return_stmt(&mut self, n: &ReturnStmt) {
        if let Some(arg) = &n.arg {
            if expr_is_jsx(arg) {
                self.found = true;
            }
        }
        // Do NOT call visit_children_with here. This prevents the visitor from
        // descending into the return expression's sub-expressions — however, that
        // is NOT what stops nested-function returns from being seen. Nested
        // function and arrow scopes are intercepted BEFORE any `return` inside
        // them is reached, by the empty `visit_arrow_expr` / `visit_function`
        // overrides below that terminate descent at scope boundaries.
    }
    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::parse_and_collect;
    use crate::source::Lang;

    fn unit(src: &str, symbol: &str) -> crate::functions::FnUnit {
        parse_and_collect(src, "t.tsx", Lang::Tsx)
            .unwrap()
            .into_iter()
            .find(|u| u.symbol == symbol)
            .expect("unit")
    }

    // parse_and_collect DROPS the SourceMap, so this helper uses parse_module
    // (which returns the cm) + collect — it cannot wrap parse_and_collect.
    fn unit_with_lines(src: &str, symbol: &str) -> (crate::functions::FnUnit, SpanLines) {
        use crate::functions::{collect, parse_module};
        let (module, cm) = parse_module(src, "t.tsx", Lang::Tsx).unwrap();
        let lines = SpanLines::new(cm);
        let u = collect(&module, "t.tsx", &lines)
            .into_iter()
            .find(|u| u.symbol == symbol)
            .expect("unit");
        (u, lines)
    }

    #[test]
    fn usecontext_emits_ambient_read() {
        let (u, lines) = unit_with_lines(
            "function C(){ const t = useContext(Theme); return <i/>; }",
            "C",
        );
        let effs = context_reads(&u.body, &lines);
        assert_eq!(effs.len(), 1);
        assert_eq!(effs[0].kind, EffectKind::AmbientRead);
        assert_eq!(effs[0].class, 2);
        assert_eq!(effs[0].subreason.as_deref(), Some("useContext-read"));
    }

    #[test]
    fn usestate_decl_emits_state_transition() {
        let (u, lines) = unit_with_lines(
            "function C(){ const [v,setV]=useState(0); return <i/>; }",
            "C",
        );
        let effs = state_transitions(&u.body, &lines);
        assert_eq!(effs.len(), 1);
        assert_eq!(effs[0].kind, EffectKind::StateTransition);
        assert_eq!(effs[0].class, 1);
    }

    #[test]
    fn recognizes_return_null_component() {
        let u = unit("function Widget(){ useState(0); return null; }", "Widget");
        let s = super::is_component(&u, "src/Widget.tsx");
        assert!(
            s.is,
            "PascalCase + hook call ⇒ component even when it returns null"
        );
    }

    #[test]
    fn pascalcase_tsx_alone_is_lower_confidence_component() {
        let u = unit("function Box(){ return null; }", "Box");
        let s = super::is_component(&u, "src/Box.tsx");
        assert!(
            s.is && s.confidence < 1.0,
            "single weak signal ⇒ lower confidence"
        );
    }

    #[test]
    fn lowercase_helper_is_not_component() {
        let u = unit("function helper(){ useThing(); return null; }", "helper");
        assert!(
            !super::is_component(&u, "src/h.tsx").is,
            "lowercase name ⇒ not a component"
        );
    }

    #[test]
    fn jsx_returning_component_is_full_confidence() {
        let u = unit("function C(){ return <div/>; }", "C");
        let s = super::is_component(&u, "src/C.tsx");
        assert!(s.is && (s.confidence - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn detects_jsx_return() {
        assert!(returns_jsx(
            &unit("function C(){ return <div/>; }", "C").body
        ));
        assert!(returns_jsx(&unit("const C = () => <div/>;", "C").body));
        assert!(returns_jsx(
            &unit("function C(){ if (x) return null; return <p/>; }", "C").body
        ));
        assert!(!returns_jsx(&unit("function f(){ return 1; }", "f").body));
        // nested JSX inside a callback does not make the OUTER a component:
        assert!(!returns_jsx(
            &unit("function f(){ items.map(() => <li/>); return 1; }", "f").body
        ));
    }
}
