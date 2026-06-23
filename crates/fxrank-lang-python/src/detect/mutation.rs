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
//!
//! ## Strategy
//!
//! 1. **Pre-scan** the function body for `global`/`nonlocal` declarations and
//!    bare-`Name` LHS assignments — these build the `globals`, `nonlocals`, and
//!    `locals` sets.
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
pub fn detect(unit: &FnUnit, imports: &Imports, span: &SpanIndex) -> Vec<(Effect, bool)> {
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
/// - `locals`: names introduced by bare-`Name` LHS assignments (not params,
///   not `global`/`nonlocal` — those are resolved after this pass).
///
/// Only scans the **own** body (does not descend into nested `def`/`lambda`).
fn prescan_body(
    body: &FnBody,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
) {
    match body {
        FnBody::Suite(suite) => prescan_suite(suite, globals, nonlocals, locals),
        FnBody::Expr(_) => {} // lambdas have no statements
    }
}

fn prescan_suite(
    suite: &Suite,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
) {
    match suite {
        Suite::IndentedBlock(b) => {
            for stmt in &b.body {
                prescan_stmt(stmt, globals, nonlocals, locals);
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
) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                prescan_small(small, globals, nonlocals, locals);
            }
        }
        Statement::Compound(c) => prescan_compound(c, globals, nonlocals, locals),
    }
}

fn prescan_compound(
    compound: &libcst_native::CompoundStatement,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
) {
    use libcst_native::CompoundStatement;
    match compound {
        // Nested def/lambda: do NOT descend (own-body attribution).
        CompoundStatement::FunctionDef(_) | CompoundStatement::ClassDef(_) => {}
        CompoundStatement::If(i) => {
            prescan_suite(&i.body, globals, nonlocals, locals);
            if let Some(orelse) = &i.orelse {
                prescan_orelse(orelse, globals, nonlocals, locals);
            }
        }
        CompoundStatement::For(f) => {
            prescan_suite(&f.body, globals, nonlocals, locals);
            if let Some(orelse) = &f.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals);
            }
        }
        CompoundStatement::While(w) => {
            prescan_suite(&w.body, globals, nonlocals, locals);
            if let Some(orelse) = &w.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals);
            }
        }
        CompoundStatement::Try(t) => {
            prescan_suite(&t.body, globals, nonlocals, locals);
            for h in &t.handlers {
                prescan_suite(&h.body, globals, nonlocals, locals);
            }
            if let Some(orelse) = &t.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals);
            }
            if let Some(fin) = &t.finalbody {
                prescan_suite(&fin.body, globals, nonlocals, locals);
            }
        }
        CompoundStatement::TryStar(t) => {
            prescan_suite(&t.body, globals, nonlocals, locals);
            for h in &t.handlers {
                prescan_suite(&h.body, globals, nonlocals, locals);
            }
            if let Some(orelse) = &t.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals);
            }
            if let Some(fin) = &t.finalbody {
                prescan_suite(&fin.body, globals, nonlocals, locals);
            }
        }
        CompoundStatement::With(w) => {
            prescan_suite(&w.body, globals, nonlocals, locals);
        }
        CompoundStatement::Match(m) => {
            for case in &m.cases {
                prescan_suite(&case.body, globals, nonlocals, locals);
            }
        }
    }
}

fn prescan_orelse(
    orelse: &libcst_native::OrElse,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
) {
    match orelse {
        libcst_native::OrElse::Elif(elif) => {
            prescan_suite(&elif.body, globals, nonlocals, locals);
            if let Some(inner) = &elif.orelse {
                prescan_orelse(inner, globals, nonlocals, locals);
            }
        }
        libcst_native::OrElse::Else(e) => {
            prescan_suite(&e.body, globals, nonlocals, locals);
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
            // Bare `Name` LHS → local binding (unless shadowed by global/nonlocal,
            // which we resolve after this pass).
            for target in &a.targets {
                if let AssignTargetExpression::Name(n) = &target.target {
                    locals.insert(n.value.to_owned());
                }
            }
        }
        // AnnAssign `x: T = …` also introduces a local.
        SmallStatement::AnnAssign(a) => {
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
        let line = name_line_expr(&attr.value, self.span);
        // Render the full receiver expression (the attribute chain) for evidence,
        // e.g. `self.items.append(…)` — not the misleading root-only `self.append(…)`.
        // Fall back to the root name for shapes `render_expr` doesn't model.
        let receiver = render_expr(&attr.value).unwrap_or_else(|| root.clone());
        let evidence = format!("{receiver}.{}(…)", attr.attr.value);
        self.classify_and_push(root, line, evidence);
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
                    let line = name_line(n, self.span);
                    if self.is_init {
                        self.push(
                            EffectKind::LocalMutation,
                            Tier::Heuristic,
                            line,
                            "self.x = … (constructor init, contained)".to_string(),
                            true,
                        );
                    } else {
                        self.push(
                            EffectKind::ThisMutation,
                            Tier::Heuristic,
                            line,
                            format!("self.{} = … (instance state)", attr.attr.value),
                            false,
                        );
                    }
                    return;
                }
                // Non-self attribute write: `obj.attr = …` — root is `obj`.
                if let Some(root) = root_name_of_expr(&attr.value) {
                    let line = name_line_expr(&attr.value, self.span);
                    let evidence = format!("{root}.{} = …", attr.attr.value);
                    self.classify_and_push(root, line, evidence);
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
                let line = name_line(n, self.span);
                let evidence = format!("{name} += …");
                self.classify_and_push(name, line, evidence);
            }
            // Plain `=` to a bare name declared `global`/`nonlocal` is an escaping
            // rebind, not a local binding — emit. A true local binding emits nothing.
            AssignTargetExpression::Name(n)
                if self.globals.contains(n.value) || self.nonlocals.contains(n.value) =>
            {
                let name = n.value.to_owned();
                let line = name_line(n, self.span);
                let evidence = format!("{name} = …");
                self.classify_and_push(name, line, evidence);
            }
            AssignTargetExpression::Name(_) => {}
            // `d[k] = …` subscript write — root is `d`.
            AssignTargetExpression::Subscript(sub) => {
                if let Some(root) = root_name_of_expr(&sub.value) {
                    let line = name_line_expr(&sub.value, self.span);
                    let evidence = format!("{root}[…] = …");
                    self.classify_and_push(root, line, evidence);
                }
            }
            // Starred / Tuple / List destructuring — skip (no single root).
            _ => {}
        }
    }
}

impl MutSink<'_> {
    /// Classify a write by root name and push an `(Effect, contained)` pair.
    fn classify_and_push(&mut self, root: String, line: usize, evidence: String) {
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
                evidence,
                false,
            );
            return;
        }

        // Local binding mutation: root is locally assigned (and not global/nonlocal).
        if self.locals.contains(&root) {
            self.push(EffectKind::LocalMutation, Tier::Exact, line, evidence, true);
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
                format!("{evidence} (imported `{root}`)"),
                false,
            );
            return;
        }

        // F1: the root resolves to NONE of self/global/nonlocal/param/local/import —
        // a captured outer binding (a closed-over variable, or a module-level name
        // not declared `global`). The write escapes through an opaque channel we
        // cannot bound syntactically → hidden.mutation (class 3, hidden:true,
        // contained:false), subreason "captured-binding". Mirrors the TS `captured`
        // hidden case (Milestone A left it un-emitted).
        self.push_hidden(line, evidence, "captured-binding");
    }

    /// Push a `HiddenMutation` (`hidden:true` + a `subreason`), always escaping
    /// (`contained:false`). Used for writes whose root is an opaque captured /
    /// imported binding — the analog of the TS frontend's `captured` hidden case.
    fn push_hidden(&mut self, line: usize, evidence: String, subreason: &str) {
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
                tier,
                hidden: true,
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
                tier,
                hidden: false,
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

/// 1-based line of the leftmost `Name` in an expression.
fn name_line_expr(expr: &Expression, span: &SpanIndex) -> usize {
    leftmost_name(expr).map(|n| name_line(n, span)).unwrap_or(0)
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

/// 1-based line of a `Name` node (pointer-arithmetic on its borrowed &str).
fn name_line(name: &Name, span: &SpanIndex) -> usize {
    span.line_col(anchor_of_subslice(span.src(), name.value)).0
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
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<(EffectKind, bool)>> = HashMap::new();
        for unit in &units {
            let pairs = detect(unit, &imports, &span);
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
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<(EffectKind, bool, String)>> = HashMap::new();
        for unit in &units {
            let pairs = detect(unit, &imports, &span);
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
            is_init: false,
            span: &span,
            effects: Vec::new(),
        };
        sink.push_hidden(1, "outer_acc.append(…)".to_string(), "captured-binding");

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
        let span = crate::source::SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let f = units.iter().find(|u| u.symbol == "f").unwrap();
        let pairs = detect(f, &imports, &span);
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

    /// F1/F3: a write whose root resolves to NONE of {self, globals, nonlocals, params,
    /// locals, import} is a captured outer/opaque binding. Pre-fix the cascade fell off
    /// silently; now it emits hidden.mutation/3, hidden=true, contained=false, subreason
    /// "captured-binding" (the Python analog of the TS `captured` hidden case).
    #[test]
    fn captured_binding_subreason_is_set() {
        let src = std::fs::read_to_string("tests/fixtures/mutation.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let inner = units.iter().find(|u| u.symbol == "inner").unwrap();
        let pairs = detect(inner, &imports, &span);
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
}
