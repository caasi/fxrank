//! Call-reference extraction: walks a function body for every outgoing call
//! (free-function calls and method calls) and emits a language-neutral
//! `CallSiteRef` per site.  Unlike `calls.rs`, this module records *every*
//! named callee rather than filtering to known-effectful paths — it is the
//! graph-edge source for cross-file propagation.
//!
//! # Python `qualified` rule
//! A reference is a qualified outward reference iff its leading name resolves to
//! an import in the file's `Imports` table (`os.getcwd` where `os` is imported;
//! `from sub import run; run()`). Bare locals and `self.`/receiver methods (root
//! not imported) → `qualified = false`.

use fxrank_core::record::{CallSiteRef, RefKind};
use libcst_native::Call;

use super::{
    EffectSink,
    expr::{leftmost_name, render_expr},
    walk_own_body,
};
use crate::functions::FnUnit;
use crate::imports::Imports;
use crate::source::{SpanIndex, anchor_of_subslice};

/// Extract all outgoing call references from `unit`'s own body.
///
/// For each call node encountered:
/// - `base = render_expr(&call.func)` (e.g. `"os.getcwd"`, `"self.method"`,
///   `"foo"`). Calls where `render_expr` returns `None` are skipped.
/// - `root = base.split('.').next()` ; `module = imports.resolve(root)`.
/// - `qualified = module.is_some()` — the Python rule.
/// - `kind = RefKind::Method` if `base.contains('.')` AND `module.is_none()`
///   (a receiver attribute/method like `self.foo`/`x.bar`); else `RefKind::Free`.
/// - `line`/`col` from the `leftmost_name` anchor via the span.
pub fn extract(unit: &FnUnit, imports: &Imports, span: &SpanIndex) -> Vec<CallSiteRef> {
    let mut sink = RefSink {
        imports,
        span,
        refs: Vec::new(),
    };
    walk_own_body(unit, &mut sink);
    sink.refs
}

struct RefSink<'a> {
    imports: &'a Imports,
    span: &'a SpanIndex<'a>,
    refs: Vec<CallSiteRef>,
}

impl EffectSink for RefSink<'_> {
    fn on_call(&mut self, call: &Call) {
        let Some(rendered) = render_expr(&call.func) else {
            return;
        };

        let root = rendered.split('.').next().unwrap_or(&rendered);
        let module = self.imports.resolve(root).map(|m| {
            // module = the import module of the leading name (e.g. "os" for
            // os.getcwd, "subprocess.run" for `from subprocess import run`),
            // not the full call path.
            m.to_string()
        });

        let qualified = module.is_some();
        let kind = if rendered.contains('.') && module.is_none() {
            RefKind::Method
        } else {
            RefKind::Free
        };

        let (line, col) = leftmost_name(&call.func)
            .map(|anchor| {
                let byte_off = anchor_of_subslice(self.span.src(), anchor.value);
                self.span.line_col(byte_off)
            })
            .unwrap_or((0, 0));

        let first_party = self.imports.is_relative(root);

        self.refs.push(CallSiteRef {
            kind,
            base: rendered,
            module,
            line,
            col,
            qualified,
            first_party,
            resolved_target: None,
        });
    }

    fn on_assert(&mut self, _assert: &libcst_native::Assert) {}
    fn on_raise(&mut self, _raise: &libcst_native::Raise) {}
    fn on_assign_target(&mut self, _target: &libcst_native::AssignTargetExpression, _is_aug: bool) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use fxrank_core::record::RefKind;

    /// Parse inline source and return refs for the function named `sym`.
    fn refs_for(src: &str, sym: &str) -> Vec<CallSiteRef> {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == sym)
            .unwrap_or_else(|| panic!("symbol {sym} not found"));
        extract(unit, &imports, &span)
    }

    #[test]
    fn extracts_refs_with_qualified_rule() {
        let src = "import os\nfrom sub import run\ndef f():\n    os.getcwd()\n    run()\n    self.foo()\n    bare()\n";
        let refs = refs_for(src, "f");

        // os.getcwd — root `os` resolves to import `os`, qualified=true
        let os_ref = refs
            .iter()
            .find(|r| r.base == "os.getcwd")
            .unwrap_or_else(|| panic!("os.getcwd not found; refs: {refs:?}"));
        assert_eq!(
            os_ref.module.as_deref(),
            Some("os"),
            "os.getcwd module must be Some(\"os\")"
        );
        assert!(os_ref.qualified, "os.getcwd must be qualified=true");

        // run — resolves to `sub.run`, qualified=true, kind=Free (no dot in base, module present)
        let run_ref = refs
            .iter()
            .find(|r| r.base == "run")
            .unwrap_or_else(|| panic!("run not found; refs: {refs:?}"));
        assert_eq!(
            run_ref.module.as_deref(),
            Some("sub.run"),
            "run module must be Some(\"sub.run\")"
        );
        assert!(run_ref.qualified, "run must be qualified=true");

        // self.foo — root `self` not imported, qualified=false, kind=Method (has dot, no module)
        let self_foo = refs
            .iter()
            .find(|r| r.base == "self.foo")
            .unwrap_or_else(|| panic!("self.foo not found; refs: {refs:?}"));
        assert_eq!(self_foo.module, None, "self.foo module must be None");
        assert!(!self_foo.qualified, "self.foo must be qualified=false");
        assert!(
            matches!(self_foo.kind, RefKind::Method),
            "self.foo must be RefKind::Method"
        );

        // bare — root `bare` not imported, qualified=false, kind=Free
        let bare_ref = refs
            .iter()
            .find(|r| r.base == "bare")
            .unwrap_or_else(|| panic!("bare not found; refs: {refs:?}"));
        assert_eq!(bare_ref.module, None, "bare module must be None");
        assert!(!bare_ref.qualified, "bare must be qualified=false");
    }

    #[test]
    fn first_party_set_for_relative_imports() {
        let src = "from .utils import helper\nfrom . import sibling\nimport os\ndef f():\n    helper()\n    sibling.thing()\n    os.getcwd()\n";
        let refs = refs_for(src, "f");

        let helper_ref = refs
            .iter()
            .find(|r| r.base == "helper")
            .unwrap_or_else(|| panic!("helper not found; refs: {refs:?}"));
        assert!(helper_ref.first_party, "helper must be first_party=true");

        let sibling_ref = refs
            .iter()
            .find(|r| r.base == "sibling.thing")
            .unwrap_or_else(|| panic!("sibling.thing not found; refs: {refs:?}"));
        assert!(
            sibling_ref.first_party,
            "sibling.thing must be first_party=true"
        );

        let os_ref = refs
            .iter()
            .find(|r| r.base == "os.getcwd")
            .unwrap_or_else(|| panic!("os.getcwd not found; refs: {refs:?}"));
        assert!(!os_ref.first_party, "os.getcwd must be first_party=false");
    }

    #[test]
    fn line_is_populated() {
        let src = "import os\ndef f():\n    os.getcwd()\n";
        let refs = refs_for(src, "f");
        let r = refs
            .iter()
            .find(|r| r.base == "os.getcwd")
            .expect("os.getcwd not found");
        assert_eq!(r.line, 3, "os.getcwd is on line 3");
        assert!(r.col >= 1);
    }
}
