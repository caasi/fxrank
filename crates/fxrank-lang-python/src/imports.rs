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

use std::collections::{HashMap, HashSet};

use libcst_native::{
    AssignTargetExpression, CompoundStatement, Element, Expression, ImportNames, Module,
    NameOrAttribute, OrElse, SmallStatement, Statement, Suite,
};

/// A resolved import table built from import statements anywhere in a `Module`
/// (recursing into nested function and class bodies for file-wide resolution).
pub struct Imports {
    /// local name → fully-qualified path string.
    table: HashMap<String, String>,
    /// Local names that came from a **relative** (leading-dot) `from`-import.
    /// e.g. `from . import sibling` and `from .utils import helper` both add to
    /// this set.  Plain `import` and absolute `from m import n` do not.
    relative_locals: HashSet<String>,
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
        let mut relative_locals: HashSet<String> = HashSet::new();
        let mut dynamic = false;
        for stmt in &module.body {
            collect_stmt(stmt, &mut table, &mut relative_locals, &mut dynamic);
        }
        Self {
            table,
            relative_locals,
            dynamic,
        }
    }

    /// Resolve a local name to its fully-qualified module path.
    ///
    /// Returns `None` when the name was not introduced by any import statement.
    pub fn resolve(&self, local: &str) -> Option<&str> {
        self.table.get(local).map(|s| s.as_str())
    }

    /// `true` when `local` was introduced by a **relative** (leading-dot)
    /// `from`-import (`from . import x`, `from .mod import y`, etc.).
    /// Returns `false` for absolute imports and unknown names.
    pub fn is_relative(&self, local: &str) -> bool {
        self.relative_locals.contains(local)
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
fn collect_stmt(
    stmt: &Statement,
    table: &mut HashMap<String, String>,
    relative_locals: &mut HashSet<String>,
    dynamic: &mut bool,
) {
    match stmt {
        Statement::Simple(line) => {
            for small in &line.body {
                collect_small(small, table, relative_locals, dynamic);
            }
        }
        Statement::Compound(c) => collect_compound(c, table, relative_locals, dynamic),
    }
}

fn collect_suite(
    suite: &Suite,
    table: &mut HashMap<String, String>,
    relative_locals: &mut HashSet<String>,
    dynamic: &mut bool,
) {
    match suite {
        Suite::IndentedBlock(b) => {
            for stmt in &b.body {
                collect_stmt(stmt, table, relative_locals, dynamic);
            }
        }
        Suite::SimpleStatementSuite(s) => {
            for small in &s.body {
                collect_small(small, table, relative_locals, dynamic);
            }
        }
    }
}

fn collect_compound(
    c: &CompoundStatement,
    table: &mut HashMap<String, String>,
    relative_locals: &mut HashSet<String>,
    dynamic: &mut bool,
) {
    match c {
        CompoundStatement::FunctionDef(d) => {
            collect_suite(&d.body, table, relative_locals, dynamic)
        }
        CompoundStatement::ClassDef(d) => collect_suite(&d.body, table, relative_locals, dynamic),
        CompoundStatement::If(i) => {
            collect_suite(&i.body, table, relative_locals, dynamic);
            if let Some(orelse) = &i.orelse {
                collect_orelse(orelse, table, relative_locals, dynamic);
            }
        }
        CompoundStatement::For(f) => {
            collect_suite(&f.body, table, relative_locals, dynamic);
            if let Some(e) = &f.orelse {
                collect_suite(&e.body, table, relative_locals, dynamic);
            }
        }
        CompoundStatement::While(w) => {
            collect_suite(&w.body, table, relative_locals, dynamic);
            if let Some(e) = &w.orelse {
                collect_suite(&e.body, table, relative_locals, dynamic);
            }
        }
        CompoundStatement::Try(t) => {
            collect_suite(&t.body, table, relative_locals, dynamic);
            for h in &t.handlers {
                collect_suite(&h.body, table, relative_locals, dynamic);
            }
            if let Some(e) = &t.orelse {
                collect_suite(&e.body, table, relative_locals, dynamic);
            }
            if let Some(e) = &t.finalbody {
                collect_suite(&e.body, table, relative_locals, dynamic);
            }
        }
        CompoundStatement::TryStar(t) => {
            collect_suite(&t.body, table, relative_locals, dynamic);
            for h in &t.handlers {
                collect_suite(&h.body, table, relative_locals, dynamic);
            }
            if let Some(e) = &t.orelse {
                collect_suite(&e.body, table, relative_locals, dynamic);
            }
            if let Some(e) = &t.finalbody {
                collect_suite(&e.body, table, relative_locals, dynamic);
            }
        }
        CompoundStatement::With(w) => collect_suite(&w.body, table, relative_locals, dynamic),
        CompoundStatement::Match(m) => {
            for case in &m.cases {
                collect_suite(&case.body, table, relative_locals, dynamic);
            }
        }
    }
}

fn collect_orelse(
    orelse: &OrElse,
    table: &mut HashMap<String, String>,
    relative_locals: &mut HashSet<String>,
    dynamic: &mut bool,
) {
    match orelse {
        OrElse::Elif(elif) => {
            collect_suite(&elif.body, table, relative_locals, dynamic);
            if let Some(inner) = &elif.orelse {
                collect_orelse(inner, table, relative_locals, dynamic);
            }
        }
        OrElse::Else(e) => collect_suite(&e.body, table, relative_locals, dynamic),
    }
}

/// Record imports from a single small statement into `table` / `relative_locals` / `dynamic`.
fn collect_small(
    small: &SmallStatement,
    table: &mut HashMap<String, String>,
    relative_locals: &mut HashSet<String>,
    dynamic: &mut bool,
) {
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
                // Plain `import` is never relative — no dots needed here.
            }
        }
        // `from m import n`, `from m import n as p`,
        // `from . import x`, `from .mod import y` (leading dots = relative)
        SmallStatement::ImportFrom(from) => {
            let is_relative = !from.relative.is_empty();
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
                if is_relative {
                    relative_locals.insert(local.clone());
                }
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

// ─── module top-level binding collector ───────────────────────────────────────

/// Collect the names introduced by **module top-level** statements of `module`:
/// assignment targets (`x = …`, `x: T = …`, and destructured `a, b = …` /
/// `[x, y] = …` / `*rest, last = …`), `def` names, and `class` names. Only the
/// module body is scanned; names bound inside function bodies are not collected.
/// A write whose root is one of these — when it is not a local/param/`global`-
/// declared/import in the writing function — is a write to module-shared state,
/// escalated to `global.mutation` (the Python analog of #29).
///
/// The **function-body prescan** (`detect/mutation.rs`) now covers for/with-as/
/// except-as locals: a `for _cache in …`, `with ctx as _cache`, or
/// `except E as _cache` binding shadows the module-level name inside that
/// function and is collected as a local, preventing false escalation to
/// `global.mutation`.
///
/// Not collected (accepted misses, consistent with the syntactic flat-scope
/// approximation in the other frontends): import names (handled by the F5 import
/// arm via the `Imports` table); subscript/attribute assignment targets (not new
/// bindings); names bound by module top-level `for`/`with … as`/`except … as`/
/// `match` patterns; names bound only inside nested blocks/comprehensions.
/// **Residual accepted limits in the prescan** (not chased): `match` pattern
/// captures, comprehension-scope targets (Python 3 gives them their own scope),
/// and walrus (`:=`) operator targets.
pub fn module_bindings(module: &Module) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in &module.body {
        match stmt {
            Statement::Simple(line) => {
                for small in &line.body {
                    match small {
                        SmallStatement::Assign(a) => {
                            for target in &a.targets {
                                collect_target_names(&target.target, &mut out);
                            }
                        }
                        SmallStatement::AnnAssign(a) => {
                            collect_target_names(&a.target, &mut out);
                        }
                        _ => {}
                    }
                }
            }
            Statement::Compound(c) => match c {
                CompoundStatement::FunctionDef(f) => {
                    out.insert(f.name.value.to_owned());
                }
                CompoundStatement::ClassDef(c) => {
                    out.insert(c.name.value.to_owned());
                }
                _ => {}
            },
        }
    }
    out
}

/// Collect bound names from an assignment target, recursing into destructuring.
/// Attribute/Subscript targets bind no new name. Mirrors
/// `detect::walk_assign_target_subexprs`'s enum shape.
pub(crate) fn collect_target_names(target: &AssignTargetExpression, out: &mut HashSet<String>) {
    match target {
        AssignTargetExpression::Name(n) => {
            out.insert(n.value.to_owned());
        }
        AssignTargetExpression::Tuple(t) => {
            for el in &t.elements {
                collect_element_names(el, out);
            }
        }
        AssignTargetExpression::List(l) => {
            for el in &l.elements {
                collect_element_names(el, out);
            }
        }
        AssignTargetExpression::StarredElement(s) => collect_expr_target_names(&s.value, out),
        AssignTargetExpression::Attribute(_) | AssignTargetExpression::Subscript(_) => {}
    }
}

/// A destructuring element (`(a, *rest) = …`). Mirrors `detect::walk_target_element`.
pub(crate) fn collect_element_names(el: &Element, out: &mut HashSet<String>) {
    match el {
        Element::Simple { value, .. } => collect_expr_target_names(value, out),
        Element::Starred(s) => collect_expr_target_names(&s.value, out),
    }
}

/// Destructuring elements are typed as `Expression`. Mirrors `detect::walk_target_value`.
pub(crate) fn collect_expr_target_names(expr: &Expression, out: &mut HashSet<String>) {
    match expr {
        Expression::Name(n) => {
            out.insert(n.value.to_owned());
        }
        Expression::Tuple(t) => {
            for el in &t.elements {
                collect_element_names(el, out);
            }
        }
        Expression::List(l) => {
            for el in &l.elements {
                collect_element_names(el, out);
            }
        }
        Expression::StarredElement(s) => collect_expr_target_names(&s.value, out),
        _ => {}
    }
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

    #[test]
    fn is_relative_detects_leading_dot_imports() {
        // `from .utils import helper` and `from . import sibling` are relative
        let i = build_str("from .utils import helper\nfrom . import sibling\nimport os\n");
        assert!(i.is_relative("helper"), "helper must be relative");
        assert!(i.is_relative("sibling"), "sibling must be relative");
        assert!(!i.is_relative("os"), "os must not be relative");
        assert!(!i.is_relative("unknown"), "unknown must not be relative");
    }

    #[test]
    fn module_bindings_collects_top_level_only() {
        let src = "\
import config\n\
_counter = 0\n\
shared_map = {}\n\
A, B = 1, 2\n\
[x, y] = [3, 4]\n\
def helper():\n    inner_local = 1\n    return inner_local\n\
class Box:\n    pass\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let mb = module_bindings(&module);
        // Bare names, destructured tuple/list targets, def + class names all collected:
        for name in [
            "_counter",
            "shared_map",
            "A",
            "B",
            "x",
            "y",
            "helper",
            "Box",
        ] {
            assert!(
                mb.contains(name),
                "expected module binding `{name}`, got {mb:?}"
            );
        }
        // Function-body locals are NOT collected:
        assert!(
            !mb.contains("inner_local"),
            "function-body local leaked into module_bindings"
        );
        // Imported names live in the Imports table, not here:
        assert!(
            !mb.contains("config"),
            "imported name leaked into module_bindings"
        );
    }
}
