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
///
/// `referencing_mod` is the module the calling unit lives in (e.g.
/// `["crate","net"]`), used to expand `self::`/`super::` relative paths.
pub fn extract(
    block: &syn::Block,
    imports: &ImportTable,
    referencing_mod: &[String],
) -> Vec<CallSiteRef> {
    let mut walker = RefsWalker {
        imports,
        referencing_mod,
        refs: Vec::new(),
    };
    walker.visit_block(block);
    walker.refs
}

struct RefsWalker<'a> {
    imports: &'a ImportTable,
    referencing_mod: &'a [String],
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
            let resolved_target = resolve_in_crate(&base, self.imports, self.referencing_mod);
            self.refs.push(CallSiteRef {
                kind: RefKind::Free,
                base,
                module,
                line: start.line,
                col: start.column + 1,
                qualified,
                first_party,
                resolved_target,
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
            resolved_target: None,
        });
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Expand a written call path to an in-crate canonical segment vector, or `None`
/// for external / unresolvable targets. (025-3e §5.1)
///
/// `referencing_mod` is the module the calling unit lives in (e.g.
/// `["crate","net"]`), already anchored at `"crate"`. An EMPTY `referencing_mod`
/// means the calling file has no crate root in scope — then `self`/`super` cannot
/// be anchored and MUST return `None` (anchoring at a fabricated `["crate"]` could
/// false-resolve in an adopted mixed batch — the exact bug 3e kills).
fn resolve_in_crate(
    base: &str,
    imports: &ImportTable,
    referencing_mod: &[String],
) -> Option<Vec<String>> {
    // Resolve a bare leading name through the import table first.
    let head = base.split("::").next().unwrap_or(base);
    let effective: String = match imports.resolve(head) {
        Some(full) => {
            // replace the head with its imported full path
            let rest = base.strip_prefix(head).unwrap_or("");
            format!("{full}{rest}")
        }
        None => base.to_string(),
    };
    let segs: Vec<String> = effective.split("::").map(|s| s.to_string()).collect();
    match segs.first().map(String::as_str) {
        Some("crate") => Some(segs),
        Some("self") | Some("super") => {
            if referencing_mod.is_empty() {
                return None; // cannot anchor a relative path with no module context
            }
            let mut module = referencing_mod.to_vec();
            // Consume the LEADING run of self/super, walking up for each super.
            let mut rest = segs.into_iter().peekable();
            while let Some(seg) = rest.peek() {
                match seg.as_str() {
                    "self" => {
                        rest.next();
                    }
                    "super" => {
                        rest.next();
                        module.pop(); // up one module
                    }
                    _ => break,
                }
            }
            // After walking up, the module must still be anchored at `crate`;
            // otherwise the path escaped the crate root → not in-crate → None.
            if module.first().map(String::as_str) != Some("crate") {
                return None;
            }
            module.extend(rest);
            Some(module)
        }
        _ => None, // std::, other crates, unresolved bare names → external/opaque
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
        extract(&block, &imports, &[])
    }

    fn refs_with_ctx(src: &str, referencing_mod: &[&str]) -> Vec<CallSiteRef> {
        let file = syn::parse_file(src).unwrap();
        let imports = ImportTable::from_file(&file);
        // Find the FIRST fn item — the fixture may start with a `use` (so items[0]
        // is not a fn). Mirrors the existing `refs_of` helper pattern in this file.
        let block = file
            .items
            .iter()
            .find_map(|it| match it {
                syn::Item::Fn(f) => Some((*f.block).clone()),
                _ => None,
            })
            .expect("fixture must contain a fn item");
        let rmod: Vec<String> = referencing_mod.iter().map(|s| s.to_string()).collect();
        extract(&block, &imports, &rmod)
    }

    #[test]
    fn resolves_crate_self_super_paths() {
        let src = r#"
            fn caller() {
                crate::helpers::write();
                self::local();
                super::sibling();
                std::fs::write();
            }
        "#;
        // referencing module = crate::net (so super:: → crate)
        let refs = refs_with_ctx(src, &["crate", "net"]);
        let t = |b: &str| {
            refs.iter()
                .find(|r| r.base == b)
                .unwrap()
                .resolved_target
                .clone()
        };
        assert_eq!(
            t("crate::helpers::write"),
            Some(vec!["crate".into(), "helpers".into(), "write".into()])
        );
        assert_eq!(
            t("self::local"),
            Some(vec!["crate".into(), "net".into(), "local".into()])
        );
        assert_eq!(
            t("super::sibling"),
            Some(vec!["crate".into(), "sibling".into()])
        );
        // std:: is external → None (→ qualified miss → opaque; the false-resolve fix)
        assert_eq!(t("std::fs::write"), None);
    }

    #[test]
    fn bare_import_resolved_to_crate_path() {
        let src = r#"
            use crate::helpers::write;
            fn caller() { write(); }
        "#;
        let refs = refs_with_ctx(src, &["crate"]);
        let w = refs.iter().find(|r| r.base == "write").unwrap();
        assert_eq!(
            w.resolved_target,
            Some(vec!["crate".into(), "helpers".into(), "write".into()])
        );
    }

    #[test]
    fn super_super_walks_up_two_modules() {
        let src = r#"fn caller() { super::super::sibling(); }"#;
        // referencing module crate::a::b → super::super → crate
        let refs = refs_with_ctx(src, &["crate", "a", "b"]);
        let t = refs
            .iter()
            .find(|r| r.base == "super::super::sibling")
            .unwrap();
        assert_eq!(
            t.resolved_target,
            Some(vec!["crate".into(), "sibling".into()])
        );
    }

    #[test]
    fn relative_paths_unanchorable_in_rootless_file_are_none() {
        // Root-less file → empty referencing module → self/super cannot anchor → None
        // (must NOT fabricate a ["crate"] target that could false-resolve).
        let src = r#"fn caller() { self::local(); super::up(); }"#;
        let refs = refs_with_ctx(src, &[]); // empty referencing module
        assert_eq!(
            refs.iter()
                .find(|r| r.base == "self::local")
                .unwrap()
                .resolved_target,
            None
        );
        assert_eq!(
            refs.iter()
                .find(|r| r.base == "super::up")
                .unwrap()
                .resolved_target,
            None
        );
    }

    #[test]
    fn super_past_crate_root_is_none() {
        // super from a unit directly at the crate root pops "crate" → escapes → None.
        let src = r#"fn caller() { super::x(); }"#;
        let refs = refs_with_ctx(src, &["crate"]);
        assert_eq!(
            refs.iter()
                .find(|r| r.base == "super::x")
                .unwrap()
                .resolved_target,
            None
        );
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
