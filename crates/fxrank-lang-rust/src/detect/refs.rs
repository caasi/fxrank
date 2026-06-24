//! Call-reference extraction: walks a function body for every outgoing call
//! (free-function calls and method calls) and emits a language-neutral
//! `CallSiteRef` per site.  Unlike `calls.rs`, this module records *every*
//! named callee rather than filtering to known-effectful paths — it is the
//! graph-edge source for cross-file propagation.

use crate::imports::ImportTable;
use fxrank_core::record::{CallSiteRef, RefKind};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Extract all outgoing call references from `block`.
///
/// - `Expr::Call` with a `Path` callee → `RefKind::Free`; the leading segment
///   is resolved through `imports` to produce `module`.
/// - `Expr::MethodCall` → `RefKind::Method`; `module` is always `None` (no
///   type information available at the syntactic level).
///
/// Nested calls are captured because the default `visit_*` recursion is called
/// after recording each site.
pub fn extract(block: &syn::Block, imports: &ImportTable) -> Vec<CallSiteRef> {
    let mut walker = RefsWalker {
        imports,
        refs: Vec::new(),
    };
    walker.visit_block(block);
    walker.refs
}

struct RefsWalker<'a> {
    imports: &'a ImportTable,
    refs: Vec<CallSiteRef>,
}

impl<'a, 'ast> Visit<'ast> for RefsWalker<'a> {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func {
            let base = render_path(&p.path);
            let head = base.split("::").next().unwrap_or(&base);
            let module = self.imports.resolve(head).map(|s| s.to_string());
            let start = node.span().start();
            // A call is qualified if it has `::` in the written base OR if the
            // leading segment resolves through a `use` import (unqualified call of
            // an imported name — the external surface must not be silently dropped).
            let qualified = base.contains("::") || module.is_some();
            // first_party: true when the (syntactic or resolved) path originates
            // from this crate (`crate::`/`super::`/`self::`).  Use the resolved
            // path when the call was written as a bare name via `use`.
            let effective_path = module.as_deref().unwrap_or(&base);
            let first_party = effective_path.starts_with("crate::")
                || effective_path.starts_with("super::")
                || effective_path.starts_with("self::");
            self.refs.push(CallSiteRef {
                kind: RefKind::Free,
                base,
                module,
                line: start.line,
                col: start.column + 1,
                qualified,
                first_party,
            });
        }
        // Recurse so nested calls inside arguments are also captured.
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let base = node.method.to_string();
        let start = node.span().start();
        self.refs.push(CallSiteRef {
            kind: RefKind::Method,
            base,
            module: None,
            line: start.line,
            col: start.column + 1,
            qualified: false,
            first_party: false,
        });
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Render a `syn::Path` to its `::`-joined segment idents (`std::fs::write`).
/// Type-qualified leading segments (`<T as Tr>::f`) are skipped — rare for
/// call targets.
fn render_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imports::ImportTable;

    fn refs_of(src: &str) -> Vec<CallSiteRef> {
        let f = syn::parse_file(src).unwrap();
        let imports = ImportTable::from_file(&f);
        // Grab the first fn body.
        let block = f
            .items
            .into_iter()
            .find_map(|i| {
                if let syn::Item::Fn(func) = i {
                    Some(*func.block)
                } else {
                    None
                }
            })
            .unwrap();
        extract(&block, &imports)
    }

    #[test]
    fn extracts_free_and_method_calls() {
        let refs = refs_of("use std::fs; fn f() { fs::write(p, b); x.push(1); g(); }");
        // fs::write → Free, module resolved to std::fs
        assert!(
            refs.iter()
                .any(|r| r.base == "fs::write" && r.module.as_deref() == Some("std::fs")),
            "expected fs::write with module std::fs, got: {refs:?}"
        );
        // .push → Method
        assert!(
            refs.iter()
                .any(|r| matches!(r.kind, RefKind::Method) && r.base == "push"),
            "expected push method call, got: {refs:?}"
        );
        // g() → Free, no module
        assert!(
            refs.iter().any(|r| r.base == "g"),
            "expected bare g() call, got: {refs:?}"
        );
    }

    #[test]
    fn qualified_flag_set_correctly() {
        let refs = refs_of("use std::fs; fn f() { fs::write(p, b); x.push(1); g(); }");
        // fs::write — path call with `::` → qualified = true
        let fs_write = refs
            .iter()
            .find(|r| r.base == "fs::write")
            .expect("fs::write not found");
        assert!(
            fs_write.qualified,
            "fs::write (:: path) should be qualified=true, got: {fs_write:?}"
        );
        // .push — method call → qualified = false
        let push = refs
            .iter()
            .find(|r| matches!(r.kind, RefKind::Method) && r.base == "push")
            .expect("push not found");
        assert!(
            !push.qualified,
            "push (method call) should be qualified=false, got: {push:?}"
        );
        // g() — bare free call, no `::` → qualified = false
        let g = refs.iter().find(|r| r.base == "g").expect("g not found");
        assert!(
            !g.qualified,
            "g (bare free call) should be qualified=false, got: {g:?}"
        );
    }

    #[test]
    fn captures_nested_calls() {
        let refs = refs_of("fn f() { outer(inner()); }");
        assert!(refs.iter().any(|r| r.base == "outer"), "missing outer");
        assert!(refs.iter().any(|r| r.base == "inner"), "missing inner");
    }

    #[test]
    fn first_party_tagged_by_path_prefix() {
        let refs = refs_of(
            "fn f() { crate::helpers::foo(); super::bar(); self::baz(); std::fs::write(p, b); serde::to_string(x); bare(); }",
        );
        let crate_ref = refs
            .iter()
            .find(|r| r.base == "crate::helpers::foo")
            .expect("crate::helpers::foo not found");
        assert!(
            crate_ref.first_party,
            "crate:: path should be first_party=true, got: {crate_ref:?}"
        );
        let super_ref = refs
            .iter()
            .find(|r| r.base == "super::bar")
            .expect("super::bar not found");
        assert!(
            super_ref.first_party,
            "super:: path should be first_party=true, got: {super_ref:?}"
        );
        let self_ref = refs
            .iter()
            .find(|r| r.base == "self::baz")
            .expect("self::baz not found");
        assert!(
            self_ref.first_party,
            "self:: path should be first_party=true, got: {self_ref:?}"
        );
        let std_ref = refs
            .iter()
            .find(|r| r.base == "std::fs::write")
            .expect("std::fs::write not found");
        assert!(
            !std_ref.first_party,
            "std:: path should be first_party=false, got: {std_ref:?}"
        );
        let serde_ref = refs
            .iter()
            .find(|r| r.base == "serde::to_string")
            .expect("serde::to_string not found");
        assert!(
            !serde_ref.first_party,
            "serde:: path should be first_party=false, got: {serde_ref:?}"
        );
        let bare_ref = refs
            .iter()
            .find(|r| r.base == "bare")
            .expect("bare not found");
        assert!(
            !bare_ref.first_party,
            "bare call should be first_party=false, got: {bare_ref:?}"
        );
    }

    #[test]
    fn use_imported_external_call_is_qualified_and_third_party() {
        // `use serde_json::to_string; to_string(x)` — the call site has no `::`
        // in `base`, but the import table resolves `to_string` → `serde_json::to_string`.
        // The ref must be: qualified=true, first_party=false, module=Some("serde_json::to_string").
        let refs = refs_of("use serde_json::to_string; fn f(x: &str) { to_string(x); }");
        let r = refs
            .iter()
            .find(|r| r.base == "to_string")
            .expect("to_string not found");
        assert!(
            r.qualified,
            "use-imported call must be qualified=true, got: {r:?}"
        );
        assert!(
            !r.first_party,
            "serde_json import must be first_party=false, got: {r:?}"
        );
        assert_eq!(
            r.module.as_deref(),
            Some("serde_json::to_string"),
            "module must be Some(\"serde_json::to_string\"), got: {r:?}"
        );
    }

    #[test]
    fn use_imported_crate_call_is_first_party() {
        // `use crate::helpers::foo; foo()` — bare call, but resolved path starts
        // with `crate::` → qualified=true, first_party=true.
        let refs = refs_of("use crate::helpers::foo; fn f() { foo(); }");
        let r = refs
            .iter()
            .find(|r| r.base == "foo")
            .expect("foo not found");
        assert!(
            r.qualified,
            "use-imported crate call must be qualified=true, got: {r:?}"
        );
        assert!(
            r.first_party,
            "crate::helpers::foo import must be first_party=true, got: {r:?}"
        );
        assert_eq!(
            r.module.as_deref(),
            Some("crate::helpers::foo"),
            "module must be Some(\"crate::helpers::foo\"), got: {r:?}"
        );
    }

    #[test]
    fn bare_unimported_call_remains_unqualified() {
        // A genuinely bare free-fn call with no `use` covering it → qualified=false.
        let refs = refs_of("fn f() { helper(); }");
        let r = refs
            .iter()
            .find(|r| r.base == "helper")
            .expect("helper not found");
        assert!(
            !r.qualified,
            "bare unimported call must be qualified=false, got: {r:?}"
        );
        assert!(
            !r.first_party,
            "bare unimported call must be first_party=false, got: {r:?}"
        );
        assert_eq!(
            r.module, None,
            "bare unimported call must have module=None, got: {r:?}"
        );
    }

    #[test]
    fn syntactic_path_qualified_unchanged() {
        // `std::fs::write(...)` — syntactic `::` in base → qualified=true, first_party=false.
        let refs = refs_of("fn f() { std::fs::write(p, b); }");
        let r = refs
            .iter()
            .find(|r| r.base == "std::fs::write")
            .expect("std::fs::write not found");
        assert!(
            r.qualified,
            "std::fs::write must be qualified=true, got: {r:?}"
        );
        assert!(
            !r.first_party,
            "std:: must be first_party=false, got: {r:?}"
        );
    }

    #[test]
    fn line_and_col_are_populated() {
        let refs = refs_of("fn f() {\n    g();\n}");
        let g_ref = refs.iter().find(|r| r.base == "g").expect("g not found");
        assert_eq!(g_ref.line, 2);
        assert!(g_ref.col >= 1);
    }
}
