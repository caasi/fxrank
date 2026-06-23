use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use std::collections::HashSet;

/// Classifies and matches `--exclude` entries. See `docs/superpowers/specs/004-corpus-hygiene-excludes.md`.
///
/// Top-level split is on `/`; no-`/` entries are then split into literal vs wildcard:
/// - **literal** (no `/`, no glob metachar): prunes a matching directory AND excludes
///   a matching file (base-name equality).
/// - **wildcard** (no `/`, has a glob metachar): excludes matching FILES only — it
///   never prunes a directory (so `*.stories.*` can't silently drop a `x.stories.d/`).
/// - **path glob** (contains `/`): excludes matching files by `/`-normalized path.
pub struct ExcludeMatcher {
    literals: HashSet<String>,
    name_globs: GlobSet,
    path_globs: GlobSet,
}

/// A no-`/` entry is a wildcard glob iff it contains a glob metacharacter.
fn has_glob_meta(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

impl ExcludeMatcher {
    /// Build from raw entries (already comma-split by clap). Empty/whitespace
    /// entries are ignored. Returns a human-readable error if any glob fails to
    /// compile (surfaced by the CLI as a startup JSON error).
    pub fn build(entries: &[String]) -> Result<Self, String> {
        let mut literals = HashSet::new();
        let mut name_builder = GlobSetBuilder::new();
        let mut path_builder = GlobSetBuilder::new();

        for raw in entries {
            let entry = raw.trim();
            if entry.is_empty() {
                continue;
            }
            if entry.contains('/') {
                let glob = GlobBuilder::new(entry)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| format!("invalid --exclude pattern '{entry}': {e}"))?;
                path_builder.add(glob);
            } else if has_glob_meta(entry) {
                let glob = Glob::new(entry)
                    .map_err(|e| format!("invalid --exclude pattern '{entry}': {e}"))?;
                name_builder.add(glob);
            } else {
                literals.insert(entry.to_string());
            }
        }

        Ok(ExcludeMatcher {
            literals,
            name_globs: name_builder
                .build()
                .map_err(|e| format!("building --exclude matcher: {e}"))?,
            path_globs: path_builder
                .build()
                .map_err(|e| format!("building --exclude matcher: {e}"))?,
        })
    }

    /// A directory is pruned iff its base name is a literal entry. Wildcard and
    /// path globs never prune (spec 004 §matcher).
    pub fn dir_pruned(&self, dir_name: &str) -> bool {
        self.literals.contains(dir_name)
    }

    /// A file is excluded if its base name is a literal, OR its base name matches a
    /// filename glob, OR its `/`-normalized relative path matches a path glob.
    pub fn file_excluded(&self, file_name: &str, rel_path: &str) -> bool {
        self.literals.contains(file_name)
            || self.name_globs.is_match(file_name)
            || self.path_globs.is_match(rel_path)
    }
}

#[cfg(test)]
mod tests {
    use super::ExcludeMatcher;

    fn m(entries: &[&str]) -> ExcludeMatcher {
        ExcludeMatcher::build(&entries.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .expect("valid matcher")
    }

    #[test]
    fn literal_prunes_dir_and_excludes_file() {
        let m = m(&["node_modules", "mockServiceWorker.js"]);
        assert!(m.dir_pruned("node_modules"));
        assert!(m.file_excluded("mockServiceWorker.js", "public/mockServiceWorker.js"));
        assert!(!m.dir_pruned("src"));
    }

    #[test]
    fn wildcard_excludes_files_only_never_prunes() {
        let m = m(&["*.stories.*", "*.min.js"]);
        // file matches
        assert!(m.file_excluded("x.stories.tsx", "ui/x.stories.tsx"));
        assert!(m.file_excluded("a.min.js", "a.min.js"));
        // a directory whose name matches the glob is NOT pruned
        assert!(!m.dir_pruned("x.stories.d"));
    }

    #[test]
    fn path_glob_matches_relative_path_only() {
        let m = m(&["src/legacy/**", "**/*.stories.*"]);
        assert!(m.file_excluded("foo.ts", "src/legacy/foo.ts"));
        assert!(m.file_excluded("x.stories.tsx", "pkg/ui/x.stories.tsx"));
        // path glob does not match by base name alone
        assert!(!m.file_excluded("foo.ts", "src/app/foo.ts"));
        // path globs never prune directories
        assert!(!m.dir_pruned("legacy"));
    }

    #[test]
    fn empty_entries_are_inert() {
        let m = m(&["", "  ", "node_modules"]);
        assert!(m.dir_pruned("node_modules"));
        assert!(!m.file_excluded("", ""));
    }

    #[test]
    fn invalid_glob_is_an_error() {
        let err = ExcludeMatcher::build(&["[".to_string()]);
        assert!(err.is_err(), "unterminated class should fail to compile");
    }

    #[test]
    fn path_glob_star_does_not_cross_separator() {
        let single = m(&["src/*.ts"]);
        assert!(single.file_excluded("x.ts", "src/x.ts")); // one segment → matches
        assert!(!single.file_excluded("x.ts", "src/nested/x.ts")); // `*` must NOT cross `/`
        // `**` is still recursive:
        let recursive = m(&["src/**"]);
        assert!(recursive.file_excluded("x.ts", "src/a/b/x.ts"));
    }
}
