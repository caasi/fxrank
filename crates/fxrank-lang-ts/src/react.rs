//! React-specific syntax recognition for the TS frontend.

use swc_ecma_ast::{Expr, ReturnStmt};
use swc_ecma_visit::Visit;

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
