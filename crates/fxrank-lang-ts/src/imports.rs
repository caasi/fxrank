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

use swc_ecma_ast::{Callee, Decl, Expr, Lit, Module, ModuleDecl, ModuleItem, Pat, Stmt, VarDecl};

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
                _ => {}
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
    fn check_dynamic_import(&mut self, call: &swc_ecma_ast::CallExpr) {
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

/// Parse `src` as a TypeScript module and build an `ImportTable`.
///
/// This is a test-only convenience: it owns the swc parse plumbing and returns
/// the table. Production callers will use `from_module` with an already-parsed
/// `Module`.
#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
