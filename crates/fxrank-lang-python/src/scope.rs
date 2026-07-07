//! Frontend-internal scope-aware binding resolution (spec 078).
//! See docs/scope-aware-binding-resolution-guideline.md. NOTHING here crosses
//! the UnitRecord boundary; fxrank-core knows none of these types.

use std::collections::{HashMap, HashSet};

use libcst_native::Module;

use crate::imports::Imports;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // not yet wired into the mutation detector (spec 078, Task 2)
pub(crate) enum BindingOrigin {
    Param,
    Local,
    ModuleImport,
    FromImport,
    ModuleBinding,
    Global,
    EnclosingCaptured,
    HostGlobalIo,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // not yet wired into the mutation detector (spec 078, Task 2)
pub(crate) struct BindingId {
    pub scope_segments: Vec<String>,
    pub kind_name: String,
}

/// The `"kind:"` prefix used in a `BindingId.kind_name`, per the guideline's
/// binding-id scheme (kind-tagged names keep same-name/different-origin
/// bindings distinct even within one scope).
#[allow(dead_code)] // not yet wired into the mutation detector (spec 078, Task 2)
fn kind_prefix(origin: BindingOrigin) -> &'static str {
    match origin {
        BindingOrigin::Param => "param",
        BindingOrigin::Local => "local",
        BindingOrigin::ModuleImport => "import",
        BindingOrigin::FromImport => "from",
        BindingOrigin::ModuleBinding => "module",
        BindingOrigin::Global => "global",
        BindingOrigin::EnclosingCaptured => "captured",
        BindingOrigin::HostGlobalIo => "hostio",
        BindingOrigin::Unresolved => "unresolved",
    }
}

/// A scope-tree node: the module scope, or (from a later task on) a
/// function/method scope. `body_ptr` and `name` are `None` for the module
/// scope; a function scope fills both (`body_ptr` for `unit_scope_of`,
/// `name` for `scope_of_name`/`BindingId` construction).
#[allow(dead_code)] // fields consumed starting from the Task 3/4 parent walk
struct Scope {
    kind: ScopeKind,
    /// The enclosing scope; `None` for the module scope (the tree root).
    parent: Option<ScopeId>,
    /// The function body node's address, used by `unit_scope_of` to map a
    /// unit back to its scope. `None` for the module scope.
    body_ptr: Option<usize>,
    /// The def name, used by `scope_of_name`/`BindingId` construction.
    /// `None` for the module scope.
    name: Option<String>,
    bindings: HashMap<String, BindingOrigin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    Module,
    #[allow(dead_code)] // Function is not constructed until the Task 4 function-scope pass
    Function,
}

#[allow(dead_code)] // not yet wired into the mutation detector (spec 078, Task 2)
pub(crate) type ScopeId = usize;

/// The file's scope tree — `module → function/method → block` (spec 078).
/// This task builds only the module scope (id 0); later tasks add one scope
/// per function/method def and generalize `resolve` to the parent walk.
#[allow(dead_code)] // not yet wired into the mutation detector (spec 078, Task 2)
pub(crate) struct ScopeTree {
    scopes: Vec<Scope>,
}

impl ScopeTree {
    /// Build the scope tree from a parsed `Module`, its file-wide `Imports`
    /// table, and its module-level `module_bindings`.
    ///
    /// Module-scope construction order is load-bearing and byte-identical to
    /// the flat lookups it replaces: every `module_bindings` name is inserted
    /// first as `ModuleBinding`, then the imports overlay it as `ModuleImport`
    /// / `FromImport` — imports **overwrite** any same-named `ModuleBinding`,
    /// reproducing the existing mutation cascade's "imports (F5) checked
    /// before module_bindings (F7)" precedence.
    #[allow(dead_code)] // until Phase-2 consumers land
    pub(crate) fn build(
        _module: &Module,
        imports: &Imports,
        module_bindings: &HashSet<String>,
    ) -> Self {
        let mut bindings = HashMap::new();
        for name in module_bindings {
            bindings.insert(name.clone(), BindingOrigin::ModuleBinding);
        }
        for name in imports.iter_local_names() {
            let origin = if imports.is_module_import(name) {
                BindingOrigin::ModuleImport
            } else {
                BindingOrigin::FromImport
            };
            bindings.insert(name.to_owned(), origin);
        }
        let module_scope = Scope {
            kind: ScopeKind::Module,
            parent: None,
            body_ptr: None,
            name: None,
            bindings,
        };
        Self {
            scopes: vec![module_scope],
        }
    }

    /// The module scope's id — always `0`.
    #[allow(dead_code)] // until Phase-2 consumers land
    pub(crate) fn module_scope(&self) -> ScopeId {
        0
    }

    /// Resolve `name` in `scope`.
    ///
    /// Provisional: this task only checks `scope`'s own bindings map, then
    /// falls back to `Unresolved`. Task 3 generalizes this to the
    /// nearest-enclosing parent walk described in the guideline's
    /// resolution rule.
    #[allow(dead_code)] // until Phase-2 consumers land
    pub(crate) fn resolve(&self, scope: ScopeId, name: &str) -> (BindingOrigin, BindingId) {
        match self.scopes[scope].bindings.get(name) {
            Some(&origin) => {
                let kind_name = format!("{}:{name}", kind_prefix(origin));
                (
                    origin,
                    BindingId {
                        scope_segments: Vec::new(),
                        kind_name,
                    },
                )
            }
            None => (
                BindingOrigin::Unresolved,
                BindingId {
                    scope_segments: Vec::new(),
                    kind_name: format!("{}:{name}", kind_prefix(BindingOrigin::Unresolved)),
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_str(src: &str) -> ScopeTree {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let mb = crate::imports::module_bindings(&module);
        ScopeTree::build(&module, &imports, &mb)
    }

    #[test]
    fn module_scope_collects_module_bindings_and_file_wide_imports() {
        let t = build_str("import os\nfrom cfg import CACHE\nx = 0\n");
        let m = t.module_scope();
        assert_eq!(t.resolve(m, "os").0, BindingOrigin::ModuleImport);
        assert_eq!(t.resolve(m, "CACHE").0, BindingOrigin::FromImport);
        assert_eq!(t.resolve(m, "x").0, BindingOrigin::ModuleBinding);
        assert_eq!(t.resolve(m, "nope").0, BindingOrigin::Unresolved);
    }

    #[test]
    fn binding_origin_variants_exist() {
        // Compile-time coverage: every guideline origin is representable.
        let _ = [
            BindingOrigin::Param,
            BindingOrigin::Local,
            BindingOrigin::ModuleImport,
            BindingOrigin::FromImport,
            BindingOrigin::ModuleBinding,
            BindingOrigin::Global,
            BindingOrigin::EnclosingCaptured,
            BindingOrigin::HostGlobalIo,
            BindingOrigin::Unresolved,
        ];
        let id = BindingId {
            scope_segments: vec!["b3".into()],
            kind_name: "local:x".into(),
        };
        assert_eq!(id.kind_name, "local:x");
    }
}
