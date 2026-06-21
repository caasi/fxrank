//! Import-resolution table: `import`, `from … import`, and `… as …` forms.
//!
//! `Imports::build` walks the **entire file**, recursing into function and class
//! bodies (nested suites), and fills a map from **local name** → **fully-qualified
//! module path** (a dot-joined string). Function-local imports are therefore
//! resolved file-wide (an intentional over-approximation for a syntactic tool).
//! This mirrors the Rust frontend's `imports` module and feeds the call-site
//! detector so it can identify high-risk imported symbols.
//!
//! # Mapping rules
//!
//! | Source form                  | local key | resolved path |
//! |------------------------------|-----------|---------------|
//! | `import os`                  | `os`      | `"os"`        |
//! | `import a.b.c`               | `a`       | `"a.b.c"`     |
//! | `import numpy as np`         | `np`      | `"numpy"`     |
//! | `from subprocess import run` | `run`     | `"subprocess.run"` |
//! | `from m import n as p`       | `p`       | `"m.n"`       |
//!
//! For `import a.b.c` (no alias) the local key is the root component (`a`).
//! Aliases always win: `import a.b as x` → key `x`, path `"a.b"`.
//!
//! # `has_dynamic`
//!
//! Set to `true` when `importlib` appears as an imported module name or
//! `__import__` appears as an imported name inside a `from … import` statement.
//! This detects dynamic-import *infrastructure* being imported, not call-site
//! usage; actual call-site detection (e.g. `importlib.import_module(…)`) is
//! handled by `detect/risk.rs`.

use std::collections::HashMap;

use libcst_native::{
    AssignTargetExpression, CompoundStatement, Expression, ImportNames, Module, NameOrAttribute,
    OrElse, SmallStatement, Statement, Suite,
};

/// A resolved import table built from import statements anywhere in a `Module`
/// (recursing into nested function and class bodies for file-wide resolution).
pub struct Imports {
    /// local name → fully-qualified path string.
    table: HashMap<String, String>,
    /// `true` when `importlib` or `__import__` appears as an imported name.
    dynamic: bool,
}

impl Imports {
    /// Build an `Imports` table from a parsed `Module`.
    ///
    /// Imports are collected **file-wide** — recursing into function and class
    /// bodies, not just the module's top-level statements — so a function-local
    /// `def f(): import subprocess; …` still resolves call-site names. The flat
    /// table is file-scoped (not block-scoped): an import in one function can
    /// resolve a same-named call in another. This over-approximation is acceptable
    /// for a syntactic heuristic (see spec *Deferred / Future work*).
    pub fn build(module: &Module) -> Self {
        let mut table: HashMap<String, String> = HashMap::new();
        let mut dynamic = false;
        for stmt in &module.body {
            collect_stmt(stmt, &mut table, &mut dynamic);
        }
        Self { table, dynamic }
    }

    /// Resolve a local name to its fully-qualified module path.
    ///
    /// Returns `None` when the name was not introduced by any import statement.
    pub fn resolve(&self, local: &str) -> Option<&str> {
        self.table.get(local).map(|s| s.as_str())
    }

    /// `true` when `importlib` or `__import__` appears as an imported name,
    /// indicating the file uses dynamic-import infrastructure. Callers may
    /// apply a confidence penalty when resolving import-dependent signals.
    pub fn has_dynamic(&self) -> bool {
        self.dynamic
    }
}

// ─── file-wide statement traversal ────────────────────────────────────────────

/// Collect imports from a statement, recursing into compound-statement bodies
/// (functions, classes, branches, loops) so function-local imports are captured.
fn collect_stmt(stmt: &Statement, table: &mut HashMap<String, String>, dynamic: &mut bool) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                collect_small(small, table, dynamic);
            }
        }
        Statement::Compound(c) => collect_compound(c, table, dynamic),
    }
}

fn collect_suite(suite: &Suite, table: &mut HashMap<String, String>, dynamic: &mut bool) {
    match suite {
        Suite::IndentedBlock(b) => {
            for stmt in &b.body {
                collect_stmt(stmt, table, dynamic);
            }
        }
        Suite::SimpleStatementSuite(s) => {
            for small in &s.body {
                collect_small(small, table, dynamic);
            }
        }
    }
}

fn collect_compound(
    c: &CompoundStatement,
    table: &mut HashMap<String, String>,
    dynamic: &mut bool,
) {
    match c {
        CompoundStatement::FunctionDef(d) => collect_suite(&d.body, table, dynamic),
        CompoundStatement::ClassDef(d) => collect_suite(&d.body, table, dynamic),
        CompoundStatement::If(i) => {
            collect_suite(&i.body, table, dynamic);
            if let Some(orelse) = &i.orelse {
                collect_orelse(orelse, table, dynamic);
            }
        }
        CompoundStatement::For(f) => {
            collect_suite(&f.body, table, dynamic);
            if let Some(e) = &f.orelse {
                collect_suite(&e.body, table, dynamic);
            }
        }
        CompoundStatement::While(w) => {
            collect_suite(&w.body, table, dynamic);
            if let Some(e) = &w.orelse {
                collect_suite(&e.body, table, dynamic);
            }
        }
        CompoundStatement::Try(t) => {
            collect_suite(&t.body, table, dynamic);
            for h in &t.handlers {
                collect_suite(&h.body, table, dynamic);
            }
            if let Some(e) = &t.orelse {
                collect_suite(&e.body, table, dynamic);
            }
            if let Some(e) = &t.finalbody {
                collect_suite(&e.body, table, dynamic);
            }
        }
        CompoundStatement::TryStar(t) => {
            collect_suite(&t.body, table, dynamic);
            for h in &t.handlers {
                collect_suite(&h.body, table, dynamic);
            }
            if let Some(e) = &t.orelse {
                collect_suite(&e.body, table, dynamic);
            }
            if let Some(e) = &t.finalbody {
                collect_suite(&e.body, table, dynamic);
            }
        }
        CompoundStatement::With(w) => collect_suite(&w.body, table, dynamic),
        CompoundStatement::Match(m) => {
            for case in &m.cases {
                collect_suite(&case.body, table, dynamic);
            }
        }
    }
}

fn collect_orelse(orelse: &OrElse, table: &mut HashMap<String, String>, dynamic: &mut bool) {
    match orelse {
        OrElse::Elif(elif) => {
            collect_suite(&elif.body, table, dynamic);
            if let Some(inner) = &elif.orelse {
                collect_orelse(inner, table, dynamic);
            }
        }
        OrElse::Else(e) => collect_suite(&e.body, table, dynamic),
    }
}

/// Record imports from a single small statement into `table` / `dynamic`.
fn collect_small(small: &SmallStatement, table: &mut HashMap<String, String>, dynamic: &mut bool) {
    match small {
        // `import a`, `import a.b.c`, `import a.b as x`
        SmallStatement::Import(imp) => {
            for alias in &imp.names {
                let path = noa_to_string(&alias.name);
                // Any importlib submodule (`importlib`, `importlib.util`, …) is a
                // dynamic-import surface, not just the bare package.
                if path == "importlib" || path.starts_with("importlib.") {
                    *dynamic = true;
                }
                let local = if let Some(asname) = &alias.asname {
                    ate_to_string(&asname.name)
                } else {
                    // bare `import a.b.c` — local key is root component
                    root_component(&path)
                };
                table.insert(local, path);
            }
        }
        // `from m import n`, `from m import n as p`
        SmallStatement::ImportFrom(from) => {
            let module_path = from
                .module
                .as_ref()
                .map(|m| noa_to_string(m))
                .unwrap_or_default();
            if module_path == "importlib" || module_path.starts_with("importlib.") {
                *dynamic = true;
            }
            let ImportNames::Aliases(aliases) = &from.names else {
                // `from m import *` — skip; we can't know the local names
                return;
            };
            for alias in aliases {
                let name = noa_to_string(&alias.name);
                if name == "__import__" {
                    *dynamic = true;
                }
                let full = if module_path.is_empty() {
                    name.clone()
                } else {
                    format!("{module_path}.{name}")
                };
                let local = if let Some(asname) = &alias.asname {
                    ate_to_string(&asname.name)
                } else {
                    name
                };
                table.insert(local, full);
            }
        }
        _ => {}
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────────

/// Convert a `NameOrAttribute` node to its dot-joined string representation.
///
/// `Name("os")` → `"os"`, `Attribute(Name("a"), Name("b"))` → `"a.b"`.
fn noa_to_string(noa: &NameOrAttribute) -> String {
    match noa {
        NameOrAttribute::N(n) => n.value.to_owned(),
        NameOrAttribute::A(a) => {
            // Flatten `value.attr` recursively via the Expression::Attribute arm.
            let mut parts: Vec<String> = Vec::new();
            collect_attr_parts_expr(&a.value, &mut parts);
            parts.push(a.attr.value.to_owned());
            parts.join(".")
        }
    }
}

/// Collect dot-separated components from an `Expression` that may be a chain
/// of `Attribute` nodes (e.g. `a.b.c` → `["a", "b", "c"]`).
fn collect_attr_parts_expr(expr: &Expression, out: &mut Vec<String>) {
    match expr {
        Expression::Name(n) => out.push(n.value.to_owned()),
        Expression::Attribute(a) => {
            collect_attr_parts_expr(&a.value, out);
            out.push(a.attr.value.to_owned());
        }
        // Other expression forms are not expected in import positions.
        _ => {}
    }
}

/// Convert an `AssignTargetExpression` (the alias name, e.g. `as np`) to a string.
///
/// An import `asname` is always a plain `Name` (`import a as b.c` is a syntax
/// error), so only the `Name` arm is reachable; all other forms fall back to an
/// empty string.
fn ate_to_string(ate: &AssignTargetExpression) -> String {
    match ate {
        AssignTargetExpression::Name(n) => n.value.to_owned(),
        _ => String::new(),
    }
}

/// Return the first dot-component of a dotted path string.
///
/// `"a.b.c"` → `"a"`, `"os"` → `"os"`.
fn root_component(path: &str) -> String {
    path.split('.').next().unwrap_or(path).to_owned()
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn build_str(src: &str) -> Imports {
        Imports::build(&libcst_native::parse_module(src, None).unwrap())
    }

    #[test]
    fn resolves_import_forms() {
        let i = build_str("import os\nimport numpy as np\nfrom subprocess import run\n");
        assert_eq!(i.resolve("os"), Some("os"));
        assert_eq!(i.resolve("np"), Some("numpy"));
        assert_eq!(i.resolve("run"), Some("subprocess.run"));
    }

    #[test]
    fn resolves_dotted_import_without_alias() {
        // `import a.b.c` — local key is root component `a`
        let i = build_str("import a.b.c\n");
        assert_eq!(i.resolve("a"), Some("a.b.c"));
        assert_eq!(i.resolve("a.b.c"), None);
    }

    #[test]
    fn resolves_from_import_with_alias() {
        let i = build_str("from m import n as p\n");
        assert_eq!(i.resolve("p"), Some("m.n"));
        assert_eq!(i.resolve("n"), None);
    }

    #[test]
    fn from_import_star_does_not_crash() {
        // `from m import *` — nothing resolvable, but must not panic
        let i = build_str("from os.path import *\n");
        assert_eq!(i.resolve("join"), None);
        assert!(!i.has_dynamic());
    }

    #[test]
    fn detects_importlib_dynamic() {
        let i = build_str("import importlib\n");
        assert!(i.has_dynamic());
    }

    #[test]
    fn detects_from_importlib_dynamic() {
        let i = build_str("from importlib import import_module\n");
        assert!(i.has_dynamic());
    }

    #[test]
    fn detects_importlib_submodule_dynamic() {
        // submodules of importlib are also a dynamic-import surface
        assert!(build_str("import importlib.util\n").has_dynamic());
        assert!(build_str("from importlib.util import find_spec\n").has_dynamic());
    }

    #[test]
    fn no_dynamic_for_normal_imports() {
        let i = build_str("import os\nfrom sys import path\n");
        assert!(!i.has_dynamic());
    }

    /// FIX 5: imports declared INSIDE a function body must be collected file-wide,
    /// so a function-local `import subprocess` resolves the call-site name.
    #[test]
    fn resolves_function_local_imports() {
        let i = build_str("def f():\n    import subprocess\n    subprocess.run(c, shell=True)\n");
        assert_eq!(i.resolve("subprocess"), Some("subprocess"));
    }

    /// FIX 5: function-local `from … import …` and `importlib` dynamic-flagging
    /// also work inside nested scopes (class → method).
    #[test]
    fn resolves_nested_class_method_imports_and_dynamic() {
        let i = build_str(
            "class C:\n    def m(self):\n        from subprocess import run\n        import importlib\n",
        );
        assert_eq!(i.resolve("run"), Some("subprocess.run"));
        assert!(i.has_dynamic());
    }
}
