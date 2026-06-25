//! Python module map: dotted module keys via `__init__.py` package roots, and
//! absolute/relative import resolution against the in-batch set, by path
//! convention (spec 025-3e §5.3). No disk, no sys.path, no libcst.

use std::collections::HashSet;

use fxrank_core::frontend::SourceFile;

pub struct PyModuleMap {
    keys: HashSet<Vec<String>>,
    // dir paths (with trailing '/') that contain an __init__.py in the batch.
    pkg_dirs: HashSet<String>,
}

impl PyModuleMap {
    pub fn build(files: &[SourceFile]) -> Self {
        let mut pkg_dirs = HashSet::new();
        for f in files {
            if f.path.ends_with("/__init__.py") || f.path == "__init__.py" {
                pkg_dirs.insert(dir_of(&f.path));
            }
        }
        let mut keys = HashSet::new();
        for f in files {
            if !f.path.ends_with(".py") {
                continue;
            }
            if let Some(k) = dotted_key(&f.path, &pkg_dirs) {
                keys.insert(k);
            }
        }
        Self { keys, pkg_dirs }
    }

    pub fn module_of(&self, file_path: &str) -> Option<Vec<String>> {
        if !file_path.ends_with(".py") {
            return None;
        }
        dotted_key(file_path, &self.pkg_dirs)
    }

    /// True when the file is a package `__init__.py` (its module key IS its package).
    pub fn is_package(&self, file_path: &str) -> bool {
        file_path.ends_with("/__init__.py") || file_path == "__init__.py"
    }

    pub fn resolve_absolute(&self, dotted: &str) -> Option<Vec<String>> {
        let segs: Vec<String> = dotted.split('.').map(|s| s.to_string()).collect();
        if self.keys.contains(&segs) {
            Some(segs)
        } else {
            None
        }
    }

    /// Resolve a relative import. The relative anchor is the importing module's
    /// PACKAGE: the key itself when the importer is a package `__init__.py`
    /// (`is_package`), else the key minus its module stem. `level` dots then walk
    /// up `level-1` more packages from that anchor (Python: level 1 = the package
    /// containing the importer). This `is_package` distinction is REQUIRED — a key
    /// like `["pkg","sub"]` is ambiguous (regular module `pkg/sub.py` vs package
    /// `pkg/sub/__init__.py`) and the two anchor differently.
    pub fn resolve_relative(
        &self,
        referencing: &[String],
        is_package: bool,
        level: usize,
        suffix: &str,
    ) -> Option<Vec<String>> {
        if level == 0 {
            return None; // not a relative import
        }
        let anchor: Vec<String> = if is_package {
            referencing.to_vec()
        } else if referencing.is_empty() {
            return None;
        } else {
            referencing[..referencing.len() - 1].to_vec()
        };
        // A relative import REQUIRES a containing package. An empty anchor means
        // the referencing module has no parent package (a top-level `top.py`, or a
        // file under a non-`__init__` dir) — Python errors here ("no known parent
        // package"), so we must NOT resolve against a root-level module. (P2, round 3)
        if anchor.is_empty() {
            return None;
        }
        let up = level - 1; // level 1 = the anchor package itself
        if up > anchor.len() {
            return None; // escaped above the top package
        }
        let mut target: Vec<String> = anchor[..anchor.len() - up].to_vec();
        if !suffix.is_empty() {
            target.extend(suffix.split('.').map(|s| s.to_string()));
        }
        if self.keys.contains(&target) {
            Some(target)
        } else {
            None
        }
    }
}

/// Directory of a path, WITH trailing '/'. `"pkg/sub/mod.py"` → `"pkg/sub/"`.
fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..=i].to_string(),
        None => String::new(),
    }
}

/// Dotted module key for a `.py` file: walk up from its dir while each dir is a
/// package (has `__init__.py` in the batch); the outermost non-package dir is the
/// root (excluded). An `__init__.py` keys to its package (no `__init__` segment).
fn dotted_key(path: &str, pkg_dirs: &HashSet<String>) -> Option<Vec<String>> {
    let stem = path.strip_suffix(".py")?;
    // Split into directory segments + file stem.
    let (dir_part, file_stem) = match stem.rfind('/') {
        Some(i) => (&stem[..i], &stem[i + 1..]),
        None => ("", stem),
    };
    // Collect the package segments: starting at the file's dir, walk up while the
    // dir is a package. Build the dir prefix incrementally to test membership.
    let dir_segs: Vec<&str> = if dir_part.is_empty() {
        Vec::new()
    } else {
        dir_part.split('/').collect()
    };
    // Find the deepest ancestor index that is NOT a package → everything below it is the module path.
    let mut first_pkg = dir_segs.len(); // index of the first package dir from the left
    for i in (0..dir_segs.len()).rev() {
        let prefix = format!("{}/", dir_segs[..=i].join("/"));
        if pkg_dirs.contains(&prefix) {
            first_pkg = i;
        } else {
            break;
        }
    }
    let mut segs: Vec<String> = dir_segs[first_pkg..]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if file_stem != "__init__" {
        segs.push(file_stem.to_string());
    }
    Some(segs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::frontend::SourceFile;
    fn sf(p: &str) -> SourceFile {
        SourceFile {
            path: p.into(),
            text: String::new(),
        }
    }

    fn batch() -> Vec<SourceFile> {
        vec![
            sf("pkg/__init__.py"),
            sf("pkg/sub/__init__.py"),
            sf("pkg/sub/mod.py"),
            sf("pkg/util.py"),
            sf("top.py"), // no __init__.py sibling → top-level module
        ]
    }

    #[test]
    fn module_key_via_init_packages() {
        let m = PyModuleMap::build(&batch());
        assert_eq!(
            m.module_of("pkg/sub/mod.py"),
            Some(vec!["pkg".into(), "sub".into(), "mod".into()])
        );
        assert_eq!(
            m.module_of("pkg/util.py"),
            Some(vec!["pkg".into(), "util".into()])
        );
        assert_eq!(
            m.module_of("pkg/sub/__init__.py"),
            Some(vec!["pkg".into(), "sub".into()])
        );
        assert_eq!(m.module_of("top.py"), Some(vec!["top".into()]));
    }

    #[test]
    fn resolve_absolute_in_batch_only() {
        let m = PyModuleMap::build(&batch());
        assert_eq!(
            m.resolve_absolute("pkg.sub.mod"),
            Some(vec!["pkg".into(), "sub".into(), "mod".into()])
        );
        assert_eq!(
            m.resolve_absolute("pkg.util"),
            Some(vec!["pkg".into(), "util".into()])
        );
        assert_eq!(m.resolve_absolute("os.path"), None); // stdlib, not in batch
        assert_eq!(m.resolve_absolute("pkg.missing"), None);
    }

    #[test]
    fn resolve_relative_via_package_walk() {
        let m = PyModuleMap::build(&batch());
        let mod_ref = vec!["pkg".to_string(), "sub".into(), "mod".into()]; // regular module pkg/sub/mod.py
        // from pkg.sub.mod (regular, is_package=false): `from .. import util` (level 2) →
        // anchor=pkg.sub, up=1 → pkg, + "util" = pkg.util
        assert_eq!(
            m.resolve_relative(&mod_ref, false, 2, "util"),
            Some(vec!["pkg".into(), "util".into()])
        );
        // `from . import mod` (level 1) from pkg.sub.mod → anchor=pkg.sub, up=0 → pkg.sub, + "mod"
        assert_eq!(
            m.resolve_relative(&mod_ref, false, 1, "mod"),
            Some(vec!["pkg".into(), "sub".into(), "mod".into()])
        );
        // level exceeding depth → None
        assert_eq!(m.resolve_relative(&["top".into()], false, 3, "x"), None);
    }

    #[test]
    fn resolve_relative_from_package_init_anchors_at_itself() {
        // The C1 case: referencing module is the PACKAGE __init__ (key ["pkg","sub"],
        // is_package=true). `from . import mod` (level 1) must anchor at pkg.sub ITSELF
        // (not pkg) → pkg.sub.mod. The off-by-one bug would give pkg.mod (None).
        let m = PyModuleMap::build(&batch());
        let pkg_ref = vec!["pkg".to_string(), "sub".into()]; // pkg/sub/__init__.py
        assert_eq!(
            m.resolve_relative(&pkg_ref, true, 1, "mod"),
            Some(vec!["pkg".into(), "sub".into(), "mod".into()])
        );
        // `from .. import util` (level 2) from the pkg.sub package → anchor=pkg.sub, up=1 → pkg, +util
        assert_eq!(
            m.resolve_relative(&pkg_ref, true, 2, "util"),
            Some(vec!["pkg".into(), "util".into()])
        );
    }

    #[test]
    fn relative_import_from_top_level_module_is_none() {
        // top.py (no parent package): `from .util import write` is invalid Python
        // ("no known parent package") — must NOT resolve to a root-level util. (P2 round 3)
        let m = PyModuleMap::build(&batch());
        assert_eq!(m.resolve_relative(&["top".into()], false, 1, "util"), None);
    }
}
