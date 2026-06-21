//! Signature annotation-slot coverage + `Any`/decorator analysis — the Python
//! analog of `fxrank-lang-ts`'s `coverage.rs`. The project thesis ("types lower
//! the score") made operational for gradual-typed Python: we measure how much of
//! a function's signature is *explicitly annotated* (its boundary), so the
//! boundary-containment discount in `analyze_unit` can shift **contained** effects
//! down when the boundary is honest, and void the discount when `Any` poisons it.
//!
//! # Slots (spec §"Signature coverage")
//! A "slot" is one declared boundary position: each parameter (**excluding**
//! `self`/`cls` — convention never annotates them), plus one return slot.
//! `*args` / `**kwargs` are **one slot each** (a typed star-param counts; an
//! untyped one degrades coverage — the escape-hatch rule). A slot is **typed**
//! iff it carries an *explicit* annotation whose top-level type is **not** `Any`.
//! `t` = typed slots, `S` = total slots → `None` (`t = 0`), `Partial`
//! (`0 < t < S`), `Full` (`t = S`).
//!
//! # `Any` poison (two cases, spec §"Poison & confidence rules")
//! - **Signature** (`x: Any`, `-> Any`) → that slot is untyped *and*
//!   `any_in_signature` is set (an `Any`-typed boundary is a non-boundary).
//! - **Body** (`cast(Any, …)`, an `Any`-annotated local) → `any_in_body` is set;
//!   `analyze_unit` forces the boundary shift to 0 (the discount is voided).
//!
//! Both cases drive a `type.escape` risk in `analyze_unit`.
//!
//! # Decorators (spec §"Poison & confidence rules")
//! An unknown / dynamic decorator (outside a known-pure allowlist) does **not**
//! degrade coverage — the written annotations are real signal — but lowers the
//! function's confidence ("typed, but a wrapper may be lying").
//!
//! **Top-level only.** `Any` detection is shallow: a parameter typed `list[Any]`
//! or `dict[str, Any]` counts as *typed* (its top-level type is `list`/`dict`,
//! not the bare `Any` name). Matching the Milestone-A scope, we do not descend
//! into subscript type arguments.

use fxrank_core::score::BoundaryCoverage;
use libcst_native::{
    CompoundStatement, Decorator, Expression, OrElse, Param, SmallStatement, StarArg, Statement,
    Suite,
};

use crate::detect::expr::render_expr;
use crate::functions::{FnBody, FnUnit};
use crate::imports::Imports;

/// Annotation-slot coverage + `Any`/decorator signals for one function unit.
pub struct Coverage {
    /// The boundary tier fed to `apply_boundary_discount`.
    pub boundary: BoundaryCoverage,
    /// A signature slot's explicit annotation top-level type IS `Any`.
    pub any_in_signature: bool,
    /// The body contains `cast(Any, …)` or an `Any`-annotated local (`AnnAssign`).
    pub any_in_body: bool,
    /// A decorator is outside the known-pure allowlist.
    pub unknown_decorator: bool,
}

/// Compute the annotation-slot coverage + `Any`/decorator signals of `unit`.
///
/// `imports` lets `typing.Any` / `typing.cast` be recognized by resolving the
/// attribute base through the file's import table (so an aliased `import typing as
/// t` matches, while an unrelated `mymod.Any` / `obj.cast(...)` does not).
pub fn of(unit: &FnUnit, imports: &Imports) -> Coverage {
    let mut typed = 0usize;
    let mut total = 0usize;
    let mut any_in_signature = false;

    // ── parameter slots (excluding self/cls) ─────────────────────────────────
    let mut visit_param = |p: &Param, total: &mut usize, typed: &mut usize| {
        if is_receiver(p.name.value) {
            return;
        }
        *total += 1;
        match classify_annotation(p.annotation.as_ref().map(|a| &a.annotation), imports) {
            SlotKind::Typed => *typed += 1,
            SlotKind::Any => any_in_signature = true,
            SlotKind::Untyped => {}
        }
    };

    for p in unit
        .params
        .posonly_params
        .iter()
        .chain(&unit.params.params)
        .chain(&unit.params.kwonly_params)
    {
        visit_param(p, &mut total, &mut typed);
    }
    // `*args` / `**kwargs`: one slot each (a bare `*` separator is NOT a slot).
    if let Some(StarArg::Param(p)) = &unit.params.star_arg {
        visit_param(p, &mut total, &mut typed);
    }
    if let Some(p) = &unit.params.star_kwarg {
        visit_param(p, &mut total, &mut typed);
    }

    // ── return slot ──────────────────────────────────────────────────────────
    total += 1;
    match classify_annotation(unit.returns.map(|a| &a.annotation), imports) {
        SlotKind::Typed => typed += 1,
        SlotKind::Any => any_in_signature = true,
        SlotKind::Untyped => {}
    }

    let boundary = if total > 0 && typed == total {
        BoundaryCoverage::Full
    } else if typed > 0 {
        BoundaryCoverage::Partial
    } else {
        BoundaryCoverage::None
    };

    Coverage {
        boundary,
        any_in_signature,
        any_in_body: body_has_any(&unit.body, imports),
        unknown_decorator: unit.decorators.iter().any(|d| !is_pure_decorator(d)),
    }
}

/// First-param receiver names excluded from the slot count.
fn is_receiver(name: &str) -> bool {
    name == "self" || name == "cls"
}

/// Per-slot annotation classification.
enum SlotKind {
    /// Explicit annotation, top-level type is not `Any` → typed slot.
    Typed,
    /// Explicit annotation, top-level type IS `Any` → untyped + signature poison.
    Any,
    /// No explicit annotation → untyped slot.
    Untyped,
}

/// Classify an optional annotation expression by its top-level type.
fn classify_annotation(ann: Option<&Expression>, imports: &Imports) -> SlotKind {
    match ann {
        None => SlotKind::Untyped,
        Some(expr) if is_any_type(expr, imports) => SlotKind::Any,
        Some(_) => SlotKind::Typed,
    }
}

/// Is `expr` the bare `Any` type (top-level only)? Accepts a `Name("Any")`
/// (`from typing import Any`) or an **attribute whose base resolves to `typing`**
/// (`typing.Any`, or an aliased `import typing as t; t.Any`).
///
/// Resolving the base through the import table (rather than matching any final
/// `.Any` component) avoids falsely poisoning on an unrelated `mymod.Any`.
fn is_any_type(expr: &Expression, imports: &Imports) -> bool {
    match expr {
        Expression::Name(n) => n.value == "Any",
        Expression::Attribute(a) => a.attr.value == "Any" && base_is_typing(&a.value, imports),
        _ => false,
    }
}

/// Does an attribute's base expression resolve to the `typing` module through the
/// import table? Renders the base to a dotted string and resolves its root, so a
/// bare `typing.X` (`import typing`) and an aliased `t.X` (`import typing as t`)
/// both match, while an unrelated `mymod.X` does not.
fn base_is_typing(base: &Expression, imports: &Imports) -> bool {
    let Some(rendered) = render_expr(base) else {
        return false;
    };
    let root = rendered.split('.').next().unwrap_or(&rendered);
    imports.resolve(root) == Some("typing")
}

// ─── decorator allowlist ──────────────────────────────────────────────────────

/// Is `dec` a known-pure decorator that does not erase the signature?
///
/// Allowlist (spec): `property`, `staticmethod`, `classmethod`, `dataclass`,
/// `abstractmethod`, `functools.wraps`, `functools.cached_property`,
/// `abc.abstractmethod`, and framework route decorators (`app.route`, `app.get`,
/// `app.post`, … — any `*.route`/`*.get`/`*.post`/`*.put`/`*.delete`/`*.patch`).
fn is_pure_decorator(dec: &Decorator) -> bool {
    // A decorator may be a bare name, an attribute, or a call (`@app.route("/")`).
    // Unwrap a call to its callee, then match the name/attribute form.
    let callee = match &dec.decorator {
        Expression::Call(c) => c.func.as_ref(),
        other => other,
    };
    match callee {
        Expression::Name(n) => matches!(
            n.value,
            "property" | "staticmethod" | "classmethod" | "dataclass" | "abstractmethod"
        ),
        Expression::Attribute(a) => {
            // `functools.wraps`, `functools.cached_property`.
            if matches!(a.attr.value, "wraps" | "cached_property") {
                return true;
            }
            // `dataclass` imported via attribute (`dataclasses.dataclass`).
            if a.attr.value == "dataclass" {
                return true;
            }
            // `abc.abstractmethod` and similar bare `abstractmethod` attributes.
            if a.attr.value == "abstractmethod" {
                return true;
            }
            // Framework route decorators: `app.route`, `router.get`, `bp.post`, …
            is_route_method(a.attr.value)
        }
        _ => false,
    }
}

/// HTTP-style framework route decorator method names.
fn is_route_method(name: &str) -> bool {
    matches!(
        name,
        "route" | "get" | "post" | "put" | "delete" | "patch" | "head" | "options"
    )
}

// ─── body `Any` detection (`cast(Any, …)` / `Any`-annotated local) ────────────

fn body_has_any(body: &FnBody, imports: &Imports) -> bool {
    match body {
        FnBody::Suite(suite) => suite_has_any(suite, imports),
        // A lambda body is a single expression; only a `cast(Any, …)` could appear.
        FnBody::Expr(e) => expr_has_any(e, imports),
    }
}

fn suite_has_any(suite: &Suite, imports: &Imports) -> bool {
    match suite {
        Suite::IndentedBlock(b) => b.body.iter().any(|s| stmt_has_any(s, imports)),
        Suite::SimpleStatementSuite(s) => s.body.iter().any(|s| small_has_any(s, imports)),
    }
}

fn stmt_has_any(stmt: &Statement, imports: &Imports) -> bool {
    match stmt {
        Statement::Simple(line) => line.body.iter().any(|s| small_has_any(s, imports)),
        Statement::Compound(c) => compound_has_any(c, imports),
    }
}

fn compound_has_any(c: &CompoundStatement, imports: &Imports) -> bool {
    match c {
        // Nested def/lambda/class — their own units; do not descend.
        CompoundStatement::FunctionDef(_) | CompoundStatement::ClassDef(_) => false,
        CompoundStatement::If(i) => {
            expr_has_any(&i.test, imports)
                || suite_has_any(&i.body, imports)
                || i.orelse
                    .as_ref()
                    .is_some_and(|o| orelse_has_any(o, imports))
        }
        CompoundStatement::For(f) => {
            expr_has_any(&f.iter, imports)
                || suite_has_any(&f.body, imports)
                || f.orelse
                    .as_ref()
                    .is_some_and(|e| suite_has_any(&e.body, imports))
        }
        CompoundStatement::While(w) => {
            expr_has_any(&w.test, imports)
                || suite_has_any(&w.body, imports)
                || w.orelse
                    .as_ref()
                    .is_some_and(|e| suite_has_any(&e.body, imports))
        }
        CompoundStatement::Try(t) => {
            suite_has_any(&t.body, imports)
                || t.handlers.iter().any(|h| suite_has_any(&h.body, imports))
                || t.orelse
                    .as_ref()
                    .is_some_and(|e| suite_has_any(&e.body, imports))
                || t.finalbody
                    .as_ref()
                    .is_some_and(|e| suite_has_any(&e.body, imports))
        }
        CompoundStatement::TryStar(t) => {
            suite_has_any(&t.body, imports)
                || t.handlers.iter().any(|h| suite_has_any(&h.body, imports))
                || t.orelse
                    .as_ref()
                    .is_some_and(|e| suite_has_any(&e.body, imports))
                || t.finalbody
                    .as_ref()
                    .is_some_and(|e| suite_has_any(&e.body, imports))
        }
        CompoundStatement::With(w) => {
            w.items.iter().any(|item| expr_has_any(&item.item, imports))
                || suite_has_any(&w.body, imports)
        }
        CompoundStatement::Match(m) => {
            expr_has_any(&m.subject, imports)
                || m.cases
                    .iter()
                    .any(|case| suite_has_any(&case.body, imports))
        }
    }
}

fn orelse_has_any(orelse: &OrElse, imports: &Imports) -> bool {
    match orelse {
        OrElse::Elif(elif) => {
            expr_has_any(&elif.test, imports)
                || suite_has_any(&elif.body, imports)
                || elif
                    .orelse
                    .as_ref()
                    .is_some_and(|o| orelse_has_any(o, imports))
        }
        OrElse::Else(e) => suite_has_any(&e.body, imports),
    }
}

fn small_has_any(small: &SmallStatement, imports: &Imports) -> bool {
    match small {
        // `x: Any = …` / `x: Any` — an `Any`-annotated local.
        SmallStatement::AnnAssign(a) => {
            if is_any_type(&a.annotation.annotation, imports) {
                return true;
            }
            a.value.as_ref().is_some_and(|v| expr_has_any(v, imports))
        }
        SmallStatement::Assign(a) => expr_has_any(&a.value, imports),
        SmallStatement::AugAssign(a) => expr_has_any(&a.value, imports),
        SmallStatement::Expr(e) => expr_has_any(&e.value, imports),
        SmallStatement::Return(r) => r.value.as_ref().is_some_and(|v| expr_has_any(v, imports)),
        SmallStatement::Raise(r) => r.exc.as_ref().is_some_and(|e| expr_has_any(e, imports)),
        _ => false,
    }
}

/// Walk an expression for a `cast(Any, …)` call.
fn expr_has_any(expr: &Expression, imports: &Imports) -> bool {
    match expr {
        Expression::Call(c) => {
            if is_cast_any(c, imports) {
                return true;
            }
            expr_has_any(&c.func, imports) || c.args.iter().any(|a| expr_has_any(&a.value, imports))
        }
        Expression::Attribute(a) => expr_has_any(&a.value, imports),
        Expression::Subscript(s) => {
            expr_has_any(&s.value, imports)
                || s.slice
                    .iter()
                    .any(|el| base_slice_has_any(&el.slice, imports))
        }
        Expression::BinaryOperation(b) => {
            expr_has_any(&b.left, imports) || expr_has_any(&b.right, imports)
        }
        Expression::BooleanOperation(b) => {
            expr_has_any(&b.left, imports) || expr_has_any(&b.right, imports)
        }
        Expression::UnaryOperation(u) => expr_has_any(&u.expression, imports),
        Expression::Comparison(c) => {
            expr_has_any(&c.left, imports)
                || c.comparisons
                    .iter()
                    .any(|cmp| expr_has_any(&cmp.comparator, imports))
        }
        Expression::IfExp(i) => {
            expr_has_any(&i.test, imports)
                || expr_has_any(&i.body, imports)
                || expr_has_any(&i.orelse, imports)
        }
        // List/set/tuple literals.
        Expression::List(l) => l.elements.iter().any(|e| element_has_any(e, imports)),
        Expression::Set(s) => s.elements.iter().any(|e| element_has_any(e, imports)),
        Expression::Tuple(t) => t.elements.iter().any(|e| element_has_any(e, imports)),
        Expression::Dict(d) => d.elements.iter().any(|el| match el {
            libcst_native::DictElement::Simple { key, value, .. } => {
                expr_has_any(key, imports) || expr_has_any(value, imports)
            }
            libcst_native::DictElement::Starred(s) => expr_has_any(&s.value, imports),
        }),
        // Comprehensions (eager and lazy alike — a `cast(Any, …)` anywhere in the
        // body re-opens the boundary regardless of execution timing).
        Expression::ListComp(l) => {
            expr_has_any(&l.elt, imports) || comp_for_has_any(&l.for_in, imports)
        }
        Expression::SetComp(s) => {
            expr_has_any(&s.elt, imports) || comp_for_has_any(&s.for_in, imports)
        }
        Expression::DictComp(d) => {
            expr_has_any(&d.key, imports)
                || expr_has_any(&d.value, imports)
                || comp_for_has_any(&d.for_in, imports)
        }
        Expression::GeneratorExp(g) => {
            expr_has_any(&g.elt, imports) || comp_for_has_any(&g.for_in, imports)
        }
        Expression::FormattedString(fs) => fs.parts.iter().any(|p| {
            if let libcst_native::FormattedStringContent::Expression(e) = p {
                if expr_has_any(&e.expression, imports) {
                    return true;
                }
                // `{x:{cast(Any, y)}}` — format_spec parts are eager.
                if let Some(spec_parts) = &e.format_spec {
                    return spec_parts.iter().any(|sp| {
                        matches!(sp, libcst_native::FormattedStringContent::Expression(se) if expr_has_any(&se.expression, imports))
                    });
                }
            }
            false
        }),
        // Nested def/lambda are their own units — do not descend into a lambda body.
        Expression::Lambda(_) => false,
        Expression::Await(a) => expr_has_any(&a.expression, imports),
        Expression::Yield(y) => y.value.as_ref().is_some_and(|v| match v.as_ref() {
            libcst_native::YieldValue::Expression(e) => expr_has_any(e, imports),
            libcst_native::YieldValue::From(f) => expr_has_any(&f.item, imports),
        }),
        Expression::NamedExpr(n) => expr_has_any(&n.value, imports),
        Expression::StarredElement(s) => expr_has_any(&s.value, imports),
        _ => false,
    }
}

/// Does an `Element` (list/set/tuple member) contain a `cast(Any, …)`?
fn element_has_any(el: &libcst_native::Element, imports: &Imports) -> bool {
    match el {
        libcst_native::Element::Simple { value, .. } => expr_has_any(value, imports),
        libcst_native::Element::Starred(s) => expr_has_any(&s.value, imports),
    }
}

/// Does a comprehension `for … in …` clause contain a `cast(Any, …)`?
fn comp_for_has_any(comp: &libcst_native::CompFor, imports: &Imports) -> bool {
    expr_has_any(&comp.iter, imports)
        || comp.ifs.iter().any(|c| expr_has_any(&c.test, imports))
        || comp
            .inner_for_in
            .as_ref()
            .is_some_and(|i| comp_for_has_any(i, imports))
}

/// Does a subscript slice (`Index`/`Slice`) contain a `cast(Any, …)`?
fn base_slice_has_any(slice: &libcst_native::BaseSlice, imports: &Imports) -> bool {
    match slice {
        libcst_native::BaseSlice::Index(i) => expr_has_any(&i.value, imports),
        libcst_native::BaseSlice::Slice(s) => {
            s.lower.as_ref().is_some_and(|e| expr_has_any(e, imports))
                || s.upper.as_ref().is_some_and(|e| expr_has_any(e, imports))
                || s.step.as_ref().is_some_and(|e| expr_has_any(e, imports))
        }
    }
}

/// Is `call` a `cast(Any, …)` (the first type argument is the bare `Any` type)?
/// Matches a bare `cast(...)` (`from typing import cast`) and a `typing.cast(...)`
/// whose base **resolves to `typing`** through the import table (so an aliased
/// `t.cast(...)` matches, but an unrelated `obj.cast(...)` does not). The first
/// positional argument must be the bare `Any` type.
fn is_cast_any(call: &libcst_native::Call, imports: &Imports) -> bool {
    let is_cast = match call.func.as_ref() {
        Expression::Name(n) => n.value == "cast",
        Expression::Attribute(a) => a.attr.value == "cast" && base_is_typing(&a.value, imports),
        _ => false,
    };
    if !is_cast {
        return false;
    }
    call.args
        .first()
        .is_some_and(|arg| is_any_type(&arg.value, imports))
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SpanIndex;

    /// Parse `src`, build imports, and return `Coverage` for the unit named `symbol`.
    fn coverage_of(src: &str, symbol: &str) -> Coverage {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == symbol)
            .expect("unit not found");
        of(unit, &imports)
    }

    /// Copilot FIX 3: a signature `typing.Any` (with `import typing`) must poison
    /// (resolves through the import table), but an unrelated `mymod.Any` must NOT.
    ///
    /// Pre-fix `is_any_type` matched ANY attribute whose final component is `Any`,
    /// so `mymod.Any` falsely poisoned.
    #[test]
    fn signature_typing_any_poisons_but_unrelated_attr_any_does_not() {
        let typing_any = coverage_of(
            "import typing\ndef f(x: typing.Any) -> int:\n    return 0\n",
            "f",
        );
        assert!(
            typing_any.any_in_signature,
            "typing.Any (import typing) must set any_in_signature"
        );

        let aliased = coverage_of(
            "import typing as t\ndef f(x: t.Any) -> int:\n    return 0\n",
            "f",
        );
        assert!(
            aliased.any_in_signature,
            "aliased t.Any (import typing as t) must set any_in_signature"
        );

        let unrelated = coverage_of(
            "import mymod\ndef f(x: mymod.Any) -> int:\n    return 0\n",
            "f",
        );
        assert!(
            !unrelated.any_in_signature,
            "unrelated mymod.Any must NOT set any_in_signature (does not resolve to typing)"
        );
    }

    /// Copilot FIX 4: `typing.cast(Any, x)` in the body (with `import typing` and a
    /// bare `Any` in scope) must be detected as body-Any, but `obj.cast(Any, x)`
    /// where `obj` is not `typing` must NOT.
    ///
    /// Pre-fix `is_cast_any` matched any attribute call whose final component is
    /// `cast`, so `obj.cast(...)` falsely fired.
    #[test]
    fn body_typing_cast_any_detected_but_unrelated_cast_not() {
        // `import typing` for the base; `from typing import Any` so the bare `Any`
        // arg resolves through the Name arm.
        let typing_cast = coverage_of(
            "import typing\nfrom typing import Any\ndef f(x: int) -> int:\n    y = typing.cast(Any, x)\n    return y\n",
            "f",
        );
        assert!(
            typing_cast.any_in_body,
            "typing.cast(Any, x) must set any_in_body"
        );

        let unrelated_cast = coverage_of(
            "import obj\nfrom typing import Any\ndef f(x: int) -> int:\n    y = obj.cast(Any, x)\n    return y\n",
            "f",
        );
        assert!(
            !unrelated_cast.any_in_body,
            "obj.cast(Any, x) (obj not typing) must NOT set any_in_body"
        );
    }
}
