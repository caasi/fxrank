//! Per-function effect/risk detection and `Hotspot` assembly.
//!
//! `detect/mod.rs` owns the **own-body recursion driver** ([`walk_own_body`]) — the
//! single place that decides, per the spec's wrapper/inner-call attribution rules,
//! which sub-nodes are *evaluated in the enclosing body* (and so charged to this
//! function) and which are *deferred* (a nested `def`/`lambda` body, or a lazy
//! generator-expression element body — their own unit or simply uncounted).
//!
//! Detectors ([`calls`], later `mutation`/`risk`) stay pure: they receive the driver's
//! callbacks and push `Effect`s. `analyze_unit` is the single owner of turning the
//! collected effects/risks into a scored [`Hotspot`].

pub mod calls;
pub mod expr;
pub mod mutation;
pub mod refs;
pub mod risk;

use std::collections::HashSet;

use crate::coverage;
use crate::functions::{FnBody, FnUnit};
use crate::imports::Imports;
use crate::source::SpanIndex;
use fxrank_core::confidence::function_confidence;
use fxrank_core::effect::{RiskFeature, RiskKind, Tier};
use fxrank_core::model::Hotspot;
use fxrank_core::score::{
    BoundaryCoverage, apply_boundary_discount, max_class, own_score, weight_for_class,
};

use libcst_native::{
    Assert, AssignTargetExpression, Call, CompoundStatement, Decorator, Element, Expression,
    FormattedStringContent, Parameters, Raise, SmallStatement, Statement, Suite,
};

/// A sink that receives the **eagerly-evaluated** effect sites of a function's own
/// body, as decided by [`walk_own_body`]. Each method is a `classify_* → push` hook.
pub trait EffectSink {
    /// A function/method call evaluated in the enclosing body.
    fn on_call(&mut self, call: &Call);
    /// A bare `assert` statement (conditional abort; stripped under `-O`).
    fn on_assert(&mut self, assert: &Assert);
    /// A `raise` statement.
    fn on_raise(&mut self, raise: &Raise);
    /// An assignment target that may be an env write (`os.environ[...] = …`) or a
    /// mutation. `is_aug` is true for an augmented assignment (`+=`, `|=`, …),
    /// false for a plain `=`. A plain `=` to a **bare local name** is a *binding*,
    /// not a mutation of pre-existing state (spec §"effect table": `local.mutation`
    /// is `.append()` / `d[k] = …` / `+=` on a locally-created binding — not the
    /// binding itself); subscript/attribute `=` targets still mutate.
    fn on_assign_target(&mut self, target: &AssignTargetExpression, is_aug: bool);
    /// An attribute read that may be an ambient-read signal (e.g. `sys.argv`).
    /// Default: no-op (most sinks don't care).
    fn on_attribute_read(&mut self, _attr: &Expression) {}
}

/// Walk a function-unit's **own body** and drive `sink` over every effect site that
/// is *evaluated in the enclosing body*, per the spec's attribution rules.
///
/// Descends into: the body suite (or lambda body expr), `with`-items, **eager**
/// list/set/dict-comprehension element + iterable expressions, f-string format
/// expressions, and — for any **nested** `def`/`lambda` encountered while walking
/// — that nested callable's **decorators** and **parameter default** expressions
/// (they run when the nested `def`/`lambda` statement executes, i.e. in THIS
/// function's body → charged here).
///
/// Does **not** descend into: a nested `def` body or a `Lambda` body (their own
/// units), nor a **generator-expression** element/condition body (lazy — only its
/// outermost iterable runs in the enclosing body, so only that is descended). It
/// also never charges **annotation** expressions (lazy/stringized — Task 9 inspects
/// them only syntactically).
///
/// Crucially it does **not** charge THIS unit's OWN decorators / parameter defaults
/// to itself: those ran in the unit's *enclosing* scope (when its own `def`
/// statement executed), not when the unit is called. They are own-body effects of
/// the enclosing function (or, for a top-level def, of module scope → uncounted),
/// and are charged there by the enclosing unit's own `walk_own_body` pass.
pub fn walk_own_body<'a>(unit: &FnUnit<'a>, sink: &mut dyn EffectSink) {
    match &unit.body {
        FnBody::Suite(suite) => walk_suite(suite, sink),
        FnBody::Expr(expr) => walk_expr(expr, sink),
        // The synthetic `<module>` unit: walk each top-level statement.
        // Special handling for top-level `ClassDef`: Python class bodies run at
        // class-definition / import time, so class-level statements ARE import-time
        // effects of `<module>`.  Walk the class body suite directly so that
        // `class C: DATA = open("y")` charges `open("y")` to `<module>`.
        // Nested `def` / method bodies inside the class are still their own units;
        // `walk_suite` → `walk_compound` → `FunctionDef` → `walk_nested_def_header`
        // already handles that (decorators + defaults only, NOT the method body).
        // For all other statements (including top-level `FunctionDef`), the existing
        // `walk_statement` → `walk_compound` boundary is correct:
        //   top-level FunctionDef → `walk_nested_def_header` (decorators + defaults, NOT body)
        //   everything else       → descend normally
        FnBody::Module(stmts) => {
            for stmt in *stmts {
                match stmt {
                    Statement::Compound(CompoundStatement::ClassDef(class_def)) => {
                        // Walk the class body to capture class-level import-time effects.
                        // `walk_suite` already makes nested `def` (methods) charge only
                        // their header (decorators/defaults), never their bodies.
                        walk_suite(&class_def.body, sink);
                        // Also walk the class HEADER expressions, which run at
                        // class-definition / import time (per guideline):
                        //   @register(handler)          — decorator expression
                        //   class C(make_base(), metaclass=M())  — base/keyword args
                        // These are import-time effects charged to `<module>`, mirroring
                        // how `walk_nested_def_header` handles a function's decorators and
                        // parameter defaults (run in the enclosing scope, not inside).
                        walk_class_header(class_def, sink);
                    }
                    other => walk_statement(other, sink),
                }
            }
        }
    }
}

/// Descend into a nested callable's **decorators** + **parameter default** value
/// expressions (charged to the CURRENT function), without entering its body. Used
/// for a nested `def` (decorators + defaults) and a nested `lambda` (defaults only;
/// Python `lambda`s carry no decorators).
fn walk_nested_def_header(def: &libcst_native::FunctionDef, sink: &mut dyn EffectSink) {
    for dec in &def.decorators {
        walk_decorator(dec, sink);
    }
    walk_param_defaults(&def.params, sink);
}

/// Walk a top-level `ClassDef`'s **header expressions** — decorators and
/// base-class / keyword-argument expressions — which run at class-definition /
/// import time and are therefore import-time effects of `<module>`.
///
/// Mirrors `walk_nested_def_header` (function decorators + param defaults).
/// Does NOT descend into the class body (handled separately by the caller).
fn walk_class_header(class_def: &libcst_native::ClassDef, sink: &mut dyn EffectSink) {
    for dec in &class_def.decorators {
        walk_decorator(dec, sink);
    }
    // `bases` covers positional base-class args: `class C(Base(), ...)`.
    // `keywords` covers keyword args: `class C(metaclass=M())`.
    // Both are `Vec<Arg>` with a `value: Expression` field.
    for arg in class_def.bases.iter().chain(class_def.keywords.iter()) {
        walk_expr(&arg.value, sink);
    }
}

fn walk_decorator(dec: &Decorator, sink: &mut dyn EffectSink) {
    walk_expr(&dec.decorator, sink);
}

fn walk_param_defaults(params: &Parameters, sink: &mut dyn EffectSink) {
    let all = params
        .posonly_params
        .iter()
        .chain(&params.params)
        .chain(&params.kwonly_params);
    for p in all {
        if let Some(default) = &p.default {
            walk_expr(default, sink);
        }
    }
    // star_arg / star_kwarg may carry defaults too (rare), handle for completeness.
    if let Some(libcst_native::StarArg::Param(p)) = &params.star_arg
        && let Some(default) = &p.default
    {
        walk_expr(default, sink);
    }
    if let Some(p) = &params.star_kwarg
        && let Some(default) = &p.default
    {
        walk_expr(default, sink);
    }
}

// ─── statement traversal ──────────────────────────────────────────────────────

fn walk_suite(suite: &Suite, sink: &mut dyn EffectSink) {
    match suite {
        Suite::IndentedBlock(b) => {
            for stmt in &b.body {
                walk_statement(stmt, sink);
            }
        }
        Suite::SimpleStatementSuite(s) => {
            for small in &s.body {
                walk_small(small, sink);
            }
        }
    }
}

fn walk_statement(stmt: &Statement, sink: &mut dyn EffectSink) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                walk_small(small, sink);
            }
        }
        Statement::Compound(c) => walk_compound(c, sink),
    }
}

fn walk_compound(compound: &CompoundStatement, sink: &mut dyn EffectSink) {
    match compound {
        // Nested `def` is its OWN unit — do NOT descend into its body. But its
        // decorators + parameter defaults run when THIS `def` statement executes
        // (in the enclosing body) → charge them to the CURRENT function.
        CompoundStatement::FunctionDef(d) => walk_nested_def_header(d, sink),
        // A nested class's methods are their own units; do not descend.
        CompoundStatement::ClassDef(_) => {}
        CompoundStatement::If(i) => {
            walk_expr(&i.test, sink);
            walk_suite(&i.body, sink);
            if let Some(orelse) = &i.orelse {
                walk_or_else(orelse, sink);
            }
        }
        CompoundStatement::For(f) => {
            walk_expr(&f.iter, sink);
            walk_suite(&f.body, sink);
            if let Some(orelse) = &f.orelse {
                walk_suite(&orelse.body, sink);
            }
        }
        CompoundStatement::While(w) => {
            walk_expr(&w.test, sink);
            walk_suite(&w.body, sink);
            if let Some(orelse) = &w.orelse {
                walk_suite(&orelse.body, sink);
            }
        }
        CompoundStatement::Try(t) => {
            walk_suite(&t.body, sink);
            for handler in &t.handlers {
                walk_suite(&handler.body, sink);
            }
            if let Some(orelse) = &t.orelse {
                walk_suite(&orelse.body, sink);
            }
            if let Some(finalbody) = &t.finalbody {
                walk_suite(&finalbody.body, sink);
            }
        }
        CompoundStatement::TryStar(t) => {
            walk_suite(&t.body, sink);
            for handler in &t.handlers {
                walk_suite(&handler.body, sink);
            }
            if let Some(orelse) = &t.orelse {
                walk_suite(&orelse.body, sink);
            }
            if let Some(finalbody) = &t.finalbody {
                walk_suite(&finalbody.body, sink);
            }
        }
        CompoundStatement::With(w) => {
            // `with open(...) as f:` — the with-items are evaluated in the enclosing
            // body, so descend into them (wrapper attribution).
            for item in &w.items {
                walk_expr(&item.item, sink);
            }
            walk_suite(&w.body, sink);
        }
        CompoundStatement::Match(m) => {
            walk_expr(&m.subject, sink);
            for case in &m.cases {
                walk_suite(&case.body, sink);
            }
        }
    }
}

fn walk_or_else(orelse: &libcst_native::OrElse, sink: &mut dyn EffectSink) {
    match orelse {
        libcst_native::OrElse::Elif(elif) => {
            walk_expr(&elif.test, sink);
            walk_suite(&elif.body, sink);
            if let Some(inner) = &elif.orelse {
                walk_or_else(inner, sink);
            }
        }
        libcst_native::OrElse::Else(e) => {
            walk_suite(&e.body, sink);
        }
    }
}

fn walk_small(small: &SmallStatement, sink: &mut dyn EffectSink) {
    match small {
        SmallStatement::Expr(e) => walk_expr(&e.value, sink),
        SmallStatement::Return(r) => {
            if let Some(v) = &r.value {
                walk_expr(v, sink);
            }
        }
        SmallStatement::Assign(a) => {
            for target in &a.targets {
                sink.on_assign_target(&target.target, false);
                walk_assign_target_subexprs(&target.target, sink);
            }
            walk_expr(&a.value, sink);
        }
        SmallStatement::AnnAssign(a) => {
            // The annotation is NOT charged (lazy/stringized). The value IS.
            sink.on_assign_target(&a.target, false);
            walk_assign_target_subexprs(&a.target, sink);
            if let Some(v) = &a.value {
                walk_expr(v, sink);
            }
        }
        SmallStatement::AugAssign(a) => {
            sink.on_assign_target(&a.target, true);
            walk_assign_target_subexprs(&a.target, sink);
            walk_expr(&a.value, sink);
        }
        SmallStatement::Assert(a) => {
            sink.on_assert(a);
            walk_expr(&a.test, sink);
            if let Some(msg) = &a.msg {
                walk_expr(msg, sink);
            }
        }
        SmallStatement::Raise(r) => {
            sink.on_raise(r);
            if let Some(exc) = &r.exc {
                walk_expr(exc, sink);
            }
        }
        // Pass / Break / Continue / Import / ImportFrom / Global / Nonlocal /
        // Del / TypeAlias hold no eagerly-evaluated effect sites we charge.
        _ => {}
    }
}

/// Descend into an **assignment target's** eagerly-evaluated sub-expressions, so
/// effects/risks/awaits *inside* the target are charged to the enclosing body.
///
/// Assignment targets evaluate some sub-expressions eagerly: `xs[f()] = v`
/// evaluates `f()` (the subscript index) and `get_obj().attr = v` evaluates
/// `get_obj()` (the attribute base). The mutation detector separately classifies
/// the target's **root** (`xs` / `get_obj`) via `on_assign_target`; this walk only
/// feeds the target's index/base sub-expressions to `walk_expr`, so it adds the
/// `f()` / `get_obj()` effects **without** re-classifying (or double-counting) the
/// target's mutation — `walk_expr` never calls `on_assign_target`, and the
/// mutation sink's `on_call` only fires for mutating *methods* (an attribute-call
/// like `requests.get(u)` is not one).
fn walk_assign_target_subexprs(target: &AssignTargetExpression, sink: &mut dyn EffectSink) {
    match target {
        // A bare name target evaluates nothing — the root is the mutation, no sub-exprs.
        AssignTargetExpression::Name(_) => {}
        // `obj.attr = v` / `get_obj().attr = v` — the base expression is eagerly
        // evaluated. Walk it (a bare `obj` Name yields nothing; a `get_obj()` Call
        // surfaces its effect).
        AssignTargetExpression::Attribute(a) => walk_expr(&a.value, sink),
        // `xs[k] = v` / `get_dict()[k] = v` — both the base value AND the index/slice
        // are eagerly evaluated. Walk both (the base may itself be an effectful call;
        // the index expression may contain calls/awaits like `xs[f()]`).
        AssignTargetExpression::Subscript(s) => {
            walk_expr(&s.value, sink);
            for element in &s.slice {
                walk_base_slice(&element.slice, sink);
            }
        }
        // Destructuring targets — recurse into each element's nested target sub-exprs.
        AssignTargetExpression::Tuple(t) => {
            for el in &t.elements {
                walk_target_element(el, sink);
            }
        }
        AssignTargetExpression::List(l) => {
            for el in &l.elements {
                walk_target_element(el, sink);
            }
        }
        AssignTargetExpression::StarredElement(s) => walk_target_value(&s.value, sink),
    }
}

/// Walk a destructuring-target element (`(a, b[f()]) = …`) for nested target
/// sub-expressions.
fn walk_target_element(el: &Element, sink: &mut dyn EffectSink) {
    match el {
        Element::Simple { value, .. } => walk_target_value(value, sink),
        Element::Starred(s) => walk_target_value(&s.value, sink),
    }
}

/// Walk a target-position **expression** (an element of a tuple/list target) for
/// its eagerly-evaluated sub-expressions, mirroring `walk_assign_target_subexprs`
/// but over an `Expression` (destructuring elements are typed as expressions).
fn walk_target_value(expr: &Expression, sink: &mut dyn EffectSink) {
    match expr {
        Expression::Name(_) => {}
        Expression::Attribute(a) => walk_expr(&a.value, sink),
        Expression::Subscript(s) => {
            walk_expr(&s.value, sink);
            for element in &s.slice {
                walk_base_slice(&element.slice, sink);
            }
        }
        Expression::Tuple(t) => {
            for el in &t.elements {
                walk_target_element(el, sink);
            }
        }
        Expression::List(l) => {
            for el in &l.elements {
                walk_target_element(el, sink);
            }
        }
        Expression::StarredElement(s) => walk_target_value(&s.value, sink),
        _ => {}
    }
}

// ─── expression traversal ─────────────────────────────────────────────────────

fn walk_expr(expr: &Expression, sink: &mut dyn EffectSink) {
    match expr {
        Expression::Call(c) => {
            sink.on_call(c);
            walk_expr(&c.func, sink);
            for arg in &c.args {
                walk_expr(&arg.value, sink);
            }
        }
        // A nested `lambda` is its OWN unit — do NOT descend into its body. But its
        // parameter defaults run when the `lambda` expression is evaluated (in the
        // enclosing body) → charge them to the CURRENT function. (Lambdas carry no
        // decorators in Python.)
        Expression::Lambda(l) => walk_param_defaults(&l.params, sink),

        Expression::Attribute(a) => {
            sink.on_attribute_read(expr);
            walk_expr(&a.value, sink);
        }
        Expression::Subscript(s) => {
            // Fire on_attribute_read for `sys.argv[N]` — the subscript's value may be
            // `sys.argv` (an Attribute), which the recursive walk_expr will also surface.
            // The sink is responsible for deduplication if it tracks both forms.
            walk_expr(&s.value, sink);
            // The index/slice expression(s) are eagerly evaluated (`xs[f()]`,
            // `xs[a:b]`) → descend into them too.
            for element in &s.slice {
                walk_base_slice(&element.slice, sink);
            }
        }
        Expression::BinaryOperation(b) => {
            walk_expr(&b.left, sink);
            walk_expr(&b.right, sink);
        }
        Expression::BooleanOperation(b) => {
            walk_expr(&b.left, sink);
            walk_expr(&b.right, sink);
        }
        Expression::UnaryOperation(u) => walk_expr(&u.expression, sink),
        Expression::Comparison(c) => {
            walk_expr(&c.left, sink);
            for comp in &c.comparisons {
                walk_expr(&comp.comparator, sink);
            }
        }
        Expression::IfExp(i) => {
            walk_expr(&i.test, sink);
            walk_expr(&i.body, sink);
            walk_expr(&i.orelse, sink);
        }
        Expression::Tuple(t) => {
            for el in &t.elements {
                walk_element(el, sink);
            }
        }
        Expression::List(l) => {
            for el in &l.elements {
                walk_element(el, sink);
            }
        }
        Expression::Set(s) => {
            for el in &s.elements {
                walk_element(el, sink);
            }
        }
        Expression::Dict(d) => {
            for el in &d.elements {
                match el {
                    libcst_native::DictElement::Simple { key, value, .. } => {
                        walk_expr(key, sink);
                        walk_expr(value, sink);
                    }
                    libcst_native::DictElement::Starred(s) => walk_expr(&s.value, sink),
                }
            }
        }
        // EAGER comprehensions: descend into both the element and the iterable
        // (both evaluated in the enclosing body).
        Expression::ListComp(l) => {
            walk_expr(&l.elt, sink);
            walk_comp_for(&l.for_in, sink, true);
        }
        Expression::SetComp(s) => {
            walk_expr(&s.elt, sink);
            walk_comp_for(&s.for_in, sink, true);
        }
        Expression::DictComp(d) => {
            walk_expr(&d.key, sink);
            walk_expr(&d.value, sink);
            walk_comp_for(&d.for_in, sink, true);
        }
        // LAZY generator expression: only its OUTERMOST iterable runs in the
        // enclosing body. The element + condition bodies are deferred → NOT charged
        // (no separate unit — simply uncounted). `eager = false` walks only iterables.
        Expression::GeneratorExp(g) => {
            walk_comp_for(&g.for_in, sink, false);
        }
        Expression::FormattedString(fs) => {
            for part in &fs.parts {
                if let FormattedStringContent::Expression(e) = part {
                    walk_expr(&e.expression, sink);
                    // `{x:{width()}}` — the format_spec is itself a sequence of
                    // FormattedStringContent parts evaluated eagerly.
                    if let Some(spec_parts) = &e.format_spec {
                        for sp in spec_parts {
                            if let FormattedStringContent::Expression(se) = sp {
                                walk_expr(&se.expression, sink);
                            }
                        }
                    }
                }
            }
        }
        Expression::Yield(y) => {
            if let Some(v) = &y.value {
                match &**v {
                    libcst_native::YieldValue::Expression(e) => walk_expr(e, sink),
                    libcst_native::YieldValue::From(f) => walk_expr(&f.item, sink),
                }
            }
        }
        Expression::Await(a) => walk_expr(&a.expression, sink),
        Expression::NamedExpr(n) => walk_expr(&n.value, sink),
        Expression::StarredElement(s) => walk_expr(&s.value, sink),

        // Leaf / non-effectful expressions.
        _ => {}
    }
}

/// Walk a comprehension's `for … in …` clause(s).
///
/// `eager`: when `true` (list/set/dict comprehension) the element bodies were
/// already walked by the caller and we descend into **every** iterable and `if`
/// filter. When `false` (generator expression — lazy) we descend into **only the
/// outermost iterable**, never the `if` filters or nested-`for` clauses, since
/// those run on consumption, not in the enclosing body.
fn walk_comp_for(comp: &libcst_native::CompFor, sink: &mut dyn EffectSink, eager: bool) {
    // The outermost iterable always runs in the enclosing body (eager or lazy).
    walk_expr(&comp.iter, sink);
    if eager {
        for cond in &comp.ifs {
            walk_expr(&cond.test, sink);
        }
        if let Some(inner) = &comp.inner_for_in {
            walk_comp_for(inner, sink, true);
        }
    }
}

fn walk_element(el: &Element, sink: &mut dyn EffectSink) {
    match el {
        Element::Simple { value, .. } => walk_expr(value, sink),
        Element::Starred(s) => walk_expr(&s.value, sink),
    }
}

/// Walk a subscript slice (`Index` value, or `Slice` lower/upper/step) for effects.
fn walk_base_slice(slice: &libcst_native::BaseSlice, sink: &mut dyn EffectSink) {
    match slice {
        libcst_native::BaseSlice::Index(i) => walk_expr(&i.value, sink),
        libcst_native::BaseSlice::Slice(s) => {
            if let Some(lower) = &s.lower {
                walk_expr(lower, sink);
            }
            if let Some(upper) = &s.upper {
                walk_expr(upper, sink);
            }
            if let Some(step) = &s.step {
                walk_expr(step, sink);
            }
        }
    }
}

// ─── await counting ───────────────────────────────────────────────────────────

/// Count `await` expressions in the unit's own body.
///
/// Uses a separate recursive pass rather than the `EffectSink` driver because the
/// driver fires on call/assert/raise/assign, not on `await` as a distinct event.
/// The attribution rules (no nested `def`/`lambda` bodies) are mirrored manually.
fn count_awaits(unit: &FnUnit) -> usize {
    fn count_in_body(body: &FnBody) -> usize {
        match body {
            FnBody::Suite(suite) => count_in_suite(suite),
            FnBody::Expr(expr) => count_in_expr(expr),
            // The `<module>` unit's body is a flat list of top-level statements.
            // Top-level ClassDef: count awaits in both the class body (same as
            // walk_own_body) and the class header expressions (decorators + base/
            // keyword args, which run at import time — mirrors walk_class_header).
            FnBody::Module(stmts) => stmts
                .iter()
                .map(|stmt| {
                    if let libcst_native::Statement::Compound(
                        libcst_native::CompoundStatement::ClassDef(c),
                    ) = stmt
                    {
                        count_in_stmt(stmt)
                            + c.decorators
                                .iter()
                                .map(|dec| count_in_expr(&dec.decorator))
                                .sum::<usize>()
                            + c.bases
                                .iter()
                                .chain(c.keywords.iter())
                                .map(|arg| count_in_expr(&arg.value))
                                .sum::<usize>()
                    } else {
                        count_in_stmt(stmt)
                    }
                })
                .sum(),
        }
    }

    fn count_in_suite(suite: &libcst_native::Suite) -> usize {
        match suite {
            libcst_native::Suite::IndentedBlock(b) => b.body.iter().map(count_in_stmt).sum(),
            libcst_native::Suite::SimpleStatementSuite(s) => {
                s.body.iter().map(count_in_small).sum()
            }
        }
    }

    fn count_in_stmt(stmt: &libcst_native::Statement) -> usize {
        match stmt {
            libcst_native::Statement::Simple(line) => line.body.iter().map(count_in_small).sum(),
            libcst_native::Statement::Compound(c) => count_in_compound(c),
        }
    }

    fn count_in_compound(c: &libcst_native::CompoundStatement) -> usize {
        match c {
            // Nested def — its body is NOT counted (own attribution), but its
            // decorators + parameter defaults run in the enclosing body → count
            // any `await` there.
            libcst_native::CompoundStatement::FunctionDef(d) => count_in_def_header(d),
            libcst_native::CompoundStatement::ClassDef(_) => 0,
            libcst_native::CompoundStatement::If(i) => {
                count_in_expr(&i.test)
                    + count_in_suite(&i.body)
                    + i.orelse.as_ref().map_or(0, |o| count_in_orelse(o))
            }
            libcst_native::CompoundStatement::For(f) => {
                count_in_expr(&f.iter)
                    + count_in_suite(&f.body)
                    + f.orelse.as_ref().map_or(0, |e| count_in_suite(&e.body))
            }
            libcst_native::CompoundStatement::While(w) => {
                count_in_expr(&w.test)
                    + count_in_suite(&w.body)
                    + w.orelse.as_ref().map_or(0, |e| count_in_suite(&e.body))
            }
            libcst_native::CompoundStatement::Try(t) => {
                count_in_suite(&t.body)
                    + t.handlers
                        .iter()
                        .map(|h| count_in_suite(&h.body))
                        .sum::<usize>()
                    + t.orelse.as_ref().map_or(0, |e| count_in_suite(&e.body))
                    + t.finalbody.as_ref().map_or(0, |e| count_in_suite(&e.body))
            }
            libcst_native::CompoundStatement::TryStar(t) => {
                count_in_suite(&t.body)
                    + t.handlers
                        .iter()
                        .map(|h| count_in_suite(&h.body))
                        .sum::<usize>()
                    + t.orelse.as_ref().map_or(0, |e| count_in_suite(&e.body))
                    + t.finalbody.as_ref().map_or(0, |e| count_in_suite(&e.body))
            }
            libcst_native::CompoundStatement::With(w) => {
                w.items
                    .iter()
                    .map(|item| count_in_expr(&item.item))
                    .sum::<usize>()
                    + count_in_suite(&w.body)
            }
            libcst_native::CompoundStatement::Match(m) => {
                count_in_expr(&m.subject)
                    + m.cases
                        .iter()
                        .map(|case| count_in_suite(&case.body))
                        .sum::<usize>()
            }
        }
    }

    fn count_in_orelse(orelse: &libcst_native::OrElse) -> usize {
        match orelse {
            libcst_native::OrElse::Elif(elif) => {
                count_in_expr(&elif.test)
                    + count_in_suite(&elif.body)
                    + elif.orelse.as_ref().map_or(0, |o| count_in_orelse(o))
            }
            libcst_native::OrElse::Else(e) => count_in_suite(&e.body),
        }
    }

    fn count_in_small(small: &libcst_native::SmallStatement) -> usize {
        match small {
            libcst_native::SmallStatement::Expr(e) => count_in_expr(&e.value),
            libcst_native::SmallStatement::Return(r) => r.value.as_ref().map_or(0, count_in_expr),
            libcst_native::SmallStatement::Assign(a) => {
                a.targets
                    .iter()
                    .map(|t| count_in_assign_target(&t.target))
                    .sum::<usize>()
                    + count_in_expr(&a.value)
            }
            libcst_native::SmallStatement::AnnAssign(a) => {
                count_in_assign_target(&a.target) + a.value.as_ref().map_or(0, count_in_expr)
            }
            libcst_native::SmallStatement::AugAssign(a) => {
                count_in_assign_target(&a.target) + count_in_expr(&a.value)
            }
            libcst_native::SmallStatement::Assert(a) => {
                count_in_expr(&a.test) + a.msg.as_ref().map_or(0, count_in_expr)
            }
            libcst_native::SmallStatement::Raise(r) => r.exc.as_ref().map_or(0, count_in_expr),
            _ => 0,
        }
    }

    fn count_in_expr(expr: &libcst_native::Expression) -> usize {
        match expr {
            libcst_native::Expression::Await(a) => {
                // Count the await itself; descend into its inner expression too
                // (nested awaits inside the awaited expression are possible in theory).
                1 + count_in_expr(&a.expression)
            }
            // Nested lambda — its body is NOT counted (own attribution), but its
            // parameter defaults run in the enclosing body → count awaits there.
            libcst_native::Expression::Lambda(l) => count_in_params_defaults(&l.params),
            libcst_native::Expression::Call(c) => {
                count_in_expr(&c.func)
                    + c.args
                        .iter()
                        .map(|a| count_in_expr(&a.value))
                        .sum::<usize>()
            }
            libcst_native::Expression::Attribute(a) => count_in_expr(&a.value),
            libcst_native::Expression::Subscript(s) => {
                count_in_expr(&s.value)
                    + s.slice
                        .iter()
                        .map(|e| count_in_base_slice(&e.slice))
                        .sum::<usize>()
            }
            libcst_native::Expression::BinaryOperation(b) => {
                count_in_expr(&b.left) + count_in_expr(&b.right)
            }
            libcst_native::Expression::BooleanOperation(b) => {
                count_in_expr(&b.left) + count_in_expr(&b.right)
            }
            libcst_native::Expression::UnaryOperation(u) => count_in_expr(&u.expression),
            libcst_native::Expression::Comparison(c) => {
                count_in_expr(&c.left)
                    + c.comparisons
                        .iter()
                        .map(|comp| count_in_expr(&comp.comparator))
                        .sum::<usize>()
            }
            libcst_native::Expression::IfExp(i) => {
                count_in_expr(&i.test) + count_in_expr(&i.body) + count_in_expr(&i.orelse)
            }
            libcst_native::Expression::Tuple(t) => t.elements.iter().map(count_in_element).sum(),
            libcst_native::Expression::List(l) => l.elements.iter().map(count_in_element).sum(),
            libcst_native::Expression::Set(s) => s.elements.iter().map(count_in_element).sum(),
            libcst_native::Expression::Dict(d) => d
                .elements
                .iter()
                .map(|el| match el {
                    libcst_native::DictElement::Simple { key, value, .. } => {
                        count_in_expr(key) + count_in_expr(value)
                    }
                    libcst_native::DictElement::Starred(s) => count_in_expr(&s.value),
                })
                .sum(),
            libcst_native::Expression::ListComp(l) => {
                count_in_expr(&l.elt) + count_in_comp_for(&l.for_in)
            }
            libcst_native::Expression::SetComp(s) => {
                count_in_expr(&s.elt) + count_in_comp_for(&s.for_in)
            }
            libcst_native::Expression::DictComp(d) => {
                count_in_expr(&d.key) + count_in_expr(&d.value) + count_in_comp_for(&d.for_in)
            }
            // LAZY generator expression: only the outermost iterable runs in the
            // enclosing body. The element/condition bodies and nested-for clauses
            // are deferred — awaits there do NOT count toward the enclosing
            // function's await_count / async_boundary. Mirror walk_comp_for's
            // `eager = false` branch: only descend into `comp.iter`.
            libcst_native::Expression::GeneratorExp(g) => count_in_expr(&g.for_in.iter),
            libcst_native::Expression::FormattedString(fs) => fs
                .parts
                .iter()
                .map(|p| {
                    if let libcst_native::FormattedStringContent::Expression(e) = p {
                        let in_expr = count_in_expr(&e.expression);
                        // `{x:{await w()}}` — format_spec parts are also eager.
                        let in_spec = e
                            .format_spec
                            .as_deref()
                            .unwrap_or(&[])
                            .iter()
                            .map(|sp| {
                                if let libcst_native::FormattedStringContent::Expression(se) = sp {
                                    count_in_expr(&se.expression)
                                } else {
                                    0
                                }
                            })
                            .sum::<usize>();
                        in_expr + in_spec
                    } else {
                        0
                    }
                })
                .sum(),
            libcst_native::Expression::Yield(y) => {
                y.value.as_ref().map_or(0, |v| match v.as_ref() {
                    libcst_native::YieldValue::Expression(e) => count_in_expr(e),
                    libcst_native::YieldValue::From(f) => count_in_expr(&f.item),
                })
            }
            libcst_native::Expression::NamedExpr(n) => count_in_expr(&n.value),
            libcst_native::Expression::StarredElement(s) => count_in_expr(&s.value),
            _ => 0,
        }
    }

    /// Awaits in a nested `def`'s header (decorators + parameter defaults), which
    /// run in the enclosing body. The def's BODY is not counted (own attribution).
    fn count_in_def_header(def: &libcst_native::FunctionDef) -> usize {
        def.decorators
            .iter()
            .map(|dec| count_in_expr(&dec.decorator))
            .sum::<usize>()
            + count_in_params_defaults(&def.params)
    }

    /// Awaits in a parameter list's default-value expressions (eager at def-time).
    fn count_in_params_defaults(params: &libcst_native::Parameters) -> usize {
        let mut n = 0;
        let all = params
            .posonly_params
            .iter()
            .chain(&params.params)
            .chain(&params.kwonly_params);
        for p in all {
            if let Some(default) = &p.default {
                n += count_in_expr(default);
            }
        }
        if let Some(libcst_native::StarArg::Param(p)) = &params.star_arg
            && let Some(default) = &p.default
        {
            n += count_in_expr(default);
        }
        if let Some(p) = &params.star_kwarg
            && let Some(default) = &p.default
        {
            n += count_in_expr(default);
        }
        n
    }

    fn count_in_comp_for(comp: &libcst_native::CompFor) -> usize {
        count_in_expr(&comp.iter)
            + comp
                .ifs
                .iter()
                .map(|c| count_in_expr(&c.test))
                .sum::<usize>()
            + comp
                .inner_for_in
                .as_ref()
                .map_or(0, |inner| count_in_comp_for(inner))
    }

    /// Count awaits in an assignment **target's** eagerly-evaluated sub-expressions
    /// (mirrors `walk_assign_target_subexprs`): a subscript target's base + index/
    /// slice, an attribute target's base, recursing through destructuring elements.
    fn count_in_assign_target(target: &libcst_native::AssignTargetExpression) -> usize {
        use libcst_native::AssignTargetExpression as T;
        match target {
            T::Name(_) => 0,
            T::Attribute(a) => count_in_expr(&a.value),
            T::Subscript(s) => {
                count_in_expr(&s.value)
                    + s.slice
                        .iter()
                        .map(|e| count_in_base_slice(&e.slice))
                        .sum::<usize>()
            }
            T::Tuple(t) => t.elements.iter().map(count_in_target_element).sum(),
            T::List(l) => l.elements.iter().map(count_in_target_element).sum(),
            T::StarredElement(s) => count_in_target_value(&s.value),
        }
    }

    /// Count awaits in a destructuring-target element's nested sub-expressions.
    fn count_in_target_element(el: &libcst_native::Element) -> usize {
        match el {
            libcst_native::Element::Simple { value, .. } => count_in_target_value(value),
            libcst_native::Element::Starred(s) => count_in_target_value(&s.value),
        }
    }

    /// Count awaits in a target-position expression (a tuple/list element).
    fn count_in_target_value(expr: &libcst_native::Expression) -> usize {
        match expr {
            libcst_native::Expression::Name(_) => 0,
            libcst_native::Expression::Attribute(a) => count_in_expr(&a.value),
            libcst_native::Expression::Subscript(s) => {
                count_in_expr(&s.value)
                    + s.slice
                        .iter()
                        .map(|e| count_in_base_slice(&e.slice))
                        .sum::<usize>()
            }
            libcst_native::Expression::Tuple(t) => {
                t.elements.iter().map(count_in_target_element).sum()
            }
            libcst_native::Expression::List(l) => {
                l.elements.iter().map(count_in_target_element).sum()
            }
            libcst_native::Expression::StarredElement(s) => count_in_target_value(&s.value),
            _ => 0,
        }
    }

    fn count_in_base_slice(slice: &libcst_native::BaseSlice) -> usize {
        match slice {
            libcst_native::BaseSlice::Index(i) => count_in_expr(&i.value),
            libcst_native::BaseSlice::Slice(s) => {
                s.lower.as_ref().map_or(0, count_in_expr)
                    + s.upper.as_ref().map_or(0, count_in_expr)
                    + s.step.as_ref().map_or(0, count_in_expr)
            }
        }
    }

    fn count_in_element(el: &libcst_native::Element) -> usize {
        match el {
            libcst_native::Element::Simple { value, .. } => count_in_expr(value),
            libcst_native::Element::Starred(s) => count_in_expr(&s.value),
        }
    }

    count_in_body(&unit.body)
}

// ─── unit assembly ────────────────────────────────────────────────────────────

/// Run every detector over `unit`'s own body and return the gathered
/// `(effects, risks, await_count, async_boundary)` tuple.
///
/// Both [`analyze_unit`] and [`build_record`] call this helper so the two
/// callers can never silently diverge — any future detector addition touches
/// one place. Mirrors the private `gather` in `fxrank-lang-rust/src/detect/mod.rs`.
/// Return type of [`gather`]: `(effects, risks, await_count, async_boundary, unknown_decorator)`.
///
/// `unknown_decorator` is forwarded to `analyze_unit` only — it lowers confidence
/// without touching coverage.  `build_record` discards it (records carry no
/// confidence field).
type GatherOutput = (
    Vec<fxrank_core::effect::Effect>,
    Vec<RiskFeature>,
    usize,
    bool,
    bool,
);

fn gather(
    unit: &FnUnit,
    path: &str,
    imports: &Imports,
    module_bindings: &HashSet<String>,
    span: &SpanIndex,
) -> GatherOutput {
    let mut effects = calls::detect(unit, imports, span);

    // Signature annotation coverage + `Any`/decorator signals.
    let cov = coverage::of(unit, imports);

    // Apply the boundary-containment discount per the `contained` flag: a contained
    // (local-state) effect under an honest, typed boundary shifts down. Body `Any`
    // re-opens the boundary, so it voids the discount (coverage forced to `None`).
    // Escaping effects (`contained == false`) are never discounted.
    let discount_coverage = if cov.any_in_body {
        BoundaryCoverage::None
    } else {
        cov.boundary
    };
    let mut_pairs = mutation::detect(unit, imports, module_bindings, span);
    effects.extend(mut_pairs.into_iter().map(|(mut e, contained)| {
        // Wire the tuple's containment bool onto the Effect so propagation logic
        // (`Effect::escapes()`) sees the real value (not the default `false` stub).
        e.contained = contained;
        // Only record a discount when the boundary actually shifts the class —
        // i.e. Partial/Full coverage. `None` (incl. a typed boundary voided by a
        // body `Any`) produces no shift, so we leave `discounted_to`/`discount`
        // unset rather than claim a no-op discount in the report (mirrors TS).
        if contained && discount_coverage != BoundaryCoverage::None {
            e.discounted_to = Some(apply_boundary_discount(e.class, discount_coverage, true));
            e.discount = Some(
                match discount_coverage {
                    BoundaryCoverage::Full => "contained, Full-typed boundary",
                    BoundaryCoverage::Partial => "contained, Partial-typed boundary",
                    BoundaryCoverage::None => unreachable!("guarded above"),
                }
                .to_string(),
            );
            e.sync_weight();
        }
        e
    }));

    // The coverage gate owns the `Any`-family `type.escape` risk (class 3, exact):
    // an explicit `Any` in the signature or body is the `any ≈ unsafe` escape hatch.
    // risk::detect adds eval/exec/pickle/yaml/importlib/setattr/shell=True.
    let mut risks: Vec<RiskFeature> = risk::detect(unit, imports, span, path);
    if cov.any_in_signature || cov.any_in_body {
        let class = RiskKind::TypeEscape.class();
        risks.push(RiskFeature {
            kind: RiskKind::TypeEscape,
            class,
            weight: weight_for_class(class),
            path: path.into(),
            line: unit.line,
            col: unit.col,
            evidence: "explicit Any (signature or body) — type-escape hatch".into(),
            tier: Tier::Exact,
        });
    }

    let await_count = count_awaits(unit);
    let async_boundary = unit.is_async || await_count > 0;

    (
        effects,
        risks,
        await_count,
        async_boundary,
        cov.unknown_decorator,
    )
}

/// Analyze one function-unit into an owned [`Hotspot`].
///
/// # Gather → Fold
/// 1. **gather**: drive each detector over the own body to collect `Vec<Effect>`.
/// 2. **fold**: compute `own_score`, `max_class`, function-level `confidence`
///    (weakest-link min over per-effect confidences, plus 0.8 synthetic when there
///    are unresolved awaited calls), and `await_count` / `async_boundary`.
///
/// Adding a detector is a one-line addition to the gather step (in [`gather`]).
pub fn analyze_unit(
    unit: &FnUnit,
    path: &str,
    imports: &Imports,
    module_bindings: &HashSet<String>,
    span: &SpanIndex,
) -> Hotspot {
    // ── gather ───────────────────────────────────────────────────────────────
    let (effects, risks, await_count, async_boundary, unknown_decorator) =
        gather(unit, path, imports, module_bindings, span);

    // ── fold ─────────────────────────────────────────────────────────────────
    let weights: Vec<u32> = effects.iter().map(|e| e.weight).collect();
    let classes: Vec<u8> = effects.iter().map(|e| e.effective_class()).collect();

    // Function confidence = weakest-link min of per-effect confidences.
    // Per the spec: per-effect confidence is NOT serialized; it surfaces only here.
    // When there are unresolved awaited calls, add a synthetic 0.8 entry —
    // an async fn that awaits may hide IO effects we cannot see statically
    // (mirrors the Rust and TS frontends). An unknown decorator may erase the
    // signature to `Any`, so it lowers confidence (a 0.8 step) without touching
    // coverage (the written annotations are still real signal).
    let mut confidences: Vec<f64> = effects.iter().map(|e| e.confidence).collect();
    if await_count > 0 {
        confidences.push(0.8);
    }
    if unknown_decorator {
        confidences.push(0.8);
    }

    // Fold risks into scoring (generalized — Task 9 introduces the first real risk;
    // Task 10 plugs more into the same Vec). risk_class = max class over features.
    let risk_class = risks.iter().map(|r| r.class).max().unwrap_or(0);
    let risk_weight = if risks.is_empty() {
        0
    } else {
        weight_for_class(risk_class)
    };

    let mc = max_class(&classes, risk_class);
    let os = own_score(&weights);
    Hotspot {
        id: format!("{}:{}:{}:{}", path, unit.line, unit.col, unit.symbol),
        symbol: unit.symbol.clone(),
        path: path.into(),
        line: unit.line,
        risk_weight,
        confidence: function_confidence(&confidences),
        async_boundary,
        await_count,
        effects,
        risk_features: risks,
        // Propagated fields default to own (cross-file folding overwrites them).
        ..Hotspot::own_seed(os, mc)
    }
}

// ─── build_record ─────────────────────────────────────────────────────────────

/// Path-meaningful segments for a Python symbol. Synthetic `<module>`/`<lambda@…>`
/// symbols (identified by leading `<`) are not importable → `None`. Real names
/// return `Some(vec![symbol])` (a single segment; Python symbols carry no dots).
/// Module-level-vs-method filtering is done by `is_module_level` at the call
/// site in `build_record`; this only guards the synthetic forms.
fn symbol_segments(symbol: &str) -> Option<Vec<String>> {
    if symbol.starts_with('<') {
        None
    } else {
        Some(vec![symbol.to_string()])
    }
}

/// Build a language-neutral [`fxrank_core::record::UnitRecord`] for `unit`.
///
/// Calls the shared [`gather`] helper (same as [`analyze_unit`]) so the record's
/// `effects`/`risks`/`async_boundary`/`await_count` are byte-identical to the
/// Hotspot. Also extracts outgoing call references via [`refs::extract`] for the
/// cross-file resolver.
///
/// INVARIANT: this recomputes own-body via the same `gather` as `analyze_unit`.
/// This stays correct only while `analyze_unit` does NO post-`gather` mutation of
/// effects/risks (unlike TS, which absorbs React signals and so must copy from the
/// final Hotspot). If you add a post-gather step here, switch to copying from the
/// Hotspot or the record's own-body will drift from it.
pub fn build_record(
    unit: &FnUnit,
    path: &str,
    imports: &Imports,
    module_bindings: &HashSet<String>,
    span: &SpanIndex,
    module_map: &crate::module_map::PyModuleMap,
) -> fxrank_core::record::UnitRecord {
    // ── gather ─────────────────────────────────────────────────────────────
    // `unknown_decorator` is a confidence-only signal; records carry no
    // confidence field, so we discard it here.
    let (effects, risks, await_count, async_boundary, _unknown_decorator) =
        gather(unit, path, imports, module_bindings, span);

    // ── canonical_path ─────────────────────────────────────────────────────
    // Only a module-level def is importable as `module.<name>`. Methods/nested
    // defs/lambdas/<module> get an empty canonical_path so they cannot be a
    // false-resolve target (Python symbols are bare — a method `write` would
    // otherwise collide with module-level `write`). (P2-1)
    let canonical_path = if !unit.is_module_level {
        vec![]
    } else {
        match (module_map.module_of(path), symbol_segments(&unit.symbol)) {
            (Some(mut m), Some(seg)) => {
                m.extend(seg);
                m
            }
            _ => vec![], // no module in scope, OR a synthetic symbol
        }
    };

    // ── refs ───────────────────────────────────────────────────────────────
    let referencing_module = module_map.module_of(path).unwrap_or_default();
    let referencing_is_package = module_map.is_package(path);
    let call_refs = refs::extract(
        unit,
        imports,
        span,
        &referencing_module,
        referencing_is_package,
        module_map,
    );

    fxrank_core::record::UnitRecord {
        unit_id: format!("{}:{}:{}:{}", path, unit.line, unit.col, unit.symbol),
        path: path.into(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,
        canonical_path,
        aliases: vec![],
        effects,
        risks,
        refs: call_refs,
        async_boundary,
        await_count,
        language: fxrank_core::frontend::Language::Python,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::model::Hotspot;

    /// Parse `tests/fixtures/<name>.py`, run `analyze_unit` for every collected
    /// function-unit, and return the resulting `Vec<Hotspot>`.  Mirrors the
    /// `analyze_fixture` helper in `calls.rs` but returns full `Hotspot`s so
    /// scoring fields (`own_score`, `max_class`, `confidence`, …) can be asserted.
    fn scan_fixture_hotspots(name: &str) -> Vec<Hotspot> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}.py")).unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, &src, &span, &anchors);
        units
            .iter()
            .map(|unit| {
                analyze_unit(
                    unit,
                    &format!("tests/fixtures/{name}.py"),
                    &imports,
                    &module_bindings,
                    &span,
                )
            })
            .collect()
    }

    /// FIX 2: a nested `def`'s parameter-default expression runs when the ENCLOSING
    /// `def` statement executes → charged to the enclosing function, NOT the nested
    /// one. A top-level def's own default runs at module time → uncounted on itself.
    #[test]
    fn def_header_defaults_charge_to_enclosing_scope() {
        let h = scan_fixture_hotspots("attribution");
        let net = |sym: &str| {
            h.iter()
                .find(|x| x.symbol == sym)
                .unwrap_or_else(|| panic!("symbol {sym} not found"))
                .effects
                .iter()
                .any(|e| e.kind.wire() == "net.fs.db")
        };
        // `def inner(x=open(p))` inside `outer` → `open(p)` charged to OUTER.
        assert!(
            net("outer"),
            "open(p) default must be charged to enclosing outer"
        );
        assert!(
            !net("inner"),
            "open(p) must NOT be charged to nested inner (its default runs in outer)"
        );
        // Top-level `def top_default(x=open('f'))` → default runs at module time,
        // uncounted on top_default itself.
        assert!(
            !net("top_default"),
            "a top-level def's own param default is module-time → uncounted on itself"
        );
    }

    /// FIX 3: a subscript index/slice expression is eagerly evaluated and must be
    /// traversed for effects (and awaits). `xs[requests.get(u)]` → net.fs.db.
    #[test]
    fn subscript_index_expression_is_traversed() {
        let h = scan_fixture_hotspots("attribution");
        let si = h.iter().find(|x| x.symbol == "subscript_index").unwrap();
        assert!(
            si.effects.iter().any(|e| e.kind.wire() == "net.fs.db"),
            "subscript index requests.get(u) must surface net.fs.db, got: {:?}",
            si.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );
    }

    /// Copilot FIX 1: an assignment TARGET's sub-expressions are eagerly evaluated
    /// and must be traversed for effects — a subscript target's index and an
    /// attribute target's base. CRITICALLY, the index/base walk must NOT
    /// double-count the target's own mutation: `xs[requests.get(u)] = 1` charges
    /// NetFsDb (from the index) AND exactly ONE param.mutation for `xs`.
    ///
    /// Pre-fix `walk_small`'s Assign/AnnAssign/AugAssign arms only called
    /// `on_assign_target` then walked the VALUE — never the target's sub-exprs — so
    /// the index/base call effects were silently dropped.
    #[test]
    fn assign_target_subexprs_are_traversed_without_double_counting() {
        let h = scan_fixture_hotspots("attribution");

        // ── subscript-index arm: `xs[requests.get(u)] = 1` ──────────────────────
        let s = h
            .iter()
            .find(|x| x.symbol == "assign_target_subscript_index")
            .unwrap();
        let net_count = s
            .effects
            .iter()
            .filter(|e| e.kind.wire() == "net.fs.db")
            .count();
        assert_eq!(
            net_count,
            1,
            "subscript-target index requests.get(u) must surface exactly one net.fs.db, got: {:?}",
            s.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );
        // Double-count guard: the param mutation of `xs` must be emitted EXACTLY once.
        let param_mut_count = s
            .effects
            .iter()
            .filter(|e| e.kind.wire() == "param.mutation")
            .count();
        assert_eq!(
            param_mut_count,
            1,
            "the subscript target `xs` must emit exactly ONE param.mutation (no double-count), got: {:?}",
            s.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );

        // ── attribute-base arm: `requests.get(u).attr = 1` ──────────────────────
        let a = h
            .iter()
            .find(|x| x.symbol == "assign_target_attr_base")
            .unwrap();
        assert!(
            a.effects.iter().any(|e| e.kind.wire() == "net.fs.db"),
            "attribute-target base requests.get(u) must surface net.fs.db, got: {:?}",
            a.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );
    }

    /// FIX 3 (await arm): an `await` in a subscript index counts toward await_count.
    #[test]
    fn subscript_index_await_counts() {
        let src = "async def f(xs):\n    return xs[await key()]\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).unwrap();
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let f = units.iter().find(|u| u.symbol == "f").unwrap();
        let h = analyze_unit(f, "x.py", &imports, &module_bindings, &span);
        assert!(
            h.await_count >= 1,
            "await in subscript index must count, got await_count={}",
            h.await_count
        );
    }

    /// Copilot FIX 2: an `await` in an assignment TARGET's sub-expression must be
    /// counted by `count_awaits`. `xs[await f()] = 1` — the await lives in the
    /// subscript-target index, which pre-fix `count_in_small` never visited (it
    /// only counted `a.value`), so await_count was 0.
    #[test]
    fn assign_target_subscript_index_await_counts() {
        let src = "async def f(xs):\n    xs[await key()] = 1\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).unwrap();
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let f = units.iter().find(|u| u.symbol == "f").unwrap();
        let h = analyze_unit(f, "x.py", &imports, &module_bindings, &span);
        assert!(
            h.await_count >= 1,
            "await in an assignment-target subscript index must count, got await_count={}",
            h.await_count
        );
        assert!(
            h.async_boundary,
            "await in an assignment-target subscript index must set async_boundary"
        );
    }

    /// FIX 5: a function-local `import subprocess` must be collected file-wide so
    /// `subprocess.run(c, shell=True)` resolves → process.control effect AND
    /// dynamic.code risk. Pre-fix, `Imports::build` scanned only top-level
    /// statements, so neither resolved.
    #[test]
    fn function_local_import_resolves_effect_and_risk() {
        let h = scan_fixture_hotspots("local_import");
        let f = h.iter().find(|x| x.symbol == "f").unwrap();
        assert!(
            f.effects.iter().any(|e| e.kind.wire() == "process.control"),
            "function-local import must resolve subprocess.run → process.control, got: {:?}",
            f.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );
        assert!(
            f.risk_features
                .iter()
                .any(|r| r.kind.wire() == "dynamic.code"),
            "shell=True must emit dynamic.code once the local import resolves"
        );
    }

    #[test]
    fn analyze_unit_scores_world_effects() {
        let h = scan_fixture_hotspots("calls");
        let io = h.iter().find(|x| x.symbol == "io_boundary").unwrap();
        // open(…) + requests.get(…) → NetFsDb class 7, weight 21 each.
        // logging.info(…)           → Logging class 2, weight 2.
        // weights = [21, 21, 2] → own_score = 21 + 0.5*(21+2) = 32.5
        assert_eq!(io.max_class, 7);
        assert!(
            io.own_score >= 21.0,
            "expected own_score >= 21.0, got {}",
            io.own_score
        );
    }

    // ── Task 9: boundary discount + Any poison + decorator confidence ──────────

    /// Parse a tiny `src` module and return `coverage::of` for the unit named `symbol`.
    fn coverage_of_symbol(src: &str, symbol: &str) -> crate::coverage::Coverage {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == symbol)
            .expect("unit not found");
        crate::coverage::of(unit, &imports)
    }

    #[test]
    fn boundary_discount_zeros_contained_local_when_typed() {
        let h = scan_fixture_hotspots("coverage");
        let ft = h.iter().find(|x| x.symbol == "fully_typed").unwrap();
        assert_eq!(ft.own_score, 0.0); // local.mutation class 1 → 0 under Full coverage
    }

    #[test]
    fn any_emits_type_escape_and_blocks_discount() {
        let h = scan_fixture_hotspots("coverage");
        // RiskFeature.kind is a RiskKind enum (effect.rs), not a String — compare via .wire().
        let has_type_escape = h
            .iter()
            .find(|x| x.symbol == "has_any")
            .unwrap()
            .risk_features
            .iter()
            .any(|r| r.kind.wire() == "type.escape");
        assert!(has_type_escape); // signature Any → type.escape
        let ba = h.iter().find(|x| x.symbol == "body_any").unwrap();
        assert!(
            ba.risk_features
                .iter()
                .any(|r| r.kind.wire() == "type.escape")
        ); // body Any → type.escape
        assert!(ba.own_score >= 1.0); // discount voided → local.mutation stays class 1
    }

    /// FIX 4: body-`Any` detection must descend into list/tuple/dict literals,
    /// f-strings, and comprehensions. A fully-typed contained-mutation fn with a
    /// `cast(Any, …)` in such an eager context must emit `type.escape` AND have its
    /// boundary discount voided (local.mutation stays class 1, own_score >= 1.0).
    #[test]
    fn body_any_in_eager_containers_emits_escape_and_voids_discount() {
        let h = scan_fixture_hotspots("coverage");
        for sym in [
            "body_any_in_list",
            "body_any_in_fstring",
            "body_any_in_comprehension",
        ] {
            let f = h.iter().find(|x| x.symbol == sym).unwrap();
            assert!(
                f.risk_features
                    .iter()
                    .any(|r| r.kind.wire() == "type.escape"),
                "{sym}: body Any in an eager container must emit type.escape"
            );
            assert!(
                f.own_score >= 1.0,
                "{sym}: body Any must void the discount (local.mutation stays class 1), \
                 got own_score={}",
                f.own_score
            );
        }
    }

    /// FIX 6: when the boundary discount fires the effect's human-readable
    /// `discount` rationale must be set (mirrors the TS frontend).
    #[test]
    fn discounted_effect_sets_rationale_string() {
        let h = scan_fixture_hotspots("coverage");
        let ft = h.iter().find(|x| x.symbol == "fully_typed").unwrap();
        let lm = ft
            .effects
            .iter()
            .find(|e| e.kind.wire() == "local.mutation")
            .expect("fully_typed must have a local.mutation effect");
        assert_eq!(
            lm.discount.as_deref(),
            Some("contained, Full-typed boundary"),
            "discounted effect must carry the Full-boundary rationale"
        );
    }

    #[test]
    fn coverage_tiers_and_decorator_confidence() {
        let h = scan_fixture_hotspots("coverage");
        let score = |s: &str| h.iter().find(|x| x.symbol == s).unwrap().own_score;
        assert_eq!(score("untyped"), 1.0); // None coverage → local.mutation stays class 1
        assert_eq!(score("partial"), 0.0); // any coverage > None floors class-1 local to 0
        let dec = h.iter().find(|x| x.symbol == "decorated").unwrap();
        assert!(dec.confidence < 1.0); // unknown decorator reduces confidence
    }

    #[test]
    fn coverage_excludes_self_and_degrades_untyped_star_args() {
        use fxrank_core::score::BoundaryCoverage;
        let src = "class C:\n    def m(self, x: int) -> int:\n        return x\ndef v(*args) -> int:\n    return 0\n";
        let cov_m = coverage_of_symbol(src, "m");
        assert_eq!(cov_m.boundary, BoundaryCoverage::Full); // self excluded → (x, return) both typed
        let cov_v = coverage_of_symbol(src, "v");
        assert_ne!(cov_v.boundary, BoundaryCoverage::Full); // untyped *args degrades coverage
    }

    /// Regression for FIX B (Copilot round-2): `count_awaits` must NOT count
    /// `await` expressions that appear in a generator-expression `if` condition
    /// or a nested `for` clause's iterable — those are lazy and execute in the
    /// consumer's scope, not the enclosing function's body. Only the genexp's
    /// **outermost iterable** is eager and therefore counted.
    ///
    /// The concrete pre-fix bug: old code called `count_in_comp_for(&g.for_in)`
    /// which descended into `comp.ifs` (if conditions) and `inner_for_in` (nested
    /// for clauses) — both lazy in a genexp. The fix is `count_in_expr(&g.for_in.iter)`
    /// (outermost iterable only), mirroring `walk_comp_for`'s `eager = false` branch.
    ///
    /// Contrast with a list-comprehension: its `if` conditions ARE eager and
    /// their awaits DO count.
    ///
    /// Uses `tests/fixtures/genexp_await.py`.
    #[test]
    fn count_awaits_genexp_if_and_nested_for_are_lazy_outermost_iterable_is_eager() {
        let h = scan_fixture_hotspots("genexp_await");
        let find = |sym: &str| {
            h.iter()
                .find(|x| x.symbol == sym)
                .unwrap_or_else(|| panic!("symbol {sym} not found in hotspots"))
        };

        // `genexp_await_in_if_condition`: `await predicate(x)` is in the genexp
        // IF condition — lazy. await_count must be 0.
        // (Pre-fix: old code used count_in_comp_for which visited comp.ifs, so
        // it would return await_count=1. This test fails on old code, passes with fix.)
        let lazy_if = find("genexp_await_in_if_condition");
        assert_eq!(
            lazy_if.await_count, 0,
            "genexp `if` condition await must NOT count toward enclosing await_count; \
             got await_count={} for genexp_await_in_if_condition",
            lazy_if.await_count
        );

        // `listcomp_await_in_if_condition`: `await predicate(x)` is in a LIST-COMP
        // IF condition — eager. await_count must be >= 1.
        let eager_listcomp = find("listcomp_await_in_if_condition");
        assert!(
            eager_listcomp.await_count >= 1,
            "list-comp `if` condition await IS eager and MUST count toward await_count; \
             got await_count={} for listcomp_await_in_if_condition",
            eager_listcomp.await_count
        );
        assert!(
            eager_listcomp.async_boundary,
            "list-comp `if` condition await must set async_boundary; \
             got async_boundary={} for listcomp_await_in_if_condition",
            eager_listcomp.async_boundary
        );

        // `genexp_await_in_nested_for_iterable`: `await get_items()` is in a
        // NESTED for clause's iterable inside a genexp — lazy. await_count must be 0.
        // (Pre-fix: old code used count_in_comp_for which recursed into inner_for_in.)
        let lazy_nested = find("genexp_await_in_nested_for_iterable");
        assert_eq!(
            lazy_nested.await_count, 0,
            "genexp nested-for iterable await must NOT count toward enclosing await_count; \
             got await_count={} for genexp_await_in_nested_for_iterable",
            lazy_nested.await_count
        );

        // `genexp_await_in_outermost_iterable`: `await get_items()` is in the
        // OUTERMOST iterable — always eager. Must count.
        let eager_iterable = find("genexp_await_in_outermost_iterable");
        assert!(
            eager_iterable.await_count >= 1,
            "genexp outermost-iterable await IS eager and MUST count; \
             got await_count={} for genexp_await_in_outermost_iterable",
            eager_iterable.await_count
        );
    }

    /// Copilot FIX 2: `walk_expr` must traverse f-string `format_spec` expression parts.
    /// `f"{x:{requests.get(u)}}"` — `requests.get(u)` is in the format-spec and is eager.
    #[test]
    fn fstring_format_spec_walk_expr_charges_effects() {
        let src = "import requests\ndef f(x, u):\n    return f\"{x:{requests.get(u)}}\"\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let unit = units.iter().find(|u| u.symbol == "f").unwrap();
        let h = analyze_unit(unit, "x.py", &imports, &module_bindings, &span);
        assert!(
            h.effects.iter().any(|e| e.kind.wire() == "net.fs.db"),
            "requests.get(u) inside f-string format_spec must emit net.fs.db; got: {:?}",
            h.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );
    }

    /// Copilot FIX 3: `count_awaits` must count awaits in f-string `format_spec`.
    /// `f"{x:{await w()}}"` — the `await` is in the format-spec, which is eager.
    #[test]
    fn fstring_format_spec_await_counts() {
        let src = "async def f(x):\n    async def w(): ...\n    return f\"{x:{await w()}}\"\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let outer = units.iter().find(|u| u.symbol == "f").unwrap();
        let h = analyze_unit(outer, "x.py", &imports, &module_bindings, &span);
        assert!(
            h.await_count >= 1,
            "await inside f-string format_spec must count; got await_count={}",
            h.await_count
        );
        assert!(
            h.async_boundary,
            "await inside f-string format_spec must set async_boundary"
        );
    }

    /// Copilot FIX 4: body-`Any` detection must descend into f-string `format_spec`.
    /// `cast(Any, x)` inside a format-spec must emit `type.escape` and void the discount.
    #[test]
    fn fstring_format_spec_body_any_emits_type_escape() {
        let src = "from typing import Any, cast\ndef f(x: int, y: int) -> int:\n    acc: list[int] = []\n    _ = f\"{x:{cast(Any, y)}}\"\n    return x\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let unit = units.iter().find(|u| u.symbol == "f").unwrap();
        let h = analyze_unit(unit, "x.py", &imports, &module_bindings, &span);
        assert!(
            h.risk_features
                .iter()
                .any(|r| r.kind.wire() == "type.escape"),
            "cast(Any, …) inside f-string format_spec must emit type.escape; got: {:?}",
            h.risk_features
                .iter()
                .map(|r| r.kind.wire())
                .collect::<Vec<_>>()
        );
    }

    /// Task 2 (Phase-3a): the mutation tuple's `contained` bool must flow through
    /// to `Effect.contained` after `gather` assembles the effects list.
    ///
    /// - A body-local mutation (`acc.append(1)` where `acc` is a local) is
    ///   `LocalMutation, contained=true` from the detector; `Effect.contained`
    ///   must be `true` and `escapes()` must be `false`.
    /// - An escaping mutation (`global g; g = 1` → `GlobalMutation`) is
    ///   `contained=false`; `Effect.contained` must be `false` and `escapes()` must
    ///   be `true`.
    #[test]
    fn gather_sets_effect_contained_from_mutation_tuple() {
        // --- contained: body-local list build → LocalMutation, contained=true ---
        let contained_src =
            "def builds_local():\n    acc = []\n    acc.append(1)\n    return acc\n";
        let module = libcst_native::parse_module(contained_src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(contained_src);
        let anchors = crate::source::lambda_anchors(contained_src).expect("tokenize");
        let (units, _) = crate::functions::collect(&module, contained_src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == "builds_local")
            .expect("builds_local not found");
        let h = analyze_unit(unit, "test.py", &imports, &module_bindings, &span);
        let local_mut = h
            .effects
            .iter()
            .find(|e| e.kind.wire() == "local.mutation")
            .expect("expected a local.mutation effect");
        assert!(
            local_mut.contained,
            "local.mutation from a body-local build must have contained=true, got: {:?}",
            local_mut
        );
        assert!(
            !local_mut.escapes(),
            "a contained local.mutation must not escape, got: {:?}",
            local_mut
        );

        // --- escaping: global mutation → GlobalMutation, contained=false ---
        let escaping_src = "_g = 0\ndef uses_global():\n    global _g\n    _g += 1\n";
        let module2 = libcst_native::parse_module(escaping_src, None).unwrap();
        let imports2 = Imports::build(&module2);
        let module_bindings2 = crate::imports::module_bindings(&module2);
        let span2 = SpanIndex::new(escaping_src);
        let anchors2 = crate::source::lambda_anchors(escaping_src).expect("tokenize");
        let (units2, _) = crate::functions::collect(&module2, escaping_src, &span2, &anchors2);
        let unit2 = units2
            .iter()
            .find(|u| u.symbol == "uses_global")
            .expect("uses_global not found");
        let h2 = analyze_unit(unit2, "test.py", &imports2, &module_bindings2, &span2);
        let global_mut = h2
            .effects
            .iter()
            .find(|e| e.kind.wire() == "global.mutation")
            .expect("expected a global.mutation effect");
        assert!(
            !global_mut.contained,
            "global.mutation must have contained=false, got: {:?}",
            global_mut
        );
        assert!(
            global_mut.escapes(),
            "a non-contained global.mutation must escape, got: {:?}",
            global_mut
        );
    }

    /// Task 4: `build_record` emits a `UnitRecord` 1:1 with the Hotspot.
    /// Parse `import os\ndef writer():\n    os.getcwd()`; build the record;
    /// assert: symbol "writer", a ref `base "os.getcwd"` with `qualified true`,
    /// `unit_id` ends `:writer`, `language == Python`.
    /// The frontend always emits `is_root: false`; the CLI sets the real value.
    #[test]
    fn build_record_emits_record_for_python_unit() {
        use crate::module_map::PyModuleMap;
        use fxrank_core::frontend::SourceFile;
        let src = "import os\ndef writer():\n    os.getcwd()\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == "writer")
            .expect("writer unit not found");
        let mmap = PyModuleMap::build(&[SourceFile {
            path: "test.py".into(),
            text: String::new(),
        }]);
        let rec = build_record(unit, "test.py", &imports, &module_bindings, &span, &mmap);

        assert_eq!(rec.symbol, "writer");
        assert!(
            rec.unit_id.ends_with(":writer"),
            "unit_id must end with ':writer', got: {}",
            rec.unit_id
        );
        assert_eq!(
            rec.language,
            fxrank_core::frontend::Language::Python,
            "language must be Python"
        );
        // Frontend always emits is_root: false; CLI sets the real value.
        assert!(
            !rec.is_root,
            "frontend build_record must emit is_root=false (CLI sets the real value)"
        );
        let os_ref = rec
            .refs
            .iter()
            .find(|r| r.base == "os.getcwd")
            .expect("expected a ref with base 'os.getcwd'");
        assert!(
            os_ref.qualified,
            "os.getcwd ref must have qualified=true (os is imported)"
        );
    }

    // ── Task 2 (Phase-3d): module-init unit ───────────────────────────────────

    /// Helper: parse an inline Python `src`, build a synthetic `<module>` unit
    /// (if any), run `analyze_unit` on it, and return the `Hotspot`.
    fn module_init_hotspot(src: &str) -> Option<Hotspot> {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let unit = crate::functions::module_init_unit(&module)?;
        Some(analyze_unit(
            &unit,
            "test.py",
            &imports,
            &module_bindings,
            &span,
        ))
    }

    /// A module with import-time effects MUST emit a `<module>` hotspot whose
    /// own-body effects capture only the TOP-LEVEL statements — NOT effects from
    /// inside nested `def` bodies (own-body isolation assertion).
    ///
    /// Source:
    /// ```python
    /// import os
    /// CONFIG = os.environ["X"]   # ← top-level subscript read of os.environ
    /// print("loading")           # ← top-level call (logging effect)
    /// def impure():
    ///     open("f")              # ← inside def body — must NOT appear on <module>
    /// def pure():
    ///     return 1
    /// ```
    #[test]
    fn module_init_captures_top_level_effects_not_nested_def_body() {
        let src = concat!(
            "import os\n",
            "CONFIG = os.environ[\"X\"]\n",
            "print(\"loading\")\n",
            "def impure():\n",
            "    open(\"f\")\n",
            "def pure():\n",
            "    return 1\n",
        );
        let h = module_init_hotspot(src).expect("<module> hotspot must exist for impure module");

        // The `<module>` symbol is correct.
        assert_eq!(
            h.symbol, "<module>",
            "synthetic unit must have symbol '<module>'"
        );

        // The `<module>` hotspot must have at least one effect (the top-level call
        // to `print` or the `os.environ` subscript read).
        assert!(
            !h.effects.is_empty(),
            "<module> must have ≥1 effect from top-level statements; got: {:?}",
            h.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );

        // ISOLATION ASSERTION (the load-bearing correctness check):
        // `impure`'s `open("f")` is inside a `def` body — it MUST NOT appear on
        // the `<module>` unit's effects (own-body semantics: nested def bodies are
        // separate units, never charged to the enclosing module scope).
        let module_effect_kinds: Vec<&str> = h.effects.iter().map(|e| e.kind.wire()).collect();
        assert!(
            !module_effect_kinds.contains(&"net.fs.db"),
            "impure()'s open('f') is inside a def body and must NOT surface on <module>; \
             got module effects: {:?}",
            module_effect_kinds
        );

        // `impure` and `pure` are their own separate units (scored via scan_fixture).
        // Verify they can be found by the regular collect path.
        let module2 = libcst_native::parse_module(src, None).unwrap();
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize");
        let imports = Imports::build(&module2);
        let module_bindings = crate::imports::module_bindings(&module2);
        let (units, _) = crate::functions::collect(&module2, src, &span, &anchors);
        let impure_h = analyze_unit(
            units
                .iter()
                .find(|u| u.symbol == "impure")
                .expect("impure unit"),
            "test.py",
            &imports,
            &module_bindings,
            &span,
        );
        assert!(
            impure_h
                .effects
                .iter()
                .any(|e| e.kind.wire() == "net.fs.db"),
            "impure must have its own net.fs.db from open('f'); got: {:?}",
            impure_h
                .effects
                .iter()
                .map(|e| e.kind.wire())
                .collect::<Vec<_>>()
        );
    }

    /// A pure module (only `import` declarations and `def`/`class` definitions
    /// with no executable top-level statements) must produce NO `<module>` hotspot.
    #[test]
    fn pure_module_emits_no_module_init_hotspot() {
        let src = "import os\ndef f():\n    return 1\n";
        assert!(
            module_init_hotspot(src).is_none(),
            "a pure module (import + def, no top-level effects) must not emit a <module> hotspot"
        );
    }

    // ── Feature 025: module-init captures class decorators + base-class exprs ──

    /// A top-level class decorator that is an effectful call must be captured on
    /// `<module>`. `@open("y")` on a top-level class runs at import time.
    ///
    /// Uses `open(...)` which resolves to `net.fs.db` (class 7) — a concrete scored
    /// effect rather than an unscored call reference.
    #[test]
    fn module_init_captures_class_decorator_effect() {
        let src = concat!("@open(\"y\")\n", "class C:\n", "    pass\n",);
        let h = module_init_hotspot(src)
            .expect("<module> hotspot must exist when a class has an effectful decorator");
        assert!(
            h.effects.iter().any(|e| e.kind.wire() == "net.fs.db"),
            "class decorator open(\"y\") must charge net.fs.db to <module>; got: {:?}",
            h.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );
    }

    /// A top-level class base-class expression that is an effectful call must be
    /// captured on `<module>`. `class C(open("y")):` runs `open("y")` at import time.
    #[test]
    fn module_init_captures_class_base_expr_effect() {
        let src = concat!("class C(open(\"y\")):\n", "    pass\n",);
        let h = module_init_hotspot(src)
            .expect("<module> hotspot must exist when a class has an effectful base expression");
        assert!(
            h.effects.iter().any(|e| e.kind.wire() == "net.fs.db"),
            "class base open(\"y\") must charge net.fs.db to <module>; got: {:?}",
            h.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );
    }

    /// A method body's call MUST NOT appear on `<module>` — isolation is preserved
    /// even when the class has an effectful base expression that IS captured.
    ///
    /// `class C(open("y")): def m(self): open("z")`
    ///   → `open("y")` (base)  → captured on `<module>` (net.fs.db, exactly 1)
    ///   → `open("z")` (method body) → NOT on `<module>` (remains on `m`)
    #[test]
    fn module_init_class_header_captured_method_body_not() {
        let src = concat!(
            "class C(open(\"y\")):\n",
            "    def m(self):\n",
            "        open(\"z\")\n",
        );
        let h = module_init_hotspot(src)
            .expect("<module> hotspot must exist when a class has an effectful base expression");

        // The base-class `open("y")` must appear on `<module>`.
        assert!(
            h.effects.iter().any(|e| e.kind.wire() == "net.fs.db"),
            "class base open(\"y\") must charge net.fs.db to <module>; got: {:?}",
            h.effects.iter().map(|e| e.kind.wire()).collect::<Vec<_>>()
        );

        // ISOLATION GUARD: the method body contributes a SECOND `open` call which
        // must NOT appear on `<module>`. The count must be exactly 1 (only the base).
        let net_count = h
            .effects
            .iter()
            .filter(|e| e.kind.wire() == "net.fs.db")
            .count();
        assert_eq!(
            net_count, 1,
            "method body open(\"z\") must NOT be double-counted on <module>; \
             expected exactly 1 net.fs.db effect, got {net_count}"
        );
    }

    // ── Task 2 (Phase-3e): canonical_path in build_record ─────────────────────

    /// A module-level `write` function in `pkg/util.py` gets canonical_path
    /// `["pkg", "util", "write"]` (the module key + the function name).
    #[test]
    fn build_record_sets_canonical_path() {
        use crate::module_map::PyModuleMap;
        use fxrank_core::frontend::SourceFile;
        // Module pkg.util with a top-level `write`.
        let mmap = PyModuleMap::build(&[
            SourceFile {
                path: "pkg/__init__.py".into(),
                text: String::new(),
            },
            SourceFile {
                path: "pkg/util.py".into(),
                text: String::new(),
            },
        ]);
        let src = "def write():\n    pass\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == "write")
            .expect("write unit not found");
        let rec = build_record(
            unit,
            "pkg/util.py",
            &imports,
            &module_bindings,
            &span,
            &mmap,
        );
        assert_eq!(
            rec.canonical_path,
            vec!["pkg".to_string(), "util".into(), "write".into()],
            "module-level write in pkg/util.py must get canonical_path [pkg, util, write]"
        );
    }

    /// A CLASS method `write` inside `pkg/util.py` is NOT module-level and must get
    /// an empty canonical_path (so it can never false-resolve for
    /// `from pkg.util import write`).
    #[test]
    fn method_unit_gets_empty_canonical_path() {
        use crate::module_map::PyModuleMap;
        use fxrank_core::frontend::SourceFile;
        let mmap = PyModuleMap::build(&[
            SourceFile {
                path: "pkg/__init__.py".into(),
                text: String::new(),
            },
            SourceFile {
                path: "pkg/util.py".into(),
                text: String::new(),
            },
        ]);
        let src = "class C:\n    def write(self):\n        pass\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let method_unit = units
            .iter()
            .find(|u| u.symbol == "write")
            .expect("write method unit not found");
        assert!(
            !method_unit.is_module_level,
            "a class method must have is_module_level=false, got true"
        );
        let rec = build_record(
            method_unit,
            "pkg/util.py",
            &imports,
            &module_bindings,
            &span,
            &mmap,
        );
        assert!(
            rec.canonical_path.is_empty(),
            "a method must not get an importable canonical_path; got: {:?}",
            rec.canonical_path
        );
    }
}
