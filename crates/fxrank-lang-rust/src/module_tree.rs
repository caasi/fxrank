//! Crate module-tree reconstruction from a flat `SourceFile` batch, by
//! filesystem-path convention (spec 025-3e §5.1). No AST, no disk, no cargo.
//!
//! Maps each in-batch file path to its crate-relative module segments. `#[path]`
//! attributes and inline-`#[path]` are NOT honored (documented misses → the file
//! degrades to an empty canonical_path).

use std::collections::HashMap;

use fxrank_core::frontend::SourceFile;

pub struct ModuleTree {
    /// file path → crate-relative module segments (`[]` for a crate root).
    by_path: HashMap<String, Vec<String>>,
}

impl ModuleTree {
    pub fn build(files: &[SourceFile]) -> Self {
        // 1. Collect crate source-root directories from root files in the batch.
        //    A root file is `lib.rs`/`main.rs` DIRECTLY under a `src/` dir, or any
        //    file directly under a `src/bin/` dir (each its own binary crate root).
        let mut roots: Vec<String> = Vec::new(); // source-root directory prefixes (incl. trailing '/')
        let mut bin_files: Vec<String> = Vec::new();
        for f in files {
            let p = f.path.as_str();
            if let Some(dir) = root_dir_of(p) {
                if !roots.contains(&dir) {
                    roots.push(dir);
                }
            }
            if is_bin_file(p) {
                bin_files.push(p.to_string());
            }
        }

        // 2. Map each file to module segments relative to its owning source root.
        //    The bin check runs FIRST (a bin file is its own crate root, module []),
        //    BEFORE the longest-root-prefix assignment — reordering breaks bin
        //    classification (a src/bin/tool.rs would otherwise get ["bin","tool"]).
        let mut by_path = HashMap::new();
        for f in files {
            let p = f.path.as_str();
            if bin_files.contains(&p.to_string()) {
                by_path.insert(p.to_string(), vec![]); // a bin file is its own root
                continue;
            }
            // Find the longest matching source root that is a prefix of this file.
            if let Some(root) = roots
                .iter()
                .filter(|r| p.starts_with(r.as_str()))
                .max_by_key(|r| r.len())
            {
                let rel = &p[root.len()..]; // e.g. "util/config.rs"
                by_path.insert(p.to_string(), segments_of(rel));
            }
            // else: no crate root in scope → omit (module_of returns None).
        }

        Self { by_path }
    }

    pub fn module_of(&self, file_path: &str) -> Option<Vec<String>> {
        self.by_path.get(file_path).cloned()
    }
}

/// If `path` is a crate root, return its source-root dir (with a trailing '/').
///
/// A crate root is a `lib.rs`/`main.rs` whose PARENT directory is named `src`
/// (the standard `<crate>/src/lib.rs` layout), OR a `src/bin/<name>/main.rs`
/// multi-file binary root. A `main.rs`/`lib.rs` nested deeper (e.g.
/// `src/cli/main.rs`) is a regular out-of-line module, NOT a crate root — so it
/// must NOT create a spurious source root. (spec 025-3e §5.1)
fn root_dir_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name != "lib.rs" && name != "main.rs" {
        return None;
    }
    let dir = &path[..path.len() - name.len()]; // includes trailing '/'
    let trimmed = dir.strip_suffix('/').unwrap_or(dir);
    let parent = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if parent == "src" {
        // standard <crate>/src/lib.rs | <crate>/src/main.rs
        Some(dir.to_string())
    } else if name == "main.rs" && dir.contains("/src/bin/") {
        // src/bin/<name>/main.rs → its own binary crate root
        Some(dir.to_string())
    } else if !path.contains("/src/") && !path.starts_with("src/") {
        // No `src/` ancestor at all (a directly-scanned bare `lib.rs`/`main.rs`,
        // or a non-`src` project layout). Treat as a standalone crate root so the
        // partition still adopts. (spec 025-3e §5.1 — a scanned lib.rs/main.rs is a root.)
        Some(dir.to_string())
    } else {
        None // nested inside a `src/` tree but not at `src/` → a module, not a root
    }
}

/// A file directly inside a `src/bin/` directory (`…/src/bin/tool.rs`). Each is
/// its own binary crate root (module `[]`). (A `src/bin/<name>/main.rs` is caught
/// by `root_dir_of` as a normal `main.rs` root.)
fn is_bin_file(path: &str) -> bool {
    if !path.ends_with(".rs") {
        return false;
    }
    if let Some(idx) = path.rfind("/src/bin/") {
        let rest = &path[idx + "/src/bin/".len()..];
        // direct child only: no further '/'
        !rest.contains('/')
    } else {
        false
    }
}

/// Convert a source-root-relative path (`util/config.rs`, `net/mod.rs`, `lib.rs`)
/// to module segments: drop `.rs`; a trailing `mod` always adds no segment
/// (directory owner); a trailing `lib`/`main` adds no segment ONLY when it is the
/// sole segment (the crate root file itself — `lib.rs`/`main.rs` at the source
/// root). A NESTED `cli/main.rs` keeps `main` (→ `["cli","main"]`), since it is a
/// regular module, not a crate root.
fn segments_of(rel: &str) -> Vec<String> {
    let stem = rel.strip_suffix(".rs").unwrap_or(rel);
    let mut segs: Vec<String> = stem.split('/').map(|s| s.to_string()).collect();
    match segs.last().map(String::as_str) {
        Some("mod") => {
            segs.pop();
        }
        Some("lib") | Some("main") if segs.len() == 1 => {
            segs.pop();
        }
        _ => {}
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::frontend::SourceFile;

    fn sf(path: &str) -> SourceFile {
        SourceFile {
            path: path.into(),
            text: String::new(),
        }
    }

    #[test]
    fn maps_files_to_crate_relative_modules() {
        let files = vec![
            sf("crates/foo/src/lib.rs"),
            sf("crates/foo/src/util.rs"),
            sf("crates/foo/src/util/config.rs"),
            sf("crates/foo/src/net/mod.rs"),
            sf("crates/foo/src/net/http.rs"),
        ];
        let mt = ModuleTree::build(&files);
        assert_eq!(mt.module_of("crates/foo/src/lib.rs"), Some(vec![]));
        assert_eq!(
            mt.module_of("crates/foo/src/util.rs"),
            Some(vec!["util".into()])
        );
        assert_eq!(
            mt.module_of("crates/foo/src/util/config.rs"),
            Some(vec!["util".into(), "config".into()])
        );
        assert_eq!(
            mt.module_of("crates/foo/src/net/mod.rs"),
            Some(vec!["net".into()])
        );
        assert_eq!(
            mt.module_of("crates/foo/src/net/http.rs"),
            Some(vec!["net".into(), "http".into()])
        );
    }

    #[test]
    fn binary_root_and_bin_dir() {
        let files = vec![
            sf("app/src/main.rs"),
            sf("app/src/cli.rs"),
            sf("app/src/bin/tool.rs"),
        ];
        let mt = ModuleTree::build(&files);
        assert_eq!(mt.module_of("app/src/main.rs"), Some(vec![]));
        assert_eq!(mt.module_of("app/src/cli.rs"), Some(vec!["cli".into()]));
        assert_eq!(
            mt.module_of("app/src/bin/tool.rs"),
            Some(vec![]),
            "a bin file is its own crate root"
        );
    }

    #[test]
    fn no_crate_root_in_scope_returns_none() {
        // A subdirectory scan with no lib.rs/main.rs in the batch.
        let files = vec![
            sf("crates/foo/src/util/config.rs"),
            sf("crates/foo/src/util.rs"),
        ];
        let mt = ModuleTree::build(&files);
        assert_eq!(mt.module_of("crates/foo/src/util/config.rs"), None);
        assert_eq!(mt.module_of("crates/foo/src/util.rs"), None);
    }

    #[test]
    fn separate_workspace_crates_are_independent() {
        let files = vec![
            sf("crates/a/src/lib.rs"),
            sf("crates/a/src/x.rs"),
            sf("crates/b/src/lib.rs"),
            sf("crates/b/src/x.rs"),
        ];
        let mt = ModuleTree::build(&files);
        // Both x.rs map to ["x"] within THEIR crate; the tree keys by full file path so they don't collide.
        assert_eq!(mt.module_of("crates/a/src/x.rs"), Some(vec!["x".into()]));
        assert_eq!(mt.module_of("crates/b/src/x.rs"), Some(vec!["x".into()]));
    }

    #[test]
    fn nested_main_rs_is_a_module_not_a_crate_root() {
        // src/cli/main.rs is the module crate::cli::main, NOT a second crate root —
        // it must not create a spurious source root that re-modules its siblings.
        let files = vec![
            sf("app/src/main.rs"),
            sf("app/src/cli/main.rs"),
            sf("app/src/cli/parse.rs"),
        ];
        let mt = ModuleTree::build(&files);
        assert_eq!(
            mt.module_of("app/src/main.rs"),
            Some(vec![]),
            "src/main.rs IS the root"
        );
        assert_eq!(
            mt.module_of("app/src/cli/main.rs"),
            Some(vec!["cli".into(), "main".into()])
        );
        assert_eq!(
            mt.module_of("app/src/cli/parse.rs"),
            Some(vec!["cli".into(), "parse".into()]),
            "sibling must be crate::cli::parse, not crate::parse"
        );
    }

    #[test]
    fn multifile_binary_main_is_its_own_root() {
        // src/bin/<name>/main.rs is a multi-file binary crate root (module []).
        let files = vec![
            sf("app/src/lib.rs"),
            sf("app/src/bin/tool/main.rs"),
            sf("app/src/bin/tool/helper.rs"),
        ];
        let mt = ModuleTree::build(&files);
        assert_eq!(
            mt.module_of("app/src/bin/tool/main.rs"),
            Some(vec![]),
            "bin/<name>/main.rs is its own root"
        );
        assert_eq!(
            mt.module_of("app/src/bin/tool/helper.rs"),
            Some(vec!["helper".into()])
        );
    }
}
