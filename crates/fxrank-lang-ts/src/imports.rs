//! Import table for ES `import` declarations and `require()` calls in a
//! TypeScript / JavaScript source file.
//!
//! Resolves a local name (as it appears after an `import` specifier or a
//! `const x = require(...)` binding) back to its module source string
//! (`"node:fs"`, `"./util"`, etc.), and flags dynamic / unresolvable imports
//! that make name-to-module matching uncertain.
//!
//! This is the swc analog of `fxrank-lang-rust`'s `ImportTable`: the Rust
//! variant maps `use`-imported names to fully-qualified `::` paths and flags
//! glob imports (`has_glob`); this variant maps ES/CJS-imported names to
//! module specifiers and flags dynamic imports (`has_dynamic`).

use std::collections::HashMap;

use swc_ecma_ast::{
    CallExpr, Callee, Decl, Expr, ExprStmt, Lit, Module, ModuleDecl, ModuleItem, Pat, Stmt, VarDecl,
};
use swc_ecma_visit::{Visit, VisitWith};

/// Mapping from local names to their module source strings, built from the
/// `import` declarations and top-level `const x = require(...)` calls in a
/// single swc `Module`.
pub struct ImportTable {
    map: HashMap<String, String>,
    has_dynamic: bool,
}

impl ImportTable {
    /// Build an `ImportTable` from the top-level items of a parsed module.
    pub fn from_module(module: &Module) -> Self {
        let mut table = ImportTable {
            map: HashMap::new(),
            has_dynamic: false,
        };

        for item in &module.body {
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(decl)) => {
                    // `Wtf8Atom` has no `Display`; `to_atom_lossy()` produces a
                    // UTF-8 `Atom` (reallocates only for lone surrogates).
                    let src = decl.src.value.to_atom_lossy().to_string();
                    for spec in &decl.specifiers {
                        use swc_ecma_ast::ImportSpecifier::*;
                        let local = match spec {
                            Named(s) => s.local.sym.to_string(),
                            Default(s) => s.local.sym.to_string(),
                            Namespace(s) => s.local.sym.to_string(),
                        };
                        table.map.insert(local, src.clone());
                    }
                }
                ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => {
                    table.scan_var_decl(var);
                }
                ModuleItem::Stmt(Stmt::Expr(ExprStmt { expr, .. })) => {
                    // Top-level expression statement: check for a bare `import(...)`
                    // call or an `await import(...)` — both indicate a dynamic import
                    // that is not captured by the static import table.
                    table.check_expr_for_dynamic_import(expr);
                }
                _ => {}
            }
        }

        // Walk the full module for any `import(...)` nested inside function
        // bodies (e.g. `const f = async () => { await import(name); }`).
        // This catches cases the top-level scan above cannot reach.
        if !table.has_dynamic {
            let mut walker = DynamicImportWalker { found: false };
            module.visit_with(&mut walker);
            if walker.found {
                table.has_dynamic = true;
            }
        }

        table
    }

    /// Scan a top-level `var`/`let`/`const` declaration for `require()` calls.
    ///
    /// Only the simple `const x = require('literal')` shape is handled.
    /// `require(expr)` with a non-literal argument sets `has_dynamic = true`.
    fn scan_var_decl(&mut self, var: &VarDecl) {
        for decl in &var.decls {
            let Some(init) = &decl.init else { continue };

            // Look for `require(...)` calls; only `Expr::Call` shapes are relevant.
            let Expr::Call(call) = init.as_ref() else {
                continue;
            };

            // Check for bare `import(expr)` (dynamic import expression).
            if matches!(&call.callee, Callee::Import(_)) {
                self.check_dynamic_import(call);
                continue;
            }

            // Must be `require(...)` — callee is the ident `require`.
            let is_require = match &call.callee {
                Callee::Expr(expr) => matches!(
                    expr.as_ref(),
                    Expr::Ident(id) if id.sym.as_str() == "require"
                ),
                _ => false,
            };
            if !is_require {
                continue;
            }

            // Single argument must be a string literal; otherwise it's dynamic.
            let Some(first_arg) = call.args.first() else {
                continue;
            };
            let module_src = match first_arg.expr.as_ref() {
                Expr::Lit(Lit::Str(s)) => s.value.to_atom_lossy().to_string(),
                _ => {
                    self.has_dynamic = true;
                    continue;
                }
            };

            // Bind the left-hand side identifier to the module source.
            if let Pat::Ident(binding) = &decl.name {
                self.map.insert(binding.id.sym.to_string(), module_src);
            }
        }
    }

    /// Check whether a `CallExpr` with `callee: Callee::Import` has a
    /// non-literal argument; if so, mark this table as having a dynamic import.
    fn check_dynamic_import(&mut self, call: &CallExpr) {
        if !matches!(&call.callee, Callee::Import(_)) {
            return;
        }
        let Some(arg) = call.args.first() else {
            self.has_dynamic = true;
            return;
        };
        if !matches!(arg.expr.as_ref(), Expr::Lit(Lit::Str(_))) {
            self.has_dynamic = true;
        }
    }

    /// Check a top-level expression for a bare or awaited `import(...)` call.
    ///
    /// Handles: `import('x')` and `await import('x')` as expression statements.
    /// Any `import(...)` — literal or not — signals an unbound dynamic load
    /// (fire-and-forget), so `has_dynamic` is set to `true` regardless of the
    /// argument type.
    fn check_expr_for_dynamic_import(&mut self, expr: &Expr) {
        match expr {
            Expr::Call(call) if matches!(&call.callee, Callee::Import(_)) => {
                self.has_dynamic = true;
            }
            Expr::Await(await_expr) => {
                // `await import(...)` — unwrap the await and check the inner expr.
                self.check_expr_for_dynamic_import(&await_expr.arg);
            }
            _ => {}
        }
    }

    /// Resolve a local name to its module source string.
    ///
    /// Returns `None` if the name is not covered by any `import` declaration or
    /// `require()` call found in this file.
    pub fn resolve(&self, local: &str) -> Option<&str> {
        self.map.get(local).map(String::as_str)
    }

    /// Returns `true` if any dynamic or unresolvable import was found.
    ///
    /// Callers may apply a confidence penalty when this is `true`, because a
    /// dynamic import means a bare name might resolve to an unknown module that
    /// cannot be matched against a known effect list.
    pub fn has_dynamic(&self) -> bool {
        self.has_dynamic
    }
}

/// Visitor that sets `found = true` the moment it encounters any `import(...)`
/// call expression, regardless of nesting depth (function bodies, arrow
/// functions, method bodies, await expressions, etc.).
struct DynamicImportWalker {
    found: bool,
}

impl Visit for DynamicImportWalker {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if matches!(&node.callee, Callee::Import(_)) {
            self.found = true;
            // No need to recurse further — we already found one.
            return;
        }
        // Continue walking children for other call expressions.
        node.visit_children_with(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `src` as a TypeScript module and build an `ImportTable`.
    ///
    /// Test-only convenience: owns the swc parse plumbing and returns the table.
    /// Production callers use `from_module` with an already-parsed `Module`.
    fn table(src: &str) -> ImportTable {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, lexer::Lexer};

        use crate::source::Lang;

        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(FileName::Custom("t.ts".into()).into(), src.to_string());
        let lexer = Lexer::new(
            Lang::Ts.syntax(),
            Default::default(),
            StringInput::from(&*fm),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let module = parser.parse_module().expect("parse failed");
        ImportTable::from_module(&module)
    }

    #[test]
    fn resolves_named_default_namespace() {
        let t = table(
            "import { readFile } from 'node:fs'; \
             import fs from 'node:fs'; \
             import * as os from 'node:os';",
        );
        assert_eq!(t.resolve("readFile"), Some("node:fs"));
        assert_eq!(t.resolve("fs"), Some("node:fs"));
        assert_eq!(t.resolve("os"), Some("node:os"));
        assert!(!t.has_dynamic());
    }

    #[test]
    fn resolves_require_literal() {
        let t = table("const fs = require('node:fs');");
        assert_eq!(t.resolve("fs"), Some("node:fs"));
        assert!(!t.has_dynamic());
    }

    #[test]
    fn dynamic_require_sets_has_dynamic() {
        let t = table("const m = require(name);");
        assert!(t.has_dynamic());
        // The binding is not added (no string literal to map to).
        assert_eq!(t.resolve("m"), None);
    }

    #[test]
    fn unknown_name_returns_none() {
        let t = table("import { readFile } from 'node:fs';");
        assert_eq!(t.resolve("writeFile"), None);
    }

    #[test]
    fn renamed_import_resolves_to_local_name() {
        // `import { readFile as rf }` — local name is `rf`.
        let t = table("import { readFile as rf } from 'node:fs';");
        assert_eq!(t.resolve("rf"), Some("node:fs"));
        assert_eq!(t.resolve("readFile"), None);
    }

    // ── FIX 2: bare / awaited expression-statement dynamic import() ──

    #[test]
    fn bare_dynamic_import_expression_stmt_sets_has_dynamic() {
        // `import('x');` as a top-level expression statement (fire-and-forget).
        let t = table("import('x');");
        assert!(
            t.has_dynamic(),
            "bare import('x'); expression statement should set has_dynamic"
        );
    }

    #[test]
    fn dynamic_import_non_literal_expression_stmt_sets_has_dynamic() {
        // `import(name);` — non-literal argument as expression statement.
        let t = table("import(name);");
        assert!(
            t.has_dynamic(),
            "import(name); expression statement should set has_dynamic"
        );
    }

    #[test]
    fn awaited_dynamic_import_in_async_fn_sets_has_dynamic() {
        // `await import(name);` inside an async function body.
        // Top-level await requires a module context that swc may or may not accept in
        // test mode, so wrap in an async arrow to ensure it parses cleanly.
        let t = table("const f = async () => { await import(name); };");
        assert!(
            t.has_dynamic(),
            "await import(name) inside a function body should set has_dynamic"
        );
    }

    #[test]
    fn static_import_only_does_not_set_has_dynamic() {
        // A file with only a static `import { x } from 'y';` must not set has_dynamic.
        let t = table("import { readFile } from 'node:fs';");
        assert!(
            !t.has_dynamic(),
            "static import only should NOT set has_dynamic"
        );
    }
}
