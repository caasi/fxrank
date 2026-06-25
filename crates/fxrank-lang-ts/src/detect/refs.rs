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
/// - `resolved_target` — `Some([module_key, export_name])` for provably-safe
///   in-project calls; `None` for external/unresolvable/ambiguous shapes.
/// - Recurse so nested calls (`f(g())`, `a.b().c()`) are all captured.
pub fn extract(
    body: &FnBodyOwned,
    imports: &ImportTable,
    lines: &SpanLines,
    referencing_file: &str,
    module_map: &crate::module_map::TsModuleMap,
) -> Vec<CallSiteRef> {
    let referencing_key = module_map.module_of(referencing_file);
    let mut walker = RefsWalker {
        imports,
        lines,
        referencing_file,
        referencing_key,
        module_map,
        refs: Vec::new(),
    };
    body.walk_with(&mut walker);
    walker.refs
}

struct RefsWalker<'a> {
    imports: &'a ImportTable,
    lines: &'a SpanLines,
    referencing_file: &'a str,
    referencing_key: String,
    module_map: &'a crate::module_map::TsModuleMap,
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

            use crate::imports::ImportTarget;
            let has_member = base.contains('.');
            let resolved_target = if matches!(kind, RefKind::Method) {
                None
            } else if let Some(spec) = &module {
                // Resolve the relative module, then pick the export name ONLY for
                // provably-safe shapes (never guess → never false-resolve).
                match self.module_map.resolve_import(self.referencing_file, spec) {
                    Some(key) => match (self.imports.import_target(root), has_member) {
                        // bare `local()` from a named import → the original export name
                        (Some(ImportTarget::Named(export)), false) => {
                            Some(vec![key, export.clone()])
                        }
                        // `ns.member()` from a namespace import → the member is the export
                        (Some(ImportTarget::Namespace), true) => {
                            let member = base.split('.').nth(1).unwrap_or(root);
                            Some(vec![key, member.to_string()])
                        }
                        // default()/default.member()/namespace() → ambiguous → opaque
                        _ => None,
                    },
                    None => None, // non-relative / out-of-batch spec → opaque
                }
            } else if !has_member {
                // bare same-module free call → own module
                Some(vec![self.referencing_key.clone(), root.to_string()])
            } else {
                None
            };

            self.refs.push(CallSiteRef {
                kind,
                base,
                module,
                line,
                col,
                qualified,
                first_party,
                resolved_target,
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
        use crate::module_map::TsModuleMap;
        use fxrank_core::frontend::SourceFile;
        let (module, cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        let mmap = TsModuleMap::build(&[SourceFile {
            path: "t.ts".into(),
            text: String::new(),
        }]);
        extract(&unit.body, &imports, &lines, "t.ts", &mmap)
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

    fn refs_with_map(src: &str, file: &str, fn_name: &str, files: &[&str]) -> Vec<CallSiteRef> {
        use crate::module_map::TsModuleMap;
        let sfs: Vec<fxrank_core::frontend::SourceFile> = files
            .iter()
            .map(|p| fxrank_core::frontend::SourceFile {
                path: (*p).into(),
                text: String::new(),
            })
            .collect();
        let mmap = TsModuleMap::build(&sfs);
        let (module, cm) =
            crate::functions::parse_module(src, file, crate::source::Lang::Ts).unwrap();
        let lines = crate::source::SpanLines::new(cm); // SpanLines::new takes the Lrc<SourceMap> by value, 1 arg
        let imports = crate::imports::ImportTable::from_module(&module);
        let units = crate::functions::collect(&module, file, &lines);
        let unit = units.iter().find(|u| u.symbol == fn_name).unwrap();
        extract(&unit.body, &imports, &lines, file, &mmap)
    }

    #[test]
    fn relative_import_call_resolves() {
        let src = "import { fetchUser } from './util';\nexport function caller() { fetchUser(); }";
        let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
        let r = refs.iter().find(|r| r.base == "fetchUser").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["src/util".into(), "fetchUser".into()])
        );
    }

    #[test]
    fn node_import_call_stays_unresolved_for_opaque() {
        // fs.readFile from node:fs must NOT resolve to a local readFile → None → opaque.
        let src = "import fs from 'node:fs';\nexport function caller() { fs.readFile('x', cb); }";
        let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
        let r = refs.iter().find(|r| r.base.starts_with("fs")).unwrap();
        assert_eq!(
            r.resolved_target, None,
            "node:fs call must be unresolved (→ opaque), never a local readFile"
        );
    }

    #[test]
    fn same_module_bare_call_resolves_to_own_module() {
        let src = "function helper() {}\nexport function caller() { helper(); }";
        let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts"]);
        let r = refs.iter().find(|r| r.base == "helper").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["src/app".into(), "helper".into()])
        );
    }

    #[test]
    fn namespace_import_member_resolves_to_member_name() {
        // import * as util from './util'; util.fetchUser() → ["src/util","fetchUser"]
        // (the member, NOT the namespace binding "util").
        let src = "import * as util from './util';\nexport function caller() { util.fetchUser(); }";
        let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
        let r = refs.iter().find(|r| r.base.starts_with("util")).unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["src/util".into(), "fetchUser".into()])
        );
    }

    #[test]
    fn renamed_import_resolves_to_original_export_name() {
        // import { readFile as rf } from './util'; rf() → ["src/util","readFile"]
        // (the ORIGINAL export, not the local alias `rf`) — no false-resolve.
        let src = "import { readFile as rf } from './util';\nexport function caller() { rf(); }";
        let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
        let r = refs.iter().find(|r| r.base == "rf").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["src/util".into(), "readFile".into()])
        );
    }

    #[test]
    fn default_import_member_call_is_unresolved() {
        // import client from './util'; client.get() — `get` is a method on the default
        // export VALUE, NOT a module export → must NOT resolve to a coincidental `get`.
        let src = "import client from './util';\nexport function caller() { client.get(); }";
        let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
        let r = refs.iter().find(|r| r.base.starts_with("client")).unwrap();
        assert_eq!(
            r.resolved_target, None,
            "default-import member call must be unresolved (→ opaque)"
        );
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
