//! Shared callee-rendering helpers used by both `calls` and `risk` detectors.

use libcst_native::{Expression, Name};

/// Render a callee expression into a dotted string: `Name("open")` → `"open"`,
/// `Attribute(Name("requests"), "get")` → `"requests.get"`. Returns `None` for
/// shapes we don't model (calls-of-calls, subscript callees, etc.).
pub fn render_expr(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Name(n) => Some(n.value.to_owned()),
        Expression::Attribute(a) => {
            let base = render_expr(&a.value)?;
            Some(format!("{base}.{}", a.attr.value))
        }
        _ => None,
    }
}

/// The leftmost `Name` of an expression chain — the anchor for line resolution.
pub fn leftmost_name<'a>(expr: &'a Expression<'a>) -> Option<&'a Name<'a>> {
    match expr {
        Expression::Name(n) => Some(n),
        Expression::Attribute(a) => leftmost_name(&a.value),
        Expression::Call(c) => leftmost_name(&c.func),
        Expression::Subscript(s) => leftmost_name(&s.value),
        Expression::Await(a) => leftmost_name(&a.expression),
        _ => None,
    }
}
