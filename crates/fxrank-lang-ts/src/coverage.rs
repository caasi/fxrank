//! Signature type-coverage analysis — the project thesis ("types lower the
//! score") made operational. We measure how much of a function's signature is
//! *explicitly typed* (its boundary), so Task 9's boundary-containment discount
//! can shift **contained** effects down when the boundary is honest, and void
//! the discount entirely when `any` poisons it.
//!
//! A "slot" is one declared boundary position: each parameter, plus the return
//! position for non-constructors (constructors have no return slot). A slot is
//! **typed** when its top-level annotation is `Some` and is NOT the `any`
//! keyword. `has_any` is set when ANY slot's top-level annotation IS `any`, or
//! the body contains an `as any` assertion or a `: any` local annotation — any
//! of those re-opens the boundary the types were supposed to seal, so the gate
//! is voided (`tier == None`).
//!
//! **Top-level only.** `is_any` is a shallow check: a parameter typed
//! `Array<any>` or `{ x: any }` counts as *typed* (its top-level annotation is
//! not the bare `any` keyword). Matching the spec's Milestone-A scope, we do not
//! descend into generic args or member types.
//!
//! **Deferred (noted limitation).** Two escape hatches are NOT detected:
//! `as unknown as T` double-assertions (the inner `as unknown` is not `any`, and
//! we do not pattern-match the double form), and `@ts-ignore` /
//! `@ts-expect-error` comment directives (swc keeps comments in a side table we
//! do not thread through). Both can defeat the type boundary without tripping
//! `has_any`; recovering them is a Milestone-B candidate.

use fxrank_core::score::BoundaryCoverage;
use swc_ecma_ast::{TsAsExpr, TsKeywordType, TsKeywordTypeKind, TsType, TsTypeAnn, VarDeclarator};
use swc_ecma_visit::{Visit, VisitWith};

use crate::functions::{FnBodyOwned, FnSig};

/// Signature type-coverage for one function unit.
pub struct Coverage {
    /// The boundary tier fed to `apply_boundary_discount`. Voided to `None`
    /// whenever `has_any` is true.
    pub tier: BoundaryCoverage,
    /// Whether `any` appears in the signature OR the body (`as any` / `: any`).
    pub has_any: bool,
    /// Count of slots whose top-level annotation is present and not `any`.
    pub typed_slots: usize,
    /// Total boundary slots: params + (1 return slot unless constructor).
    pub total_slots: usize,
}

/// Is `ty` the bare `any` keyword (top-level check only)?
fn is_any(ty: &TsType) -> bool {
    matches!(
        ty,
        TsType::TsKeywordType(TsKeywordType {
            kind: TsKeywordTypeKind::TsAnyKeyword,
            ..
        })
    )
}

/// Classify a single optional annotation: `(is_typed, is_any)`.
///
/// `is_typed` means present and not `any`; `is_any` means present and `any`.
fn classify_ann(ann: &Option<TsTypeAnn>) -> (bool, bool) {
    match ann {
        None => (false, false),
        Some(a) if is_any(&a.type_ann) => (false, true),
        Some(_) => (true, false),
    }
}

/// Pull the top-level annotation off a parameter pattern.
///
/// A typed `Pat::Ident` carries `.type_ann`; the destructuring/default/rest
/// variants carry it on the pat (or its inner binding). We read each variant's
/// own top-level `type_ann`; for `Pat::Assign` / `Pat::Rest` we fall through to
/// the wrapped pattern so `(x: T = …)` and `(...xs: T[])` still count as typed.
fn param_type_ann(pat: &swc_ecma_ast::Pat) -> Option<&TsTypeAnn> {
    use swc_ecma_ast::Pat;
    match pat {
        Pat::Ident(b) => b.type_ann.as_deref(),
        Pat::Array(a) => a.type_ann.as_deref(),
        Pat::Object(o) => o.type_ann.as_deref(),
        Pat::Rest(r) => r.type_ann.as_deref().or_else(|| param_type_ann(&r.arg)),
        Pat::Assign(a) => param_type_ann(&a.left),
        _ => None,
    }
}

/// A small visitor flagging body-level `any` escape hatches: `expr as any`
/// (`TsAsExpr`) and a `: any` local annotation (`VarDeclarator` binding).
struct AnyBodyWalker {
    found: bool,
}

impl Visit for AnyBodyWalker {
    fn visit_ts_as_expr(&mut self, node: &TsAsExpr) {
        if is_any(&node.type_ann) {
            self.found = true;
        }
        node.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if let swc_ecma_ast::Pat::Ident(b) = &node.name
            && let Some(ann) = b.type_ann.as_deref()
            && is_any(&ann.type_ann)
        {
            self.found = true;
        }
        node.visit_children_with(self);
    }
}

fn body_has_any(body: &FnBodyOwned) -> bool {
    let mut walker = AnyBodyWalker { found: false };
    body.walk_with(&mut walker);
    walker.found
}

/// Compute the signature type-coverage of a function unit.
pub fn analyze(sig: &FnSig, is_constructor: bool, body: &FnBodyOwned) -> Coverage {
    let return_slots = if is_constructor { 0 } else { 1 };
    let total_slots = sig.params.len() + return_slots;

    let mut typed_slots = 0usize;
    let mut has_any = false;

    for pat in &sig.params {
        let (typed, anyish) = classify_ann(&param_type_ann(pat).cloned());
        if typed {
            typed_slots += 1;
        }
        if anyish {
            has_any = true;
        }
    }

    if !is_constructor {
        let (typed, anyish) = classify_ann(&sig.return_type);
        if typed {
            typed_slots += 1;
        }
        if anyish {
            has_any = true;
        }
    }

    if body_has_any(body) {
        has_any = true;
    }

    let tier = if has_any {
        // The gate is voided: an `any` anywhere re-opens the boundary.
        BoundaryCoverage::None
    } else if total_slots > 0 && typed_slots == total_slots {
        BoundaryCoverage::Full
    } else if typed_slots > 0 {
        BoundaryCoverage::Partial
    } else {
        BoundaryCoverage::None
    };

    Coverage {
        tier,
        has_any,
        typed_slots,
        total_slots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use crate::source::Lang;

    fn cov(src: &str, fn_name: &str) -> Coverage {
        let units = functions::parse_and_collect(src, "cov.ts", Lang::Ts).expect("parse");
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        analyze(&unit.sig, unit.is_constructor, &unit.body)
    }

    const FIXTURE: &str = r#"
function fullyTyped(xs: number[]): number[] { const a: number[] = []; a.push(1); return a; }
function partlyTyped(xs: number[]) { const a: number[] = []; a.push(1); return a; }
function untyped(xs) { const a = []; a.push(1); return a; }
function poisoned(xs: number[]): number[] { const a = xs as any; a.push(1); return a; }
"#;

    #[test]
    fn fully_typed_is_full_no_any() {
        let c = cov(FIXTURE, "fullyTyped");
        assert_eq!(c.tier, BoundaryCoverage::Full);
        assert!(!c.has_any);
        assert_eq!(c.typed_slots, 2);
        assert_eq!(c.total_slots, 2);
    }

    #[test]
    fn partly_typed_is_partial_no_any() {
        let c = cov(FIXTURE, "partlyTyped");
        assert_eq!(c.tier, BoundaryCoverage::Partial);
        assert!(!c.has_any);
        assert_eq!(c.typed_slots, 1); // param typed, return untyped
        assert_eq!(c.total_slots, 2);
    }

    #[test]
    fn untyped_is_none_no_any() {
        let c = cov(FIXTURE, "untyped");
        assert_eq!(c.tier, BoundaryCoverage::None);
        assert!(!c.has_any);
        assert_eq!(c.typed_slots, 0);
        assert_eq!(c.total_slots, 2);
    }

    #[test]
    fn poisoned_sets_has_any_and_voids_tier() {
        let c = cov(FIXTURE, "poisoned");
        assert!(c.has_any, "as any in body must set has_any");
        assert_eq!(c.tier, BoundaryCoverage::None, "any voids the gate");
    }

    #[test]
    fn any_in_signature_sets_has_any() {
        let src = "function f(x: any): number { return 1; }";
        let c = cov(src, "f");
        assert!(c.has_any);
        assert_eq!(c.tier, BoundaryCoverage::None);
    }

    #[test]
    fn local_any_annotation_sets_has_any() {
        let src = "function f(x: number): number { const y: any = x; return y; }";
        let c = cov(src, "f");
        assert!(c.has_any, ": any local annotation must set has_any");
    }
}
