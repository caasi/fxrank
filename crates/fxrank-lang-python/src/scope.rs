//! Frontend-internal scope-aware binding resolution (spec 078).
//! See docs/scope-aware-binding-resolution-guideline.md. NOTHING here crosses
//! the UnitRecord boundary; fxrank-core knows none of these types.

use std::collections::{HashMap, HashSet};

use libcst_native::{
    AssignTargetExpression, BaseSlice, ClassDef, CompFor, CompoundStatement, DelTargetExpression,
    DictElement, Element, Expression, FormattedStringContent, FunctionDef, ImportNames,
    MatchPattern, MatchSequence, Module, OrElse, Parameters, SmallStatement, StarArg,
    StarrableMatchSequenceElement, Statement, Suite, YieldValue,
};

use crate::functions::FnBody;
use crate::imports::Imports;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingOrigin {
    Param,
    Local,
    ModuleImport,
    FromImport,
    ModuleBinding,
    Global,
    EnclosingCaptured,
    #[allow(dead_code)] // not constructed until a later task (ambient IO host globals)
    HostGlobalIo,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindingId {
    pub scope_segments: Vec<String>,
    pub kind_name: String,
}

/// The `"kind:"` prefix used in a `BindingId.kind_name`, per the guideline's
/// binding-id scheme (kind-tagged names keep same-name/different-origin
/// bindings distinct even within one scope).
fn kind_prefix(origin: BindingOrigin) -> &'static str {
    match origin {
        BindingOrigin::Param => "param",
        BindingOrigin::Local => "local",
        BindingOrigin::ModuleImport => "import",
        BindingOrigin::FromImport => "from",
        BindingOrigin::ModuleBinding => "module",
        BindingOrigin::Global => "global",
        BindingOrigin::EnclosingCaptured => "captured",
        BindingOrigin::HostGlobalIo => "hostio",
        BindingOrigin::Unresolved => "unresolved",
    }
}

/// A scope-tree node: the module scope, or a function/lambda scope.
/// `body_ptr` and `name` are `None` for the module scope; a function scope
/// fills both (`body_ptr` for `unit_scope_of`, `name` for
/// `scope_of_name`/`BindingId` construction — `None` for a lambda, which has
/// no def-name).
struct Scope {
    kind: ScopeKind,
    /// The enclosing scope; `None` for the module scope (the tree root).
    parent: Option<ScopeId>,
    /// The function body node's address, used by `unit_scope_of` to map a
    /// unit back to its scope. `None` for the module scope.
    body_ptr: Option<usize>,
    /// The def name, used by `scope_of_name`/`BindingId` construction.
    /// `None` for the module scope.
    // Read only by `scope_of_name`, a #[cfg(test)] helper, until BindingId's
    // scope_segments consume it.
    #[allow(dead_code)]
    name: Option<String>,
    bindings: HashMap<String, BindingOrigin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    Module,
    Function,
}

pub(crate) type ScopeId = usize;

/// The file's scope tree — `module → function/method` (spec 078; comprehensions
/// are not their own scope yet, see #55/Task 8). The module BINDING scope is
/// always id `0`; every `def`/`async def`/`lambda` gets its own `Function`
/// scope, parented to its lexical enclosing scope (a class body is not a scope —
/// methods parent directly to their enclosing function/module scope).
///
/// One extra `Function` scope — the **module-unit scope** (`module_unit_scope`)
/// — models the synthetic `<module>` unit's OWN body. It is distinct from the
/// module binding scope (id 0): scope 0 serves every OTHER unit (a module-level
/// `x = {}` is a `ModuleBinding` to a function that references it), while the
/// module-unit scope serves the `<module>` unit itself (its top-level
/// assign/`for`/`with` targets are its own `Local`s — byte-identical to the
/// flat model's per-unit `locals` for `FnBody::Module`, `seed_defs = false`).
/// The same module-level name legitimately resolves to different origins for
/// the two roles, so one scope cannot serve both.
pub(crate) struct ScopeTree {
    scopes: Vec<Scope>,
    /// The `Function` scope modeling the `<module>` unit's own body. Found by
    /// `unit_scope_of`'s `FnBody::Module` arm (no body-pointer match needed).
    module_unit_scope: ScopeId,
}

impl ScopeTree {
    /// Build the scope tree from a parsed `Module`, its module-level
    /// `module_bindings`, and the module's own imports (collected from
    /// `module` directly — see below; the `_imports` parameter is unused for
    /// that purpose, kept only so callers' arity is unchanged, same as
    /// `detect::mutation::detect`'s `_imports`/`_module_bindings`).
    ///
    /// Module-scope construction order is load-bearing and byte-identical to
    /// the flat lookups it replaces: every `module_bindings` name is inserted
    /// first as `ModuleBinding`, then the imports overlay it as `ModuleImport`
    /// / `FromImport` — imports **overwrite** any same-named `ModuleBinding`,
    /// reproducing the existing mutation cascade's "imports (F5) checked
    /// before module_bindings (F7)" precedence.
    ///
    /// # Per-function import scoping (#78)
    ///
    /// Unlike the flat `Imports` table (file-wide: a function-local `import x`
    /// resolves same-named uses in every OTHER function too — an intentional
    /// over-approximation there, see `imports.rs`'s doc comment), module scope
    /// here holds **only top-level imports** — statements directly in
    /// `module.body`, or nested inside top-level control flow (`if`/`for`/
    /// `while`/`try`/`with`/`match`, none of which are Python scopes), but NOT
    /// inside a `def`/`async def`/`lambda` or `class` body. A function-body
    /// import is instead collected into that function's OWN scope (see
    /// `build_funcdef`) — it binds locally and never reaches module scope, so
    /// it can no longer collide with another function's same-named binding.
    ///
    /// A **class-body** import (`class C: import os`) is collected into
    /// neither: a class body is not a lexical scope Python methods see (see
    /// `build_classdef`), so a class-body import was never a legitimate
    /// binding oracle for a method's mutations — dropping it here is a small
    /// bundled correctness gain, not merely a compat concession.
    #[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
    pub(crate) fn build(
        module: &Module,
        _imports: &Imports,
        module_bindings: &HashSet<String>,
    ) -> Self {
        let mut bindings = HashMap::new();
        for name in module_bindings {
            bindings.insert(name.clone(), BindingOrigin::ModuleBinding);
        }
        let module_scope = Scope {
            kind: ScopeKind::Module,
            parent: None,
            body_ptr: None,
            name: None,
            bindings,
        };
        let mut tree = Self {
            scopes: vec![module_scope],
            module_unit_scope: 0, // fixed up immediately below
        };
        let module_scope_id = tree.module_scope();

        // The module-unit scope M: models the `<module>` unit's OWN body.
        // Prescan `module.body` with `seed_defs = false` (a top-level def/class
        // name is a `ModuleBinding` to this unit, NOT a local — mirrors
        // `mutation.rs::prescan_body`'s `FnBody::Module` arm) and insert its
        // `locals` as `Local`. Params/globals/nonlocals are empty at module
        // level, but handle them uniformly for safety. M parents to scope 0, so
        // a name M misses (e.g. a top-level `class C` or an import) still
        // resolves through scope 0's `ModuleBinding`/`ModuleImport`/`FromImport`.
        //
        // This SAME walk — same shape as `prescan_*`'s "recurse into control
        // flow, stop at nested def/class" traversal — also collects the
        // module's TOP-LEVEL imports (`m_imports`), which is exactly the set
        // scope 0 (the module BINDING scope) needs to overlay as
        // `ModuleImport`/`FromImport` (#78): M itself does not store them (a
        // name M misses for imports falls through to scope 0 via the parent
        // chain, unchanged from before this task).
        let mut m_globals = HashSet::new();
        let mut m_nonlocals = HashSet::new();
        let mut m_locals = HashSet::new();
        let mut m_imports = HashMap::new();
        for stmt in &module.body {
            prescan_stmt(
                stmt,
                &mut m_globals,
                &mut m_nonlocals,
                &mut m_locals,
                &mut m_imports,
                false, // seed_defs=false: top-level def/class names are ModuleBindings
            );
        }
        // Overlay the top-level imports onto scope 0 — imports overwrite any
        // same-named `ModuleBinding`, same precedence as before this task.
        for (name, origin) in &m_imports {
            tree.scopes[module_scope_id]
                .bindings
                .insert(name.clone(), *origin);
        }
        let mut m_bindings = HashMap::new();
        for name in m_locals {
            m_bindings.insert(name, BindingOrigin::Local);
        }
        for name in m_nonlocals {
            m_bindings.insert(name, BindingOrigin::EnclosingCaptured);
        }
        for name in m_globals {
            m_bindings.insert(name, BindingOrigin::Global);
        }
        tree.module_unit_scope = tree.scopes.len();
        tree.scopes.push(Scope {
            kind: ScopeKind::Function,
            parent: Some(module_scope_id),
            body_ptr: None, // found via unit_scope_of's FnBody::Module arm, not by pointer
            name: None,
            bindings: m_bindings,
        });

        // Real `def`/`async def`/`lambda` scopes parent to the module BINDING
        // scope (id 0), NOT to M — a function referencing a module-level `x`
        // must see it as `ModuleBinding`, exactly as the flat model does.
        for stmt in &module.body {
            build_in_statement(&mut tree, module_scope_id, stmt);
        }
        tree
    }

    /// The module binding scope's id — always `0`.
    pub(crate) fn module_scope(&self) -> ScopeId {
        0
    }

    /// Push a new `Function` scope (a `def`/`async def`/`lambda`), parented to
    /// `parent`, and return its id. Internal builder helper.
    #[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
    fn push_function_scope(
        &mut self,
        parent: ScopeId,
        body_ptr: usize,
        name: Option<String>,
        bindings: HashMap<String, BindingOrigin>,
    ) -> ScopeId {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            kind: ScopeKind::Function,
            parent: Some(parent),
            body_ptr: Some(body_ptr),
            name,
            bindings,
        });
        id
    }

    /// Resolve `name` starting at `from_scope`: check `from_scope`, then follow
    /// `parent` up to **and including** the nearest enclosing `Function` scope,
    /// then check `module_scope()`; first match wins, else `Unresolved`.
    ///
    /// Today every scope is either `Module` or `Function` (no block scopes), so
    /// this is effectively `from_scope → module_scope()` — but the walk is
    /// written generally so it stays correct if a sub-function scope (e.g. a
    /// comprehension scope, #55/Task 8) is ever inserted between a use and its
    /// enclosing function.
    #[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
    pub(crate) fn resolve(&self, from_scope: ScopeId, name: &str) -> (BindingOrigin, BindingId) {
        let mut scope = from_scope;
        loop {
            if let Some(&origin) = self.scopes[scope].bindings.get(name) {
                return (origin, self.binding_id_for(name, origin));
            }
            if self.scopes[scope].kind == ScopeKind::Function {
                // Just checked (and missed) the nearest enclosing function scope —
                // do not keep walking parents; jump straight to module scope below.
                break;
            }
            match self.scopes[scope].parent {
                Some(parent) => scope = parent,
                None => break, // scope is the module scope (already checked above)
            }
        }
        if scope != self.module_scope()
            && let Some(&origin) = self.scopes[self.module_scope()].bindings.get(name)
        {
            return (origin, self.binding_id_for(name, origin));
        }
        (
            BindingOrigin::Unresolved,
            self.binding_id_for(name, BindingOrigin::Unresolved),
        )
    }

    /// `BindingId` construction is frontend-internal and not byte-asserted by
    /// any consumer (§ guideline's binding-id scheme). Every binding produced
    /// by this task lives at its scope's body root, so `scope_segments` is
    /// always empty; a later task that pushes sub-scopes below a function
    /// (comprehensions, Task 8) is what would give it real segments.
    #[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
    fn binding_id_for(&self, name: &str, origin: BindingOrigin) -> BindingId {
        BindingId {
            scope_segments: Vec::new(),
            kind_name: format!("{}:{name}", kind_prefix(origin)),
        }
    }

    /// Map `unit` to its `Function` scope by **body-node pointer identity**
    /// (never `(line, col)` — see the guideline's rationale). The synthetic
    /// `<module>` unit maps to the **module-unit scope** (`module_unit_scope`),
    /// NOT the module binding scope (id 0) — the `<module>` unit's own top-level
    /// assign/`for`/`with` targets are its `Local`s, which live in M.
    ///
    /// A miss is loud, never silent: exactly one scope must match a
    /// non-module unit's body pointer. `unit_scope_of` and `ScopeTree::build`
    /// must be run over the **same** parsed `Module` reference for the
    /// pointers to coincide (see `functions.rs`'s single-borrowed-pass doc).
    #[allow(dead_code)] // wired in by Task 4
    pub(crate) fn unit_scope_of(&self, unit: &crate::functions::FnUnit) -> ScopeId {
        let ptr = match &unit.body {
            FnBody::Module(_) => return self.module_unit_scope,
            FnBody::Suite(suite) => (*suite) as *const Suite as usize,
            FnBody::Expr(expr) => (*expr) as *const Expression as usize,
        };
        let mut found: Option<ScopeId> = None;
        let mut hits = 0usize;
        for (id, scope) in self.scopes.iter().enumerate() {
            if scope.body_ptr == Some(ptr) {
                hits += 1;
                found = Some(id);
            }
        }
        debug_assert_eq!(
            hits, 1,
            "unit_scope_of: expected exactly one scope for unit `{}` (body pointer {ptr:#x}), \
             found {hits} — pointer identity between functions::collect and scope::build broke",
            unit.symbol,
        );
        found.unwrap_or_else(|| {
            panic!(
                "unit_scope_of: no scope found for unit `{}` (body pointer {ptr:#x}) — \
                 pointer identity between functions::collect and scope::build broke",
                unit.symbol
            )
        })
    }

    /// Test helper: the module-unit scope M (models the `<module>` unit's own
    /// body). Production code reaches it via `unit_scope_of`'s `FnBody::Module`
    /// arm; tests use this accessor since they have no `FnUnit` to hand.
    #[cfg(test)]
    fn module_unit_scope(&self) -> ScopeId {
        self.module_unit_scope
    }

    /// Test helper: find the `Function` scope whose recorded def-name equals
    /// `name`. Production code never resolves a scope by name — only by
    /// body-pointer (`unit_scope_of`).
    #[cfg(test)]
    fn scope_of_name(&self, name: &str) -> ScopeId {
        self.scopes
            .iter()
            .position(|s| s.kind == ScopeKind::Function && s.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no function scope named `{name}`"))
    }
}

// ─── function/lambda scope construction ──────────────────────────────────────
//
// Walks the module looking for every `def`/`async def`/`lambda`, pushing a
// `Function` scope for each, parented to its LEXICAL enclosing scope (a class
// body is not a scope boundary — see `build_classdef`). Mirrors the traversal
// shape of `functions::collect`, but tracks a `ScopeId` "current scope" cursor
// instead of collecting `FnUnit`s, and does not need line/col/anchors (scope
// identity is body-pointer, not position).

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_statement(tree: &mut ScopeTree, cur: ScopeId, stmt: &Statement) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                build_in_small(tree, cur, small);
            }
        }
        Statement::Compound(c) => build_in_compound(tree, cur, c),
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_compound(tree: &mut ScopeTree, cur: ScopeId, compound: &CompoundStatement) {
    match compound {
        CompoundStatement::FunctionDef(f) => build_funcdef(tree, cur, f),
        CompoundStatement::ClassDef(c) => build_classdef(tree, cur, c),
        CompoundStatement::If(i) => {
            build_in_expr(tree, cur, &i.test);
            build_in_suite(tree, cur, &i.body);
            if let Some(orelse) = &i.orelse {
                build_in_orelse(tree, cur, orelse);
            }
        }
        CompoundStatement::For(f) => {
            build_in_expr(tree, cur, &f.iter);
            build_in_suite(tree, cur, &f.body);
            if let Some(orelse) = &f.orelse {
                build_in_suite(tree, cur, &orelse.body);
            }
        }
        CompoundStatement::While(w) => {
            build_in_expr(tree, cur, &w.test);
            build_in_suite(tree, cur, &w.body);
            if let Some(orelse) = &w.orelse {
                build_in_suite(tree, cur, &orelse.body);
            }
        }
        CompoundStatement::Try(t) => {
            build_in_suite(tree, cur, &t.body);
            for h in &t.handlers {
                build_in_suite(tree, cur, &h.body);
            }
            if let Some(orelse) = &t.orelse {
                build_in_suite(tree, cur, &orelse.body);
            }
            if let Some(fin) = &t.finalbody {
                build_in_suite(tree, cur, &fin.body);
            }
        }
        CompoundStatement::TryStar(t) => {
            build_in_suite(tree, cur, &t.body);
            for h in &t.handlers {
                build_in_suite(tree, cur, &h.body);
            }
            if let Some(orelse) = &t.orelse {
                build_in_suite(tree, cur, &orelse.body);
            }
            if let Some(fin) = &t.finalbody {
                build_in_suite(tree, cur, &fin.body);
            }
        }
        CompoundStatement::With(w) => {
            for item in &w.items {
                build_in_expr(tree, cur, &item.item);
            }
            build_in_suite(tree, cur, &w.body);
        }
        CompoundStatement::Match(m) => {
            build_in_expr(tree, cur, &m.subject);
            for case in &m.cases {
                build_in_suite(tree, cur, &case.body);
            }
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_orelse(tree: &mut ScopeTree, cur: ScopeId, orelse: &OrElse) {
    match orelse {
        OrElse::Elif(elif) => {
            build_in_expr(tree, cur, &elif.test);
            build_in_suite(tree, cur, &elif.body);
            if let Some(inner) = &elif.orelse {
                build_in_orelse(tree, cur, inner);
            }
        }
        OrElse::Else(e) => build_in_suite(tree, cur, &e.body),
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_suite(tree: &mut ScopeTree, cur: ScopeId, suite: &Suite) {
    match suite {
        Suite::IndentedBlock(b) => {
            for stmt in &b.body {
                build_in_statement(tree, cur, stmt);
            }
        }
        Suite::SimpleStatementSuite(s) => {
            for small in &s.body {
                build_in_small(tree, cur, small);
            }
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_small(tree: &mut ScopeTree, cur: ScopeId, small: &SmallStatement) {
    match small {
        SmallStatement::Assign(a) => build_in_expr(tree, cur, &a.value),
        SmallStatement::AnnAssign(a) => {
            if let Some(v) = &a.value {
                build_in_expr(tree, cur, v);
            }
        }
        SmallStatement::AugAssign(a) => build_in_expr(tree, cur, &a.value),
        SmallStatement::Return(r) => {
            if let Some(v) = &r.value {
                build_in_expr(tree, cur, v);
            }
        }
        SmallStatement::Expr(e) => build_in_expr(tree, cur, &e.value),
        SmallStatement::Raise(r) => {
            if let Some(exc) = &r.exc {
                build_in_expr(tree, cur, exc);
            }
            if let Some(from) = &r.cause {
                build_in_expr(tree, cur, &from.item);
            }
        }
        SmallStatement::Assert(a) => {
            build_in_expr(tree, cur, &a.test);
            if let Some(msg) = &a.msg {
                build_in_expr(tree, cur, msg);
            }
        }
        SmallStatement::Del(d) => build_in_del_target(tree, cur, &d.target),
        _ => {}
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_del_target(tree: &mut ScopeTree, cur: ScopeId, target: &DelTargetExpression) {
    match target {
        DelTargetExpression::Attribute(a) => build_in_expr(tree, cur, &a.value),
        DelTargetExpression::Subscript(s) => {
            build_in_expr(tree, cur, &s.value);
            for element in &s.slice {
                build_in_base_slice(tree, cur, &element.slice);
            }
        }
        DelTargetExpression::Tuple(t) => {
            for el in &t.elements {
                build_in_element(tree, cur, el);
            }
        }
        DelTargetExpression::List(l) => {
            for el in &l.elements {
                build_in_element(tree, cur, el);
            }
        }
        DelTargetExpression::Name(_) => {}
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_element(tree: &mut ScopeTree, cur: ScopeId, el: &Element) {
    match el {
        Element::Simple { value, .. } => build_in_expr(tree, cur, value),
        Element::Starred(s) => build_in_expr(tree, cur, &s.value),
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_base_slice(tree: &mut ScopeTree, cur: ScopeId, slice: &BaseSlice) {
    match slice {
        BaseSlice::Index(i) => build_in_expr(tree, cur, &i.value),
        BaseSlice::Slice(s) => {
            if let Some(lower) = &s.lower {
                build_in_expr(tree, cur, lower);
            }
            if let Some(upper) = &s.upper {
                build_in_expr(tree, cur, upper);
            }
            if let Some(step) = &s.step {
                build_in_expr(tree, cur, step);
            }
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_comp_for(tree: &mut ScopeTree, cur: ScopeId, comp: &CompFor) {
    build_in_expr(tree, cur, &comp.iter);
    for cond in &comp.ifs {
        build_in_expr(tree, cur, &cond.test);
    }
    if let Some(inner) = &comp.inner_for_in {
        build_in_comp_for(tree, cur, inner);
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_fstring_parts(tree: &mut ScopeTree, cur: ScopeId, parts: &[FormattedStringContent]) {
    for part in parts {
        if let FormattedStringContent::Expression(e) = part {
            build_in_expr(tree, cur, &e.expression);
            if let Some(spec) = &e.format_spec {
                build_in_fstring_parts(tree, cur, spec);
            }
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_expr(tree: &mut ScopeTree, cur: ScopeId, expr: &Expression) {
    match expr {
        Expression::Lambda(l) => {
            // A lambda's own scope: params → Param. No suite to prescan (a
            // lambda body is a single expression, never a statement list).
            let mut bindings = HashMap::new();
            insert_param_bindings(&mut bindings, &l.params);
            let body_ptr = (&*l.body) as *const Expression as usize;
            let lambda_scope = tree.push_function_scope(cur, body_ptr, None, bindings);
            // Parameter defaults evaluate in the ENCLOSING scope, def-time.
            build_in_params(tree, cur, &l.params);
            // The body is evaluated lazily, in the lambda's OWN scope.
            build_in_expr(tree, lambda_scope, &l.body);
        }
        Expression::BinaryOperation(b) => {
            build_in_expr(tree, cur, &b.left);
            build_in_expr(tree, cur, &b.right);
        }
        Expression::BooleanOperation(b) => {
            build_in_expr(tree, cur, &b.left);
            build_in_expr(tree, cur, &b.right);
        }
        Expression::UnaryOperation(u) => build_in_expr(tree, cur, &u.expression),
        Expression::Comparison(c) => {
            build_in_expr(tree, cur, &c.left);
            for comp in &c.comparisons {
                build_in_expr(tree, cur, &comp.comparator);
            }
        }
        Expression::IfExp(i) => {
            build_in_expr(tree, cur, &i.test);
            build_in_expr(tree, cur, &i.body);
            build_in_expr(tree, cur, &i.orelse);
        }
        Expression::Call(c) => {
            build_in_expr(tree, cur, &c.func);
            for arg in &c.args {
                build_in_expr(tree, cur, &arg.value);
            }
        }
        Expression::Attribute(a) => build_in_expr(tree, cur, &a.value),
        Expression::Subscript(s) => {
            build_in_expr(tree, cur, &s.value);
            for element in &s.slice {
                build_in_base_slice(tree, cur, &element.slice);
            }
        }
        Expression::Tuple(t) => {
            for el in &t.elements {
                build_in_element(tree, cur, el);
            }
        }
        Expression::List(l) => {
            for el in &l.elements {
                build_in_element(tree, cur, el);
            }
        }
        Expression::Set(s) => {
            for el in &s.elements {
                build_in_element(tree, cur, el);
            }
        }
        Expression::Dict(d) => {
            for el in &d.elements {
                match el {
                    DictElement::Simple { key, value, .. } => {
                        build_in_expr(tree, cur, key);
                        build_in_expr(tree, cur, value);
                    }
                    DictElement::Starred(s) => build_in_expr(tree, cur, &s.value),
                }
            }
        }
        Expression::ListComp(l) => {
            build_in_expr(tree, cur, &l.elt);
            build_in_comp_for(tree, cur, &l.for_in);
        }
        Expression::SetComp(s) => {
            build_in_expr(tree, cur, &s.elt);
            build_in_comp_for(tree, cur, &s.for_in);
        }
        Expression::GeneratorExp(g) => {
            build_in_expr(tree, cur, &g.elt);
            build_in_comp_for(tree, cur, &g.for_in);
        }
        Expression::DictComp(d) => {
            build_in_expr(tree, cur, &d.key);
            build_in_expr(tree, cur, &d.value);
            build_in_comp_for(tree, cur, &d.for_in);
        }
        Expression::FormattedString(fs) => build_in_fstring_parts(tree, cur, &fs.parts),
        Expression::Yield(y) => {
            if let Some(v) = &y.value {
                match &**v {
                    YieldValue::Expression(e) => build_in_expr(tree, cur, e),
                    YieldValue::From(f) => build_in_expr(tree, cur, &f.item),
                }
            }
        }
        Expression::Await(a) => build_in_expr(tree, cur, &a.expression),
        Expression::NamedExpr(n) => build_in_expr(tree, cur, &n.value),
        Expression::StarredElement(s) => build_in_expr(tree, cur, &s.value),

        // Leaf expressions (Name, Ellipsis, Integer, Float, Imaginary,
        // SimpleString, ConcatenatedString, TemplatedString) contain no lambdas.
        _ => {}
    }
}

/// Lambda/def parameter DEFAULT expressions evaluate in the enclosing scope,
/// at def-time, and may themselves contain lambdas.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_in_params(tree: &mut ScopeTree, cur: ScopeId, params: &Parameters) {
    let positional = params
        .posonly_params
        .iter()
        .chain(&params.params)
        .chain(&params.kwonly_params);
    for p in positional {
        if let Some(default) = &p.default {
            build_in_expr(tree, cur, default);
        }
    }
    if let Some(StarArg::Param(p)) = &params.star_arg
        && let Some(default) = &p.default
    {
        build_in_expr(tree, cur, default);
    }
    if let Some(p) = &params.star_kwarg
        && let Some(default) = &p.default
    {
        build_in_expr(tree, cur, default);
    }
}

/// Insert every parameter name in `params` as `Param` — positional-only,
/// regular, keyword-only, `*args`, `**kwargs`. Mirrors
/// `detect::mutation::collect_param_names`.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn insert_param_bindings(bindings: &mut HashMap<String, BindingOrigin>, params: &Parameters) {
    let all = params
        .posonly_params
        .iter()
        .chain(&params.params)
        .chain(&params.kwonly_params);
    for p in all {
        bindings.insert(p.name.value.to_owned(), BindingOrigin::Param);
    }
    if let Some(StarArg::Param(p)) = &params.star_arg {
        bindings.insert(p.name.value.to_owned(), BindingOrigin::Param);
    }
    if let Some(p) = &params.star_kwarg {
        bindings.insert(p.name.value.to_owned(), BindingOrigin::Param);
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_funcdef(tree: &mut ScopeTree, cur: ScopeId, f: &FunctionDef) {
    // Decorators and parameter defaults evaluate in the ENCLOSING scope, at
    // def-time — before the new Function scope even exists.
    for dec in &f.decorators {
        build_in_expr(tree, cur, &dec.decorator);
    }
    build_in_params(tree, cur, &f.params);

    // This def's own Function scope: params, then the prescan-replicated
    // locals/globals/nonlocals (see the prescan_* functions below — a faithful
    // port of `detect::mutation::prescan_*`, kept in lock-step so both collect
    // the same names). Insertion order matches `classify_and_push`'s
    // precedence (global > nonlocal > param > local): locals first, then
    // params, then nonlocals, then globals last so later inserts win on a
    // same-named clash.
    //
    // #78: `prescan_suite`'s traversal also collects this function's OWN
    // top-level `import`/`from … import` statements (own-body attribution —
    // it does not descend into a nested def/class, same as locals/globals/
    // nonlocals). A local import binds at the same "local" precedence tier —
    // inserted alongside `locals`, before params/nonlocals/globals — so it
    // never reaches module scope and never leaks to another function.
    let mut bindings = HashMap::new();
    let mut globals = HashSet::new();
    let mut nonlocals = HashSet::new();
    let mut locals = HashSet::new();
    let mut imports = HashMap::new();
    prescan_suite(
        &f.body,
        &mut globals,
        &mut nonlocals,
        &mut locals,
        &mut imports,
        true,
    );
    for name in locals {
        bindings.insert(name, BindingOrigin::Local);
    }
    for (name, origin) in imports {
        bindings.insert(name, origin);
    }
    insert_param_bindings(&mut bindings, &f.params);
    for name in nonlocals {
        bindings.insert(name, BindingOrigin::EnclosingCaptured);
    }
    for name in globals {
        bindings.insert(name, BindingOrigin::Global);
    }

    let body_ptr = (&f.body) as *const Suite as usize;
    let scope = tree.push_function_scope(cur, body_ptr, Some(f.name.value.to_owned()), bindings);

    // Recurse into the body — in the NEW scope — for nested defs/classes/lambdas.
    build_in_suite(tree, scope, &f.body);
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn build_classdef(tree: &mut ScopeTree, cur: ScopeId, c: &ClassDef) {
    // Classes are NOT a scope in this model (Python methods don't see the
    // class body as an enclosing scope): decorators, base-class args/keywords,
    // and the body all resolve in the ENCLOSING function/module scope. Methods
    // defined inside therefore parent directly to `cur`, skipping the class.
    for dec in &c.decorators {
        build_in_expr(tree, cur, &dec.decorator);
    }
    for base in &c.bases {
        build_in_expr(tree, cur, &base.value);
    }
    for kw in &c.keywords {
        build_in_expr(tree, cur, &kw.value);
    }
    build_in_suite(tree, cur, &c.body);
}

// ─── pre-scan: global/nonlocal declarations + local bindings + imports ───────
//
// A faithful replica of `detect::mutation::prescan_body`/`prescan_suite`/
// `prescan_stmt`/`prescan_compound`/`prescan_orelse`/`prescan_small` — see
// that module's doc comments for the full binding-form rationale (assign/
// annassign/augassign-Name targets, `for`/`with as`/`except as`/`except* as`,
// nested def/class names when `seed_defs`). The originals in `mutation.rs`
// were removed once Task 6 finished threading `ScopeTree` through the
// detector — this is a parallel collector, not a relocation, and (#78) now
// collects one thing the mutation.rs original never did: `import`/
// `from … import` statements in the SAME "own-body attribution" shape (never
// descending into a nested def/class body) — used both for a function's own
// local imports (`build_funcdef`) and, via the module-level call in
// `ScopeTree::build`, for the module's top-level-only imports.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_suite(
    suite: &Suite,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    imports: &mut HashMap<String, BindingOrigin>,
    seed_defs: bool,
) {
    match suite {
        Suite::IndentedBlock(b) => {
            for stmt in &b.body {
                prescan_stmt(stmt, globals, nonlocals, locals, imports, seed_defs);
            }
        }
        Suite::SimpleStatementSuite(s) => {
            for small in &s.body {
                prescan_small(small, globals, nonlocals, locals, imports);
            }
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_stmt(
    stmt: &Statement,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    imports: &mut HashMap<String, BindingOrigin>,
    seed_defs: bool,
) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                prescan_small(small, globals, nonlocals, locals, imports);
            }
        }
        Statement::Compound(c) => {
            prescan_compound(c, globals, nonlocals, locals, imports, seed_defs)
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_compound(
    compound: &CompoundStatement,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    imports: &mut HashMap<String, BindingOrigin>,
    seed_defs: bool,
) {
    match compound {
        // Nested def/class: do NOT descend (own-body attribution — this is
        // exactly why a nested def's local import, or ANY class-body import,
        // never reaches this level's `imports` accumulator, #78), but in a
        // FUNCTION body the def/class *name* is a local binding of the
        // enclosing function (refs #55); `seed_defs` gates this (false for
        // the `<module>` unit, which this collector never scans — see the
        // module note above).
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
            prescan_expr(&i.test, locals);
            prescan_suite(&i.body, globals, nonlocals, locals, imports, seed_defs);
            if let Some(orelse) = &i.orelse {
                prescan_orelse(orelse, globals, nonlocals, locals, imports, seed_defs);
            }
        }
        CompoundStatement::For(f) => {
            crate::imports::collect_target_names(&f.target, locals);
            prescan_expr(&f.iter, locals);
            prescan_suite(&f.body, globals, nonlocals, locals, imports, seed_defs);
            if let Some(orelse) = &f.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, imports, seed_defs);
            }
        }
        CompoundStatement::While(w) => {
            prescan_expr(&w.test, locals);
            prescan_suite(&w.body, globals, nonlocals, locals, imports, seed_defs);
            if let Some(orelse) = &w.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, imports, seed_defs);
            }
        }
        CompoundStatement::Try(t) => {
            prescan_suite(&t.body, globals, nonlocals, locals, imports, seed_defs);
            for h in &t.handlers {
                if let Some(asname) = &h.name {
                    crate::imports::collect_target_names(&asname.name, locals);
                }
                prescan_suite(&h.body, globals, nonlocals, locals, imports, seed_defs);
            }
            if let Some(orelse) = &t.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, imports, seed_defs);
            }
            if let Some(fin) = &t.finalbody {
                prescan_suite(&fin.body, globals, nonlocals, locals, imports, seed_defs);
            }
        }
        CompoundStatement::TryStar(t) => {
            prescan_suite(&t.body, globals, nonlocals, locals, imports, seed_defs);
            for h in &t.handlers {
                if let Some(asname) = &h.name {
                    crate::imports::collect_target_names(&asname.name, locals);
                }
                prescan_suite(&h.body, globals, nonlocals, locals, imports, seed_defs);
            }
            if let Some(orelse) = &t.orelse {
                prescan_suite(&orelse.body, globals, nonlocals, locals, imports, seed_defs);
            }
            if let Some(fin) = &t.finalbody {
                prescan_suite(&fin.body, globals, nonlocals, locals, imports, seed_defs);
            }
        }
        CompoundStatement::With(w) => {
            for item in &w.items {
                prescan_expr(&item.item, locals);
                if let Some(asname) = &item.asname {
                    crate::imports::collect_target_names(&asname.name, locals);
                }
            }
            prescan_suite(&w.body, globals, nonlocals, locals, imports, seed_defs);
        }
        // #55/Task 8: a `case` pattern's CAPTURES (`case [name, *args]:`,
        // `case Foo() as b:`, …) bind function-locals — collected via
        // `collect_match_pattern_locals` (see its doc comment for which
        // pattern forms bind vs. which are values-to-compare-against). The
        // subject and each guard are ordinary expressions and may themselves
        // hold a walrus target.
        CompoundStatement::Match(m) => {
            prescan_expr(&m.subject, locals);
            for case in &m.cases {
                collect_match_pattern_locals(&case.pattern, locals);
                if let Some(guard) = &case.guard {
                    prescan_expr(guard, locals);
                }
                prescan_suite(&case.body, globals, nonlocals, locals, imports, seed_defs);
            }
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_orelse(
    orelse: &OrElse,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    imports: &mut HashMap<String, BindingOrigin>,
    seed_defs: bool,
) {
    match orelse {
        OrElse::Elif(elif) => {
            prescan_expr(&elif.test, locals);
            prescan_suite(&elif.body, globals, nonlocals, locals, imports, seed_defs);
            if let Some(inner) = &elif.orelse {
                prescan_orelse(inner, globals, nonlocals, locals, imports, seed_defs);
            }
        }
        OrElse::Else(e) => {
            prescan_suite(&e.body, globals, nonlocals, locals, imports, seed_defs);
        }
    }
}

#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_small(
    small: &SmallStatement,
    globals: &mut HashSet<String>,
    nonlocals: &mut HashSet<String>,
    locals: &mut HashSet<String>,
    imports: &mut HashMap<String, BindingOrigin>,
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
            for target in &a.targets {
                crate::imports::collect_target_names(&target.target, locals);
            }
            prescan_expr(&a.value, locals);
        }
        SmallStatement::AnnAssign(a) => {
            crate::imports::collect_target_names(&a.target, locals);
            if let Some(v) = &a.value {
                prescan_expr(v, locals);
            }
        }
        SmallStatement::AugAssign(a) => {
            if let AssignTargetExpression::Name(n) = &a.target {
                locals.insert(n.value.to_owned());
            }
            prescan_expr(&a.value, locals);
        }
        // #55/Task 8: these four never bound anything themselves, but their
        // expression(s) may hold a walrus target or a comprehension — the
        // ORIGINAL `prescan_small` never walked into ANY expression at all
        // (only assign/for/with targets), so these fell through to the `_`
        // catch-all below and their walrus/comprehension names were missed.
        SmallStatement::Return(r) => {
            if let Some(v) = &r.value {
                prescan_expr(v, locals);
            }
        }
        SmallStatement::Expr(e) => prescan_expr(&e.value, locals),
        SmallStatement::Raise(r) => {
            if let Some(exc) = &r.exc {
                prescan_expr(exc, locals);
            }
            if let Some(from) = &r.cause {
                prescan_expr(&from.item, locals);
            }
        }
        SmallStatement::Assert(a) => {
            prescan_expr(&a.test, locals);
            if let Some(msg) = &a.msg {
                prescan_expr(msg, locals);
            }
        }
        // `import a[.b.c][ as x]` — a plain import binds a MODULE (see
        // `imports.rs`'s `is_module_import` doc): mirrors that module's
        // `collect_small` local-key derivation exactly (shared helpers), so
        // both places agree on what a bare `import a.b.c` or an aliased
        // import binds locally.
        SmallStatement::Import(imp) => {
            for alias in &imp.names {
                let path = crate::imports::noa_to_string(&alias.name);
                let local = if let Some(asname) = &alias.asname {
                    crate::imports::ate_to_string(&asname.name)
                } else {
                    crate::imports::root_component(&path)
                };
                imports.insert(local, BindingOrigin::ModuleImport);
            }
        }
        // `from m import n[ as p]` — binds an arbitrary member, not a module
        // (may be mutable, #50) — `from m import *` is skipped (unknowable
        // local names), same as `imports.rs`'s `collect_small`.
        SmallStatement::ImportFrom(from) => {
            let ImportNames::Aliases(aliases) = &from.names else {
                return;
            };
            for alias in aliases {
                let local = if let Some(asname) = &alias.asname {
                    crate::imports::ate_to_string(&asname.name)
                } else {
                    crate::imports::noa_to_string(&alias.name)
                };
                imports.insert(local, BindingOrigin::FromImport);
            }
        }
        _ => {}
    }
}

// ─── #55/Task 8: match-case captures, walrus targets, comprehension targets ──
//
// Three binding forms the ORIGINAL `prescan_*` above never collected, so
// their names fell to `Unresolved` -> `hidden.mutation "captured-binding"`
// (false positives). All three are collected here into the SAME function
// scope as `Local` — the same simple mechanism as the existing assign/for/
// with collection above, deliberately NOT a comprehension-own scope (see the
// module-level `ScopeTree` doc comment and the #78 plan for why a comprehension-
// own scope was ruled out: the comprehension boundary is known only to the
// shared `detect/mod.rs` walker, not to this frontend-internal builder, and an
// own-scope split would be EFFECT-IDENTICAL — `local.mutation` either way —
// so it isn't worth the `EffectSink`/driver surgery it would need).

/// Every name a `match`-case pattern BINDS (captures) — recurse through
/// sequence/mapping/class/`as`/`or`/`*rest`/`**rest` patterns. A `MatchValue`
/// (dotted-name/literal comparison, e.g. `case Color.RED:`) and a
/// `MatchSingleton` (`None`/`True`/`False`) bind nothing — they are VALUES
/// being compared against, not capture targets (mirrors the grammar's
/// `capture_pattern` — a bare name — vs. `value_pattern` — a dotted name).
/// A wildcard `_` is `MatchAs { name: None, .. }` — also not a binding.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn collect_match_pattern_locals(pattern: &MatchPattern, locals: &mut HashSet<String>) {
    match pattern {
        MatchPattern::Value(_) | MatchPattern::Singleton(_) => {}
        MatchPattern::Sequence(seq) => match seq {
            MatchSequence::MatchList(l) => {
                for el in &l.patterns {
                    collect_starrable_match_element(el, locals);
                }
            }
            MatchSequence::MatchTuple(t) => {
                for el in &t.patterns {
                    collect_starrable_match_element(el, locals);
                }
            }
        },
        MatchPattern::Mapping(m) => {
            for el in &m.elements {
                collect_match_pattern_locals(&el.pattern, locals);
            }
            if let Some(rest) = &m.rest {
                locals.insert(rest.value.to_owned());
            }
        }
        MatchPattern::Class(c) => {
            for el in &c.patterns {
                collect_match_pattern_locals(&el.value, locals);
            }
            for kwd in &c.kwds {
                collect_match_pattern_locals(&kwd.pattern, locals);
            }
        }
        MatchPattern::As(as_pat) => {
            if let Some(inner) = &as_pat.pattern {
                collect_match_pattern_locals(inner, locals);
            }
            if let Some(name) = &as_pat.name {
                locals.insert(name.value.to_owned());
            }
        }
        MatchPattern::Or(or_pat) => {
            for el in &or_pat.patterns {
                collect_match_pattern_locals(&el.pattern, locals);
            }
        }
    }
}

/// A sequence-pattern element (`case [a, *rest]:`) is either a plain
/// sub-pattern or a starred capture (`*rest`/`*_`).
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn collect_starrable_match_element(
    el: &StarrableMatchSequenceElement,
    locals: &mut HashSet<String>,
) {
    match el {
        StarrableMatchSequenceElement::Simple(s) => collect_match_pattern_locals(&s.value, locals),
        StarrableMatchSequenceElement::Starred(s) => {
            if let Some(name) = &s.name {
                locals.insert(name.value.to_owned());
            }
        }
    }
}

/// Walk `expr` collecting walrus (`NamedExpr`) targets and comprehension
/// `for` targets — the two forms `prescan_small`/`prescan_compound` above
/// need an expression-level walk for (neither is a statement-level target).
/// Recurses into every expression variant that can hold a nested walrus or
/// comprehension — same shape as `build_in_expr` below, EXCEPT a `Lambda`
/// body is not descended into: a lambda is its OWN function scope, so a
/// walrus inside it binds there, not in this (enclosing) scope.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_expr(expr: &Expression, locals: &mut HashSet<String>) {
    match expr {
        Expression::Lambda(_) => {} // own scope — not descended into, see doc comment above
        Expression::BinaryOperation(b) => {
            prescan_expr(&b.left, locals);
            prescan_expr(&b.right, locals);
        }
        Expression::BooleanOperation(b) => {
            prescan_expr(&b.left, locals);
            prescan_expr(&b.right, locals);
        }
        Expression::UnaryOperation(u) => prescan_expr(&u.expression, locals),
        Expression::Comparison(c) => {
            prescan_expr(&c.left, locals);
            for comp in &c.comparisons {
                prescan_expr(&comp.comparator, locals);
            }
        }
        Expression::IfExp(i) => {
            prescan_expr(&i.test, locals);
            prescan_expr(&i.body, locals);
            prescan_expr(&i.orelse, locals);
        }
        Expression::Call(c) => {
            prescan_expr(&c.func, locals);
            for arg in &c.args {
                prescan_expr(&arg.value, locals);
            }
        }
        Expression::Attribute(a) => prescan_expr(&a.value, locals),
        Expression::Subscript(s) => {
            prescan_expr(&s.value, locals);
            for element in &s.slice {
                prescan_base_slice(&element.slice, locals);
            }
        }
        Expression::Tuple(t) => {
            for el in &t.elements {
                prescan_element(el, locals);
            }
        }
        Expression::List(l) => {
            for el in &l.elements {
                prescan_element(el, locals);
            }
        }
        Expression::Set(s) => {
            for el in &s.elements {
                prescan_element(el, locals);
            }
        }
        Expression::Dict(d) => {
            for el in &d.elements {
                match el {
                    DictElement::Simple { key, value, .. } => {
                        prescan_expr(key, locals);
                        prescan_expr(value, locals);
                    }
                    DictElement::Starred(s) => prescan_expr(&s.value, locals),
                }
            }
        }
        // Comprehension `for` targets (#55/Task 8): bind into THIS function
        // scope as `Local` (deliberately not a comprehension-own scope, see
        // this section's header comment).
        Expression::ListComp(l) => {
            prescan_comp_for(&l.for_in, locals);
            prescan_expr(&l.elt, locals);
        }
        Expression::SetComp(s) => {
            prescan_comp_for(&s.for_in, locals);
            prescan_expr(&s.elt, locals);
        }
        Expression::GeneratorExp(g) => {
            prescan_comp_for(&g.for_in, locals);
            prescan_expr(&g.elt, locals);
        }
        Expression::DictComp(d) => {
            prescan_comp_for(&d.for_in, locals);
            prescan_expr(&d.key, locals);
            prescan_expr(&d.value, locals);
        }
        Expression::FormattedString(fs) => prescan_fstring_parts(&fs.parts, locals),
        Expression::Yield(y) => {
            if let Some(v) = &y.value {
                match &**v {
                    YieldValue::Expression(e) => prescan_expr(e, locals),
                    YieldValue::From(f) => prescan_expr(&f.item, locals),
                }
            }
        }
        Expression::Await(a) => prescan_expr(&a.expression, locals),
        // Walrus (#55/Task 8): the target is always a bare `Name` (the only
        // syntax Python's grammar allows for `:=`) — binds into THIS function
        // scope as `Local`, wherever the expression appears (a condition, a
        // comprehension, …).
        Expression::NamedExpr(n) => {
            if let Expression::Name(name) = &*n.target {
                locals.insert(name.value.to_owned());
            }
            prescan_expr(&n.value, locals);
        }
        Expression::StarredElement(s) => prescan_expr(&s.value, locals),

        // Leaf expressions (Name, Ellipsis, Integer, Float, Imaginary,
        // SimpleString, ConcatenatedString, TemplatedString) hold no nested
        // walrus/comprehension.
        _ => {}
    }
}

/// A comprehension's `for … in …[ if …]` clause chain (list/set/dict/
/// generator all share this shape) — the target binds a function-`Local`;
/// `iter` and each `if` may themselves hold a nested walrus/comprehension.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_comp_for(comp: &CompFor, locals: &mut HashSet<String>) {
    crate::imports::collect_target_names(&comp.target, locals);
    prescan_expr(&comp.iter, locals);
    for cond in &comp.ifs {
        prescan_expr(&cond.test, locals);
    }
    if let Some(inner) = &comp.inner_for_in {
        prescan_comp_for(inner, locals);
    }
}

/// A destructuring/collection-literal element (`[a, *rest]`) — mirrors
/// `build_in_element` below.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_element(el: &Element, locals: &mut HashSet<String>) {
    match el {
        Element::Simple { value, .. } => prescan_expr(value, locals),
        Element::Starred(s) => prescan_expr(&s.value, locals),
    }
}

/// A subscript slice (`x[a:b:c]` or `x[i]`) — mirrors `build_in_base_slice`
/// below.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_base_slice(slice: &BaseSlice, locals: &mut HashSet<String>) {
    match slice {
        BaseSlice::Index(i) => prescan_expr(&i.value, locals),
        BaseSlice::Slice(s) => {
            if let Some(lower) = &s.lower {
                prescan_expr(lower, locals);
            }
            if let Some(upper) = &s.upper {
                prescan_expr(upper, locals);
            }
            if let Some(step) = &s.step {
                prescan_expr(step, locals);
            }
        }
    }
}

/// An f-string's embedded `{expr}` parts — mirrors `build_in_fstring_parts`
/// below.
#[allow(dead_code)] // only exercised by this module's own tests until Task 4 wires resolve()/unit_scope_of into the mutation detector
fn prescan_fstring_parts(parts: &[FormattedStringContent], locals: &mut HashSet<String>) {
    for part in parts {
        if let FormattedStringContent::Expression(e) = part {
            prescan_expr(&e.expression, locals);
            if let Some(spec) = &e.format_spec {
                prescan_fstring_parts(spec, locals);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_str(src: &str) -> ScopeTree {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let mb = crate::imports::module_bindings(&module);
        ScopeTree::build(&module, &imports, &mb)
    }

    #[test]
    fn module_scope_collects_module_bindings_and_top_level_imports() {
        let t = build_str("import os\nfrom cfg import CACHE\nx = 0\n");
        let m = t.module_scope();
        assert_eq!(t.resolve(m, "os").0, BindingOrigin::ModuleImport);
        assert_eq!(t.resolve(m, "CACHE").0, BindingOrigin::FromImport);
        assert_eq!(t.resolve(m, "x").0, BindingOrigin::ModuleBinding);
        assert_eq!(t.resolve(m, "nope").0, BindingOrigin::Unresolved);
    }

    /// #78: a function-local import binds ONLY that function's own scope — it
    /// must never reach module scope (so it can't collide with another
    /// function's same-named binding, the bug this task closes).
    #[test]
    fn function_local_import_does_not_leak_to_module_scope() {
        let t = build_str("def g():\n    import os as CACHE\n    CACHE\n");
        let g = t.scope_of_name("g");
        assert_eq!(t.resolve(g, "CACHE").0, BindingOrigin::ModuleImport);
        // Module scope never saw `g`'s local import at all.
        assert_eq!(
            t.resolve(t.module_scope(), "CACHE").0,
            BindingOrigin::Unresolved
        );
    }

    /// #78: the collision itself — a top-level `from`-import and an unrelated
    /// function's own same-named local import must resolve independently.
    #[test]
    fn sibling_functions_do_not_share_local_import_bindings() {
        let t = build_str(
            "from cfg import CACHE\ndef f():\n    CACHE\ndef g():\n    import os as CACHE\n",
        );
        let f = t.scope_of_name("f");
        let g = t.scope_of_name("g");
        // f never declared its own CACHE, so it walks up to module scope's
        // top-level from-import — unpolluted by g's local import.
        assert_eq!(t.resolve(f, "CACHE").0, BindingOrigin::FromImport);
        // g's own local import shadows the module-level from-import.
        assert_eq!(t.resolve(g, "CACHE").0, BindingOrigin::ModuleImport);
    }

    /// #78: a class-body import is not a lexical scope Python methods see
    /// (class scope isn't in a method's lexical chain) — it must not reach
    /// module scope, and (since a class body isn't a `Function` scope either)
    /// it isn't collected anywhere at all. Small bundled correctness gain,
    /// not merely compat (per the guideline).
    #[test]
    fn class_body_import_does_not_reach_module_scope() {
        let t = build_str("class C:\n    import os\n");
        assert_eq!(
            t.resolve(t.module_scope(), "os").0,
            BindingOrigin::Unresolved
        );
    }

    #[test]
    fn function_scope_params_locals_and_decls() {
        let t = build_str(
            "import os\ndef f(p):\n    global g\n    x = 1\n    for i in p:\n        i\n    p; x; g; os\n",
        );
        let f = t.scope_of_name("f");
        assert_eq!(t.resolve(f, "p").0, BindingOrigin::Param);
        assert_eq!(t.resolve(f, "x").0, BindingOrigin::Local);
        assert_eq!(t.resolve(f, "i").0, BindingOrigin::Local); // for-target is function-local
        assert_eq!(t.resolve(f, "g").0, BindingOrigin::Global);
        assert_eq!(t.resolve(f, "os").0, BindingOrigin::ModuleImport); // walks to module
    }

    #[test]
    fn nonlocal_is_enclosing_captured() {
        // `nonlocal` needs a real enclosing binding; test it with proper nesting.
        let t = build_str("def outer():\n    n = 0\n    def f():\n        nonlocal n\n        n\n");
        let f = t.scope_of_name("f");
        assert_eq!(t.resolve(f, "n").0, BindingOrigin::EnclosingCaptured); // declared capture
    }

    #[test]
    fn param_shadows_module_import() {
        let t = build_str("import os\ndef f(os):\n    os\n");
        let f = t.scope_of_name("f");
        assert_eq!(t.resolve(f, "os").0, BindingOrigin::Param); // nearest-enclosing beats the import
    }

    #[test]
    fn implicit_capture_is_unresolved_not_local() {
        // Non-goal boundary: inner's use of outer's local resolves to Unresolved
        // (function->module walk skips ancestor functions) — byte-identical with the flat model.
        let t = build_str("def outer():\n    x = []\n    def inner():\n        x.append(1)\n");
        let inner = t.scope_of_name("inner");
        assert_eq!(t.resolve(inner, "x").0, BindingOrigin::Unresolved);
    }

    #[test]
    fn module_unit_top_level_assign_is_local() {
        // Byte-identity with the flat `<module>` unit: a top-level assign target
        // is the module UNIT's own local (LocalMutation/contained), while the
        // same name is a ModuleBinding to every OTHER unit. A top-level class
        // name is a ModuleBinding to the module unit too (seed_defs=false).
        let t = build_str("x = {}\nclass C: pass\n");
        let m = t.module_unit_scope();
        assert_eq!(t.resolve(m, "x").0, BindingOrigin::Local); // module unit's own local
        assert_eq!(
            t.resolve(t.module_scope(), "x").0,
            BindingOrigin::ModuleBinding // what a function sees for the same name
        );
        assert_eq!(t.resolve(m, "C").0, BindingOrigin::ModuleBinding); // class name not seeded
    }

    #[test]
    fn module_unit_top_level_for_target_is_local() {
        // A top-level `for i in xs:` target is a function-local of the module
        // unit — matches the flat model's `prescan_body(FnBody::Module)` locals.
        let t = build_str("xs = []\nfor i in xs:\n    i.append(1)\n");
        let m = t.module_unit_scope();
        assert_eq!(t.resolve(m, "i").0, BindingOrigin::Local);
    }

    #[test]
    fn binding_origin_variants_exist() {
        // Compile-time coverage: every guideline origin is representable.
        let _ = [
            BindingOrigin::Param,
            BindingOrigin::Local,
            BindingOrigin::ModuleImport,
            BindingOrigin::FromImport,
            BindingOrigin::ModuleBinding,
            BindingOrigin::Global,
            BindingOrigin::EnclosingCaptured,
            BindingOrigin::HostGlobalIo,
            BindingOrigin::Unresolved,
        ];
        let id = BindingId {
            scope_segments: vec!["b3".into()],
            kind_name: "local:x".into(),
        };
        assert_eq!(id.kind_name, "local:x");
    }
}
