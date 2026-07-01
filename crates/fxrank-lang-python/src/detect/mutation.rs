//! Mutation detection with escape analysis for Python — the `fxrank-lang-python`
//! analog of `fxrank-lang-ts`'s `detect/mutation.rs`.
//!
//! Python's mutation story is simpler than Rust's (no `&mut` / ownership) but
//! more nuanced than JS's: `global` and `nonlocal` declarations are function-wide
//! (not position-dependent), and `self.attr = …` inside `__init__` is construction
//! (contained), not escaping state mutation.
//!
//! ## Escape classification table
//!
//! | write site                                   | kind             | class | contained |
//! |----------------------------------------------|------------------|-------|-----------|
//! | `global x` declared, then `x = …` or `x += …` | `global.mutation` | 6  | false     |
//! | `nonlocal x` declared, then `x = …` or `x += …` | `this.mutation` | 3 | false     |
//! | `self.attr = …` in `__init__`               | `local.mutation` | 1     | **true**  |
//! | `self.attr = …` in a non-`__init__` method  | `this.mutation`  | 3     | false     |
//! | `self.x.append(…)` / `self[i] = …` (any method, incl. `__init__`) | `this.mutation` | 3 | false |
//! | write where root is a **param name**        | `param.mutation` | 3     | false     |
//! | write where root is a **local binding**     | `local.mutation` | 1     | **true**  |
//! | module top-level binding, content-mutated (no `global`) | `global.mutation` | 6 | false |
//!
//! ## Strategy
//!
//! 1. **Pre-scan** the function body for `global`/`nonlocal` declarations and
//!    local bindings — `Assign`/`AnnAssign` targets (incl. tuple/list/starred
//!    destructuring), `AugAssign` bare-`Name` targets, `for`/`async for` loop
//!    targets, `with … as`/`async with … as` names, and `except … as` /
//!    `except* … as` names — building the `globals`, `nonlocals`, and `locals`
//!    sets. (Python scoping: any binding-form in a body makes the name
//!    function-local for the whole function.)
//! 2. **Extract** parameter names from `unit.params`.
//! 3. **Walk** the body classifying write targets: `Assign`/`AnnAssign`/`AugAssign`
//!    targets, and mutating method calls (`.append`, `.update`, `.add`) via
//!    `on_call` in the EffectSink.
//!
//! The `contained` bool returned alongside each `Effect` is the
//! boundary-containment signal that Task 9's discount consumes.

use std::collections::HashSet;

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use libcst_native::{
    Assert, AssignTargetExpression, Call, Expression, Name, Parameters, Raise, SmallStatement,
    Statement, Suite,
};

use super::expr::render_expr;
use super::{EffectSink, walk_own_body};
use crate::functions::{FnBody, FnUnit};
use crate::imports::Imports;
use crate::source::{SpanIndex, anchor_of_subslice};

/// Detect mutation effects in `unit`'s own body, with escape analysis.
///
/// Returns `(Effect, contained)` pairs. The `bool` is the containment flag —
/// `true` means the write is bounded to this function's scope (local init or
/// constructor init); `false` means it escapes.
///
/// Task 9 consumes the `contained` flags to apply boundary-containment discounts.
pub fn detect(
    unit: &FnUnit,
    imports: &Imports,
    module_bindings: &HashSet<String>,
    span: &SpanIndex,
) -> Vec<(Effect, bool)> {
    // ── Step 1: collect param names from the unit's signature ────────────────
    let params = collect_param_names(unit.params);

    // ── Step 2: pre-scan body for global/nonlocal declarations + local assigns ─
    let mut globals: HashSet<String> = HashSet::new();
    let mut nonlocals: HashSet<String> = HashSet::new();
    let mut locals: HashSet<String> = HashSet::new();
    prescan_body(&unit.body, &mut globals, &mut nonlocals, &mut locals);

    // ── Step 3: classify writes via the EffectSink driver ────────────────────
    let is_init = unit.symbol == "__init__";
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
    walk_own_body(unit, &mut sink);
    sink.effects
}

// ─── parameter name extraction ────────────────────────────────────────────────

/// Extract all parameter name strings from `params`.
///
/// Covers positional-only, regular, keyword-only, and `**kwargs` params;
/// skips the `*` bare separator. The `self`/`cls` first-param convention is
/// included — callers that want to exclude it do so by not treating `self` as
/// a mutation target (it is handled specially in the write-site classifier).
fn collect_param_names(params: &Parameters) -> HashSet<String> {
    let mut out = HashSet::new();
    let all = params
        .posonly_params
        .iter()
        .chain(&params.params)
        .chain(&params.kwonly_params);
    for p in all {
        out.insert(p.name.value.to_owned());
    }
    if let Some(libcst_native::StarArg::Param(p)) = &params.star_arg {
        out.insert(p.name.value.to_owned());
    }
    if let Some(p) = &params.star_kwarg {
        out.insert(p.name.value.to_owned());
    }
    out
}

// ─── pre-scan: global/nonlocal declarations + local bindings ─────────────────

/// Walk the body suite or lambda body expression to collect:
/// - `globals`: names declared with `global`.
/// - `nonlocals`: names declared with `nonlocal`.
/// - `locals`: names introduced by assignment in the function body — including
///   `Assign`/`AnnAssign` targets (incl. tuple/list/starred destructuring),
///   `AugAssign` bare-`Name` targets, `for`/`async for` loop targets,
///   `with … as`/`async with … as` names, and `except … as` / `except* … as`
///   names.  (Python scoping: any binding-form in a body makes the name
///   function-local for the whole function.)
///
/// Residual accepted limits (not collected here): `match` pattern captures,
/// comprehension-scope targets (Python 3 gives them their own scope), and
/// walrus (`:=`) operator targets.
///
/// Only scans the **own** body (does not descend into nested `def`/`lambda`).
fn prescan_body(
    body: &FnBody,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
) {
    match body {
        // Function/lambda body: nested def/class names ARE local bindings of this
        // function, so `seed_defs = true` (refs #55).
        FnBody::Suite(suite) => prescan_suite(suite, globals, nonlocals, locals, true),
        FnBody::Expr(_) => {} // lambdas have no statements
        // The `<module>` unit: prescan each top-level statement. Module-level
        // names that appear in a `global` declaration inside a nested def are
        // handled by that nested def's own prescan; at module scope itself,
        // every bare name assignment IS a module-level binding (no `global` needed).
        // `seed_defs = false`: a top-level def/class name is a MODULE binding
        // (handled by `module_bindings` → global.mutation), not a local of the
        // `<module>` unit — seeding it would wrongly downgrade module-scope
        // `SomeClass.attr = …` from global to local (refs #55 regression guard).
        FnBody::Module(stmts) => {
            for stmt in *stmts {
                prescan_stmt(stmt, globals, nonlocals, locals, false);
            }
        }
    }
}

fn prescan_suite(
    suite: &Suite,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    seed_defs: bool,
) {
    match suite {
        Suite::IndentedBlock(b) => {
            for stmt in &b.body {
                prescan_stmt(stmt, globals, nonlocals, locals, seed_defs);
            }
        }
        Suite::SimpleStatementSuite(s) => {
            for small in &s.body {
                prescan_small(small, globals, nonlocals, locals);
            }
        }
    }
}

fn prescan_stmt(
    stmt: &Statement,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    seed_defs: bool,
) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                prescan_small(small, globals, nonlocals, locals);
            }
        }
        Statement::Compound(c) => prescan_compound(c, globals, nonlocals, locals, seed_defs),
    }
}

fn prescan_compound(
    compound: &libcst_native::CompoundStatement,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    seed_defs: bool,
) {
    use libcst_native::CompoundStatement;
    match compound {
        // Nested def/class: do NOT descend (own-body attribution), but in a FUNCTION
        // body the def/class *name* is a local binding of the enclosing function —
        // seed it so a later write to it (e.g. `helper.alters_data = True`) resolves
        // as local, not a captured opaque binding (refs #55). `seed_defs` is false for
        // the `<module>` unit, where such a name is a module binding (→ global), so it
        // must NOT be seeded there.
        CompoundStatement::FunctionDef(f) => {
            if seed_defs {
                locals.insert(f.name.value.to_owned());
            }
        }
        CompoundStatement::ClassDef(c) => {
            if seed_defs {
                locals.insert(c.name.value.to_owned());
            }
        }
        CompoundStatement::If(i) => {
            prescan_suite(&i.body, globals, nonlocals, locals, seed_defs);
            if let Some(orelse) = &i.orelse {
                prescan_orelse(orelse, globals, nonlocals, locals, seed_defs);
            }
        }
        CompoundStatement::For(f) => {
            // `for <target> in …` — the target is a Python local for the whole function
            // (PEP 3104), whether the loop is `for` or `async for` (same node, just an
            // `asynchronous` flag). Collect target names before recursing into the body.
            crate::imports::collect_target_names(&f.target, locals);
            prescan_suite(&f.body, globals, nonlocals, locals, seed_defs);
            if let Some(orelse) = &f.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, seed_defs);
            }
        }
        CompoundStatement::While(w) => {
            prescan_suite(&w.body, globals, nonlocals, locals, seed_defs);
            if let Some(orelse) = &w.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, seed_defs);
            }
        }
        CompoundStatement::Try(t) => {
            prescan_suite(&t.body, globals, nonlocals, locals, seed_defs);
            for h in &t.handlers {
                // `except SomeError as e:` — `e` is a Python local for the whole
                // function (unlike in Python 2, it is deleted after the block, but it
                // IS in scope inside the handler and binds the name function-locally).
                if let Some(asname) = &h.name {
                    crate::imports::collect_target_names(&asname.name, locals);
                }
                prescan_suite(&h.body, globals, nonlocals, locals, seed_defs);
            }
            if let Some(orelse) = &t.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, seed_defs);
            }
            if let Some(fin) = &t.finalbody {
                prescan_suite(&fin.body, globals, nonlocals, locals, seed_defs);
            }
        }
        CompoundStatement::TryStar(t) => {
            prescan_suite(&t.body, globals, nonlocals, locals, seed_defs);
            for h in &t.handlers {
                // `except* SomeError as e:` — same binding semantics as `except … as`.
                if let Some(asname) = &h.name {
                    crate::imports::collect_target_names(&asname.name, locals);
                }
                prescan_suite(&h.body, globals, nonlocals, locals, seed_defs);
            }
            if let Some(orelse) = &t.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, seed_defs);
            }
            if let Some(fin) = &t.finalbody {
                prescan_suite(&fin.body, globals, nonlocals, locals, seed_defs);
            }
        }
        CompoundStatement::With(w) => {
            // `with expr as <target>:` (and `async with`) — `target` is a Python local
            // for the whole function. Both are the same `With` node with an
            // `asynchronous` flag. Collect asname targets before recursing into the body.
            for item in &w.items {
                if let Some(asname) = &item.asname {
                    crate::imports::collect_target_names(&asname.name, locals);
                }
            }
            prescan_suite(&w.body, globals, nonlocals, locals, seed_defs);
        }
        CompoundStatement::Match(m) => {
            for case in &m.cases {
                prescan_suite(&case.body, globals, nonlocals, locals, seed_defs);
            }
        }
    }
}

fn prescan_orelse(
    orelse: &libcst_native::OrElse,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    seed_defs: bool,
) {
    match orelse {
        libcst_native::OrElse::Elif(elif) => {
            prescan_suite(&elif.body, globals, nonlocals, locals, seed_defs);
            if let Some(inner) = &elif.orelse {
                prescan_orelse(inner, globals, nonlocals, locals, seed_defs);
            }
        }
        libcst_native::OrElse::Else(e) => {
            prescan_suite(&e.body, globals, nonlocals, locals, seed_defs);
        }
    }
}

fn prescan_small(
    small: &SmallStatement,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
) {
    match small {
        SmallStatement::Global(g) => {
            for item in &g.names {
                globals.insert(item.name.value.to_owned());
            }
        }
        SmallStatement::Nonlocal(n) => {
            for item in &n.names {
                nonlocals.insert(item.name.value.to_owned());
            }
        }
        SmallStatement::Assign(a) => {
            // Collect ALL bound names recursively through tuple/list/starred
            // destructuring (not just bare `Name`). Python's scoping rule: any
            // binding-assignment in a function body — including `(a, b) = …` and
            // `[x, *rest] = …` — makes the bound names local to the whole function.
            for target in &a.targets {
                crate::imports::collect_target_names(&target.target, locals);
            }
        }
        // AnnAssign `x: T = …` also introduces a local (recurse for safety,
        // though `x: T` is always a bare Name in practice).
        SmallStatement::AnnAssign(a) => {
            crate::imports::collect_target_names(&a.target, locals);
        }
        // AugAssign `x += …` binds the name locally when the target is a bare Name.
        // Only a Name target introduces a binding; `x.attr += 1` / `x[i] += 1` do not.
        SmallStatement::AugAssign(a) => {
            if let AssignTargetExpression::Name(n) = &a.target {
                locals.insert(n.value.to_owned());
            }
        }
        _ => {}
    }
}

// ─── write-site classifier (EffectSink) ──────────────────────────────────────

struct MutSink<'a> {
    params: &'a HashSet<String>,
    globals: &'a HashSet<String>,
    nonlocals: &'a HashSet<String>,
    /// Locally-assigned names (global/nonlocal names are removed from this set
    /// in the classification logic).
    locals: &'a HashSet<String>,
    /// File-wide import table — lets the cascade resolve an import-rooted write (F5)
    /// and distinguish a captured opaque binding (F1) from a known module.
    imports: &'a Imports,
    /// Module top-level binding names (assign targets + def/class). A write whose
    /// root is one of these — and is not a local/param/global-decl/import — is
    /// module-shared state, escalated to `global.mutation` (the #29 analog).
    module_bindings: &'a HashSet<String>,
    /// True when analyzing `__init__` (so `self.attr = …` is local init).
    is_init: bool,
    span: &'a SpanIndex<'a>,
    effects: Vec<(Effect, bool)>,
}

impl EffectSink for MutSink<'_> {
    fn on_call(&mut self, call: &Call) {
        // Detect mutating method calls: `receiver.append(…)`, `.update(…)`, `.add(…)`.
        let Expression::Attribute(attr) = call.func.as_ref() else {
            return;
        };
        if !is_mutating_method(attr.attr.value) {
            return;
        }
        // The receiver's root name is the write target.
        let Some(root) = root_name_of_expr(&attr.value) else {
            return;
        };
        let (line, col) = name_line_col_expr(&attr.value, self.span);
        // Render the full receiver expression (the attribute chain) for evidence,
        // e.g. `self.items.append(…)` — not the misleading root-only `self.append(…)`.
        // Fall back to the root name for shapes `render_expr` doesn't model.
        let receiver = render_expr(&attr.value).unwrap_or_else(|| root.clone());
        let evidence = format!("{receiver}.{}(…)", attr.attr.value);
        self.classify_and_push(root, line, col, evidence);
    }

    fn on_assert(&mut self, _assert: &Assert) {}
    fn on_raise(&mut self, _raise: &Raise) {}

    fn on_assign_target(&mut self, target: &AssignTargetExpression, is_aug: bool) {
        match target {
            // `self.attr = …` → check is_init for LocalMutation vs ThisMutation.
            AssignTargetExpression::Attribute(attr) => {
                if let Expression::Name(n) = attr.value.as_ref()
                    && n.value == "self"
                {
                    let (line, col) = name_line_col(n, self.span);
                    if self.is_init {
                        self.push(
                            EffectKind::LocalMutation,
                            Tier::Heuristic,
                            line,
                            col,
                            "self.x = … (constructor init, contained)".to_string(),
                            true,
                        );
                    } else {
                        self.push(
                            EffectKind::ThisMutation,
                            Tier::Heuristic,
                            line,
                            col,
                            format!("self.{} = … (instance state)", attr.attr.value),
                            false,
                        );
                    }
                    return;
                }
                // Non-self attribute write: `obj.attr = …` — root is `obj`.
                if let Some(root) = root_name_of_expr(&attr.value) {
                    let (line, col) = name_line_col_expr(&attr.value, self.span);
                    let evidence = format!("{root}.{} = …", attr.attr.value);
                    self.classify_and_push(root, line, col, evidence);
                }
            }
            // `x = …` bare name. A plain `=` to a bare name is a *binding*, not a
            // mutation of pre-existing state (spec: `local.mutation` is `.append()` /
            // `d[k] = …` / `+=` on a locally-created binding — never the binding
            // itself) — UNLESS the name is declared `global`/`nonlocal`, in which case
            // a plain `g = …` rebinds the enclosing/global binding and IS an escaping
            // mutation. An augmented `x += …` is always a mutation.
            AssignTargetExpression::Name(n) if is_aug => {
                let name = n.value.to_owned();
                let (line, col) = name_line_col(n, self.span);
                let evidence = format!("{name} += …");
                self.classify_and_push(name, line, col, evidence);
            }
            // Plain `=` to a bare name declared `global`/`nonlocal` is an escaping
            // rebind, not a local binding — emit. A true local binding emits nothing.
            AssignTargetExpression::Name(n)
                if self.globals.contains(n.value) || self.nonlocals.contains(n.value) =>
            {
                let name = n.value.to_owned();
                let (line, col) = name_line_col(n, self.span);
                let evidence = format!("{name} = …");
                self.classify_and_push(name, line, col, evidence);
            }
            AssignTargetExpression::Name(_) => {}
            // `d[k] = …` subscript write — root is `d`.
            AssignTargetExpression::Subscript(sub) => {
                if let Some(root) = root_name_of_expr(&sub.value) {
                    let (line, col) = name_line_col_expr(&sub.value, self.span);
                    let evidence = format!("{root}[…] = …");
                    self.classify_and_push(root, line, col, evidence);
                }
            }
            // Starred / Tuple / List destructuring — skip (no single root).
            _ => {}
        }
    }
}

impl MutSink<'_> {
    /// Classify a write by root name and push an `(Effect, contained)` pair.
    fn classify_and_push(&mut self, root: String, line: usize, col: usize, evidence: String) {
        // Root-`self` method/subscript/aug writes (`self.items.append(…)`,
        // `self[i] = v`, `self += …`) mutate already-existing instance state — they
        // are escaping `ThisMutation`, contained=false, *even in `__init__`*. The
        // contained build-then-expose case is only the *direct* `self.attr = …`
        // assignment, which `on_assign_target` handles before reaching here.
        // Preserve the actual write-site evidence passed in.
        if root == "self" {
            self.push(
                EffectKind::ThisMutation,
                Tier::Heuristic,
                line,
                col,
                evidence,
                false,
            );
            return;
        }

        // Global declaration: `global x` → `GlobalMutation`. Include the write
        // expression in evidence so the report is traceable to the actual write site.
        if self.globals.contains(&root) {
            self.push(
                EffectKind::GlobalMutation,
                Tier::Exact,
                line,
                col,
                format!("global {root} ({evidence})"),
                false,
            );
            return;
        }

        // Nonlocal declaration: `nonlocal x` → `ThisMutation` (escaping outer scope).
        // Include the write expression for the same traceability reason.
        if self.nonlocals.contains(&root) {
            self.push(
                EffectKind::ThisMutation,
                Tier::Exact,
                line,
                col,
                format!("nonlocal {root} ({evidence})"),
                false,
            );
            return;
        }

        // Parameter mutation: root is a param name (but NOT `self`, handled above).
        if self.params.contains(&root) {
            self.push(
                EffectKind::ParamMutation,
                Tier::Heuristic,
                line,
                col,
                evidence,
                false,
            );
            return;
        }

        // Local binding mutation: root is locally assigned (and not global/nonlocal).
        if self.locals.contains(&root) {
            self.push(
                EffectKind::LocalMutation,
                Tier::Exact,
                line,
                col,
                evidence,
                true,
            );
            return;
        }

        // F5: root resolves through the ImportTable → module-level state (the imported
        // module/name) escaping the function. global.mutation (class 6, Heuristic),
        // contained=false. A same-named LOCAL already won above.
        if self.imports.resolve(&root).is_some() {
            self.push(
                EffectKind::GlobalMutation,
                Tier::Heuristic,
                line,
                col,
                format!("{evidence} (imported `{root}`)"),
                false,
            );
            return;
        }

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
                col,
                format!("{evidence} (module-level `{root}`)"),
                false,
            );
            return;
        }

        // F1: the root resolves to NONE of self/global/nonlocal/param/local/import/
        // module-binding — a captured outer (enclosing-function) binding we cannot
        // bound syntactically → hidden.mutation (class 3, hidden:true,
        // contained:false), subreason "captured-binding". Mirrors the TS `captured`
        // hidden case (Milestone A left it un-emitted).
        self.push_hidden(line, col, evidence, "captured-binding");
    }

    /// Push a `HiddenMutation` (`hidden:true` + a `subreason`), always escaping
    /// (`contained:false`). Used for writes whose root is an opaque captured /
    /// unresolved binding — the fallback after all named-binding cases are handled.
    fn push_hidden(&mut self, line: usize, col: usize, evidence: String, subreason: &str) {
        let kind = EffectKind::HiddenMutation;
        let tier = Tier::Heuristic;
        let class = kind.base_class();
        self.effects.push((
            Effect {
                kind,
                class,
                discounted_to: None,
                weight: weight_for_class(class),
                line,
                col,
                tier,
                hidden: true,
                contained: false,
                evidence,
                discount: None,
                subreason: Some(subreason.to_owned()),
                confidence: detection_confidence(tier, false, false),
            },
            false,
        ));
    }

    fn push(
        &mut self,
        kind: EffectKind,
        tier: Tier,
        line: usize,
        col: usize,
        evidence: String,
        contained: bool,
    ) {
        let class = kind.base_class();
        self.effects.push((
            Effect {
                kind,
                class,
                discounted_to: None,
                weight: weight_for_class(class),
                line,
                col,
                tier,
                hidden: false,
                contained: false,
                evidence,
                discount: None,
                subreason: None,
                confidence: detection_confidence(tier, false, false),
            },
            contained,
        ));
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Return the root `Name.value` of an expression chain (`a.b.c` → `"a"`,
/// `a[k]` → `"a"`, `Name("x")` → `"x"`).
fn root_name_of_expr(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Name(n) => Some(n.value.to_owned()),
        Expression::Attribute(a) => root_name_of_expr(&a.value),
        Expression::Subscript(s) => root_name_of_expr(&s.value),
        Expression::Call(c) => root_name_of_expr(&c.func),
        _ => None,
    }
}

/// True for method names that mutate their receiver.
fn is_mutating_method(name: &str) -> bool {
    matches!(
        name,
        "append"
            | "extend"
            | "insert"
            | "remove"
            | "pop"
            | "clear"
            | "sort"
            | "reverse"
            | "update"
            | "add"
            | "discard"
            | "setdefault"
    )
}

/// 1-based `(line, col)` of the leftmost `Name` in an expression.
fn name_line_col_expr(expr: &Expression, span: &SpanIndex) -> (usize, usize) {
    leftmost_name(expr)
        .map(|n| name_line_col(n, span))
        .unwrap_or((0, 0))
}

/// The leftmost `Name` in an expression chain.
fn leftmost_name<'a>(expr: &'a Expression<'a>) -> Option<&'a Name<'a>> {
    match expr {
        Expression::Name(n) => Some(n),
        Expression::Attribute(a) => leftmost_name(&a.value),
        Expression::Subscript(s) => leftmost_name(&s.value),
        Expression::Call(c) => leftmost_name(&c.func),
        _ => None,
    }
}

/// 1-based `(line, col)` of a `Name` node (pointer-arithmetic on its borrowed &str).
fn name_line_col(name: &Name, span: &SpanIndex) -> (usize, usize) {
    span.line_col(anchor_of_subslice(span.src(), name.value))
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use fxrank_core::effect::EffectKind::{self, *};
    use std::collections::HashMap;

    /// Parse `tests/fixtures/<name>.py`, collect units, run `detect` per unit, and
    /// return `symbol → Vec<(EffectKind, bool)>` (kind + contained flag).
    fn mutation_effects(name: &str) -> HashMap<String, Vec<(EffectKind, bool)>> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}.py")).unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<(EffectKind, bool)>> = HashMap::new();
        for unit in &units {
            let pairs = detect(unit, &imports, &module_bindings, &span);
            out.insert(
                unit.symbol.clone(),
                pairs.iter().map(|(e, c)| (e.kind, *c)).collect(),
            );
        }
        out
    }

    /// Like `mutation_effects` but retains each effect's evidence string.
    fn mutation_evidence(name: &str) -> HashMap<String, Vec<(EffectKind, bool, String)>> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}.py")).unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<(EffectKind, bool, String)>> = HashMap::new();
        for unit in &units {
            let pairs = detect(unit, &imports, &module_bindings, &span);
            out.insert(
                unit.symbol.clone(),
                pairs
                    .iter()
                    .map(|(e, c)| (e.kind, *c, e.evidence.clone()))
                    .collect(),
            );
        }
        out
    }

    #[test]
    fn classifies_mutation_by_escape() {
        let m = mutation_effects("mutation");

        // `global _counter` then `_counter += 1` → GlobalMutation, not contained.
        assert!(
            m["uses_global"].contains(&(GlobalMutation, false)),
            "uses_global should have GlobalMutation(contained=false), got: {:?}",
            m["uses_global"]
        );

        // `self.n += 1` in a non-__init__ method → ThisMutation, not contained.
        assert!(
            m["bump"].contains(&(ThisMutation, false)),
            "bump should have ThisMutation(contained=false), got: {:?}",
            m["bump"]
        );

        // `lst.append(1)` where lst is a param → ParamMutation, not contained.
        assert!(
            m["mutates_param"].contains(&(ParamMutation, false)),
            "mutates_param should have ParamMutation(contained=false), got: {:?}",
            m["mutates_param"]
        );

        // `acc.append(1)` where acc is a local → LocalMutation, contained.
        assert!(
            m["builds_local"].contains(&(LocalMutation, true)),
            "builds_local should have LocalMutation(contained=true), got: {:?}",
            m["builds_local"]
        );

        // `self.n = n` inside `__init__` → LocalMutation, contained (constructor init).
        assert!(
            m["__init__"].contains(&(LocalMutation, true)),
            "__init__ should have LocalMutation(contained=true), got: {:?}",
            m["__init__"]
        );
    }

    /// FIX 1: a plain `=` to a name declared `global`/`nonlocal` is an escaping
    /// rebind and MUST emit — only a TRUE local binding (`y = 1`, no declaration)
    /// is a no-emit binding. Pre-fix the classifier only ran for bare names when
    /// `is_aug`, so `global g; g = 1` and `nonlocal x; x = 1` were silently dropped.
    #[test]
    fn plain_assign_to_global_nonlocal_names_escapes() {
        let m = mutation_effects("mutation");

        // `global _counter; _counter = 1` → GlobalMutation, not contained.
        assert!(
            m["plain_global_rebind"].contains(&(GlobalMutation, false)),
            "plain `=` to a global name must emit GlobalMutation(false), got: {:?}",
            m["plain_global_rebind"]
        );

        // `nonlocal x; x = 1` (inside the nested def) → ThisMutation, not contained.
        assert!(
            m["plain_nonlocal_rebind"].contains(&(ThisMutation, false)),
            "plain `=` to a nonlocal name must emit ThisMutation(false), got: {:?}",
            m["plain_nonlocal_rebind"]
        );

        // `y = 1` with no global/nonlocal declaration → NO mutation effect (binding).
        assert!(
            m["plain_local_binding"].is_empty(),
            "plain `=` to a true local must emit NO mutation, got: {:?}",
            m["plain_local_binding"]
        );
    }

    /// FIX 1: root-`self` method/subscript mutations are escaping instance-state
    /// (`ThisMutation`, contained=false) even inside `__init__` — they mutate
    /// already-existing instance state, NOT the contained build-then-expose
    /// `self.attr = …` case. The direct `self.attr = …`-in-init case stays
    /// `LocalMutation` contained.
    #[test]
    fn self_method_and_subscript_mutations_escape_even_in_init() {
        let m = mutation_effects("mutation");

        // `self.items = []` in __init__ → LocalMutation contained (unchanged).
        assert!(
            m["__init__"].contains(&(LocalMutation, true)),
            "direct `self.attr = …` in __init__ stays LocalMutation(true), got: {:?}",
            m["__init__"]
        );

        // `self.items.append(1)` in __init__ → ThisMutation, contained=false
        // (escaping — was wrongly contained before the fix).
        assert!(
            m["__init__"].contains(&(ThisMutation, false)),
            "`self.items.append(…)` in __init__ must be ThisMutation(false), got: {:?}",
            m["__init__"]
        );

        // `self[i] = v` in a non-init method → ThisMutation, contained=false.
        assert!(
            m["store"].contains(&(ThisMutation, false)),
            "`self[i] = v` must be ThisMutation(false), got: {:?}",
            m["store"]
        );
    }

    /// PREREQ 1: MutSink can emit a HiddenMutation (hidden:true + subreason) —
    /// the channel Python has never used. `push` stays the honest hidden:false path.
    #[test]
    fn push_hidden_emits_hidden_mutation_with_subreason() {
        let params = std::collections::HashSet::new();
        let globals = std::collections::HashSet::new();
        let nonlocals = std::collections::HashSet::new();
        let locals = std::collections::HashSet::new();
        let src = "x\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let span = crate::source::SpanIndex::new(src);
        let mut sink = MutSink {
            params: &params,
            globals: &globals,
            nonlocals: &nonlocals,
            locals: &locals,
            imports: &imports,
            module_bindings: &HashSet::new(),
            is_init: false,
            span: &span,
            effects: Vec::new(),
        };
        sink.push_hidden(1, 1, "outer_acc.append(…)".to_string(), "captured-binding");

        assert_eq!(sink.effects.len(), 1);
        let (effect, contained) = &sink.effects[0];
        assert_eq!(effect.kind, EffectKind::HiddenMutation);
        assert_eq!(effect.class, 3);
        assert!(effect.hidden, "push_hidden must set hidden:true");
        assert_eq!(effect.subreason.as_deref(), Some("captured-binding"));
        assert!(!contained, "hidden writes escape — contained=false");
    }

    /// PREREQ 2: mutation::detect accepts the ImportTable so F5 + F1 can resolve roots.
    #[test]
    fn detect_accepts_imports_param() {
        let src = "def f(lst):\n    lst.append(1)\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = crate::source::SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let f = units.iter().find(|u| u.symbol == "f").unwrap();
        let pairs = detect(f, &imports, &module_bindings, &span);
        assert!(
            pairs.iter().any(|(e, _)| e.kind == ParamMutation),
            "lst.append where lst is a param → ParamMutation, got: {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
    }

    /// F5: a write whose root resolves through the ImportTable is module-level state
    /// escaping the function → global.mutation/6, contained=false. Inserted AFTER the
    /// `locals` arm (a same-named local shadows the import) and BEFORE the F1 fallback.
    #[test]
    fn import_rooted_write_is_global_mutation() {
        let m = mutation_effects("mutation");
        assert!(
            m["mutates_imported_module"].contains(&(GlobalMutation, false)),
            "config.settings.append(…) where `config` is imported must be GlobalMutation(false), got: {:?}",
            m["mutates_imported_module"]
        );
    }

    /// refs #55: attribute-tagging a locally-defined nested `def` (the Django
    /// `fn.alters_data = True` pattern) must be LocalMutation(contained), not a
    /// spurious `hidden.mutation` "captured-binding" — the def name IS a local binding.
    #[test]
    fn nested_def_name_is_local_binding_not_captured() {
        let m = mutation_effects("mutation");
        let effs = &m["tags_nested_def"];
        assert!(
            effs.contains(&(LocalMutation, true)),
            "helper.alters_data = True should be LocalMutation(contained=true), got: {effs:?}"
        );
        assert!(
            !effs.iter().any(|(k, _)| *k == HiddenMutation),
            "must not be a captured-binding hidden.mutation, got: {effs:?}"
        );
    }

    /// refs #55 (Opus review): the `seed_defs` flag must reach nested defs inside
    /// CONTROL FLOW, not just at the top of a function body. A `def` nested in an
    /// `if` is still a function-local, so tagging it is LocalMutation — this guards
    /// that the `If`/orelse recursion arm forwards `seed_defs`.
    #[test]
    fn nested_def_inside_control_flow_is_local() {
        let m = mutation_effects("mutation");
        let effs = &m["tags_nested_def_in_branch"];
        assert!(
            effs.contains(&(LocalMutation, true)),
            "helper_in_if.alters_data (nested def inside `if`) should be LocalMutation, got: {effs:?}"
        );
        assert!(
            !effs.iter().any(|(k, _)| *k == HiddenMutation),
            "must not be captured-binding hidden.mutation, got: {effs:?}"
        );
    }

    /// refs #55 (regression guard): the nested-def-name seeding must NOT reach the
    /// synthetic `<module>` unit. At module scope a def/class name is a module-level
    /// binding, so `module_level_taggable.alters_data = True` stays GlobalMutation/6
    /// (via module_bindings), not LocalMutation. The `<module>` unit is built by
    /// `module_init_unit`, not `functions::collect`, so this test drives it directly.
    #[test]
    fn module_level_def_attr_write_stays_global_not_local() {
        let src = std::fs::read_to_string("tests/fixtures/mutation.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = crate::source::SpanIndex::new(&src);
        let unit =
            crate::functions::module_init_unit(&module).expect("module has executable top-level");
        let effs: Vec<(EffectKind, bool)> = detect(&unit, &imports, &module_bindings, &span)
            .iter()
            .map(|(e, c)| (e.kind, *c))
            .collect();
        assert!(
            effs.contains(&(GlobalMutation, false)),
            "module-level `def.attr = …` must be GlobalMutation(false), got: {effs:?}"
        );
        assert!(
            !effs.contains(&(LocalMutation, true)),
            "module-level def-attr write must NOT be downgraded to LocalMutation, got: {effs:?}"
        );
    }

    /// F1/F3: a write whose root resolves to NONE of {self, globals, nonlocals, params,
    /// locals, import} is a captured outer/opaque binding. Pre-fix the cascade fell off
    /// silently; now it emits hidden.mutation/3, hidden=true, contained=false, subreason
    /// "captured-binding" (the Python analog of the TS `captured` hidden case).
    #[test]
    fn captured_binding_subreason_is_set() {
        let src = std::fs::read_to_string("tests/fixtures/mutation.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let inner = units.iter().find(|u| u.symbol == "inner").unwrap();
        let pairs = detect(inner, &imports, &module_bindings, &span);
        let hidden = pairs
            .iter()
            .find(|(e, _)| e.kind == HiddenMutation)
            .map(|(e, _)| e)
            .expect("inner must emit a HiddenMutation");
        assert_eq!(hidden.class, 3);
        assert!(
            hidden.hidden,
            "captured-binding HiddenMutation must be hidden:true"
        );
        assert_eq!(hidden.subreason.as_deref(), Some("captured-binding"));
        assert!(
            pairs.iter().any(|(e, c)| e.kind == HiddenMutation && !*c),
            "captured-binding write escapes — contained=false"
        );
    }

    /// FIX 2: mutating-method evidence renders the full receiver expression
    /// (the attribute chain) — `self.items.append(…)`, not the misleading
    /// `self.append(…)` built from just the root name.
    #[test]
    fn mutating_method_evidence_uses_full_receiver() {
        let m = mutation_evidence("mutation");
        let init = &m["__init__"];
        let append = init
            .iter()
            .find(|(k, _, _)| *k == ThisMutation)
            .unwrap_or_else(|| panic!("expected a ThisMutation in __init__, got: {init:?}"));
        assert!(
            append.2.contains("self.items"),
            "evidence must name the full receiver `self.items`, got: {:?}",
            append.2
        );
    }

    fn detect_src(src: &str, fn_name: &str) -> Vec<(Effect, bool)> {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let span = crate::source::SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
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

    /// Prescan fix (for/with-as/except-as): a name introduced by a `for` loop target
    /// in a function body is a Python local for the WHOLE function (PEP 3104). A
    /// module binding of the same name must be shadowed by the for-target local.
    #[test]
    fn for_target_shadow_stays_local() {
        // `_cache` is a module-level binding; `for _cache in []` shadows it locally.
        // The write `_cache['k'] = 1` inside the loop must be LocalMutation (contained),
        // not GlobalMutation. Before the prescan fix this was GlobalMutation because
        // the prescan did not collect for-loop targets.
        let src = "_cache = {}\ndef f():\n    for _cache in []:\n        _cache['k'] = 1\n";
        let pairs = detect_src(src, "f");
        let writes: Vec<_> = pairs
            .iter()
            .filter(|(e, _)| {
                matches!(
                    e.kind,
                    LocalMutation | GlobalMutation | HiddenMutation | ThisMutation
                )
            })
            .collect();
        assert!(
            writes.iter().any(|(e, c)| e.kind == LocalMutation && *c),
            "expected LocalMutation(contained=true) for for-target shadow, got: {:?}",
            writes.iter().map(|(e, c)| (e.kind, *c)).collect::<Vec<_>>()
        );
        assert!(
            !writes.iter().any(|(e, _)| e.kind == GlobalMutation),
            "expected NO GlobalMutation for for-target shadow, got: {:?}",
            writes.iter().map(|(e, c)| (e.kind, *c)).collect::<Vec<_>>()
        );
    }

    /// Prescan fix: a name introduced by DESTRUCTURING assignment in a function body
    /// is a Python local (Python scoping: any binding-assignment makes the name local
    /// for the whole function). A module binding of the same name must be shadowed.
    #[test]
    fn local_destructured_shadow_stays_local() {
        // `_cache` is a module-level binding. `f` rebinds it via tuple destructuring
        // `(_cache,) = ({},)` — Python considers `_cache` local to `f` for the whole
        // function. The subsequent `_cache['k'] = 1` must be LocalMutation (contained),
        // not GlobalMutation. Before the prescan fix this was GlobalMutation because
        // the prescan only collected bare-Name targets.
        let src = "_cache = {}\ndef f():\n    (_cache,) = ({},)\n    _cache['k'] = 1\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs.iter().any(|(e, c)| e.kind == LocalMutation && *c),
            "destructuring-local `_cache` must be LocalMutation(true), got: {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
        assert!(
            !pairs.iter().any(|(e, _)| e.kind == GlobalMutation),
            "destructured local must not escalate to GlobalMutation"
        );
    }
}
