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

use std::collections::{HashMap, HashSet};

use swc_ecma_ast::{
    CallExpr, Callee, Decl, DefaultDecl, Expr, ExprStmt, Lit, Module, ModuleDecl, ModuleItem, Pat,
    Stmt, VarDecl,
};
use swc_ecma_visit::{Visit, VisitWith};

use crate::detect::mutation::collect_pat_bindings;

/// Collect the names introduced by **top-level** declarations of `module`.
///
/// These are the module's own shared bindings: top-level `const`/`let`/`var`
/// declarators (including destructuring patterns), `function` declarations, and
/// `class` declarations — bare, `export`ed (`export const`/`export function`/
/// `export class`), or a **named** default (`export default function f(){}` /
/// `export default class C{}`, which binds `f`/`C`). Only the module body is
/// scanned; names introduced inside function bodies are NOT collected, so a
/// write to one of these names from inside a function is a write to
/// module-shared state (the "module var used for cross-component communication"
/// anti-pattern), which the mutation walker escalates to `global.mutation`
/// (issue #29).
///
/// **Not** collected: export specifiers / re-exports (`export { foo }`,
/// `export { foo as bar }`, `export * from "x"`) introduce no local declaration;
/// anonymous default exports have no name to bind; TS-only forms
/// (`interface`/`type`) have no runtime binding. `enum`/`namespace` DO bind at
/// runtime but mutating module-shared enum/namespace state is an accepted miss
/// for this pass (revisit if dogfooding surfaces it). Likewise, only **direct**
/// module-body declaration items are scanned: a `var` hoisted out of a top-level
/// `if`/`for` block (`if (c) { var shared = 0; }`) is an accepted miss.
///
/// A mutated module `const`'s contents (`sharedMap.set(...)`, `arr.push(...)`)
/// already registers as a write on the base ident, so collecting the `const`
/// name is enough — no `const`-vs-`let` special-casing is needed.
pub fn module_bindings(module: &Module) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &module.body {
        // Bare top-level declarations, `export`ed ones, and NAMED default
        // exports contribute. Export specifiers / re-exports and anonymous
        // defaults do not (see doc above).
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                match &export.decl {
                    DefaultDecl::Fn(f) => {
                        if let Some(ident) = &f.ident {
                            out.insert(ident.sym.to_string());
                        }
                    }
                    DefaultDecl::Class(c) => {
                        if let Some(ident) = &c.ident {
                            out.insert(ident.sym.to_string());
                        }
                    }
                    DefaultDecl::TsInterfaceDecl(_) => {}
                }
                continue;
            }
            _ => continue,
        };
        match decl {
            Decl::Var(var) => {
                for d in &var.decls {
                    collect_pat_bindings(&d.name, &mut out);
                }
            }
            Decl::Fn(f) => {
                out.insert(f.ident.sym.to_string());
            }
            Decl::Class(c) => {
                out.insert(c.ident.sym.to_string());
            }
            // TS-only / enum / namespace forms: see doc above (accepted misses).
            _ => {}
        }
    }
    out
}

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

    #[test]
    fn module_bindings_collects_top_level_only() {
        use crate::functions;
        use crate::source::Lang;
        let src = "\
import x from 'm';\n\
const sharedMap = new Map();\n\
let counter = 0;\n\
var legacy;\n\
const { a, b } = obj;\n\
function helper() {}\n\
class Box {}\n\
export const exported = 1;\n\
export function exportedFn() {}\n\
export class ExportedClass {}\n\
export default function namedDefault() {}\n\
function withLocals() { const innerLocal = 1; let p = 2; return innerLocal + p; }\n";
        let (module, _cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let mb = module_bindings(&module);
        // Top-level declarations are collected — bare, exported, and named default.
        // Note `withLocals` itself IS a top-level function, so its NAME is collected;
        // only the bindings *inside* its body are not (asserted below).
        for name in [
            "sharedMap",
            "counter",
            "legacy",
            "a",
            "b",
            "helper",
            "Box",
            "exported",
            "exportedFn",
            "ExportedClass",
            "namedDefault",
            "withLocals",
        ] {
            assert!(
                mb.contains(name),
                "expected module binding `{name}`, got {mb:?}"
            );
        }
        // Function-body locals are NOT collected:
        assert!(
            !mb.contains("innerLocal"),
            "function-body local leaked into module_bindings"
        );
        assert!(
            !mb.contains("p"),
            "function-body local leaked into module_bindings"
        );
        // Imported names are NOT module-owned declarations (they live in the ImportTable):
        assert!(
            !mb.contains("x"),
            "imported name leaked into module_bindings"
        );

        // A module allows only one `export default`, so the named-default CLASS branch
        // needs its own parse (the fixture above exercised the default FUNCTION branch):
        let (m2, _cm2) = functions::parse_module(
            "export default class NamedDefaultClass {}",
            "t2.ts",
            Lang::Ts,
        )
        .expect("parse");
        assert!(
            module_bindings(&m2).contains("NamedDefaultClass"),
            "named `export default class` should be collected"
        );
    }
}
