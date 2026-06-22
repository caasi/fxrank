//! Mutation detection with escape analysis — the swc analog of
//! `fxrank-lang-rust`'s `detect/mutation.rs`.
//!
//! JavaScript/TypeScript has no `&mut` to read containment off the signature, so
//! the discount story is recovered by *escape analysis*: each write site is
//! classified by where its base binding lives, and that decides whether the
//! mutation is *contained* (visible and bounded — a body-local, or a
//! constructor initialising its own `this`) or *escaping* (a param the caller
//! shares, a captured outer binding, a real `this` field write on a method, or a
//! global).
//!
//! The walker seeds two binding sets — `params` (from the signature) and
//! `locals` (every `const`/`let`/`var` declarator seen while descending) — then
//! classifies each write by its **base ident**:
//!
//! | base `b`                                   | kind             | class | contained | hidden |
//! |--------------------------------------------|------------------|-------|-----------|--------|
//! | `this` in a constructor                    | `local.mutation` | 1     | **yes**   | no     |
//! | `this` in a normal method                  | `this.mutation`  | 3     | no        | no     |
//! | `b` ∈ body-declared locals                 | `local.mutation` | 1     | **yes**   | no     |
//! | `b` ∈ params                               | `param.mutation` | 3     | no        | no     |
//! | `globalThis` / `window` / imported binding | `global.mutation`| 6     | no        | no     |
//! | otherwise (captured outer / module-level)  | `hidden.mutation`| 3     | no        | **yes**|
//!
//! Note: only `globalThis`, `window`, and imported bindings are recognised as
//! `global.mutation`; other host globals (`document`, `navigator`, …) currently
//! fall through to `hidden.mutation` (full DOM coverage is a deferred Milestone-B item).
//!
//! The `contained` bool is returned alongside each `Effect`; Task 9's
//! boundary-containment discount is its sole consumer. Per spec Deferred #3 a
//! captured enclosing-local and a module-level binding are *both*
//! `hidden.mutation` here — we do not distinguish them in Milestone A.
//!
//! Write sites we recognise: `=` and compound assignments (`AssignExpr`),
//! `++`/`--` (`UpdateExpr`), the `delete` unary operator (`UnaryExpr`), and
//! mutating method calls (`xs.push(…)`, `m.set(…)`, …) where the receiver's
//! base is taken as written.

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use std::collections::HashSet;
use swc_ecma_ast::{
    AssignExpr, AssignTarget, BindingIdent, Callee, Expr, MemberExpr, MemberProp, ObjectPatProp,
    Pat, SimpleAssignTarget, UnaryExpr, UnaryOp, UpdateExpr, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};

use crate::functions::{FnBodyOwned, FnSig};
use crate::imports::ImportTable;
use crate::source::SpanLines;

/// Detect mutation effects in `body`, classifying each write by escape analysis.
///
/// Returns `(Effect, contained)` pairs: the bool is the boundary-containment
/// signal Task 9's discount consumes.
pub fn detect(
    body: &FnBodyOwned,
    sig: &FnSig,
    is_constructor: bool,
    lines: &SpanLines,
    imports: &ImportTable,
) -> Vec<(Effect, bool)> {
    detect_with_refs(body, sig, is_constructor, lines, imports, &HashSet::new())
}

/// Like [`detect`], but pre-seeds the walker's `ref_bindings` with `extra_refs`.
///
/// This is the inheritance path (Task 9): when an inline hook callback is
/// absorbed into its owning component, a write to `r.current` inside the
/// callback refers to a `const r = useRef(…)` declared in the *component* body,
/// not the callback's own scope. Pre-seeding the component's ref-binding names
/// lets that write still classify as `ref-cell-write` (`hidden.mutation`).
pub fn detect_with_refs(
    body: &FnBodyOwned,
    sig: &FnSig,
    is_constructor: bool,
    lines: &SpanLines,
    imports: &ImportTable,
    extra_refs: &HashSet<String>,
) -> Vec<(Effect, bool)> {
    let mut walker = MutationWalker::seed(sig, is_constructor, lines, imports);
    walker.ref_bindings.extend(extra_refs.iter().cloned());
    body.walk_with(&mut walker);
    walker.effects
}

struct MutationWalker<'a> {
    /// Idents bound by the signature's parameter patterns.
    params: HashSet<String>,
    /// Idents introduced by `const`/`let`/`var` in the body (populated while
    /// walking; a flat function-scoped set, the Milestone-A approximation).
    ///
    /// Ordering note: a write to a `var`-declared binding that appears BEFORE
    /// its declarator in source order is classified as `hidden.mutation` rather
    /// than `local.mutation` (TDZ makes this a runtime error for `let`/`const`,
    /// so practical impact is limited to `var` hoisting).
    locals: HashSet<String>,
    /// Idents bound to `useRef(...)` calls (`const r = useRef(0)` → `r`).
    /// Writes to `r.current` are `HiddenMutation` class 3 (ref-cell semantic),
    /// not the `LocalMutation` class 1 that `locals` would produce.
    ref_bindings: HashSet<String>,
    /// True when this unit is a class constructor (so `this.x = …` is local init).
    is_constructor: bool,
    lines: &'a SpanLines,
    imports: &'a ImportTable,
    effects: Vec<(Effect, bool)>,
}

impl<'a> MutationWalker<'a> {
    fn seed(
        sig: &FnSig,
        is_constructor: bool,
        lines: &'a SpanLines,
        imports: &'a ImportTable,
    ) -> Self {
        let mut params = HashSet::new();
        for pat in &sig.params {
            collect_pat_bindings(pat, &mut params);
        }
        MutationWalker {
            params,
            locals: HashSet::new(),
            ref_bindings: HashSet::new(),
            is_constructor,
            lines,
            imports,
            effects: Vec::new(),
        }
    }

    /// Classify a write to a place expression by its base ident and emit.
    ///
    /// `verb` describes the write for the evidence string (`"write to"` for an
    /// assignment, `".push on"` for a mutating-method call).
    fn record_write(&mut self, place: &Expr, line: usize, verb: &str) {
        let Some(base) = base_ident(place) else {
            return;
        };
        // A ref binding (`const r = useRef(...)`) only qualifies as a ref-cell
        // write when the place targets `.current` (e.g. `r.current = 5`).
        // A bare reassignment of the binding itself (`r = makeRef()`) is NOT a
        // ref-cell write — it is a normal local mutation and must not be dropped.
        let is_ref_cell = self.ref_bindings.contains(&base) && has_current_in_chain(place);
        let c = if is_ref_cell {
            // Ref-cell write: hidden mutation class 3, not contained.
            Classification::new(
                EffectKind::HiddenMutation,
                3,
                false,
                true,
                Tier::Heuristic,
                "ref cell",
            )
            .with_subreason("ref-cell-write")
        } else {
            // Normal classification by escape analysis (locals/params/global/captured).
            self.classify(&base)
        };
        let effect = Effect {
            kind: c.kind,
            class: c.class,
            discounted_to: None,
            weight: weight_for_class(c.class),
            line,
            tier: c.tier,
            hidden: c.hidden,
            evidence: format!("{verb} {} {base}", c.role),
            discount: None,
            subreason: c.subreason.map(String::from),
            confidence: detection_confidence(c.tier, false, false),
        };
        self.effects.push((effect, c.contained));
    }

    /// The escape-classification table: map a write's base ident to its effect
    /// kind, severity class, `contained` flag, `hidden` flag, tier, and an
    /// evidence role-word.
    ///
    /// Note: the `ref_bindings` case is intentionally NOT handled here. Ref-cell
    /// detection requires the full place expression (to check for `.current`), so
    /// it is handled in `record_write` before `classify` is called. By the time
    /// `classify` sees a ref-binding base, either it was a `.current` write (and
    /// `record_write` already emitted the ref-cell effect and returned) or it was a
    /// bare reassignment of the binding itself (e.g. `r = makeRef()`) and should
    /// fall through to normal classification here — `r` is in `locals`, so it
    /// becomes `local.mutation` class 1.
    fn classify(&self, base: &str) -> Classification {
        use EffectKind::*;
        if base == "this" {
            if self.is_constructor {
                // Constructor initialising its own `this` — local init, contained.
                Classification::new(LocalMutation, 1, true, false, Tier::Heuristic, "ctor this")
            } else {
                Classification::new(ThisMutation, 3, false, false, Tier::Heuristic, "this field")
            }
        } else if self.locals.contains(base) {
            Classification::new(LocalMutation, 1, true, false, Tier::Exact, "local")
        } else if self.params.contains(base) {
            Classification::new(ParamMutation, 3, false, false, Tier::Heuristic, "param")
        } else if base == "globalThis" || base == "window" || self.imports.resolve(base).is_some() {
            Classification::new(GlobalMutation, 6, false, false, Tier::Heuristic, "global")
        } else {
            // Captured outer/module binding — hidden from the signature.
            Classification::new(HiddenMutation, 3, false, true, Tier::Heuristic, "captured")
        }
    }
}

/// The result of escape-classifying a write's base ident.
struct Classification {
    kind: EffectKind,
    class: u8,
    contained: bool,
    hidden: bool,
    tier: Tier,
    /// Role word for the evidence string (`"local"`, `"param"`, …).
    role: &'static str,
    /// Optional sub-reason threaded into the emitted `Effect.subreason`.
    subreason: Option<&'static str>,
}

impl Classification {
    fn new(
        kind: EffectKind,
        class: u8,
        contained: bool,
        hidden: bool,
        tier: Tier,
        role: &'static str,
    ) -> Self {
        Classification {
            kind,
            class,
            contained,
            hidden,
            tier,
            role,
            subreason: None,
        }
    }

    fn with_subreason(mut self, s: &'static str) -> Self {
        self.subreason = Some(s);
        self
    }
}

impl Visit for MutationWalker<'_> {
    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        // Every `const`/`let`/`var` binding in the body is a function-scope local.
        collect_pat_bindings(&node.name, &mut self.locals);
        // Additionally, if the init is a `useRef(...)` call, record the bound
        // ident(s) as ref bindings so `.current` writes are classified correctly.
        if let Some(init) = &node.init
            && is_use_ref_call(init)
        {
            collect_pat_bindings(&node.name, &mut self.ref_bindings);
        }
        node.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, node: &AssignExpr) {
        // Both plain `=` and compound (`+=`, `-=`, …) ops are writes.
        let line = self.lines.line(node.span);
        if let Some(base) = assign_target_base(&node.left) {
            self.record_write(&base, line, "write to");
        }
        node.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, node: &UpdateExpr) {
        // `x++` / `--y` write to `node.arg`.
        let line = self.lines.line(node.span);
        self.record_write(&node.arg, line, "update");
        node.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, node: &UnaryExpr) {
        // `delete obj.key` writes to (deletes a property of) the operand's base.
        if node.op == UnaryOp::Delete {
            let line = self.lines.line(node.span);
            self.record_write(&node.arg, line, "delete on");
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        // A mutating method call (`xs.push(…)`) writes to the receiver's base.
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(MemberExpr { obj, prop, .. }) = callee.as_ref()
            && let MemberProp::Ident(method) = prop
            && is_mutating_method(&method.sym)
        {
            let line = self.lines.line(node.span);
            self.record_write(obj, line, &format!(".{} on", method.sym));
        }
        node.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
    fn visit_constructor(&mut self, _n: &swc_ecma_ast::Constructor) {}
}

/// Reconstruct the base place expression of an `AssignTarget`.
///
/// `Simple(Ident)` → the bound ident; `Simple(Member)` → recurse into the
/// member object via `base_ident`. TS-wrapper targets (`TsAs`, `TsNonNull`,
/// `TsSatisfies`, `TsTypeAssertion`) unwrap their inner expression and resolve
/// the base via `base_ident`, symmetric with the `Expr::Ts*` arms there.
/// Destructuring targets (`[a] = …`, `{a} = …`) are best-effort `None`.
fn assign_target_base(target: &AssignTarget) -> Option<Expr> {
    match target {
        AssignTarget::Simple(SimpleAssignTarget::Ident(BindingIdent { id, .. })) => {
            Some(Expr::Ident(id.clone()))
        }
        AssignTarget::Simple(SimpleAssignTarget::Member(m)) => Some(Expr::Member(m.clone())),
        AssignTarget::Simple(SimpleAssignTarget::Paren(p)) => Some((*p.expr).clone()),
        // TS-only wrappers: unwrap the inner expression, symmetric with the
        // `Expr::Ts*` arms in `base_ident`.
        AssignTarget::Simple(SimpleAssignTarget::TsAs(e)) => Some((*e.expr).clone()),
        AssignTarget::Simple(SimpleAssignTarget::TsNonNull(e)) => Some((*e.expr).clone()),
        AssignTarget::Simple(SimpleAssignTarget::TsSatisfies(e)) => Some((*e.expr).clone()),
        AssignTarget::Simple(SimpleAssignTarget::TsTypeAssertion(e)) => Some((*e.expr).clone()),
        _ => None,
    }
}

/// Resolve the base ident of a place expression.
///
/// `u.dirty` → `u` (recurse into the member object); `xs[i]` → `xs`;
/// `this.v` → `this`; `(p)` → recurse. Mirrors the Rust `base_ident`.
fn base_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.sym.to_string()),
        Expr::This(_) => Some("this".to_string()),
        Expr::Member(m) => base_ident(&m.obj),
        Expr::Paren(p) => base_ident(&p.expr),
        // See through TS-only wrappers: `(globalThis as any).z`, `x!.y`, etc.
        Expr::TsAs(e) => base_ident(&e.expr),
        Expr::TsNonNull(e) => base_ident(&e.expr),
        Expr::TsTypeAssertion(e) => base_ident(&e.expr),
        Expr::TsSatisfies(e) => base_ident(&e.expr),
        _ => None,
    }
}

/// Collect every binding ident introduced by a pattern into `out`.
///
/// Handles `Ident`, array/object destructuring, defaults (`= v`), and rest
/// (`...rest`). Best-effort for nested destructuring (the same spirit as the
/// Rust `collect_pat_bindings`).
pub(crate) fn collect_pat_bindings(pat: &Pat, out: &mut HashSet<String>) {
    match pat {
        Pat::Ident(b) => {
            out.insert(b.id.sym.to_string());
        }
        Pat::Array(a) => {
            for elem in a.elems.iter().flatten() {
                collect_pat_bindings(elem, out);
            }
        }
        Pat::Object(o) => {
            for prop in &o.props {
                match prop {
                    ObjectPatProp::KeyValue(kv) => collect_pat_bindings(&kv.value, out),
                    ObjectPatProp::Assign(a) => {
                        out.insert(a.key.id.sym.to_string());
                    }
                    ObjectPatProp::Rest(r) => collect_pat_bindings(&r.arg, out),
                }
            }
        }
        Pat::Assign(a) => collect_pat_bindings(&a.left, out),
        Pat::Rest(r) => collect_pat_bindings(&r.arg, out),
        _ => {}
    }
}

/// Mutating methods whose receiver-base we treat as written. Receiver type is
/// unknown, so this is conservative (collection / Map / Set mutators).
fn is_mutating_method(name: &str) -> bool {
    matches!(
        name,
        "push"
            | "pop"
            | "shift"
            | "unshift"
            | "splice"
            | "sort"
            | "reverse"
            | "fill"
            | "copyWithin"
            | "set"
            | "add"
            | "delete"
            | "clear"
    )
}

/// Return `true` when `expr` is a direct `useRef(...)` call (or `React.useRef(...)`).
///
/// Recognises both the bare form (`useRef(0)`) and the qualified form
/// (`React.useRef(0)`). Does not require any imports — callee is matched
/// syntactically.
pub(crate) fn is_use_ref_call(expr: &Expr) -> bool {
    let call = match expr {
        Expr::Call(c) => c,
        _ => return false,
    };
    match &call.callee {
        Callee::Expr(callee_expr) => match callee_expr.as_ref() {
            // bare `useRef(...)`
            Expr::Ident(id) => id.sym.as_ref() == "useRef",
            // qualified `React.useRef(...)`
            Expr::Member(MemberExpr { prop, .. }) => {
                matches!(prop, MemberProp::Ident(id) if id.sym.as_ref() == "useRef")
            }
            _ => false,
        },
        _ => false,
    }
}

/// Return `true` when the place expression's member chain contains `.current`
/// at any level (e.g. `r.current` or `r.current.foo`).
fn has_current_in_chain(expr: &Expr) -> bool {
    match expr {
        Expr::Member(m) => {
            if matches!(&m.prop, MemberProp::Ident(id) if id.sym.as_ref() == "current") {
                return true;
            }
            has_current_in_chain(&m.obj)
        }
        Expr::Paren(p) => has_current_in_chain(&p.expr),
        Expr::TsAs(e) => has_current_in_chain(&e.expr),
        Expr::TsNonNull(e) => has_current_in_chain(&e.expr),
        Expr::TsTypeAssertion(e) => has_current_in_chain(&e.expr),
        Expr::TsSatisfies(e) => has_current_in_chain(&e.expr),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use crate::imports::ImportTable;
    use crate::source::Lang;
    use fxrank_core::effect::EffectKind;

    /// Parse `src` as JS/TS, find the function named `fn_name`, run the mutation
    /// detector, and return the `(Effect, contained)` pairs.
    fn detect_in_fn(src: &str, fn_name: &str) -> Vec<(Effect, bool)> {
        let (module, cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        detect(&unit.body, &unit.sig, unit.is_constructor, &lines, &imports)
    }

    /// Shorthand: parse `src`, find the first function named `C`, run detection.
    fn detect_in(src: &str) -> Vec<(Effect, bool)> {
        detect_in_fn(src, "C")
    }

    #[test]
    fn useref_current_write_is_hidden_mutation() {
        // a component body: const r = useRef(0); r.current = 5;
        let effects = detect_in("function C(){ const r = useRef(0); r.current = 5; return null; }");
        let e = effects
            .iter()
            .map(|(e, _)| e)
            .find(|e| e.kind == EffectKind::HiddenMutation)
            .expect("hidden mutation");
        assert_eq!(e.effective_class(), 3);
        assert_eq!(e.subreason.as_deref(), Some("ref-cell-write"));
        // and it must NOT be classified as a contained local:
        assert!(
            effects
                .iter()
                .all(|(e, contained)| !(*contained && e.kind == EffectKind::HiddenMutation))
        );
    }

    #[test]
    fn local_write_stays_local_mutation() {
        // a plain local write should still be LocalMutation class 1
        let effects = detect_in("function C(){ let x = 0; x = 5; }");
        let e = effects
            .iter()
            .map(|(e, _)| e)
            .find(|e| e.kind == EffectKind::LocalMutation)
            .expect("local mutation");
        assert_eq!(e.effective_class(), 1);
        assert_eq!(e.subreason, None);
    }

    #[test]
    fn bare_ref_reassign_is_local_not_dropped() {
        // Fix ①: a bare reassignment of a `useRef` binding (`r = makeRef()`) must NOT
        // be dropped. `r` is a `let`-declared local, so it should produce a LocalMutation
        // class 1 effect, not be silently discarded and not be a ref-cell-write.
        let effects = detect_in("function C(){ let r = useRef(0); r = makeRef(); return null; }");
        // Must produce at least one LocalMutation (the `r = makeRef()` write).
        let local = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::LocalMutation)
            .expect("expected a LocalMutation for bare ref reassignment — was dropped (bug)");
        assert_eq!(local.0.effective_class(), 1);
        assert_eq!(local.0.subreason, None);
        // Must NOT produce a ref-cell-write for the bare reassignment.
        let ref_cell_writes: Vec<_> = effects
            .iter()
            .filter(|(e, _)| e.subreason.as_deref() == Some("ref-cell-write"))
            .collect();
        assert!(
            ref_cell_writes.is_empty(),
            "bare ref reassignment wrongly classified as ref-cell-write"
        );
    }
}
