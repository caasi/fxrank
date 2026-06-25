//! Collect every function-unit (`def`/`async def`, method, nested `def`, `lambda`)
//! from a parsed Python `Module`.
//!
//! # Design
//! - Build `SpanIndex` **once** per file; pass it by reference through the recursion.
//! - Named units (`def`/`async def`): anchor via pointer-arithmetic on `name.value`
//!   (the libcst `Name.value: &str` borrows the original source buffer).
//! - Lambda units: collect in pre-order; zip with `lambda_anchors(src)` by index —
//!   the k-th `lambda` keyword token (source order) corresponds to the k-th `Lambda`
//!   node encountered in pre-order. For this ordinal bijection to hold, the
//!   lambda-collection walk must be **exhaustive**: it visits EVERY `Lambda` node
//!   anywhere it can syntactically appear (a **superset** of the effect driver's
//!   descent in `detect/mod.rs` — collection descends even into lazy
//!   generator-expression element bodies and decorator/param-default/`with`-item
//!   positions, because a lambda there is still its own tokenized unit). `collect`
//!   guards the bijection with a `debug_assert_eq!` against the tokenizer count, so
//!   any future drift fails loudly instead of silently mis-anchoring.
//!
//! # Borrowed AST (lifetime)
//! Unlike `syn`/`swc`, libcst's inflated tree **borrows** `&'a str` slices from the
//! source buffer; it is not owned. So a `FnUnit` cannot retain an *owned* body — it
//! borrows the body suite (or lambda body expression), parameters, and decorators
//! from the live `Module`. Collection and analysis therefore run in a **single
//! borrowed pass** (`PythonFrontend::analyze` keeps `module` + `src` alive while it
//! iterates), and `analyze_unit` emits **owned** `Hotspot`s so nothing borrowed
//! outlives the pass.

use libcst_native::{
    Annotation, Arg, ClassDef, CompoundStatement, Decorator, Expression, FunctionDef, Module,
    Parameters, SmallStatement, Statement, Suite,
};

use crate::source::{SpanIndex, anchor_of_subslice};

/// The body of a function-unit: a statement suite (`def`/method), a single
/// expression (`lambda`), or a module's top-level statement list (the synthetic
/// `<module>` unit). Borrowed from the parsed `Module`.
pub enum FnBody<'a> {
    /// A `def`/`async def`/method body — a statement suite.
    Suite(&'a Suite<'a>),
    /// A `lambda` body — a single expression.
    Expr(&'a Expression<'a>),
    /// The module's top-level statement list — used by the synthetic `<module>` unit
    /// that scores import-time effects. The own-body walker treats this like a suite
    /// of statements but does NOT descend into nested `def`/`class` bodies (those are
    /// separate units), exactly matching the behaviour of `walk_compound` for nested
    /// functions.
    Module(&'a [Statement<'a>]),
}

/// A single function-shaped scope collected from the source.
///
/// `line` and `col` are 1-based (char column), matching `core::Hotspot`.
///
/// Borrows the body, parameters, and decorators from the parsed `Module`; valid
/// only within the single borrowed collect-then-analyze pass.
pub struct FnUnit<'a> {
    pub symbol: String,
    pub line: usize,
    pub col: usize,
    pub is_async: bool,
    /// `true` when this unit should be treated as test code for the purpose of
    /// source-based skipping (see `PythonFrontend::analyze`):
    /// - a `test_*`-named top-level or nested function, OR
    /// - a method whose enclosing class name starts with `Test`, OR
    /// - a method whose enclosing class subclasses `unittest.TestCase`
    ///   (base named `TestCase` or `unittest.TestCase`).
    ///
    /// Lambdas are never test units. This flag is set at collection time when
    /// the class context is available; `PythonFrontend::analyze` checks it.
    pub is_test_unit: bool,
    /// `true` ONLY for a `def`/`async def` directly at the module top level.
    /// `false` for methods (inside a `ClassDef`), nested defs (inside another `def`),
    /// lambdas, and the synthetic `<module>` unit.
    ///
    /// This is the *never-false-resolve guard* for `canonical_path`: Python
    /// `FnUnit.symbol` is the BARE name (a method `def write` has symbol `"write"`),
    /// so without this flag a method `write` would get `canonical_path = [pkg,util,write]`
    /// and a `from pkg.util import write` call could false-resolve to the method.
    /// Only module-level defs are importable as `module.<name>`.
    pub is_module_level: bool,
    /// The function body (suite or lambda expression) to walk for own-body effects.
    pub body: FnBody<'a>,
    /// The unit's own parameters — their **default** expressions are charged to it.
    pub params: &'a Parameters<'a>,
    /// The unit's own decorators — their expressions are charged to it (a `lambda`
    /// has none, so this is empty for lambdas).
    pub decorators: &'a [Decorator<'a>],
    /// The unit's return annotation (`-> T`), if any. Used by `coverage` as the
    /// return slot. `None` for lambdas and for `def`s without a return annotation.
    pub returns: Option<&'a Annotation<'a>>,
}

/// Collect every `def`, `async def`, method, nested `def`, and `lambda` from `module`.
///
/// `src` must be the **same** BOM-stripped buffer that was passed to `parse_module`
/// (so that `name.value` subslice pointer arithmetic stays valid).
///
/// `span` must be the `SpanIndex` built from the same `src` buffer. The caller
/// builds it once and passes it here so only a **single** `SpanIndex` is
/// constructed per file — this function no longer builds its own.
///
/// `anchors` must be the pre-computed `lambda_anchors(src)` result (unwrapped by the
/// caller). Accepting anchors here (rather than calling `lambda_anchors` internally)
/// ensures tokenization happens exactly **once** per file — the caller obtains the
/// anchors, handles the `None` failure case, threads the slice in, and the count
/// invariant below closes the silent-drop hole without a second tokenizer pass.
///
/// Returns `(units, lambda_node_count)` where `lambda_node_count` is the number of
/// `Lambda` AST nodes encountered during the walk — incremented once per node,
/// regardless of whether an anchor was available and a unit was emitted. The caller
/// must compare this against `anchors.len()` to detect N≠M bijection breaks in
/// release builds (the in-body `debug_assert_eq!` catches drift in debug/test only).
pub fn collect<'a>(
    module: &'a Module<'a>,
    src: &str,
    span: &SpanIndex,
    anchors: &[(usize, usize)],
) -> (Vec<FnUnit<'a>>, usize) {
    let mut ctx = Ctx {
        src,
        span,
        anchors,
        lambda_idx: 0,
        class_stack: Vec::new(),
        def_depth: 0,
        out: Vec::new(),
    };

    for stmt in &module.body {
        collect_in_statement(stmt, &mut ctx);
    }

    // Safety invariant: the lambda-collection walk MUST visit exactly as many
    // `Lambda` nodes as `lambda_anchors` counted `lambda` keyword tokens, in the
    // same order. `lambda_idx` is incremented once per Lambda node encountered
    // (even when `anchors.get(lambda_idx)` returns `None` and no unit is emitted),
    // so it equals the total Lambda-node count. If the two walks ever drift (a new
    // expression position holding a lambda that collection misses), this fails loudly
    // in debug/test builds rather than silently mis-anchoring every subsequent lambda.
    debug_assert_eq!(
        ctx.lambda_idx,
        anchors.len(),
        "lambda collection ({}) drifted from tokenizer lambda count ({}); \
         a Lambda-bearing expression position is not visited by collect_in_expr",
        ctx.lambda_idx,
        anchors.len(),
    );

    let lambda_node_count = ctx.lambda_idx;
    (ctx.out, lambda_node_count)
}

/// Build a synthetic [`FnUnit`] representing a module's top-level initialisation
/// code — the statements that execute when the module is first imported.
///
/// The unit gets symbol `"<module>"`, `line = 1`, `col = 1`, and
/// `body = FnBody::Module(&module.body)`. `is_root` is always `false` at the
/// frontend level — the CLI sets the real value for explicit-file entries.
///
/// **Own-body semantics are preserved**: the own-body walker walks each top-level
/// statement but does NOT descend into nested `def`/`class` bodies (those are
/// separate units). Import statements (`import`, `from … import`) have no own-body
/// runtime effect and are simply skipped by the detectors' `walk_small`
/// (`Pass`/`Import`/`ImportFrom`/`Global`/`Nonlocal` → no-op arm).
///
/// Returns `None` when the module has no top-level executable statements (e.g.
/// a module containing only `import` declarations and function/class definitions),
/// because the caller will score first and skip emission when there are no effects
/// — returning `None` early avoids building empty units. Callers should additionally
/// skip emitting the resulting `Hotspot` when `hotspot.effects.is_empty()`.
pub fn module_init_unit<'a>(module: &'a Module<'a>) -> Option<FnUnit<'a>> {
    // A module with only imports and function/class definitions has no top-level
    // executable statements that can produce import-time effects. Detect this
    // cheaply: if every statement is either an import or a compound def/class,
    // return None early. (The walker itself would produce no effects anyway, but
    // returning None avoids building the synthetic unit at all.)
    let has_executable = module.body.iter().any(|stmt| {
        match stmt {
            Statement::Simple(line) => line.body.iter().any(|small| {
                // Import* and Pass and Global/Nonlocal/Break/Continue carry no
                // import-time effects; everything else (Expr, Assign, AugAssign,
                // AnnAssign, Return, Raise, Assert, Del, TypeAlias) might.
                !matches!(
                    small,
                    SmallStatement::Import(_)
                        | SmallStatement::ImportFrom(_)
                        | SmallStatement::Pass(_)
                        | SmallStatement::Global(_)
                        | SmallStatement::Nonlocal(_)
                        | SmallStatement::Break(_)
                        | SmallStatement::Continue(_)
                )
            }),
            Statement::Compound(c) => {
                // `def` at top level is its OWN unit; only the `def` statement itself
                // runs at import time (no body effects) — not executable for module-init.
                // A `class` at top level IS executable: its body runs at class-definition
                // time (import time).  `class C: DATA = load_config()` runs `load_config()`
                // at import.  (Methods inside are their own units and are handled by the
                // walker, not here.)
                // Other compound statements (if, for, while, try, with, match) also run
                // at import time.
                !matches!(c, CompoundStatement::FunctionDef(_))
            }
        }
    });

    if !has_executable {
        return None;
    }

    Some(FnUnit {
        symbol: "<module>".to_owned(),
        line: 1,
        col: 1,
        is_async: false,
        is_test_unit: false,
        // The synthetic `<module>` unit is not a real importable function.
        is_module_level: false,
        body: FnBody::Module(&module.body),
        params: &EMPTY_PARAMS,
        decorators: &[],
        returns: None,
    })
}

/// A static empty `Parameters` used as the `params` field for the synthetic
/// `<module>` unit (which has no parameters).
///
/// All fields are empty / `None`, so `Parameters<'static>` has no actual
/// lifetime-dependent borrows. A `&'static Parameters<'static>` satisfies any
/// `&'a Parameters<'a>` constraint via lifetime coercion (`'static: 'a`).
static EMPTY_PARAMS: std::sync::LazyLock<libcst_native::Parameters<'static>> =
    std::sync::LazyLock::new(|| libcst_native::Parameters {
        params: vec![],
        posonly_params: vec![],
        star_arg: None,
        kwonly_params: vec![],
        star_kwarg: None,
        posonly_ind: None,
    });

/// Enclosing class context for source-based test detection.
///
/// When collecting methods inside a `ClassDef`, we push a `ClassCtx` so that
/// each emitted method `FnUnit` can be tagged `is_test_unit` if the class is a
/// test class (name starts with `Test` or it subclasses `unittest.TestCase`).
#[derive(Clone)]
struct ClassCtx {
    /// `true` when the enclosing class is a test class.
    is_test_class: bool,
}

/// Shared traversal context — bundles the immutable lookup tables and the mutable
/// lambda cursor + output, so the recursion signatures stay short.
struct Ctx<'a, 'b> {
    src: &'b str,
    span: &'b SpanIndex<'b>,
    anchors: &'b [(usize, usize)],
    lambda_idx: usize,
    /// Stack of enclosing class contexts (outermost first).
    /// Empty when not inside any class body.
    class_stack: Vec<ClassCtx>,
    /// Number of enclosing `def`/`async def` scopes. A `def` at the module top
    /// level has `def_depth == 0` at the point it is emitted; nested defs and
    /// methods have `def_depth >= 1`. Used to set `FnUnit.is_module_level`.
    def_depth: usize,
    out: Vec<FnUnit<'a>>,
}

// ─── statement-level traversal ────────────────────────────────────────────────

fn collect_in_statement<'a>(stmt: &'a Statement<'a>, ctx: &mut Ctx<'a, '_>) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                collect_in_small(small, ctx);
            }
        }
        Statement::Compound(compound) => {
            collect_in_compound(compound, ctx);
        }
    }
}

fn collect_in_compound<'a>(compound: &'a CompoundStatement<'a>, ctx: &mut Ctx<'a, '_>) {
    match compound {
        CompoundStatement::FunctionDef(f) => {
            collect_funcdef(f, ctx);
        }
        CompoundStatement::ClassDef(c) => {
            collect_classdef(c, ctx);
        }
        CompoundStatement::If(i) => {
            collect_in_expr(&i.test, ctx);
            collect_in_suite(&i.body, ctx);
            // `elif`/`else` clauses are flattened into `orelse` — traverse if present
            if let Some(orelse) = &i.orelse {
                collect_in_or_else(orelse, ctx);
            }
        }
        CompoundStatement::For(f) => {
            collect_in_expr(&f.iter, ctx);
            collect_in_suite(&f.body, ctx);
            if let Some(orelse) = &f.orelse {
                collect_in_suite(&orelse.body, ctx);
            }
        }
        CompoundStatement::While(w) => {
            collect_in_expr(&w.test, ctx);
            collect_in_suite(&w.body, ctx);
            if let Some(orelse) = &w.orelse {
                collect_in_suite(&orelse.body, ctx);
            }
        }
        CompoundStatement::Try(t) => {
            collect_in_suite(&t.body, ctx);
            for handler in &t.handlers {
                collect_in_suite(&handler.body, ctx);
            }
            if let Some(orelse) = &t.orelse {
                collect_in_suite(&orelse.body, ctx);
            }
            if let Some(finalbody) = &t.finalbody {
                collect_in_suite(&finalbody.body, ctx);
            }
        }
        CompoundStatement::TryStar(t) => {
            collect_in_suite(&t.body, ctx);
            for handler in &t.handlers {
                collect_in_suite(&handler.body, ctx);
            }
            if let Some(orelse) = &t.orelse {
                collect_in_suite(&orelse.body, ctx);
            }
            if let Some(finalbody) = &t.finalbody {
                collect_in_suite(&finalbody.body, ctx);
            }
        }
        CompoundStatement::With(w) => {
            // `with`-item context expressions are evaluated here and may hold lambdas
            // (e.g. `with (lambda: cm())() as c:`), so descend into them.
            for item in &w.items {
                collect_in_expr(&item.item, ctx);
            }
            collect_in_suite(&w.body, ctx);
        }
        CompoundStatement::Match(m) => {
            collect_in_expr(&m.subject, ctx);
            for case in &m.cases {
                collect_in_suite(&case.body, ctx);
            }
        }
    }
}

/// Traverse an `If`'s `orelse` field, which is itself an `Elif` or an `Else`.
fn collect_in_or_else<'a>(orelse: &'a libcst_native::OrElse<'a>, ctx: &mut Ctx<'a, '_>) {
    match orelse {
        libcst_native::OrElse::Elif(elif) => {
            collect_in_expr(&elif.test, ctx);
            collect_in_suite(&elif.body, ctx);
            if let Some(inner) = &elif.orelse {
                collect_in_or_else(inner, ctx);
            }
        }
        libcst_native::OrElse::Else(e) => {
            collect_in_suite(&e.body, ctx);
        }
    }
}

fn collect_funcdef<'a>(f: &'a FunctionDef<'a>, ctx: &mut Ctx<'a, '_>) {
    // Anchor on the function name subslice (borrows `src`).
    let off = anchor_of_subslice(ctx.src, f.name.value);
    let (line, col) = ctx.span.line_col(off);

    // Source-based test detection:
    // - `test_*` named function (at any nesting level), OR
    // - method of an enclosing class that is a test class.
    let in_test_class = ctx.class_stack.last().is_some_and(|c| c.is_test_class);
    let is_test_unit = f.name.value.starts_with("test_") || in_test_class;

    // Module-level guard: a def is module-level only when it is directly in the
    // top-level statement list (def_depth == 0, class_stack empty). A method
    // (class_stack non-empty) and a nested def (def_depth >= 1) are NOT
    // module-level and thus not importable as `module.<name>`. (P2-1)
    let is_module_level = ctx.def_depth == 0 && ctx.class_stack.is_empty();

    ctx.out.push(FnUnit {
        symbol: f.name.value.to_owned(),
        line,
        col,
        is_async: f.asynchronous.is_some(),
        is_test_unit,
        is_module_level,
        body: FnBody::Suite(&f.body),
        params: &f.params,
        decorators: &f.decorators,
        returns: f.returns.as_ref(),
    });
    // Decorator expressions and parameter-default expressions are evaluated at
    // def-time in the enclosing scope and may contain lambdas (each still its own
    // unit + tokenized), so collection must descend into them — in source order:
    // decorators precede the `def`, parameter defaults follow it.
    for dec in &f.decorators {
        collect_in_expr(&dec.decorator, ctx);
    }
    collect_in_params(&f.params, ctx);
    // Recurse into body for nested defs and lambdas. Increment def_depth so
    // any nested defs are NOT treated as module-level.
    ctx.def_depth += 1;
    collect_in_suite(&f.body, ctx);
    ctx.def_depth -= 1;
}

/// Collect lambdas in parameter-default expressions (`def f(cb=lambda: 0)`).
fn collect_in_params<'a>(params: &'a Parameters<'a>, ctx: &mut Ctx<'a, '_>) {
    let positional = params
        .posonly_params
        .iter()
        .chain(&params.params)
        .chain(&params.kwonly_params);
    for p in positional {
        if let Some(default) = &p.default {
            collect_in_expr(default, ctx);
        }
    }
    if let Some(libcst_native::StarArg::Param(p)) = &params.star_arg
        && let Some(default) = &p.default
    {
        collect_in_expr(default, ctx);
    }
    if let Some(p) = &params.star_kwarg
        && let Some(default) = &p.default
    {
        collect_in_expr(default, ctx);
    }
}

fn collect_classdef<'a>(c: &'a ClassDef<'a>, ctx: &mut Ctx<'a, '_>) {
    // We do NOT emit a FnUnit for the class itself (classes aren't functions).
    // Push class context so nested methods know their enclosing class.
    let is_test_class = class_name_is_test(c.name.value) || class_bases_are_test_case(&c.bases);
    // Class decorators, base-class arg values, and keyword args are evaluated at
    // class-definition time in the enclosing scope and may contain lambdas (each
    // still tokenized) — collection must descend so the bijection holds.
    for dec in &c.decorators {
        collect_in_expr(&dec.decorator, ctx);
    }
    for base in &c.bases {
        collect_in_expr(&base.value, ctx);
    }
    for kw in &c.keywords {
        collect_in_expr(&kw.value, ctx);
    }
    ctx.class_stack.push(ClassCtx { is_test_class });
    collect_in_suite(&c.body, ctx);
    ctx.class_stack.pop();
}

/// Return `true` if the class name signals it is a test class (`Test*` prefix).
fn class_name_is_test(name: &str) -> bool {
    name.starts_with("Test")
}

/// Return `true` if any base in `bases` names `TestCase` or `unittest.TestCase`.
///
/// Matches:
/// - `class Foo(TestCase):`      → base is a `Name("TestCase")`
/// - `class Foo(unittest.TestCase):` → base is an `Attribute { value: Name("unittest"), attr: "TestCase" }`
fn class_bases_are_test_case(bases: &[Arg<'_>]) -> bool {
    bases.iter().any(|arg| match &arg.value {
        Expression::Name(n) => n.value == "TestCase",
        Expression::Attribute(a) => {
            a.attr.value == "TestCase"
                && matches!(&*a.value, Expression::Name(n) if n.value == "unittest")
        }
        _ => false,
    })
}

fn collect_in_suite<'a>(suite: &'a Suite<'a>, ctx: &mut Ctx<'a, '_>) {
    let stmts: &[Statement<'a>] = match suite {
        Suite::IndentedBlock(b) => &b.body,
        Suite::SimpleStatementSuite(s) => {
            // A one-liner body (`def f(): return 1`). Walk small statements for lambdas.
            for small in &s.body {
                collect_in_small(small, ctx);
            }
            return;
        }
    };
    for stmt in stmts {
        collect_in_statement(stmt, ctx);
    }
}

// ─── small-statement-level traversal ──────────────────────────────────────────

/// Walk a `SmallStatement` for `Lambda` nodes (no new named functions here).
fn collect_in_small<'a>(small: &'a SmallStatement<'a>, ctx: &mut Ctx<'a, '_>) {
    match small {
        SmallStatement::Assign(a) => {
            collect_in_expr(&a.value, ctx);
        }
        SmallStatement::AnnAssign(a) => {
            if let Some(v) = &a.value {
                collect_in_expr(v, ctx);
            }
        }
        SmallStatement::AugAssign(a) => {
            collect_in_expr(&a.value, ctx);
        }
        SmallStatement::Return(r) => {
            if let Some(v) = &r.value {
                collect_in_expr(v, ctx);
            }
        }
        SmallStatement::Expr(e) => {
            collect_in_expr(&e.value, ctx);
        }
        SmallStatement::Raise(r) => {
            if let Some(exc) = &r.exc {
                collect_in_expr(exc, ctx);
            }
            if let Some(from) = &r.cause {
                collect_in_expr(&from.item, ctx);
            }
        }
        SmallStatement::Assert(a) => {
            collect_in_expr(&a.test, ctx);
            if let Some(msg) = &a.msg {
                collect_in_expr(msg, ctx);
            }
        }
        SmallStatement::Del(d) => {
            collect_in_del_target(&d.target, ctx);
        }
        // Pass / Break / Continue / Import / ImportFrom / Global / Nonlocal /
        // TypeAlias hold no function-shaped sub-expressions we need.
        _ => {}
    }
}

// ─── expression-level traversal ───────────────────────────────────────────────

/// **Exhaustive** pre-order traversal of an expression tree, collecting every
/// `Lambda` node and recursing into **every** child-expression of every variant
/// that can contain a sub-expression.
///
/// # Bijection invariant (FIX 1)
/// This walk MUST visit every `Lambda` node anywhere it can syntactically appear,
/// so that the count and pre-order of collected lambdas EXACTLY matches the
/// `lambda` keyword tokens that `lambda_anchors` (the tokenizer) counts. A missed
/// lambda makes every subsequently-collected lambda consume the wrong anchor.
///
/// Lambda collection is therefore a **superset** of the effect driver's descent
/// (`detect/mod.rs`): the driver skips *lazy* generator-expression element bodies
/// for effect attribution, but a lambda living there is still its own unit and is
/// still tokenized — so collection descends there unconditionally. Do not copy the
/// driver's eager/lazy rules here.
fn collect_in_expr<'a>(expr: &'a Expression<'a>, ctx: &mut Ctx<'a, '_>) {
    match expr {
        Expression::Lambda(l) => {
            // Pre-order: emit this lambda BEFORE descending into its body.
            // Lambdas are never considered test units or roots, and are never
            // module-level (not importable as `module.<name>`).
            if let Some(&(line, col)) = ctx.anchors.get(ctx.lambda_idx) {
                ctx.out.push(FnUnit {
                    symbol: format!("<lambda@L{line}C{col}>"),
                    line,
                    col,
                    is_async: false,
                    is_test_unit: false,
                    is_module_level: false,
                    body: FnBody::Expr(&l.body),
                    params: &l.params,
                    decorators: &[],
                    returns: None,
                });
            }
            ctx.lambda_idx += 1;
            // A lambda's own parameter defaults are evaluated at def-time and may
            // hold nested lambdas; descend into them and the body.
            collect_in_params(&l.params, ctx);
            collect_in_expr(&l.body, ctx);
        }

        // ── compound expressions that may contain lambdas ──────────────────────
        Expression::BinaryOperation(b) => {
            collect_in_expr(&b.left, ctx);
            collect_in_expr(&b.right, ctx);
        }
        Expression::BooleanOperation(b) => {
            collect_in_expr(&b.left, ctx);
            collect_in_expr(&b.right, ctx);
        }
        Expression::UnaryOperation(u) => {
            collect_in_expr(&u.expression, ctx);
        }
        Expression::Comparison(c) => {
            collect_in_expr(&c.left, ctx);
            for comp in &c.comparisons {
                collect_in_expr(&comp.comparator, ctx);
            }
        }
        Expression::IfExp(i) => {
            collect_in_expr(&i.test, ctx);
            collect_in_expr(&i.body, ctx);
            collect_in_expr(&i.orelse, ctx);
        }
        Expression::Call(c) => {
            collect_in_expr(&c.func, ctx);
            // Every argument value — positional, keyword (`k=lambda…`), and starred
            // (`*args` / `**kw`) — is held in `arg.value`.
            for arg in &c.args {
                collect_in_expr(&arg.value, ctx);
            }
        }
        Expression::Attribute(a) => {
            collect_in_expr(&a.value, ctx);
        }
        Expression::Subscript(s) => {
            collect_in_expr(&s.value, ctx);
            // The slice keys can hold lambdas (`d[(lambda: k)()]`).
            for element in &s.slice {
                collect_in_base_slice(&element.slice, ctx);
            }
        }
        Expression::Tuple(t) => {
            for el in &t.elements {
                collect_in_element(el, ctx);
            }
        }
        Expression::List(l) => {
            for el in &l.elements {
                collect_in_element(el, ctx);
            }
        }
        Expression::Set(s) => {
            for el in &s.elements {
                collect_in_element(el, ctx);
            }
        }
        Expression::Dict(d) => {
            for el in &d.elements {
                match el {
                    libcst_native::DictElement::Simple { key, value, .. } => {
                        collect_in_expr(key, ctx);
                        collect_in_expr(value, ctx);
                    }
                    libcst_native::DictElement::Starred(s) => {
                        collect_in_expr(&s.value, ctx);
                    }
                }
            }
        }
        // Comprehensions: descend into the element/key/value AND the full `for … in`
        // clause(s) (iterable, `if` filters, nested fors). Unconditional — unlike the
        // effect driver, collection does not treat generator expressions as lazy.
        Expression::ListComp(l) => {
            collect_in_expr(&l.elt, ctx);
            collect_in_comp_for(&l.for_in, ctx);
        }
        Expression::SetComp(s) => {
            collect_in_expr(&s.elt, ctx);
            collect_in_comp_for(&s.for_in, ctx);
        }
        Expression::GeneratorExp(g) => {
            collect_in_expr(&g.elt, ctx);
            collect_in_comp_for(&g.for_in, ctx);
        }
        Expression::DictComp(d) => {
            collect_in_expr(&d.key, ctx);
            collect_in_expr(&d.value, ctx);
            collect_in_comp_for(&d.for_in, ctx);
        }
        Expression::FormattedString(fs) => {
            collect_in_fstring_parts(&fs.parts, ctx);
        }
        Expression::Yield(y) => {
            if let Some(v) = &y.value {
                match &**v {
                    libcst_native::YieldValue::Expression(e) => {
                        collect_in_expr(e, ctx);
                    }
                    libcst_native::YieldValue::From(f) => {
                        collect_in_expr(&f.item, ctx);
                    }
                }
            }
        }
        Expression::Await(a) => {
            collect_in_expr(&a.expression, ctx);
        }
        Expression::NamedExpr(n) => {
            collect_in_expr(&n.value, ctx);
        }
        Expression::StarredElement(s) => {
            collect_in_expr(&s.value, ctx);
        }

        // Leaf expressions (Name, Ellipsis, Integer, Float, Imaginary, SimpleString,
        // ConcatenatedString, TemplatedString) contain no lambdas.
        _ => {}
    }
}

/// Walk a comprehension's `for … in …` clause(s): the iterable, every `if` filter,
/// and any nested `for`. All can contain lambdas.
fn collect_in_comp_for<'a>(comp: &'a libcst_native::CompFor<'a>, ctx: &mut Ctx<'a, '_>) {
    collect_in_expr(&comp.iter, ctx);
    for cond in &comp.ifs {
        collect_in_expr(&cond.test, ctx);
    }
    if let Some(inner) = &comp.inner_for_in {
        collect_in_comp_for(inner, ctx);
    }
}

/// Walk a subscript slice (`Index` value, or `Slice` lower/upper/step) for lambdas.
fn collect_in_base_slice<'a>(slice: &'a libcst_native::BaseSlice<'a>, ctx: &mut Ctx<'a, '_>) {
    match slice {
        libcst_native::BaseSlice::Index(i) => collect_in_expr(&i.value, ctx),
        libcst_native::BaseSlice::Slice(s) => {
            if let Some(lower) = &s.lower {
                collect_in_expr(lower, ctx);
            }
            if let Some(upper) = &s.upper {
                collect_in_expr(upper, ctx);
            }
            if let Some(step) = &s.step {
                collect_in_expr(step, ctx);
            }
        }
    }
}

/// Walk f-string parts for lambdas — both the `{expr}` interpolations and any
/// nested `format_spec` (which can itself contain further interpolations).
fn collect_in_fstring_parts<'a>(
    parts: &'a [libcst_native::FormattedStringContent<'a>],
    ctx: &mut Ctx<'a, '_>,
) {
    for part in parts {
        if let libcst_native::FormattedStringContent::Expression(e) = part {
            collect_in_expr(&e.expression, ctx);
            if let Some(spec) = &e.format_spec {
                collect_in_fstring_parts(spec, ctx);
            }
        }
    }
}

/// Walk a `del` target for lambdas (only `del d[(lambda: k)()]`-style subscripts
/// can hold one, but the tokenizer still counts it, so descend for completeness).
fn collect_in_del_target<'a>(
    target: &'a libcst_native::DelTargetExpression<'a>,
    ctx: &mut Ctx<'a, '_>,
) {
    match target {
        libcst_native::DelTargetExpression::Attribute(a) => collect_in_expr(&a.value, ctx),
        libcst_native::DelTargetExpression::Subscript(s) => {
            collect_in_expr(&s.value, ctx);
            for element in &s.slice {
                collect_in_base_slice(&element.slice, ctx);
            }
        }
        libcst_native::DelTargetExpression::Tuple(t) => {
            for el in &t.elements {
                collect_in_element(el, ctx);
            }
        }
        libcst_native::DelTargetExpression::List(l) => {
            for el in &l.elements {
                collect_in_element(el, ctx);
            }
        }
        libcst_native::DelTargetExpression::Name(_) => {}
    }
}

fn collect_in_element<'a>(el: &'a libcst_native::Element<'a>, ctx: &mut Ctx<'a, '_>) {
    match el {
        libcst_native::Element::Simple { value, .. } => collect_in_expr(value, ctx),
        libcst_native::Element::Starred(s) => collect_in_expr(&s.value, ctx),
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_all_named_and_lambda_units() {
        let src = std::fs::read_to_string("tests/fixtures/functions.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let span = SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, lambda_node_count) = collect(&module, &src, &span, &anchors);
        assert_eq!(
            lambda_node_count,
            anchors.len(),
            "lambda node count must equal tokenizer anchor count"
        );
        let symbols: Vec<&str> = units.iter().map(|u| u.symbol.as_str()).collect();
        assert!(symbols.contains(&"top"));
        assert!(symbols.contains(&"method"));
        assert!(symbols.contains(&"fetcher"));
        assert!(units.iter().any(|u| u.symbol.starts_with("<lambda@L")));
        assert!(
            units
                .iter()
                .find(|u| u.symbol == "fetcher")
                .unwrap()
                .is_async
        );
        // four lambdas (g, h, nested-outer, nested-inner), each a distinct anchor —
        // proves empty-body (h) and nested (outer+inner) anchor via the ordinal bijection.
        let mut lambdas: Vec<&str> = symbols
            .iter()
            .filter(|s| s.starts_with("<lambda@L"))
            .cloned()
            .collect();
        assert_eq!(lambdas.len(), 4);
        lambdas.sort();
        lambdas.dedup();
        assert_eq!(lambdas.len(), 4, "all lambda anchors distinct");
    }

    /// Regression for the lambda anchor bijection (FIX 1). The fixture places
    /// lambdas in positions the original collection walk MISSED (comprehension
    /// iterable/condition, generator-expression element body, subscript slice,
    /// f-string expression, parameter default, `with`-item) BEFORE an effectful
    /// trailing lambda. If any leading lambda is dropped, the ordinal bijection
    /// drifts and the trailing lambda gets the wrong anchor.
    #[test]
    fn lambda_collection_count_matches_tokenizer_and_trailing_anchor_correct() {
        let src = std::fs::read_to_string("tests/fixtures/lambda_positions.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let span = SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, lambda_node_count) = collect(&module, &src, &span, &anchors);

        let lambdas: Vec<&str> = units
            .iter()
            .map(|u| u.symbol.as_str())
            .filter(|s| s.starts_with("<lambda@L"))
            .collect();

        // Count invariant: Lambda-node count (ctx.lambda_idx) == tokenizer anchor count.
        // This uses the node count, not the emitted-unit count, so N>M (more nodes than
        // anchors) is detected even when M units were still emitted successfully.
        let anchor_count = anchors.len();
        assert_eq!(
            lambda_node_count, anchor_count,
            "lambda node count must equal tokenizer count; got {lambdas:?}"
        );

        // The trailing `t = lambda z: requests.get(z)` lives on the last
        // non-blank line. Find its true (line, col) directly from the source and
        // assert the collected anchor matches.
        let (line0, line_text) = src
            .lines()
            .enumerate()
            .find(|(_, l)| l.starts_with("t = lambda"))
            .expect("trailing lambda line present");
        let line = line0 + 1;
        let col = line_text.find("lambda").unwrap() + 1; // 1-based char col (ASCII line)
        let expected = format!("<lambda@L{line}C{col}>");
        assert!(
            lambdas.contains(&expected.as_str()),
            "trailing lambda must anchor to {expected}; got {lambdas:?}"
        );
    }
}
