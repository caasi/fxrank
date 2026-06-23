//! Mutation detection + the containment discount — FxRank's flagship detector.
//!
//! The thesis: a *declared* `&mut` mutation is visible and bounded at the call
//! site, so it is **discounted** (the caller already sees the channel). A
//! `&self` method that mutates through interior mutability (`RefCell`, `Cell`,
//! atomics, `Mutex`) is **hidden** from the signature, so it scores *higher* —
//! the anti-Goodhart inversion this whole project argues for.
//!
//! We cannot see types here, so write-through detection is heuristic. The walker
//! seeds its binding sets from the signature (which params are `&mut`, whether
//! the receiver is `&mut self` / `&self`, which `&T` params are shared) and then
//! walks the body classifying each write site by the *base ident* of its place
//! expression:
//!
//! - base in `mut_params` → `param.mutation`, `Discount::MutParam` (down 2).
//! - base is `self` and `&mut self` → `param.mutation`, `Discount::MutSelf` (down 1).
//! - an interior-mutability mutator (`borrow_mut`, `set`, …) on a `shared_refs`
//!   base → `hidden.mutation` (class 3, hidden, no discount).
//! - base in `let_mut` → `local.mutation` (class 1, exact).
//! - base is a file-level `static` item (not a local/param) → `global.mutation`
//!   (class 6, heuristic — written by assignment, a mutating method, or an
//!   interior-mutability mutator like `.store()` on an atomic static).
//!
//! The discount is *cancelled* when the write sits inside an `unsafe` block (or
//! an `unsafe fn`): an `&mut` reborrow under `unsafe` may alias, so the channel
//! is no longer trustworthy.

use crate::imports::ImportTable;
use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::{Discount, apply_discount, weight_for_class};
use std::collections::HashSet;
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Detect mutation effects in `block`, seeding binding sets from `sig`.
///
/// `statics` is the file-level set of real `static` item names; `imports` is the
/// `use`-table. Both feed `record_write`'s base-resolution cascade.
pub fn detect<'a>(
    block: &syn::Block,
    sig: &syn::Signature,
    statics: &'a HashSet<String>,
    imports: &'a ImportTable,
) -> Vec<Effect> {
    let mut walker = MutationWalker::seed(sig, statics, imports);
    walker.visit_block(block);
    walker.effects
}

struct MutationWalker<'a> {
    /// Idents of `&mut T` typed params.
    mut_params: HashSet<String>,
    /// True when the receiver is `&mut self`.
    mut_self: bool,
    /// Idents of shared `&T` params, plus `self` when the receiver is `&self`.
    shared_refs: HashSet<String>,
    /// Idents introduced by `let mut x` (function-scope set; populated while walking).
    let_mut: HashSet<String>,
    /// Every binding we know is local/param (so a non-member is a global candidate).
    locals: HashSet<String>,
    /// Nesting depth of enclosing `unsafe { }` blocks.
    unsafe_depth: usize,
    /// True for the whole body when the fn itself is `unsafe`.
    unsafe_fn: bool,
    /// File-level real `static` item names (`static`/`static mut`/atomics/…).
    statics: &'a HashSet<String>,
    /// The `use`-table, for resolving a write base through an import.
    imports: &'a ImportTable,
    effects: Vec<Effect>,
}

impl<'a> MutationWalker<'a> {
    fn seed(sig: &syn::Signature, statics: &'a HashSet<String>, imports: &'a ImportTable) -> Self {
        let mut w = MutationWalker {
            mut_params: HashSet::new(),
            mut_self: false,
            shared_refs: HashSet::new(),
            let_mut: HashSet::new(),
            locals: HashSet::new(),
            unsafe_depth: 0,
            unsafe_fn: sig.unsafety.is_some(),
            statics,
            imports,
            effects: Vec::new(),
        };
        for input in &sig.inputs {
            match input {
                syn::FnArg::Receiver(recv) => {
                    // `&self` / `&mut self` both have `reference = Some`.
                    if recv.reference.is_some() {
                        if recv.mutability.is_some() {
                            w.mut_self = true;
                        } else {
                            w.shared_refs.insert("self".to_string());
                        }
                    }
                    w.locals.insert("self".to_string());
                }
                syn::FnArg::Typed(pat_type) => {
                    let mut bindings = Vec::new();
                    collect_pat_bindings(&pat_type.pat, &mut bindings);
                    for (name, _is_mut) in bindings {
                        w.locals.insert(name.clone());
                        match &*pat_type.ty {
                            syn::Type::Reference(r) if r.mutability.is_some() => {
                                w.mut_params.insert(name);
                            }
                            syn::Type::Reference(_) => {
                                w.shared_refs.insert(name);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        w
    }

    /// True when this write site sits inside an unsafe context.
    fn unsafe_enclosed(&self) -> bool {
        self.unsafe_depth > 0 || self.unsafe_fn
    }

    /// Emit a `param.mutation` for a write whose base is a `&mut` channel.
    fn push_param_mutation(&mut self, discount: Discount, line: usize, evidence: String) {
        let unsafe_enclosed = self.unsafe_enclosed();
        let discounted = apply_discount(3, discount, unsafe_enclosed);
        let reason = match discount {
            Discount::MutSelf => "&mut self",
            _ => "explicit &mut param, caller-visible",
        };
        self.effects.push(Effect {
            kind: EffectKind::ParamMutation,
            class: 3,
            discounted_to: Some(discounted),
            weight: weight_for_class(discounted),
            line,
            // The binding is exact, but the write-through is a heuristic.
            tier: Tier::Heuristic,
            hidden: false,
            evidence,
            discount: Some(reason.to_string()),
            subreason: None,
            confidence: detection_confidence(Tier::Heuristic, false, false),
        });
    }

    /// Emit a plain class-N mutation effect (hidden/local/global): no discount.
    fn push_plain(
        &mut self,
        kind: EffectKind,
        tier: Tier,
        hidden: bool,
        line: usize,
        evidence: String,
        subreason: Option<&str>,
    ) {
        let class = kind.base_class();
        self.effects.push(Effect {
            kind,
            class,
            discounted_to: None,
            weight: weight_for_class(class),
            line,
            tier,
            hidden,
            evidence,
            discount: None,
            subreason: subreason.map(str::to_string),
            confidence: detection_confidence(tier, false, false),
        });
    }

    /// Classify a write to a place expression by its base ident.
    fn record_write(&mut self, place: &syn::Expr, line: usize) {
        let Some(base) = base_ident(place) else {
            return;
        };
        if self.mut_params.contains(&base) {
            self.push_param_mutation(Discount::MutParam, line, format!("write to &mut {base}"));
        } else if base == "self" && self.mut_self {
            self.push_param_mutation(Discount::MutSelf, line, "write to &mut self".to_string());
        } else if self.let_mut.contains(&base) {
            self.push_plain(
                EffectKind::LocalMutation,
                Tier::Exact,
                false,
                line,
                format!("write to local {base}"),
                None,
            );
        } else if !self.locals.contains(&base) && self.statics.contains(&base) {
            // F2: base is bound in no local/param/let-mut set but IS a file-level
            // `static` — a real static write (direct/compound assignment, or a
            // mutating method like `STATIC_VEC.push`). Class-4 module-private
            // downgrade DEFERRED per spec — always class 6.
            self.push_plain(
                EffectKind::GlobalMutation,
                Tier::Heuristic,
                false,
                line,
                format!("write to global {base}"),
                None,
            );
        } else if !self.locals.contains(&base)
            && base != "self"
            && self.imports.resolve(&base).is_some()
        {
            // 008-F5: the base resolves through the `use`-table — module-external
            // ambient state → global.mutation. Near-vacuous for Rust; implemented
            // for symmetry with the TS/Python frontends.
            //
            // Guard: `base != "self"` prevents a misattributed nested-receiver write
            // (e.g. `self.0 += 1` inside a nested `impl` method) from falling through
            // here when "self" is in the ImportTable (e.g. via `use m::{self, …}`).
            // The real `&mut self` / `&self`-interior cases are caught above.
            self.push_plain(
                EffectKind::GlobalMutation,
                Tier::Heuristic,
                false,
                line,
                format!("write to imported {base}"),
                None,
            );
        } else if !self.locals.contains(&base) && base != "self" {
            // 008-F1: the base resolves to no local/param/self/static binding —
            // a write to a captured/unresolved outer binding, hidden from this
            // signature → hidden.mutation (class 3, hidden). TS parity.
            //
            // Guard: `base != "self"` drops misattributed nested-receiver writes
            // that reach here (same root cause as F5 guard above).
            self.push_plain(
                EffectKind::HiddenMutation,
                Tier::Heuristic,
                true,
                line,
                format!("write to captured binding {base}"),
                Some("captured-binding"),
            );
        }
    }
}

/// Collect `(ident, is_mut)` for every binding introduced by a pattern,
/// recursing through tuple / struct / tuple-struct / slice / reference /
/// or / paren / type patterns.
fn collect_pat_bindings(pat: &syn::Pat, out: &mut Vec<(String, bool)>) {
    match pat {
        syn::Pat::Ident(pi) => {
            out.push((pi.ident.to_string(), pi.mutability.is_some()));
            if let Some((_, sub)) = &pi.subpat {
                collect_pat_bindings(sub, out);
            }
        }
        syn::Pat::Tuple(t) => {
            for p in &t.elems {
                collect_pat_bindings(p, out);
            }
        }
        syn::Pat::TupleStruct(t) => {
            for p in &t.elems {
                collect_pat_bindings(p, out);
            }
        }
        syn::Pat::Struct(s) => {
            for f in &s.fields {
                collect_pat_bindings(&f.pat, out);
            }
        }
        syn::Pat::Slice(s) => {
            for p in &s.elems {
                collect_pat_bindings(p, out);
            }
        }
        syn::Pat::Reference(r) => collect_pat_bindings(&r.pat, out),
        syn::Pat::Or(o) => {
            for p in &o.cases {
                collect_pat_bindings(p, out);
            }
        }
        syn::Pat::Paren(p) => collect_pat_bindings(&p.pat, out),
        syn::Pat::Type(t) => collect_pat_bindings(&t.pat, out),
        _ => {}
    }
}

/// Mutating methods whose receiver-base we treat as a write target.
/// Conservative collection-mutation set; receiver type is unknown, so heuristic.
fn is_mutating_method(name: &str) -> bool {
    matches!(
        name,
        "push" | "insert" | "clear" | "extend" | "remove" | "pop" | "append" | "truncate"
    )
}

impl<'a, 'ast> Visit<'ast> for MutationWalker<'a> {
    fn visit_local(&mut self, node: &'ast syn::Local) {
        // Track all bindings introduced by the pattern, including destructuring.
        let mut bindings = Vec::new();
        collect_pat_bindings(&node.pat, &mut bindings);
        for (name, is_mut) in bindings {
            self.locals.insert(name.clone());
            if is_mut {
                self.let_mut.insert(name);
            }
        }
        syn::visit::visit_local(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.unsafe_depth += 1;
        syn::visit::visit_expr_unsafe(self, node);
        self.unsafe_depth -= 1;
    }

    fn visit_expr_assign(&mut self, node: &'ast syn::ExprAssign) {
        let line = node.span().start().line;
        self.record_write(&node.left, line);
        syn::visit::visit_expr_assign(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        // Compound assignment (`+=`, `-=`, …) is a Binary node, not Assign.
        if is_assign_op(&node.op) {
            let line = node.span().start().line;
            self.record_write(&node.left, line);
        }
        syn::visit::visit_expr_binary(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        let line = node.span().start().line;
        if is_interior_mutator(&method) {
            let base = base_ident(&node.receiver);
            if base
                .as_deref()
                .is_some_and(|b| self.shared_refs.contains(b))
            {
                // Hidden mutation through a shared `&` base (`&T` param, or `self`
                // when the receiver is `&self`). Checked FIRST so the anti-Goodhart
                // `&self` interior-mut case stays `hidden`.
                let base = base.expect("checked Some above");
                self.push_plain(
                    EffectKind::HiddenMutation,
                    Tier::Heuristic,
                    true,
                    line,
                    format!(".{method} on shared &{base}"),
                    Some("interior-mut"),
                );
            } else if base.as_deref().is_some_and(|b| self.statics.contains(b)) {
                // F2: the receiver base is a file-level static written via an
                // interior-mutability mutator (`.store()`/`.swap()`/`.fetch_*` on an
                // atomic, `.set()` on a Cell/OnceLock, `.borrow_mut()` on a RefCell)
                // → global.mutation, class 6. (Mutex/RwLock `.lock()` is NOT in
                // is_interior_mutator and is not caught — see the task note.)
                let base = base.expect("checked Some above");
                self.push_plain(
                    EffectKind::GlobalMutation,
                    Tier::Heuristic,
                    false,
                    line,
                    format!("interior write to global {base} via .{method}"),
                    None,
                );
            }
        } else if is_mutating_method(&method) {
            self.record_write(&node.receiver, line);
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Interior-mutability mutators: a write hidden behind a shared `&` reference
/// (`RefCell`, `Cell`, atomics, `Mutex`). Receiver type is unknown → heuristic.
fn is_interior_mutator(name: &str) -> bool {
    matches!(name, "borrow_mut" | "set" | "replace" | "store" | "swap")
        || name.starts_with("fetch_")
}

/// True for compound-assignment operators (`+=`, `-=`, `*=`, …).
fn is_assign_op(op: &syn::BinOp) -> bool {
    use syn::BinOp::*;
    matches!(
        op,
        AddAssign(_)
            | SubAssign(_)
            | MulAssign(_)
            | DivAssign(_)
            | RemAssign(_)
            | BitXorAssign(_)
            | BitAndAssign(_)
            | BitOrAssign(_)
            | ShlAssign(_)
            | ShrAssign(_)
    )
}

/// Resolve the base ident of a place expression.
///
/// `u.dirty` → `u` (recurse into `Field.base`); `*self.x` → `self` (unwrap the
/// deref); `b` → `b` (a single-segment path); index/method receivers recurse.
fn base_ident(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Path(p) if p.path.segments.len() == 1 => {
            Some(p.path.segments[0].ident.to_string())
        }
        syn::Expr::Field(f) => base_ident(&f.base),
        syn::Expr::Index(i) => base_ident(&i.expr),
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => base_ident(&u.expr),
        syn::Expr::Reference(r) => base_ident(&r.expr),
        syn::Expr::Paren(p) => base_ident(&p.expr),
        syn::Expr::MethodCall(m) => base_ident(&m.receiver),
        _ => None,
    }
}
