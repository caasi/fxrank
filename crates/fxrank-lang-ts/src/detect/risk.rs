//! Risk-feature detection for the TypeScript frontend.
//!
//! [`detect`] walks a function body for dangerous JS/TS patterns and emits a
//! [`RiskFeature`] per signal. This is the swc analog of
//! `fxrank-lang-rust`'s `detect/risk.rs`.
//!
//! # Dedup split — `any`-family is NOT re-detected here
//!
//! The `any`-family `type.escape` (`any`, `as any`, `: any`) is OWNED by the
//! coverage module (`detect::analyze_unit` emits one `type.escape` when
//! `cov.has_any`). This module detects `type.escape` ONLY for the non-null
//! assertion `x!` (`TsNonNullExpr`). Re-detecting `as any` here would
//! double-count; the dedup-split test guards against that.
//!
//! # Signals detected
//!
//! | signal                         | `RiskKind`        | tier      |
//! |-------------------------------|-------------------|-----------|
//! | `eval(...)`                    | `DynamicCode`     | exact     |
//! | `new Function(...)`            | `DynamicCode`     | exact     |
//! | `with (...) {}`               | `DynamicCode`     | exact     |
//! | `Object.setPrototypeOf(...)`   | `ProtoPollution`  | path      |
//! | `__proto__` assignment         | `ProtoPollution`  | heuristic |
//! | `.innerHTML =` / `.outerHTML=` | `HtmlInjection`   | heuristic |
//! | `.insertAdjacentHTML(...)`     | `HtmlInjection`   | heuristic |
//! | `document.write(...)`          | `HtmlInjection`   | path      |
//! | `x!` (non-null assertion)      | `TypeEscape`      | exact     |
//!
//! # Member-rendering note
//!
//! `render_expr` / `render_member` are small helpers copied from
//! `detect/calls.rs`. Factoring them into a shared module would be cleaner,
//! but a local copy is acceptable for Milestone A (noted here for the
//! Milestone-B cleanup list).

use fxrank_core::effect::{RiskFeature, RiskKind, Tier};
use fxrank_core::score::weight_for_class;
use swc_ecma_ast::{
    AssignExpr, AssignTarget, Callee, Expr, MemberExpr, MemberProp, NewExpr, SimpleAssignTarget,
    TsNonNullExpr, WithStmt,
};
use swc_ecma_visit::{Visit, VisitWith};

use crate::functions::FnBodyOwned;
use crate::source::SpanLines;

/// Detect risk features in a function body.
///
/// Returns a `Vec<RiskFeature>` for every dangerous pattern found. The
/// `any`-family type escape (`as any` / `: any`) is intentionally NOT
/// detected here — that is owned by the coverage gate in `analyze_unit`.
pub fn detect(body: &FnBodyOwned, path: &str, lines: &SpanLines) -> Vec<RiskFeature> {
    let mut walker = RiskWalker {
        path: path.to_string(),
        lines,
        features: Vec::new(),
    };
    body.walk_with(&mut walker);
    walker.features
}

struct RiskWalker<'a> {
    path: String,
    lines: &'a SpanLines,
    features: Vec<RiskFeature>,
}

impl RiskWalker<'_> {
    fn push(&mut self, kind: RiskKind, tier: Tier, line: usize, evidence: String) {
        let class = kind.class();
        self.features.push(RiskFeature {
            kind,
            class,
            weight: weight_for_class(class),
            path: self.path.clone(),
            line,
            evidence,
            tier,
        });
    }
}

impl Visit for RiskWalker<'_> {
    // ── Call expressions: eval(...), Object.setPrototypeOf(...),
    //    .insertAdjacentHTML(...), document.write(...) ──────────────────────────

    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Some(rendered) = render_expr(callee)
        {
            let line = self.lines.line(node.span);
            match rendered.as_str() {
                // eval(...) — dynamic code execution, exact.
                "eval" => {
                    self.push(RiskKind::DynamicCode, Tier::Exact, line, "eval(…)".into());
                }
                // Object.setPrototypeOf(...) — prototype pollution, path tier.
                "Object.setPrototypeOf" => {
                    self.push(
                        RiskKind::ProtoPollution,
                        Tier::Path,
                        line,
                        "Object.setPrototypeOf(…)".into(),
                    );
                }
                // document.write(...) — HTML injection via the document global, path tier.
                "document.write" => {
                    self.push(
                        RiskKind::HtmlInjection,
                        Tier::Path,
                        line,
                        "document.write(…)".into(),
                    );
                }
                other => {
                    // .insertAdjacentHTML(...) — HTML injection, heuristic (receiver unknown).
                    if let Some((_, method)) = other.rsplit_once('.')
                        && method == "insertAdjacentHTML"
                    {
                        self.push(
                            RiskKind::HtmlInjection,
                            Tier::Heuristic,
                            line,
                            format!("{other}(…)"),
                        );
                    }
                }
            }
        }
        node.visit_children_with(self);
    }

    // ── new Function(...) — dynamic code execution ────────────────────────────

    fn visit_new_expr(&mut self, node: &NewExpr) {
        if let Some(name) = render_expr(&node.callee)
            && name == "Function"
        {
            let line = self.lines.line(node.span);
            self.push(
                RiskKind::DynamicCode,
                Tier::Exact,
                line,
                "new Function(…)".into(),
            );
        }
        node.visit_children_with(self);
    }

    // ── with (...) {} — dynamic code execution ────────────────────────────────

    fn visit_with_stmt(&mut self, node: &WithStmt) {
        let line = self.lines.line(node.span);
        self.push(
            RiskKind::DynamicCode,
            Tier::Exact,
            line,
            "with (…) {}".into(),
        );
        node.visit_children_with(self);
    }

    // ── Assignment expressions: __proto__ (ProtoPollution),
    //    .innerHTML / .outerHTML (HtmlInjection) ──────────────────────────────

    fn visit_assign_expr(&mut self, node: &AssignExpr) {
        if let Some(prop_name) = assign_target_prop_name(&node.left) {
            let line = self.lines.line(node.span);
            match prop_name.as_str() {
                "__proto__" => {
                    self.push(
                        RiskKind::ProtoPollution,
                        Tier::Heuristic,
                        line,
                        ".__proto__ =".into(),
                    );
                }
                "innerHTML" | "outerHTML" => {
                    self.push(
                        RiskKind::HtmlInjection,
                        Tier::Heuristic,
                        line,
                        format!(".{prop_name} ="),
                    );
                }
                _ => {}
            }
        }
        node.visit_children_with(self);
    }

    // ── x! — non-null assertion (TypeEscape) ─────────────────────────────────
    //
    // Only `TsNonNullExpr` is detected here. The `any`-family (`as any`, `: any`)
    // is owned by the coverage gate and must NOT be re-detected here.

    fn visit_ts_non_null_expr(&mut self, node: &TsNonNullExpr) {
        let line = self.lines.line(node.span);
        self.push(
            RiskKind::TypeEscape,
            Tier::Exact,
            line,
            "x! (non-null assertion)".into(),
        );
        node.visit_children_with(self);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────
//
// Copies of `render_expr`/`render_member` from `detect/calls.rs`. A shared
// helper module would be cleaner but a local copy is acceptable for Milestone A
// (tracked for Milestone-B cleanup).

/// Render a (possibly nested) callee/expression to a dotted string.
/// Returns `None` for computed access, calls-of-calls, `this`, etc.
fn render_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.sym.to_string()),
        Expr::Member(m) => render_member(m),
        _ => None,
    }
}

/// Render a `MemberExpr` chain to `a.b.c`. Computed / private props yield `None`.
fn render_member(m: &MemberExpr) -> Option<String> {
    let obj = render_expr(&m.obj)?;
    match &m.prop {
        MemberProp::Ident(name) => Some(format!("{obj}.{}", name.sym)),
        _ => None,
    }
}

/// Extract the property name of an `AssignTarget` that is a member expression
/// with an ident property, i.e. the `prop` in `target.prop = …`.
///
/// Returns `None` for ident targets, computed props, destructuring, etc.
fn assign_target_prop_name(target: &AssignTarget) -> Option<String> {
    match target {
        AssignTarget::Simple(SimpleAssignTarget::Member(MemberExpr { prop, .. })) => {
            if let MemberProp::Ident(id) = prop {
                Some(id.sym.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}
