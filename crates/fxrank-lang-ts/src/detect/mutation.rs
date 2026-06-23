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
//! | `globalThis`/`window`/imported/module binding | `global.mutation`| 6  | no     | no     |
//! | otherwise (captured enclosing-function local) | `hidden.mutation`| 3 | no    | **yes**|
//!
//! Note: `globalThis`, `window`, imported bindings, and **module top-level
//! bindings** are recognised as `global.mutation`; other host globals
//! (`document`, `navigator`, …) currently fall through to `hidden.mutation`
//! (full DOM coverage is a deferred Milestone-B item).
//!
//! The `contained` bool is returned alongside each `Effect`; the boundary
//! discount is its sole consumer. Per spec 003 Deferred #3 (issue #29) a write
//! whose base is a **module top-level binding** (`module_bindings`) is escalated
//! to `global.mutation` (class 6) — the "module var used for cross-component
//! communication" anti-pattern — while a genuinely captured enclosing-function
//! local stays `hidden.mutation` (class 3). The distinction is syntactic/
//! best-effort (the flat-scope approximation): a local/param that shadows a
//! module binding still wins as local/param **when declared before the write**
//! (the walker collects `locals` in traversal order — see the `locals` field
//! doc), since those are checked first.
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
    AssignExpr, AssignOp, AssignTarget, BindingIdent, Callee, Expr, MemberExpr, MemberProp,
    ObjectPatProp, Pat, SimpleAssignTarget, UnaryExpr, UnaryOp, UpdateExpr, VarDeclarator,
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
    module_bindings: &HashSet<String>,
) -> Vec<(Effect, bool)> {
    detect_with_refs(
        body,
        sig,
        is_constructor,
        lines,
        imports,
        module_bindings,
        &HashSet::new(),
    )
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
    module_bindings: &HashSet<String>,
    extra_refs: &HashSet<String>,
) -> Vec<(Effect, bool)> {
    let mut walker = MutationWalker::seed(sig, is_constructor, lines, imports, module_bindings);
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
    /// Names introduced by the module's top-level declarations. A captured
    /// write whose base is one of these is module-shared state, escalated to
    /// `global.mutation` (class 6) rather than the `hidden.mutation` catch-all.
    module_bindings: &'a HashSet<String>,
    effects: Vec<(Effect, bool)>,
}

impl<'a> MutationWalker<'a> {
    fn seed(
        sig: &FnSig,
        is_constructor: bool,
        lines: &'a SpanLines,
        imports: &'a ImportTable,
        module_bindings: &'a HashSet<String>,
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
            module_bindings,
            effects: Vec::new(),
        }
    }

    /// Classify a write to a place expression by its base ident and emit.
    ///
    /// `verb` describes the write for the evidence string (`"write to"` for an
    /// assignment, `".push on"` for a mutating-method call).
    ///
    /// `is_direct_init` is `true` **only** for a plain `=` `AssignExpr`
    /// (`AssignOp::Assign`). Compound assignments (`+=`, `-=`, …), update
    /// expressions (`++`/`--`), `delete`, and mutating-method receivers all pass
    /// `false`. This is the sole gate for the constructor field-init containment
    /// discount — the verb string is NOT used for this decision.
    fn record_write(&mut self, place: &Expr, line: usize, verb: &str, is_direct_init: bool) {
        let Some(base) = base_ident(place) else {
            return;
        };
        // A ref binding (`const r = useRef(...)`) only qualifies as a ref-cell
        // write when the place targets `.current` (e.g. `r.current = 5`).
        // A bare reassignment of the binding itself (`r = makeRef()`) is NOT a
        // ref-cell write — it is a normal local mutation and must not be dropped.
        //
        // Guard: if `base` is a *parameter of this function*, do NOT treat it as
        // a ref-cell write even when the name matches a component's ref binding
        // (inherited via `detect_with_refs` / `extra_refs`). A callback param that
        // shadows a component's ref name is a normal param write — param shadow
        // wins over inherited ref binding. A genuine useRef binding is always a
        // `const/let r = useRef()` declarator (a local), never a param, so this
        // guard does not affect the component's own-body path.
        let is_ref_cell = self.ref_bindings.contains(&base)
            && !self.params.contains(&base)
            && has_current_in_chain(place);
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
            self.classify(&base, place, is_direct_init)
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
    fn classify(&self, base: &str, place: &Expr, is_direct_init: bool) -> Classification {
        use EffectKind::*;
        if base == "this" {
            if self.is_constructor && is_direct_init && direct_this_field(place) {
                // A DIRECT plain-`=` field-init `this.<ident> = …` — honest, bounded
                // local init. Compound assigns, updates, delete, and method receivers
                // are NOT direct inits and escape to this.mutation.
                Classification::new(LocalMutation, 1, true, false, Tier::Heuristic, "ctor this")
            } else {
                // Normal method, OR a constructor write that is NOT a direct field-init
                // (compound `+=`, update `++`, delete, method receiver `this.xs.push`,
                // subscript `this[i]`, deeper chain `this.a.b`) — escapes, not contained.
                Classification::new(ThisMutation, 3, false, false, Tier::Heuristic, "this field")
            }
        } else if self.locals.contains(base) {
            Classification::new(LocalMutation, 1, true, false, Tier::Exact, "local")
        } else if self.params.contains(base) {
            Classification::new(ParamMutation, 3, false, false, Tier::Heuristic, "param")
        } else if base == "globalThis"
            || base == "window"
            || self.imports.resolve(base).is_some()
            || self.module_bindings.contains(base)
        {
            // A host global (`globalThis`/`window`), an imported binding, or a
            // write to a MODULE top-level binding — module-shared state used for
            // cross-component communication (issue #29). Checked AFTER
            // locals/params, so a function-scoped binding that shadows a module
            // name still wins (the flat-scope syntactic approximation).
            Classification::new(GlobalMutation, 6, false, false, Tier::Heuristic, "global")
        } else {
            // Captured enclosing-function local — hidden from the signature, but
            // NOT module-shared (not in `module_bindings`), so it stays class 3.
            Classification::new(HiddenMutation, 3, false, true, Tier::Heuristic, "captured")
                .with_subreason("captured-binding")
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
        // Only plain `=` qualifies as a direct field-init for the ctor discount.
        let line = self.lines.line(node.span);
        let is_direct_init = node.op == AssignOp::Assign;
        if let Some(base) = assign_target_base(&node.left) {
            self.record_write(&base, line, "write to", is_direct_init);
        }
        node.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, node: &UpdateExpr) {
        // `x++` / `--y` write to `node.arg` — never a direct field-init.
        let line = self.lines.line(node.span);
        self.record_write(&node.arg, line, "update", false);
        node.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, node: &UnaryExpr) {
        // `delete obj.key` writes to (deletes a property of) the operand's base
        // — never a direct field-init.
        if node.op == UnaryOp::Delete {
            let line = self.lines.line(node.span);
            self.record_write(&node.arg, line, "delete on", false);
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        // A mutating method call (`xs.push(…)`) writes to the receiver's base
        // — never a direct field-init.
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(MemberExpr { obj, prop, .. }) = callee.as_ref()
            && let MemberProp::Ident(method) = prop
            && is_mutating_method(&method.sym)
        {
            let line = self.lines.line(node.span);
            self.record_write(obj, line, &format!(".{} on", method.sym), false);
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

/// `true` iff `place` is a DIRECT named-field place off bare `this` — `this.x`
/// or `this.#x` (a private field init is still a direct field init). The only
/// constructor write shape that stays a contained `local.mutation` (F4). A
/// computed `this[i]`, a method-call receiver (`this.xs.push`), or a deeper
/// chain (`this.a.b`) all return `false` and escape to `this.mutation`.
fn direct_this_field(place: &Expr) -> bool {
    match place {
        Expr::Member(m) => {
            // A *named* field place — `this.x` (Ident) or `this.#x` (PrivateName) —
            // is a direct field-init. A computed `this[i]` (Computed) is a member-chain
            // write, not a field-init, and escapes. (Match `&m.prop` by reference.)
            if !matches!(&m.prop, MemberProp::Ident(_) | MemberProp::PrivateName(_)) {
                return false;
            }
            matches!(strip_place_wrappers(&m.obj), Expr::This(_))
        }
        Expr::Paren(p) => direct_this_field(&p.expr),
        Expr::TsAs(e) => direct_this_field(&e.expr),
        Expr::TsNonNull(e) => direct_this_field(&e.expr),
        Expr::TsTypeAssertion(e) => direct_this_field(&e.expr),
        Expr::TsSatisfies(e) => direct_this_field(&e.expr),
        _ => false,
    }
}

/// Strip `Paren` / TS-only wrappers to reach the underlying receiver, mirroring
/// what `base_ident` sees through.
fn strip_place_wrappers(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(p) => strip_place_wrappers(&p.expr),
        Expr::TsAs(e) => strip_place_wrappers(&e.expr),
        Expr::TsNonNull(e) => strip_place_wrappers(&e.expr),
        Expr::TsTypeAssertion(e) => strip_place_wrappers(&e.expr),
        Expr::TsSatisfies(e) => strip_place_wrappers(&e.expr),
        other => other,
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
/// (`React.useRef(0)`). The member form is restricted to a receiver ident
/// of exactly `React` — `foo.useRef(...)` does NOT qualify. Does not require
/// any imports — callee is matched syntactically.
pub(crate) fn is_use_ref_call(expr: &Expr) -> bool {
    let call = match expr {
        Expr::Call(c) => c,
        _ => return false,
    };
    match &call.callee {
        Callee::Expr(callee_expr) => match callee_expr.as_ref() {
            // bare `useRef(...)`
            Expr::Ident(id) => id.sym.as_ref() == "useRef",
            // qualified `React.useRef(...)` — receiver MUST be the ident `React`
            Expr::Member(MemberExpr { obj, prop, .. }) => {
                matches!(prop, MemberProp::Ident(id) if id.sym.as_ref() == "useRef")
                    && matches!(obj.as_ref(), Expr::Ident(id) if id.sym.as_ref() == "React")
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
        let module_bindings = crate::imports::module_bindings(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        detect(
            &unit.body,
            &unit.sig,
            unit.is_constructor,
            &lines,
            &imports,
            &module_bindings,
        )
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
    fn non_react_member_useref_is_not_ref_binding() {
        // Fix ②: `foo.useRef(0)` is NOT a React ref binding — only bare `useRef`
        // or `React.useRef` count. A write to `r.current` after `foo.useRef` should
        // classify as a normal local-ish write, NOT a ref-cell-write HiddenMutation.
        let effects =
            detect_in("function C(){ const r = foo.useRef(0); r.current = 1; return null; }");
        // Must NOT produce a ref-cell-write.
        let ref_cell_writes: Vec<_> = effects
            .iter()
            .filter(|(e, _)| e.subreason.as_deref() == Some("ref-cell-write"))
            .collect();
        assert!(
            ref_cell_writes.is_empty(),
            "foo.useRef wrongly recognised as a React ref binding, got {ref_cell_writes:?}"
        );
        // The `r.current = 1` write should still produce some mutation effect
        // (local, since `r` is a `const`-declared local).
        let has_mutation = effects.iter().any(|(e, _)| {
            matches!(
                e.kind,
                EffectKind::LocalMutation
                    | EffectKind::HiddenMutation
                    | EffectKind::ParamMutation
                    | EffectKind::GlobalMutation
            )
        });
        assert!(
            has_mutation,
            "expected some mutation effect for r.current = 1"
        );
    }

    #[test]
    fn react_qualified_useref_still_works() {
        // Fix ②: `React.useRef(0)` MUST still be recognised as a ref binding, and
        // a subsequent `r.current = 1` MUST produce a ref-cell-write HiddenMutation.
        let effects =
            detect_in("function C(){ const r = React.useRef(0); r.current = 1; return null; }");
        let e = effects
            .iter()
            .map(|(e, _)| e)
            .find(|e| e.subreason.as_deref() == Some("ref-cell-write"))
            .expect("React.useRef should still produce a ref-cell-write");
        assert_eq!(e.effective_class(), 3);
        assert_eq!(e.kind, EffectKind::HiddenMutation);
    }

    /// Helper: parse `src`, find the function unit whose symbol starts with
    /// `sym_prefix`, and call `detect_with_refs` on it with `extra_refs`.
    fn detect_with_refs_in_fn(
        src: &str,
        sym_prefix: &str,
        extra_refs: HashSet<String>,
    ) -> Vec<(Effect, bool)> {
        let (module, cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let unit = units
            .iter()
            .find(|u| u.symbol.starts_with(sym_prefix))
            .unwrap_or_else(|| panic!("no unit with symbol starting with {sym_prefix:?}"));
        detect_with_refs(
            &unit.body,
            &unit.sig,
            unit.is_constructor,
            &lines,
            &imports,
            &module_bindings,
            &extra_refs,
        )
    }

    #[test]
    fn callback_param_shadowing_component_ref_is_not_ref_cell_write() {
        // Bug ⑥: a callback param `r` that shadows a component's useRef binding
        // must NOT be treated as a ref-cell write. `detect_with_refs` is called
        // with extra_refs = {"r"} (simulating the component's ref binding), but the
        // callback itself declares `r` as a parameter — so `r.current = 1` inside
        // the callback body is a param write (ParamMutation class 3), NOT a
        // HiddenMutation ref-cell-write.
        let src = "const cb = (r) => { r.current = 1; };";
        let extra_refs: HashSet<String> = ["r".to_string()].into_iter().collect();
        let effects = detect_with_refs_in_fn(src, "cb", extra_refs);
        // Must NOT produce a ref-cell-write.
        let ref_cell_writes: Vec<_> = effects
            .iter()
            .filter(|(e, _)| e.subreason.as_deref() == Some("ref-cell-write"))
            .collect();
        assert!(
            ref_cell_writes.is_empty(),
            "callback param `r` shadowing component ref wrongly classified as ref-cell-write: {ref_cell_writes:?}"
        );
        // Must produce a ParamMutation instead.
        let param_mut = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::ParamMutation)
            .expect("expected ParamMutation for r.current = 1 where r is a param");
        assert_eq!(param_mut.0.effective_class(), 3);
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

    #[test]
    fn non_current_member_write_on_ref_binding_is_not_ref_cell_write() {
        // A write to a non-`.current` member of a ref binding (`r.foo = x`) must
        // NOT produce a ref-cell-write. `has_current_in_chain` returns false for
        // `r.foo`, so it falls through to normal local-mutation classification.
        let effects = detect_in("function C(){ const r = useRef(0); r.foo = 1; return null; }");
        let ref_cell_writes: Vec<_> = effects
            .iter()
            .filter(|(e, _)| e.subreason.as_deref() == Some("ref-cell-write"))
            .collect();
        assert!(
            ref_cell_writes.is_empty(),
            "non-.current member write on ref binding wrongly classified as ref-cell-write: {ref_cell_writes:?}"
        );
    }

    #[test]
    fn ctor_direct_field_init_stays_local_mutation() {
        // F4: a DIRECT field-init `this.x = 1` in a constructor stays
        // local.mutation/1/contained (MUST NOT regress).
        let effects = detect_in_fn(
            "class C { x = 0; constructor(){ this.x = 1; } }",
            "C.constructor",
        );
        let e = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::LocalMutation)
            .expect("direct this.x = 1 must be local.mutation");
        assert_eq!(e.0.effective_class(), 1);
        assert!(e.1, "direct field-init must be contained == true");
        assert!(
            !effects
                .iter()
                .any(|(e, _)| e.kind == EffectKind::ThisMutation),
            "direct field-init must not escape to this.mutation"
        );
    }

    #[test]
    fn ctor_method_call_on_this_escapes_to_this_mutation() {
        // F4: a mutating-method receiver on `this` (`this.items.push(1)`) is NOT a
        // direct field-init — escapes to this.mutation/3/not-contained.
        let effects = detect_in_fn(
            "class C { items = []; constructor(){ this.items.push(1); } }",
            "C.constructor",
        );
        let e = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::ThisMutation)
            .expect("this.items.push(1) must escape to this.mutation");
        assert_eq!(e.0.effective_class(), 3);
        assert!(
            !e.1,
            "method-call receiver on this must be contained == false"
        );
        assert!(
            !effects
                .iter()
                .any(|(e, c)| *c && e.kind == EffectKind::LocalMutation),
            "method-call receiver on this must not collapse to contained local.mutation"
        );
    }

    #[test]
    fn ctor_subscript_write_on_this_escapes_to_this_mutation() {
        // F4: a subscript write on `this` (`this[i] = 1`) is a member-chain write,
        // not a direct `this.<ident>` field-init — escapes to this.mutation/3.
        let effects = detect_in_fn(
            "class C { constructor(i: number){ this[i] = 1; } }",
            "C.constructor",
        );
        let e = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::ThisMutation)
            .expect("this[i] = 1 must escape to this.mutation");
        assert_eq!(e.0.effective_class(), 3);
        assert!(!e.1, "subscript write on this must be contained == false");
    }

    #[test]
    fn ctor_compound_assign_on_this_escapes() {
        // Copilot finding: `this.x += 1` in a constructor must escape to
        // this.mutation/3/not-contained, not stay contained as local.mutation.
        let effects = detect_in_fn(
            "class C { x = 0; constructor(){ this.x += 1; } }",
            "C.constructor",
        );
        let e = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::ThisMutation)
            .expect("this.x += 1 must escape to this.mutation/3");
        assert_eq!(e.0.effective_class(), 3);
        assert!(!e.1, "compound assign on this must be contained == false");
        assert!(
            !effects
                .iter()
                .any(|(e, c)| *c && e.kind == EffectKind::LocalMutation),
            "compound assign on this must not stay as contained local.mutation"
        );
    }

    #[test]
    fn ctor_update_on_this_escapes() {
        // Copilot finding: `this.x++` in a constructor must escape to
        // this.mutation/3/not-contained, not stay contained as local.mutation.
        let effects = detect_in_fn(
            "class C { x = 0; constructor(){ this.x++; } }",
            "C.constructor",
        );
        let e = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::ThisMutation)
            .expect("this.x++ must escape to this.mutation/3");
        assert_eq!(e.0.effective_class(), 3);
        assert!(!e.1, "update expr on this must be contained == false");
        assert!(
            !effects
                .iter()
                .any(|(e, c)| *c && e.kind == EffectKind::LocalMutation),
            "update expr on this must not stay as contained local.mutation"
        );
    }

    #[test]
    fn captured_binding_write_has_captured_subreason() {
        // F3: a captured ENCLOSING-FUNCTION local (`counter`, declared in `outer`,
        // mutated in nested `C`) is hidden.mutation/3 with subreason
        // "captured-binding". (Pre-#29 this used a module-level `counter`; that now
        // escalates to global.mutation, so the fixture nests the binding.)
        let effects = detect_in_fn(
            "function outer(){ let counter = 0; function C(){ counter += 1; } return C; }",
            "C",
        );
        let e = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::HiddenMutation)
            .expect("captured write must be hidden.mutation");
        assert_eq!(e.0.effective_class(), 3);
        assert!(e.0.hidden, "captured write stays hidden == true");
        assert!(!e.1, "captured write stays contained == false");
        assert_eq!(e.0.subreason.as_deref(), Some("captured-binding"));
        assert_ne!(e.0.subreason.as_deref(), Some("ref-cell-write"));
    }

    // ── issue #29: module-level binding mutation escalates to global.mutation ──

    #[test]
    fn module_level_let_mutation_is_global() {
        // A write to a module-level `let shared` from inside a function is a write
        // to module-shared state — global.mutation (class 6), NOT hidden.mutation.
        let effects = detect_in_fn("let shared = {}; function f(){ shared.x = 1; }", "f");
        // Exactly one global.mutation, naming the var — guards against an accidental
        // pass if some unrelated global write is ever introduced.
        let globals: Vec<_> = effects
            .iter()
            .map(|(e, _)| e)
            .filter(|e| e.kind == EffectKind::GlobalMutation && e.evidence.contains("shared"))
            .collect();
        assert_eq!(
            globals.len(),
            1,
            "expected exactly one global.mutation naming `shared`"
        );
        assert_eq!(globals[0].effective_class(), 6);
        assert!(
            effects
                .iter()
                .all(|(e, _)| e.kind != EffectKind::HiddenMutation),
            "module-level write wrongly classified as hidden.mutation"
        );
    }

    #[test]
    fn module_level_const_map_mutation_is_global() {
        // Mutating a module `const`'s contents (`m.set(...)`) registers as a write
        // on the base ident `m`, a module binding -> global.mutation.
        let effects = detect_in_fn("const m = new Map(); function f(){ m.set('k', 1); }", "f");
        let e = effects
            .iter()
            .map(|(e, _)| e)
            .find(|e| e.kind == EffectKind::GlobalMutation)
            .expect("expected a global.mutation for .set on module-level `m`");
        assert_eq!(e.effective_class(), 6);
    }

    #[test]
    fn captured_enclosing_local_stays_hidden() {
        // A captured ENCLOSING-FUNCTION local (declared in an outer function,
        // mutated in a nested function) is NOT module-level — it must stay
        // hidden.mutation (class 3). Guards that we only escalated module bindings.
        // From `inner`'s perspective `acc` is a captured outer binding (not its
        // param/own-local, not a module binding).
        let effects = detect_in_fn(
            "function outer(){ let acc = {}; function inner(){ acc.x = 1; } return inner; }",
            "inner",
        );
        let e = effects
            .iter()
            .map(|(e, _)| e)
            .find(|e| e.kind == EffectKind::HiddenMutation)
            .expect("expected hidden.mutation for captured enclosing-function local `acc`");
        assert_eq!(e.effective_class(), 3);
        assert_eq!(e.subreason.as_deref(), Some("captured-binding"));
        assert!(
            effects
                .iter()
                .all(|(e, _)| e.kind != EffectKind::GlobalMutation),
            "captured enclosing-function local wrongly escalated to global.mutation"
        );
    }

    #[test]
    fn function_local_shadowing_module_binding_is_local() {
        // A module `let shared` AND a function that declares its OWN `let shared`
        // then writes it -> local.mutation (class 1). The shadow wins because
        // locals are checked before the global arm (the flat-scope approximation).
        let effects = detect_in_fn(
            "let shared = {}; function f(){ let shared = {}; shared.x = 1; }",
            "f",
        );
        let e = effects
            .iter()
            .map(|(e, _)| e)
            .find(|e| e.kind == EffectKind::LocalMutation)
            .expect(
                "expected local.mutation — function-scoped `shared` shadows the module binding",
            );
        assert_eq!(e.effective_class(), 1);
        assert!(
            effects
                .iter()
                .all(|(e, _)| e.kind != EffectKind::GlobalMutation),
            "shadowing local wrongly escalated to global.mutation"
        );
    }
}
