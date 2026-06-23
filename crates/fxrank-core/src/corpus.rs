//! Language-neutral corpus-hygiene model: per-ecosystem prune/exclude/test-file
//! declarations (`CorpusProfile`) + the spec-004 three-class matcher (`CorpusMatcher`).
//! Relocated here from the CLI so the CLI (walk) and the frontends (test-file
//! detection) share one matcher. `globset` is a glob engine, not a parser — core's
//! no-parser rule is intact.

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

/// Per-ecosystem hygiene declaration. Pure `&'static` data — `Copy`, no parser.
#[derive(Clone, Copy, Debug)]
pub struct CorpusProfile {
    /// Directory base names to prune (and exclude same-named files): `node_modules`, `target`, `.venv`.
    pub prune_dirs: &'static [&'static str],
    /// File globs to exclude as noise: `*.min.js`, `*_pb2.py`. Globs here never
    /// prune a dir. (A bare-literal *filename* like `mockServiceWorker.js` also
    /// matches a same-named directory per spec-004's literal rule — harmless for
    /// real filenames; put actual directory names in `prune_dirs`.)
    pub exclude_file_globs: &'static [&'static str],
    /// Name-based test-file globs (applied by the frontend → `skipped_tests`): `*.test.*`, `test_*.py`.
    pub test_file_globs: &'static [&'static str],
    /// Content-marker files: prune any directory that CONTAINS one of these: `pyvenv.cfg`.
    pub prune_marker_files: &'static [&'static str],
}

impl CorpusProfile {
    pub const EMPTY: CorpusProfile = CorpusProfile {
        prune_dirs: &[],
        exclude_file_globs: &[],
        test_file_globs: &[],
        prune_marker_files: &[],
    };
    /// Language-neutral baseline owned by no frontend (VCS metadata).
    pub const COMMON: CorpusProfile = CorpusProfile {
        prune_dirs: &[".git"],
        exclude_file_globs: &[],
        test_file_globs: &[],
        prune_marker_files: &[],
    };
}

fn has_glob_meta(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

/// The spec-004 three-class matcher. Built from a flat entry list (the union of
/// profile channels, or the user's `--exclude`).
pub struct CorpusMatcher {
    literals: std::collections::HashSet<String>,
    name_globs: GlobSet,
    path_globs: GlobSet,
    /// True for `test_matcher`: bare-name literals also match any path SEGMENT
    /// (so `__tests__`/`tests` as a directory marks files beneath it).
    segment_literals: bool,
}

impl CorpusMatcher {
    /// User-facing constructor (e.g. `--exclude`). An invalid glob is surfaced as
    /// an error (parity with the old `ExcludeMatcher::build`, spec 004) — NOT
    /// silently dropped. An empty/whitespace entry matches nothing (old guard).
    pub fn build(entries: &[String]) -> Result<CorpusMatcher, String> {
        Self::build_inner(entries, false)
    }

    /// A matcher for `test_file_globs`: bare-name entries also match path segments.
    /// Built from `&'static` profile data, which can't be invalid → infallible.
    pub fn test_matcher(globs: &[&str]) -> CorpusMatcher {
        let owned: Vec<String> = globs.iter().map(|s| s.to_string()).collect();
        Self::build_inner(&owned, true).expect("CORPUS_PROFILE test globs must be valid")
    }

    fn build_inner(entries: &[String], segment_literals: bool) -> Result<CorpusMatcher, String> {
        let mut literals = std::collections::HashSet::new();
        let mut name = GlobSetBuilder::new();
        let mut path = GlobSetBuilder::new();
        for raw in entries {
            let entry = raw.trim(); // trim parity with the old ExcludeMatcher
            if entry.is_empty() {
                continue; // empty/whitespace entry matches nothing (old guard)
            }
            if entry.contains('/') {
                // Path glob: `*` must NOT cross `/` (spec 004 invariant —
                // `src/*.ts` does not match `src/nested/x.ts`).
                let g = GlobBuilder::new(entry)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| format!("invalid exclude glob `{entry}`: {e}"))?;
                path.add(g);
            } else if has_glob_meta(entry) {
                let g =
                    Glob::new(entry).map_err(|e| format!("invalid exclude glob `{entry}`: {e}"))?;
                name.add(g);
            } else {
                literals.insert(entry.to_string());
            }
        }
        Ok(CorpusMatcher {
            literals,
            name_globs: name.build().map_err(|e| e.to_string())?,
            path_globs: path.build().map_err(|e| e.to_string())?,
            segment_literals,
        })
    }

    pub fn dir_pruned(&self, dir_name: &str) -> bool {
        self.literals.contains(dir_name)
    }

    pub fn file_excluded(&self, file_name: &str, rel_path: &str) -> bool {
        self.literals.contains(file_name)
            || self.name_globs.is_match(file_name)
            || self.path_globs.is_match(rel_path)
    }

    /// Name-based test-file match: base-name literal/glob, path glob, OR (for a
    /// test matcher) any path segment equal to a **dotless** bare-name literal.
    /// Dotless literals (e.g. `tests`, `__tests__`) are directory-marker names and
    /// participate in segment matching; filename literals like `conftest.py` (which
    /// contain a `.`) match by base name only — they will NOT match a directory that
    /// happens to share that name (e.g. `pkg/conftest.py/helpers.py`). Normalizes
    /// `\` → `/` so Windows-native paths match `/`-based path globs.
    pub fn matches_test_file(&self, path: &str) -> bool {
        use std::borrow::Cow;
        let norm: Cow<str> = if path.contains('\\') {
            Cow::Owned(path.replace('\\', "/"))
        } else {
            Cow::Borrowed(path)
        };
        let base = norm.rsplit('/').next().unwrap_or(norm.as_ref());
        if self.literals.contains(base)
            || self.name_globs.is_match(base)
            || self.path_globs.is_match(norm.as_ref())
        {
            return true;
        }
        if self.segment_literals {
            return norm
                .split('/')
                .any(|seg| !seg.contains('.') && self.literals.contains(seg));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_prunes_dir_and_excludes_file() {
        let m = CorpusMatcher::build(&[
            "node_modules".into(),
            "*.min.js".into(),
            "src/gen/**".into(),
        ])
        .unwrap();
        assert!(m.dir_pruned("node_modules"));
        assert!(!m.dir_pruned("src")); // not a literal
        assert!(m.file_excluded("node_modules", "a/node_modules")); // literal also excludes a same-named file
        assert!(m.file_excluded("app.min.js", "a/app.min.js")); // name glob
        assert!(!m.dir_pruned("app.min.js")); // a glob never prunes a dir
        assert!(m.file_excluded("x.js", "src/gen/x.js")); // path glob
        assert!(!m.file_excluded("x.js", "src/app/x.js")); // path glob misses
    }

    #[test]
    fn path_glob_star_does_not_cross_separator() {
        // T1-A regression: `*` in a path glob must NOT cross `/` (literal_separator).
        let m = CorpusMatcher::build(&["src/*.ts".into()]).unwrap();
        assert!(m.file_excluded("x.ts", "src/x.ts"));
        assert!(!m.file_excluded("x.ts", "src/nested/x.ts"));
    }

    #[test]
    fn invalid_glob_is_an_error_not_silent() {
        // T1-B: a bad user glob is surfaced (spec 004), not silently dropped.
        assert!(CorpusMatcher::build(&["[".into()]).is_err());
    }

    #[test]
    fn empty_entry_matches_nothing_and_entries_are_trimmed() {
        let m = CorpusMatcher::build(&["".into()]).unwrap();
        assert!(!m.dir_pruned(""));
        assert!(!m.file_excluded("anything", "a/anything"));
        // trim parity: a padded entry still prunes the bare name.
        let m2 = CorpusMatcher::build(&[" node_modules ".into()]).unwrap();
        assert!(m2.dir_pruned("node_modules"));
    }

    #[test]
    fn test_matcher_matches_name_globs_and_segments() {
        let m = CorpusMatcher::test_matcher(&["*.test.*", "test_*.py", "conftest.py", "__tests__"]);
        assert!(m.matches_test_file("src/a.test.ts"));
        assert!(m.matches_test_file("pkg/test_views.py"));
        assert!(m.matches_test_file("conftest.py"));
        assert!(m.matches_test_file("src/__tests__/a.ts")); // bare name → path segment
        assert!(!m.matches_test_file("src/app.ts"));
    }

    #[test]
    fn test_matcher_file_literal_does_not_segment_match() {
        let m = CorpusMatcher::test_matcher(&["conftest.py", "tests"]);
        assert!(m.matches_test_file("conftest.py")); // base-name literal → match
        assert!(m.matches_test_file("pkg/conftest.py")); // base-name in a path → match
        assert!(!m.matches_test_file("pkg/conftest.py/x.py")); // conftest.py as a DIR segment → NOT a test (filename, base-only)
        assert!(m.matches_test_file("pkg/tests/x.py")); // `tests` (dotless dir marker) → segment match
        assert!(!m.matches_test_file("pkg/mytests/x.py")); // not a whole segment → no match
    }

    #[test]
    fn empty_profile_matches_nothing() {
        let p = CorpusProfile::EMPTY;
        assert!(
            p.prune_dirs.is_empty()
                && p.exclude_file_globs.is_empty()
                && p.test_file_globs.is_empty()
                && p.prune_marker_files.is_empty()
        );
    }
}
