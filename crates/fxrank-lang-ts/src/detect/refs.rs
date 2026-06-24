//! Call-reference extraction: walks a function body for every outgoing call
//! (free-function calls and method calls) and emits a language-neutral
//! `CallSiteRef` per site.  Unlike `calls.rs`, this module records *every*
//! named callee rather than filtering to known-effectful paths — it is the
//! graph-edge source for cross-file propagation.
//!
//! # TS `qualified` rule
//! A reference is a qualified outward reference iff its leading name resolves to
//! an ES import in the file's `ImportTable` (e.g. `useState` → `"react"`, `fs` →
//! `"node:fs"`, `React` → `"react"`).  Bare globals (`fetch`, `foo`) and member
//! calls on non-imported receivers (`obj.method`, `this.foo`) → `qualified = false`.

use fxrank_core::record::{CallSiteRef, RefKind};
use swc_ecma_ast::{Callee, Expr, MemberExpr, MemberProp};
use swc_ecma_visit::{Visit, VisitWith};

use crate::functions::FnBodyOwned;
use crate::imports::ImportTable;
use crate::source::SpanLines;

/// Extract all outgoing call references from `body`.
///
/// For each `CallExpr` with a renderable callee:
/// - `root = base.split('.').next()`; `module = imports.resolve(root)`.
/// - `qualified = module.is_some()` — the TS rule (same shape as Python).
/// - `kind = RefKind::Method` if `base.contains('.')` AND `module.is_none()`
///   (member call on a non-imported receiver); else `RefKind::Free`.
/// - `line`/`col` from the call span via `lines`.
/// - Recurse so nested calls (`f(g())`, `a.b().c()`) are all captured.
pub fn extract(body: &FnBodyOwned, imports: &ImportTable, lines: &SpanLines) -> Vec<CallSiteRef> {
    let mut walker = RefsWalker {
        imports,
        lines,
        refs: Vec::new(),
    };
    body.walk_with(&mut walker);
    walker.refs
}

struct RefsWalker<'a> {
    imports: &'a ImportTable,
    lines: &'a SpanLines,
    refs: Vec<CallSiteRef>,
}

impl Visit for RefsWalker<'_> {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Some(base) = render_expr(callee)
        {
            let root = base.split('.').next().unwrap_or(&base);
            let module = self.imports.resolve(root).map(str::to_string);
            let qualified = module.is_some();
            let kind = if base.contains('.') && module.is_none() {
                RefKind::Method
            } else {
                RefKind::Free
            };
            let (line, col) = self.lines.line_col(node.span);
            let first_party = module.as_deref().is_some_and(is_first_party_specifier);
            self.refs.push(CallSiteRef {
                kind,
                base,
                module,
                line,
                col,
                qualified,
                first_party,
            });
        }
        // Recurse so nested calls (f(g()), a.b().c()) are captured.
        node.visit_children_with(self);
    }

    // Do not cross nested function boundaries — own-body only.
    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
    fn visit_constructor(&mut self, _n: &swc_ecma_ast::Constructor) {}
}

/// Check if a module specifier should be classified as first-party.
///
/// Returns true for:
/// - Relative paths starting with `.` or `/`
/// - Path aliases `@/` and `~/` (universal conventions, never valid npm packages)
///
/// Returns false for:
/// - Third-party npm packages (e.g., `react`, `lodash`)
/// - Scoped packages with non-empty scope (e.g., `@dnd-kit/core`, where `dnd-kit` is the scope)
///
/// Note: `@/` must be checked as the exact 2-char prefix to avoid matching
/// scoped packages like `@scope/name`.
fn is_first_party_specifier(m: &str) -> bool {
    m.starts_with('.') || m.starts_with('/') || m.starts_with("@/") || m.starts_with("~/")
}

/// Render a (possibly nested) callee/member `Expr` into a dotted string:
/// `Ident("fetch")` → `"fetch"`, `Date.now` → `"Date.now"`.
/// Returns `None` for shapes we don't model (computed indexing, calls-of-calls,
/// `this`, etc.).
fn render_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.sym.to_string()),
        Expr::Member(m) => render_member(m),
        _ => None,
    }
}

/// Render a `MemberExpr` chain to `a.b.c`. Only `Ident` properties on
/// renderable objects are kept; computed/private props yield `None`.
fn render_member(m: &MemberExpr) -> Option<String> {
    let obj = render_expr(&m.obj)?;
    match &m.prop {
        MemberProp::Ident(name) => Some(format!("{obj}.{}", name.sym)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use crate::source::Lang;
    use fxrank_core::record::RefKind;

    /// Parse `src`, collect the unit named `fn_name`, and return its call refs.
    fn refs_of(src: &str, fn_name: &str) -> Vec<CallSiteRef> {
        let (module, cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        extract(&unit.body, &imports, &lines)
    }

    #[test]
    fn extracts_refs_with_qualified_rule() {
        let src = "import { useState } from 'react';\n\
                   import fs from 'node:fs';\n\
                   function f() { useState(); fs.readFile('x'); obj.method(); fetch('y'); bare(); }";
        let refs = refs_of(src, "f");

        // useState — root resolves to "react", qualified=true, kind=Free (no dot)
        let use_state = refs
            .iter()
            .find(|r| r.base == "useState")
            .unwrap_or_else(|| panic!("useState not found; refs: {refs:?}"));
        assert_eq!(
            use_state.module.as_deref(),
            Some("react"),
            "useState module must be Some(\"react\")"
        );
        assert!(use_state.qualified, "useState must be qualified=true");
        assert!(
            matches!(use_state.kind, RefKind::Free),
            "useState must be RefKind::Free"
        );

        // fs.readFile — root `fs` resolves to "node:fs", qualified=true, kind=Free
        // (module is Some, so it's an imported namespace call, not an unknown receiver)
        let fs_read = refs
            .iter()
            .find(|r| r.base == "fs.readFile")
            .unwrap_or_else(|| panic!("fs.readFile not found; refs: {refs:?}"));
        assert_eq!(
            fs_read.module.as_deref(),
            Some("node:fs"),
            "fs.readFile module must be Some(\"node:fs\")"
        );
        assert!(fs_read.qualified, "fs.readFile must be qualified=true");

        // obj.method — root `obj` not imported, qualified=false, kind=Method
        let obj_method = refs
            .iter()
            .find(|r| r.base == "obj.method")
            .unwrap_or_else(|| panic!("obj.method not found; refs: {refs:?}"));
        assert_eq!(obj_method.module, None, "obj.method module must be None");
        assert!(!obj_method.qualified, "obj.method must be qualified=false");
        assert!(
            matches!(obj_method.kind, RefKind::Method),
            "obj.method must be RefKind::Method"
        );

        // fetch — root `fetch` not imported, qualified=false, kind=Free
        let fetch_ref = refs
            .iter()
            .find(|r| r.base == "fetch")
            .unwrap_or_else(|| panic!("fetch not found; refs: {refs:?}"));
        assert_eq!(fetch_ref.module, None, "fetch module must be None");
        assert!(!fetch_ref.qualified, "fetch must be qualified=false");
        assert!(
            matches!(fetch_ref.kind, RefKind::Free),
            "fetch must be RefKind::Free"
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
    fn first_party_relative_and_third_party() {
        let src = "import { a } from './util';\n\
                   import { b } from '../lib/helper';\n\
                   import { c } from 'react';\n\
                   function f() { a(); b(); c(); }";
        let refs = refs_of(src, "f");

        let a_ref = refs
            .iter()
            .find(|r| r.base == "a")
            .unwrap_or_else(|| panic!("a not found; refs: {refs:?}"));
        assert_eq!(
            a_ref.module.as_deref(),
            Some("./util"),
            "a module must be Some(\"./util\")"
        );
        assert!(a_ref.first_party, "a (./util) must be first_party=true");

        let b_ref = refs
            .iter()
            .find(|r| r.base == "b")
            .unwrap_or_else(|| panic!("b not found; refs: {refs:?}"));
        assert_eq!(
            b_ref.module.as_deref(),
            Some("../lib/helper"),
            "b module must be Some(\"../lib/helper\")"
        );
        assert!(
            b_ref.first_party,
            "b (../lib/helper) must be first_party=true"
        );

        let c_ref = refs
            .iter()
            .find(|r| r.base == "c")
            .unwrap_or_else(|| panic!("c not found; refs: {refs:?}"));
        assert_eq!(
            c_ref.module.as_deref(),
            Some("react"),
            "c module must be Some(\"react\")"
        );
        assert!(!c_ref.first_party, "c (react) must be first_party=false");
    }

    #[test]
    fn captures_nested_calls() {
        let refs = refs_of("function f() { outer(inner()); }", "f");
        assert!(
            refs.iter().any(|r| r.base == "outer"),
            "outer not found; refs: {refs:?}"
        );
        assert!(
            refs.iter().any(|r| r.base == "inner"),
            "inner not found; refs: {refs:?}"
        );
    }

    #[test]
    fn line_is_populated() {
        let refs = refs_of("function f() {\n    g();\n}", "f");
        let g_ref = refs.iter().find(|r| r.base == "g").expect("g not found");
        assert_eq!(g_ref.line, 2);
        assert!(g_ref.col >= 1);
    }

    #[test]
    fn first_party_alias_prefixes() {
        // Test that @/ and ~/ path aliases are recognized as first_party.
        let src = "import { util } from '@/util';\n\
                   import { lib } from '~/lib';\n\
                   import { dnd } from '@dnd-kit/core';\n\
                   import { react } from 'react';\n\
                   function f() { util(); lib(); dnd(); react(); }";
        let refs = refs_of(src, "f");

        // @/util — @/ prefix is first_party
        let util_ref = refs
            .iter()
            .find(|r| r.base == "util")
            .unwrap_or_else(|| panic!("util not found; refs: {refs:?}"));
        assert_eq!(
            util_ref.module.as_deref(),
            Some("@/util"),
            "util module must be Some(\"@/util\")"
        );
        assert!(
            util_ref.first_party,
            "util (@/util) must be first_party=true"
        );

        // ~/lib — ~/ prefix is first_party
        let lib_ref = refs
            .iter()
            .find(|r| r.base == "lib")
            .unwrap_or_else(|| panic!("lib not found; refs: {refs:?}"));
        assert_eq!(
            lib_ref.module.as_deref(),
            Some("~/lib"),
            "lib module must be Some(\"~/lib\")"
        );
        assert!(lib_ref.first_party, "lib (~/lib) must be first_party=true");

        // @dnd-kit/core — scoped package (non-empty scope), NOT first_party
        let dnd_ref = refs
            .iter()
            .find(|r| r.base == "dnd")
            .unwrap_or_else(|| panic!("dnd not found; refs: {refs:?}"));
        assert_eq!(
            dnd_ref.module.as_deref(),
            Some("@dnd-kit/core"),
            "dnd module must be Some(\"@dnd-kit/core\")"
        );
        assert!(
            !dnd_ref.first_party,
            "dnd (@dnd-kit/core) must be first_party=false (scoped package)"
        );

        // react — plain third-party, NOT first_party
        let react_ref = refs
            .iter()
            .find(|r| r.base == "react")
            .unwrap_or_else(|| panic!("react not found; refs: {refs:?}"));
        assert_eq!(
            react_ref.module.as_deref(),
            Some("react"),
            "react module must be Some(\"react\")"
        );
        assert!(!react_ref.first_party, "react must be first_party=false");
    }
}
