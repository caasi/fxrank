//! React-specific syntax recognition for the TS frontend.

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use std::collections::HashMap;
use swc_ecma_ast::{ArrowExpr, Expr, Pat, ReturnStmt, VarDeclarator};
use swc_ecma_visit::{Visit, VisitWith};

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
                Expr::Ident(i) => Some(i.sym.to_string()),
                _ => None,
            },
            _ => None,
        };
        if callee_name.as_deref() == Some("useContext") {
            let line = self.lines.line(node.span);
            let class: u8 = 2;
            let confidence = detection_confidence(Tier::Heuristic, false, false);
            self.effects.push(Effect {
                kind: EffectKind::AmbientRead,
                class,
                discounted_to: None,
                weight: weight_for_class(class),
                line,
                tier: Tier::Heuristic,
                hidden: false,
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
                Expr::Ident(i) => Some(i.sym.to_string()),
                _ => None,
            },
            _ => None,
        };
        let subreason = match callee_name.as_deref() {
            Some("useState") => "useState",
            Some("useReducer") => "useReducer",
            _ => return,
        };
        // Only emit for array-destructuring patterns (canonical `[v, setV]` shape).
        // Non-destructured `const state = useState(…)` does not match the hook contract.
        if !matches!(&node.name, Pat::Array(_)) {
            return;
        }
        let line = self.lines.line(node.span);
        let class: u8 = 1;
        let confidence = detection_confidence(Tier::Heuristic, false, false);
        self.effects.push(Effect {
            kind: EffectKind::StateTransition,
            class,
            discounted_to: None,
            weight: weight_for_class(class),
            line,
            tier: Tier::Heuristic,
            hidden: false,
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

/// The React lifecycle phase of an inline hook callback.
///
/// `Effect` — the callback runs after rendering and (for `useLayoutEffect`)
/// synchronously after DOM mutation. `Render` — the callback runs during the
/// render phase and must be side-effect-free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookPhase {
    Effect,
    Render,
}

/// Build a map from inline arrow `(line, col)` → [`HookPhase`] for every arrow
/// passed **directly** as a recognized hook argument in `body`.
///
/// "Directly" means single-hop: only arrows that are the immediate argument to
/// the hook call are recorded. Arrows nested inside those callbacks, or inside
/// any other nested function scope, are not mapped.
///
/// # Phase rules
/// - `useEffect`, `useLayoutEffect` → `HookPhase::Effect` for `args[0]`.
/// - `useMemo`, `useCallback` → `HookPhase::Render` for `args[0]`.
/// - `useState` → `HookPhase::Render` for `args[0]` only when it is an arrow
///   (the lazy-initializer form; skipped for non-arrow initial values).
/// - `useReducer` → `HookPhase::Render` for `args[2]` only (the optional `init`
///   function; `args[0]` / reducer and `args[1]` / initial state are NOT mapped).
///
/// The `(line, col)` key is computed via `lines.line_col(arrow.span)` — the same
/// call that `functions::collect` makes in `visit_arrow_expr` — so it matches
/// the `FnUnit`'s `(line, col)` that Task 9 uses to look up the arrow.
pub fn inherited_callbacks(
    body: &FnBodyOwned,
    lines: &SpanLines,
) -> HashMap<(usize, usize), HookPhase> {
    let mut walker = HookCallbackWalker {
        lines,
        map: HashMap::new(),
    };
    body.walk_with(&mut walker);
    walker.map
}

struct HookCallbackWalker<'a> {
    lines: &'a SpanLines,
    map: HashMap<(usize, usize), HookPhase>,
}

/// Return the hook name from a `CallExpr` callee, if it is a bare identifier.
fn hook_callee_name(node: &swc_ecma_ast::CallExpr) -> Option<&str> {
    match &node.callee {
        swc_ecma_ast::Callee::Expr(e) => match e.as_ref() {
            Expr::Ident(i) => Some(i.sym.as_ref()),
            _ => None,
        },
        _ => None,
    }
}

/// Record `arrow` in the walker's map with the given phase.
fn record_arrow(walker: &mut HookCallbackWalker<'_>, arrow: &ArrowExpr, phase: HookPhase) {
    let key = walker.lines.line_col(arrow.span);
    walker.map.insert(key, phase);
}

impl Visit for HookCallbackWalker<'_> {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        match hook_callee_name(node) {
            Some("useEffect") | Some("useLayoutEffect") => {
                // args[0] is the effect callback.
                if let Some(arg0) = node.args.first() {
                    if let Expr::Arrow(arrow) = arg0.expr.as_ref() {
                        record_arrow(self, arrow, HookPhase::Effect);
                    }
                }
            }
            Some("useMemo") | Some("useCallback") => {
                // args[0] is the memoized computation / stable callback.
                if let Some(arg0) = node.args.first() {
                    if let Expr::Arrow(arrow) = arg0.expr.as_ref() {
                        record_arrow(self, arrow, HookPhase::Render);
                    }
                }
            }
            Some("useState") => {
                // args[0] is the lazy initializer ONLY when it is an arrow.
                // A plain value (`useState(0)`) is not a callback.
                if let Some(arg0) = node.args.first() {
                    if let Expr::Arrow(arrow) = arg0.expr.as_ref() {
                        record_arrow(self, arrow, HookPhase::Render);
                    }
                }
            }
            Some("useReducer") => {
                // args[2] is the optional `init` function (lazy initializer).
                // args[0] = reducer, args[1] = initial state — not mapped.
                if let Some(arg2) = node.args.get(2) {
                    if let Expr::Arrow(arrow) = arg2.expr.as_ref() {
                        record_arrow(self, arrow, HookPhase::Render);
                    }
                }
            }
            _ => {
                // Not a recognized hook: recurse into the call so nested hook
                // calls inside non-hook calls are still found. Nested fn/arrow
                // scopes are stopped by the overrides below.
                node.visit_children_with(self);
            }
        }
        // Recognized hooks do not recurse further: hook arguments are arrow
        // scopes which are stopped by visit_arrow_expr below anyway, and we
        // don't want to accidentally pick up hook calls nested inside them.
    }

    // Stop descent at nested function scopes (single-hop only).
    fn visit_arrow_expr(&mut self, _n: &ArrowExpr) {}
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
        // do not recurse further; returns inside nested fns are stopped below.
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
    fn maps_inline_hook_callbacks_by_phase() {
        let (u, lines) = unit_with_lines(
            "function C(){ useEffect(() => { fetch('/a'); }, []); \
             const m = useMemo(() => fetch('/b'), []); return <i/>; }",
            "C",
        );
        let map = inherited_callbacks(&u.body, &lines);
        let phases: Vec<_> = map.values().copied().collect();
        assert!(phases.contains(&HookPhase::Effect));
        assert!(phases.contains(&HookPhase::Render));
        assert_eq!(map.len(), 2);
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
