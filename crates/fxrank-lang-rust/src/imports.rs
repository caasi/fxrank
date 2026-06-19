//! Import table for `use`-statements in a Rust source file.
//!
//! Resolves a local name (as it appears after a `use` declaration) back to its
//! fully-qualified `::` path, and flags glob imports (`use foo::*;`) that make
//! path-matching uncertain.  Best-effort only: leading `self`/`crate`/`super`
//! segments are kept verbatim rather than resolved against a cargo graph, which
//! is acceptable because the table feeds heuristics + a confidence penalty
//! (×0.9 for glob) rather than hard guarantees.

use std::collections::HashMap;
use syn::{Item, UseTree};

/// Mapping from local names to their fully-qualified paths, built from the
/// `use` declarations in a single `syn::File`.
pub struct ImportTable {
    map: HashMap<String, String>,
    has_glob: bool,
}

impl ImportTable {
    /// Build an `ImportTable` from the top-level items of a parsed file.
    pub fn from_file(file: &syn::File) -> Self {
        let mut table = ImportTable {
            map: HashMap::new(),
            has_glob: false,
        };
        for item in &file.items {
            if let Item::Use(u) = item {
                table.walk_tree(&u.tree, &[]);
            }
        }
        table
    }

    /// Walk a `UseTree` recursively, accumulating path `prefix` segments.
    fn walk_tree(&mut self, tree: &UseTree, prefix: &[String]) {
        match tree {
            UseTree::Path(p) => {
                let mut next = prefix.to_vec();
                next.push(p.ident.to_string());
                self.walk_tree(&p.tree, &next);
            }
            UseTree::Name(n) => {
                let local = n.ident.to_string();
                let mut parts = prefix.to_vec();
                parts.push(local.clone());
                self.map.insert(local, parts.join("::"));
            }
            UseTree::Rename(r) => {
                let original = r.ident.to_string();
                let alias = r.rename.to_string();
                let mut parts = prefix.to_vec();
                parts.push(original);
                self.map.insert(alias, parts.join("::"));
            }
            UseTree::Glob(_) => {
                self.has_glob = true;
            }
            UseTree::Group(g) => {
                for item in &g.items {
                    self.walk_tree(item, prefix);
                }
            }
        }
    }

    /// Resolve a local name to its fully-qualified path.
    ///
    /// Returns `None` if the name is not covered by any `use` declaration in
    /// this file (e.g. a crate-root or fully-qualified path written inline).
    pub fn resolve(&self, local: &str) -> Option<&str> {
        self.map.get(local).map(String::as_str)
    }

    /// Returns `true` if any `use foo::*;` glob import was found.
    ///
    /// Callers apply a ×0.9 confidence penalty when this is `true`, because a
    /// glob import means a bare name like `write` might resolve to an unknown
    /// full path that cannot be matched against a known effect list.
    pub fn has_glob(&self) -> bool {
        self.has_glob
    }
}

#[cfg(test)]
mod tests {
    use super::ImportTable;

    #[test]
    fn import_table_resolves_aliases_and_flags_glob() {
        let file =
            syn::parse_file("use std::fs; use std::fs as filesystem; use std::io::*;").unwrap();
        let t = ImportTable::from_file(&file);
        assert_eq!(t.resolve("fs"), Some("std::fs"));
        assert_eq!(t.resolve("filesystem"), Some("std::fs"));
        assert!(t.has_glob());
    }

    #[test]
    fn import_table_handles_groups() {
        let file = syn::parse_file("use std::{fs, io::Read};").unwrap();
        let t = ImportTable::from_file(&file);
        assert_eq!(t.resolve("fs"), Some("std::fs"));
        assert_eq!(t.resolve("Read"), Some("std::io::Read"));
        assert!(!t.has_glob());
    }
}
