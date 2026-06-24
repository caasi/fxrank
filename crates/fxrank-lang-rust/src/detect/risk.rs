//! Risk-feature detection for fxrank.
//!
//! Two entry points:
//!
//! - [`detect_fn_risks`] walks a function body (and checks its signature) for
//!   per-function risk signals (unsafe blocks, transmute, asm!, …).  Results
//!   attach to the function's `Hotspot` and feed `risk_class` / `risk_weight`.
//!
//! - [`detect_module_risks`] walks the top-level items of a file for module-
//!   level risks that belong to the file, not to any single function:
//!   `impl Drop for T` and `extern { }` blocks.  Results go into
//!   `FrontendOutput::module_risks`.
//!
//! # Raw-pointer-deref approximation
//!
//! We cannot know whether a `*expr` dereference (`Expr::Unary(UnOp::Deref)`)
//! involves a raw pointer without type information.  However, raw-pointer
//! dereference is only valid *inside* an `unsafe` block (or `unsafe fn`), so
//! we classify any deref that occurs inside an unsafe context as `RawPtrDeref`.
//! This may include safe deref of `Box<T>` or `&T` under unsafe, producing
//! false positives, but it is conservative in the opposite direction (it never
//! misses a true raw deref) and `Tier::Heuristic` signals the uncertainty.

use crate::functions::is_cfg_test;
use fxrank_core::effect::{RiskFeature, RiskKind, Tier};
use fxrank_core::score::weight_for_class;
use syn::spanned::Spanned;
use syn::visit::Visit;

// ── Function-body risk detection ─────────────────────────────────────────────

/// Detect risk features in a function body and signature.
///
/// `path` is the source-file path (the file the risk lives in, as passed
/// into the frontend); it is stored in every emitted `RiskFeature`.
pub fn detect_fn_risks(block: &syn::Block, sig: &syn::Signature, path: &str) -> Vec<RiskFeature> {
    let mut walker = RiskWalker {
        path: path.to_string(),
        unsafe_depth: 0,
        fn_is_unsafe: sig.unsafety.is_some(),
        features: Vec::new(),
    };

    // `unsafe fn` → UnsafeFn (class 5, Exact).
    if sig.unsafety.is_some() {
        let loc = sig.span().start();
        let line = loc.line;
        let col = loc.column + 1;
        walker.push(
            RiskKind::UnsafeFn,
            Tier::Exact,
            line,
            col,
            "unsafe fn".to_string(),
        );
    }

    walker.visit_block(block);
    walker.features
}

struct RiskWalker {
    path: String,
    /// Nesting depth of enclosing `unsafe { }` blocks.
    unsafe_depth: usize,
    /// True when the enclosing function itself is `unsafe fn`.  A deref in the
    /// function body (outside any nested `unsafe {}`) is still in an unsafe
    /// context — `fn_is_unsafe` extends `inside_unsafe()` to cover that case.
    fn_is_unsafe: bool,
    features: Vec<RiskFeature>,
}

impl RiskWalker {
    fn push(&mut self, kind: RiskKind, tier: Tier, line: usize, col: usize, evidence: String) {
        let class = kind.class();
        self.features.push(RiskFeature {
            kind,
            class,
            weight: weight_for_class(class),
            path: self.path.clone(),
            line,
            col,
            evidence,
            tier,
        });
    }

    fn inside_unsafe(&self) -> bool {
        self.unsafe_depth > 0 || self.fn_is_unsafe
    }
}

impl<'ast> Visit<'ast> for RiskWalker {
    // ── unsafe { } blocks ────────────────────────────────────────────────────

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        let loc = node.span().start();
        let line = loc.line;
        let col = loc.column + 1;
        self.push(
            RiskKind::UnsafeBlock,
            Tier::Exact,
            line,
            col,
            "unsafe { }".to_string(),
        );
        self.unsafe_depth += 1;
        syn::visit::visit_expr_unsafe(self, node);
        self.unsafe_depth -= 1;
    }

    // ── Call expressions: transmute, from_raw, MaybeUninit, ptr::*, Box::leak,
    //    mem::forget, ManuallyDrop ────────────────────────────────────────────

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func {
            let rendered = render_path(&p.path);
            let loc = node.span().start();
            let line = loc.line;
            let col = loc.column + 1;

            if let Some((kind, tier, evidence)) = classify_path_call(&rendered) {
                self.push(kind, tier, line, col, evidence);
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    // ── Method calls: get_unchecked, get_unchecked_mut, from_raw ────────────

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        let loc = node.span().start();
        let line = loc.line;
        let col = loc.column + 1;

        match method.as_str() {
            "get_unchecked" | "get_unchecked_mut" => {
                self.push(
                    RiskKind::GetUnchecked,
                    Tier::Heuristic,
                    line,
                    col,
                    format!(".{method}"),
                );
            }
            "from_raw" => {
                self.push(
                    RiskKind::FromRaw,
                    Tier::Heuristic,
                    line,
                    col,
                    ".from_raw".to_string(),
                );
            }
            _ => {}
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    // ── asm! / core::arch::asm! / std::arch::asm! macros ────────────────────

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let rendered = render_path(&node.path);
        if matches!(
            rendered.as_str(),
            "asm" | "core::arch::asm" | "std::arch::asm"
        ) {
            let loc = node.span().start();
            let line = loc.line;
            let col = loc.column + 1;
            self.push(
                RiskKind::Asm,
                Tier::Exact,
                line,
                col,
                format!("{rendered}!"),
            );
        }
        syn::visit::visit_macro(self, node);
    }

    // ── Raw-pointer dereference inside unsafe (approximation) ─────────────────
    //
    // A `*expr` (`Expr::Unary(UnOp::Deref)`) occurring inside an unsafe block
    // is classified as `RawPtrDeref` (Tier::Heuristic).  This over-approximates
    // (it includes `*box_val` and `**slice_iter` under unsafe), but raw deref of
    // a safe pointer is only ever *legal* inside unsafe, so no true raw deref is
    // missed.  The Heuristic tier signals the uncertainty.

    fn visit_expr_unary(&mut self, node: &'ast syn::ExprUnary) {
        if matches!(node.op, syn::UnOp::Deref(_)) && self.inside_unsafe() {
            let loc = node.span().start();
            let line = loc.line;
            let col = loc.column + 1;
            self.push(
                RiskKind::RawPtrDeref,
                Tier::Heuristic,
                line,
                col,
                "*<expr> inside unsafe".to_string(),
            );
        }
        syn::visit::visit_expr_unary(self, node);
    }
}

// ── Module-level risk detection ───────────────────────────────────────────────

/// Detect module-level risk features in a parsed file.
///
/// Currently detected:
/// - `impl Drop for T` → `ImplDrop` (class 2).
/// - `extern "…" { }` blocks → `ExternBlock` (class 2).
///
/// `path` is the file path; it is stored in every emitted `RiskFeature`.
/// When `include_tests` is `false`, items carrying `#[cfg(test)]` are skipped.
pub fn detect_module_risks(file: &syn::File, path: &str, include_tests: bool) -> Vec<RiskFeature> {
    let mut features = Vec::new();

    for item in &file.items {
        match item {
            // `extern "ABI" { … }` block.
            syn::Item::ForeignMod(fm) => {
                if !include_tests && is_cfg_test(&fm.attrs) {
                    continue;
                }
                let loc = fm.span().start();
                let line = loc.line;
                let col = loc.column + 1;
                let abi = fm
                    .abi
                    .name
                    .as_ref()
                    .map(|l| l.value())
                    .unwrap_or_else(|| "C".to_string());
                features.push(RiskFeature {
                    kind: RiskKind::ExternBlock,
                    class: RiskKind::ExternBlock.class(),
                    weight: weight_for_class(RiskKind::ExternBlock.class()),
                    path: path.to_string(),
                    line,
                    col,
                    evidence: format!("extern \"{abi}\" {{ }}"),
                    tier: Tier::Exact,
                });
            }

            // `impl … Drop for T { … }` — match any impl whose trait path ends in `Drop`.
            // An `unsafe impl Drop` matches ONLY this arm (the first matching arm wins;
            // Rust arms don't fall through), so its `UnsafeImpl` risk is emitted by the
            // inner `unsafety` check below. The separate `unsafe impl` arm afterwards
            // therefore only ever sees non-Drop unsafe impls.
            syn::Item::Impl(impl_block) if is_impl_drop(impl_block) => {
                if !include_tests && is_cfg_test(&impl_block.attrs) {
                    continue;
                }
                let loc = impl_block.span().start();
                let line = loc.line;
                let col = loc.column + 1;
                features.push(RiskFeature {
                    kind: RiskKind::ImplDrop,
                    class: RiskKind::ImplDrop.class(),
                    weight: weight_for_class(RiskKind::ImplDrop.class()),
                    path: path.to_string(),
                    line,
                    col,
                    evidence: "impl Drop".to_string(),
                    tier: Tier::Exact,
                });
                // An `unsafe impl Drop` only matches this arm, so emit its UnsafeImpl
                // risk here too (it won't reach the later `unsafe impl` arm).
                if impl_block.unsafety.is_some() {
                    features.push(RiskFeature {
                        kind: RiskKind::UnsafeImpl,
                        class: RiskKind::UnsafeImpl.class(),
                        weight: weight_for_class(RiskKind::UnsafeImpl.class()),
                        path: path.to_string(),
                        line,
                        col,
                        evidence: "unsafe impl".to_string(),
                        tier: Tier::Exact,
                    });
                }
            }

            // `unsafe impl Trait for T { … }` (trait ≠ Drop, already handled above).
            syn::Item::Impl(impl_block) if impl_block.unsafety.is_some() => {
                if !include_tests && is_cfg_test(&impl_block.attrs) {
                    continue;
                }
                let loc = impl_block.span().start();
                let line = loc.line;
                let col = loc.column + 1;
                features.push(RiskFeature {
                    kind: RiskKind::UnsafeImpl,
                    class: RiskKind::UnsafeImpl.class(),
                    weight: weight_for_class(RiskKind::UnsafeImpl.class()),
                    path: path.to_string(),
                    line,
                    col,
                    evidence: "unsafe impl".to_string(),
                    tier: Tier::Exact,
                });
            }

            _ => {}
        }
    }

    features
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Render a `syn::Path` to its `::`-joined segment idents.
fn render_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// True if `impl_block` implements the `Drop` trait (path ends in `Drop`).
fn is_impl_drop(impl_block: &syn::ItemImpl) -> bool {
    if let Some((_, trait_path, _)) = &impl_block.trait_ {
        let last = trait_path.segments.last().map(|s| s.ident.to_string());
        last.as_deref() == Some("Drop")
    } else {
        false
    }
}

/// Match a rendered call path to a `RiskKind`, tier, and evidence string.
///
/// Returns `None` for paths not in the risk signal matrix.
fn classify_path_call(p: &str) -> Option<(RiskKind, Tier, String)> {
    // transmute — std::mem::transmute or bare `transmute`.
    if p == "std::mem::transmute" || p == "transmute" || p == "core::mem::transmute" {
        return Some((RiskKind::Transmute, Tier::Exact, p.to_string()));
    }

    // from_raw — path ending in `::from_raw` (Box::from_raw, Arc::from_raw, …).
    if p.ends_with("::from_raw") || p == "from_raw" {
        return Some((RiskKind::FromRaw, Tier::Heuristic, p.to_string()));
    }

    // MaybeUninit — any `::`-separated segment equals `MaybeUninit` exactly.
    // Substring matching (`.contains`) misfires on unrelated symbols such as
    // `my::MaybeUninitWrapper::new`, so we split on `::` and check segments.
    if p.split("::").any(|seg| seg == "MaybeUninit") {
        return Some((RiskKind::MaybeUninit, Tier::Exact, p.to_string()));
    }

    // Raw-pointer memory operations (incl. genuine volatile read/write) → RawPtrOp (class 7).
    // std::ptr::read/write/copy_nonoverlapping are raw-memory ops, not volatile ops;
    // std::ptr::read_volatile/write_volatile are the actual volatile variants.
    if matches!(
        p,
        "std::ptr::write"
            | "std::ptr::read"
            | "std::ptr::copy_nonoverlapping"
            | "ptr::write"
            | "ptr::read"
            | "ptr::copy_nonoverlapping"
            | "std::ptr::read_volatile"
            | "std::ptr::write_volatile"
            | "ptr::read_volatile"
            | "ptr::write_volatile"
            | "read_volatile"
            | "write_volatile"
    ) {
        return Some((RiskKind::RawPtrOp, Tier::Exact, p.to_string()));
    }

    // Box::leak.
    if p == "Box::leak" || p == "std::boxed::Box::leak" {
        return Some((RiskKind::BoxLeak, Tier::Exact, p.to_string()));
    }

    // mem::forget.
    if p == "std::mem::forget" || p == "mem::forget" || p == "forget" || p == "core::mem::forget" {
        return Some((RiskKind::MemForget, Tier::Exact, p.to_string()));
    }

    // ManuallyDrop — any `::`-separated segment equals `ManuallyDrop` exactly.
    // Same rationale as MaybeUninit above: avoid false positives from wrappers
    // such as `foo::ManuallyDropGuard::new`.
    if p.split("::").any(|seg| seg == "ManuallyDrop") {
        return Some((RiskKind::ManuallyDrop, Tier::Exact, p.to_string()));
    }

    None
}
