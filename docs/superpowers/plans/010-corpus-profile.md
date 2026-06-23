# CorpusProfile — Frontend-Owned Corpus Hygiene Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move per-ecosystem corpus-hygiene defaults (directory prunes, file-exclude globs, name-based test-file globs, content-marker prunes) out of the CLI's hardcoded JS-flavored list and into each language frontend, behind a `CorpusProfile` interface in `fxrank-core`; the CLI unions the enabled frontends' profiles. Resolves issue #21.

**Architecture:** `fxrank-core` gains a pure-data `CorpusProfile` (four `&'static [&'static str]` channels) and a `CorpusMatcher` (the spec-004 three-class matcher, relocated from `fxrank-cli/src/exclude.rs` so it is the single language-neutral matcher used by both the CLI and the frontends; built on `globset`). The `Frontend` trait gains `corpus_profile()` (default returns empty). Each frontend exposes a `pub const CORPUS_PROFILE` and implements the method. The CLI replaces its hardcoded `--exclude` default with the **union** of enabled frontends' `prune_dirs` + `exclude_file_globs` (+ a `.git` common baseline), adds a content-marker directory prune (any dir containing `pyvenv.cfg`), and keeps `--exclude` **replace** semantics. Name-based test-file detection moves into the profile's `test_file_globs`, **applied by each frontend** in `analyze` (replacing the hardcoded `is_test_file`), preserving `skipped_tests`=unit-count and `--include-tests`; source-based test detection (Rust `#[test]`/`#[cfg(test)]`, Python `Test*`/`unittest.TestCase`) stays internal.

**Tech Stack:** Rust, `globset` (already a `fxrank-cli` dep; added to `fxrank-core`), clap, `fxrank-core` `Frontend` trait, `cargo test`/`fmt`/`clippy`, `insta` snapshots.

**Source / decisions:** issue #21 (its *two-phase design* + *proposed interface* + *CLI behavior* sections are the spec). User decisions this session:
- `--exclude` stays **replace**, NOT append (agents don't mind retyping) — do not add `--exclude-add`.
- The `pyvenv.cfg` **content-marker prune** is in scope for v1.
- The Django `i18n_catalog.js` routing bug is **out of scope** (agents can exclude it manually); do not touch file routing.
- Test exclusion **is** in scope and is the two-channel design (name-based in profile / source-based in frontend).
- **Apply model = Option B:** `test_file_globs` declared in the profile, applied by each frontend (not at the CLI walk) — preserves the `skipped_tests` unit-count contract.

## Global Constraints

- **`fxrank-core` stays parser-free.** `globset` is a glob engine, NOT a language parser — adding it does not violate the no-parser rule (`Cargo.toml` description: "depends on no parser"). Do not add `syn`/`swc`/`libcst` to core.
- **`CorpusProfile` is pure `&'static str` data** (four channels) — `Copy`, no allocation, no parser. It is the single source of truth for each ecosystem's hygiene patterns; the guideline doc and per-frontend declarations must agree (no duplicated pattern literals).
- **Two channels for test exclusion:** name-based (`test_file_globs`, declared in the profile, applied by the frontend → `skipped_tests`, honors `--include-tests`); source-based (Rust `#[test]`/`#[cfg(test)]`, Python `Test*`/`unittest.TestCase`, stays in `analyze_unit` → `skipped_tests`). Both feed `skipped_tests`; neither feeds `skipped_excluded`.
- **Noise vs test counting (do not conflate):** `prune_dirs`/`exclude_file_globs` → CLI walk → `scope.skipped_excluded` (file count, NOT affected by `--include-tests`). `test_file_globs` + source-based → frontend → `scope.skipped_tests` (unit count, gated by `--include-tests`).
- **`--exclude` REPLACE semantics** (spec 004): when provided, `--exclude` replaces the **glob/literal list** (the union of `prune_dirs` + `exclude_file_globs` + `.git`); when absent the default = that union. **The `pyvenv.cfg` content-marker prune is a SEPARATE, always-on safety** — independent of `--exclude` (you ~never want to scan a venv, and a content-marker isn't expressible as a glob, so `--exclude` cannot turn it off or re-add it). `default_prune_markers()` is therefore computed regardless of whether `--exclude` was given.
- **Union follows the compiled-in language set** (`#[cfg(feature = …)]`): a slim `--features ts` build gets only `.git` + TS's profile; the full binary gets all three. Correct by construction.
- **Directory prunes MUST be unioned** across all enabled frontends — at a directory the language isn't known yet, so `.venv`/`node_modules`/`target` are pruned regardless of which frontend owns them.
- **Wire/JSON output of `Scope` is unchanged** — same fields (`skipped_tests`, `skipped_excluded`), same serde order; no `Report` schema change. Snapshot stability is a gate (Task 7).
- **CI gates:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Plus all slim builds must compile: `cargo build -p fxrank --no-default-features --features rust` / `--features ts` / `--features python` / no-features.

**Branch / worktree:** code change → **feature branch** + worktree on the RAM disk (`superpowers:using-git-worktrees`). Suggested branch `feat/021-corpus-profile`. Commit this plan as the first commit on that branch.

---

### Task 1: `fxrank-core` — `CorpusProfile` + `CorpusMatcher` + `Frontend::corpus_profile()`

Add the data type, relocate the three-class matcher from the CLI into core (single source of truth), and extend the `Frontend` trait. Adding the trait method with a **default impl** keeps the frontends compiling until Tasks 3–5 override it.

**Files:**
- Create: `crates/fxrank-core/src/corpus.rs` (`CorpusProfile` + `CorpusMatcher`)
- Modify: `crates/fxrank-core/src/lib.rs` (`pub mod corpus;` + re-exports)
- Modify: `crates/fxrank-core/src/frontend.rs:24-29` (add `corpus_profile` with default)
- Modify: `crates/fxrank-core/Cargo.toml` (add `globset`)

**Interfaces — Produces:**
- `pub struct CorpusProfile { pub prune_dirs: &'static [&'static str], pub exclude_file_globs: &'static [&'static str], pub test_file_globs: &'static [&'static str], pub prune_marker_files: &'static [&'static str] }` (derives `Clone, Copy, Debug`), with `pub const EMPTY: CorpusProfile` and `pub const COMMON: CorpusProfile` (`.git` prune).
- `pub struct CorpusMatcher` with `CorpusMatcher::build(entries: &[String]) -> Result<CorpusMatcher, String>` (invalid glob → error, parity with the old matcher), `fn dir_pruned(&self, dir_name: &str) -> bool`, `fn file_excluded(&self, file_name: &str, rel_path: &str) -> bool`, and `pub fn test_matcher(globs: &[&str]) -> CorpusMatcher` (infallible; from `&'static` data) + `fn matches_test_file(&self, path: &str) -> bool`.
- `Frontend::corpus_profile(&self) -> CorpusProfile` (default returns `CorpusProfile::EMPTY`).

- [ ] **Step 1: Add the `globset` dep to core**

In `crates/fxrank-core/Cargo.toml` `[dependencies]`, add (match the version `fxrank-cli` already uses — read `crates/fxrank-cli/Cargo.toml` and copy the exact `globset = …` line):

```toml
globset = "0.4"
```

- [ ] **Step 2: Write the failing matcher + profile tests**

Create `crates/fxrank-core/src/corpus.rs` containing **only** the `#[cfg(test)] mod tests` for now, and add `pub mod corpus;` to `crates/fxrank-core/src/lib.rs` so the file is actually compiled (an undeclared module's tests never run). The tests port the spec-004 three-class behavior (read `crates/fxrank-cli/src/exclude.rs` tests and bring them over verbatim where they apply) plus the new test-file + profile tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_prunes_dir_and_excludes_file() {
        let m = CorpusMatcher::build(&["node_modules".into(), "*.min.js".into(), "src/gen/**".into()]).unwrap();
        assert!(m.dir_pruned("node_modules"));
        assert!(!m.dir_pruned("src"));                       // not a literal
        assert!(m.file_excluded("node_modules", "a/node_modules")); // literal also excludes a same-named file
        assert!(m.file_excluded("app.min.js", "a/app.min.js"));     // name glob
        assert!(!m.dir_pruned("app.min.js"));               // a glob never prunes a dir
        assert!(m.file_excluded("x.js", "src/gen/x.js"));   // path glob
        assert!(!m.file_excluded("x.js", "src/app/x.js"));  // path glob misses
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
    fn empty_profile_matches_nothing() {
        let p = CorpusProfile::EMPTY;
        assert!(p.prune_dirs.is_empty() && p.exclude_file_globs.is_empty()
            && p.test_file_globs.is_empty() && p.prune_marker_files.is_empty());
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p fxrank-core corpus`
Expected: FAIL to compile — `cannot find type CorpusProfile`/`CorpusMatcher` in this scope (the test mod references types not implemented until Step 4).

- [ ] **Step 4: Implement `corpus.rs`**

Port the three-class classifier from `crates/fxrank-cli/src/exclude.rs` (the `literals`/`name_globs`/`path_globs` split, `has_glob_meta`, `dir_pruned`, `file_excluded`) into `crates/fxrank-core/src/corpus.rs`, and add the `CorpusProfile` data type + a `test_matcher` constructor + `matches_test_file`. A bare-name test glob (no `/`, no glob meta, e.g. `__tests__`/`tests`/`conftest.py`) matches either the file's base name OR any path segment (so `__tests__/` as a directory marks files under it); name globs match the base name; path globs match the relative path.

```rust
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
                let g = Glob::new(entry)
                    .map_err(|e| format!("invalid exclude glob `{entry}`: {e}"))?;
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
    /// test matcher) any path segment equal to a bare-name literal. Normalizes
    /// `\` → `/` so Windows-native paths match `/`-based path globs.
    pub fn matches_test_file(&self, path: &str) -> bool {
        let norm = path.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        if self.literals.contains(base)
            || self.name_globs.is_match(base)
            || self.path_globs.is_match(&norm)
        {
            return true;
        }
        if self.segment_literals {
            return norm.split('/').any(|seg| self.literals.contains(seg));
        }
        false
    }
}
```

(Then write the matching `CorpusProfile`/`CorpusMatcher` exactly as the tests in Step 2 expect.)

- [ ] **Step 5: Wire the module + trait method**

In `crates/fxrank-core/src/lib.rs` add the re-export `pub use corpus::{CorpusMatcher, CorpusProfile};` (match the file's existing re-export style; the `pub mod corpus;` was already added in Step 2).

In `crates/fxrank-core/src/frontend.rs`, add to the `Frontend` trait (after `analyze`):

```rust
    /// The frontend's corpus-hygiene profile (prune dirs, exclude globs, test-file
    /// globs, content-marker prunes). Default: empty. See `docs/corpus-profile-guideline.md`.
    fn corpus_profile(&self) -> crate::corpus::CorpusProfile {
        crate::corpus::CorpusProfile::EMPTY
    }
```

- [ ] **Step 6: Run tests + fmt + clippy**

Run: `cargo test -p fxrank-core corpus && cargo fmt --package fxrank-core --check && cargo clippy -p fxrank-core --all-targets -- -D warnings`
Expected: tests PASS, clean.

- [ ] **Step 7: Commit**

```bash
git add crates/fxrank-core/src/corpus.rs crates/fxrank-core/src/lib.rs \
        crates/fxrank-core/src/frontend.rs crates/fxrank-core/Cargo.toml Cargo.lock
git commit -m "feat(core): CorpusProfile + CorpusMatcher + Frontend::corpus_profile (#21)"
```

---

### Task 2: Each frontend declares its `CORPUS_PROFILE` + applies its own `test_file_globs`

Move the per-ecosystem defaults out of the CLI's hardcoded list into the three frontends, and replace each frontend's hardcoded `is_test_file` with a `CorpusMatcher` built from its own `test_file_globs`. Rust has no path-based test detection today, so it only declares the profile.

**Files:**
- Modify: `crates/fxrank-lang-rust/src/lib.rs` (add `pub const CORPUS_PROFILE` + impl `corpus_profile`)
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (add `CORPUS_PROFILE` + impl; replace `is_test_file` body, ~line 172, with a `CorpusMatcher`)
- Modify: `crates/fxrank-lang-python/src/lib.rs` (add `CORPUS_PROFILE` + impl; replace path-based `is_test_file`, ~line 144, with a `CorpusMatcher`; keep source-based detection)

**Interfaces — Produces:** `pub const CORPUS_PROFILE: CorpusProfile` in each frontend crate (used by the CLI union in Task 3) + the `Frontend::corpus_profile` impl returning it.

- [ ] **Step 1: Rust frontend profile**

In `crates/fxrank-lang-rust/src/lib.rs`, add (near the `RustFrontend` definition; import `fxrank_core::CorpusProfile`):

```rust
/// Rust corpus hygiene. `target` is the build dir; unit tests are SOURCE-based
/// (`#[test]`/`#[cfg(test)]`, handled in `analyze`), so no `test_file_globs`.
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &["target"],
    exclude_file_globs: &[],
    test_file_globs: &[],
    prune_marker_files: &[],
};

impl Frontend for RustFrontend {
    // … existing language()/analyze() …
    fn corpus_profile(&self) -> CorpusProfile { CORPUS_PROFILE }
}
```

(Add `fn corpus_profile` inside the existing `impl Frontend for RustFrontend` block alongside `analyze`. Rust's source-based test skip at lib.rs:51-69 is unchanged.)

- [ ] **Step 2: TS frontend profile + `is_test_file` via the profile**

In `crates/fxrank-lang-ts/src/lib.rs`:

```rust
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    // `__mocks__` is a directory → prune_dirs (channel honesty). Behaviorally
    // identical under the flat union — a bare literal prunes+excludes either way —
    // but this keeps `exclude_file_globs` = "things that never prune a dir".
    prune_dirs: &["node_modules", "__mocks__"],
    exclude_file_globs: &[
        "*.min.js", "*.min.mjs", "*.min.cjs", "*.stories.*",
        "mockServiceWorker.js", "jest.setup.*", "jest.config.*",
    ],
    test_file_globs: &["*.test.*", "*.spec.*", "__tests__"],
    prune_marker_files: &[],
};
```

Add `fn corpus_profile(&self) -> CorpusProfile { CORPUS_PROFILE }` to the `impl Frontend for TsFrontend` block. Replace the hardcoded `is_test_file` (lib.rs:172-183) with a profile-driven check (build the matcher once; a `OnceLock` avoids rebuilding per file):

```rust
pub fn is_test_file(path: &str) -> bool {
    use std::sync::OnceLock;
    static M: OnceLock<fxrank_core::CorpusMatcher> = OnceLock::new();
    M.get_or_init(|| fxrank_core::CorpusMatcher::test_matcher(CORPUS_PROFILE.test_file_globs))
        .matches_test_file(path)
}
```

(The call site at lib.rs:68-70 is unchanged — `is_test_file` keeps its signature, so `skipped_tests += units.len()` semantics are preserved.)

- [ ] **Step 3: Python frontend profile + path-based `is_test_file` via the profile**

In `crates/fxrank-lang-python/src/lib.rs`:

```rust
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &[
        ".venv", "venv", ".tox", ".nox", "__pycache__", ".eggs",
        "build", "dist", ".mypy_cache", ".pytest_cache", ".ruff_cache", "site-packages",
    ],
    exclude_file_globs: &["*_pb2.py", "*_pb2_grpc.py"],
    test_file_globs: &["test_*.py", "*_test.py", "conftest.py", "tests"],
    prune_marker_files: &["pyvenv.cfg"],
};
```

Add `fn corpus_profile(&self) -> CorpusProfile { CORPUS_PROFILE }` to the `impl Frontend for PythonFrontend` block. Replace the path-based `is_test_file` (lib.rs:144-160) with the profile-driven matcher (same `OnceLock` shape as TS). **Keep** the source-based `Test*`/`unittest.TestCase` detection (functions.rs) and the per-unit skip (lib.rs:116-120) unchanged.

- [ ] **Step 4: Add characterization + trait-consistency tests; verify equivalence**

Add **characterization tests** that pin the exact `is_test_file` behavior across the edge cases (so any future glob/substring drift is caught — the highest-risk point). For TS (`crates/fxrank-lang-ts/src/lib.rs` tests):

```rust
#[test]
fn is_test_file_characterization() {
    for p in ["a.test.ts", "a.spec.tsx", "x.b.test.js", "src/__tests__/a.ts", "a/__tests__/b/c.ts"] {
        assert!(is_test_file(p), "expected test file: {p}");
    }
    for p in ["app.ts", "src/app.tsx", "my.test.project/app.ts", "testdata.ts", "a.contest.ts"] {
        assert!(!is_test_file(p), "expected NON-test file: {p}");
    }
}
```

For Python (`crates/fxrank-lang-python/src/lib.rs` tests):

```rust
#[test]
fn is_test_file_characterization() {
    for p in ["test_views.py", "views_test.py", "conftest.py", "pkg/tests/helpers.py", "tests/x.py"] {
        assert!(is_test_file(p), "expected test file: {p}");
    }
    for p in ["views.py", "pkg/mytests/foo.py", "tests.py", "contest.py", "test_views.txt"] {
        assert!(!is_test_file(p), "expected NON-test file: {p}");
    }
}
```

(Note `pkg/mytests/foo.py` and `tests.py` are NON-test: `tests` matches only a whole path *segment*, never a substring; `test_*.py` requires the `test_` prefix on the base name. Confirm these match the OLD logic before changing it — run them against the pre-swap implementation if doing strict TDD.)

Add a **trait-consistency** test in each frontend that the method returns the const (the CLI unions the const directly, feature-gated, so this guards drift between the two):

```rust
#[test]
fn corpus_profile_method_returns_const() {
    use fxrank_core::Frontend;
    let p = /* the frontend's value, e.g. */ RustFrontend { include_tests: false }.corpus_profile();
    assert_eq!(p.prune_dirs, CORPUS_PROFILE.prune_dirs);
    assert_eq!(p.test_file_globs, CORPUS_PROFILE.test_file_globs);
}
```

Run: `cargo test -p fxrank-lang-rust && cargo test -p fxrank-lang-ts && cargo test -p fxrank-lang-python`
Expected: PASS — `is_test_file` is behaviorally equivalent to the old literals (verified in review). If a characterization case fails, the glob set diverged — reconcile so behavior is identical.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
git add crates/fxrank-lang-rust/src/lib.rs crates/fxrank-lang-ts/src/lib.rs crates/fxrank-lang-python/src/lib.rs
git commit -m "feat(frontends): declare CORPUS_PROFILE; test-file detection via the profile (#21)"
```

---

### Task 3: CLI — union the profiles, content-marker prune, retire the hardcoded list

Replace the CLI's hardcoded `--exclude` default and its local `exclude.rs` matcher with the union of enabled frontends' profiles + core's `CorpusMatcher`; add the `pyvenv.cfg` content-marker directory prune.

**Files:**
- Modify: `crates/fxrank-cli/src/main.rs` (args `--exclude` default → union; `walk_dir` uses `CorpusMatcher` + marker prune)
- Delete: `crates/fxrank-cli/src/exclude.rs` (relocated to core in Task 1) — and its `mod exclude;` in `main.rs`
- Modify: `crates/fxrank-cli/Cargo.toml` (drop `globset` if now unused by the CLI directly; keep if still used elsewhere — check)

**Interfaces — Consumes:** `fxrank_core::{CorpusProfile, CorpusMatcher}` (Task 1); `fxrank_lang_{rust,ts,python}::CORPUS_PROFILE` (Task 2).

- [ ] **Step 1: Add the union helpers (feature-gated) + the union-parity test**

Add to `main.rs` a function that collects the enabled frontends' profiles + the common baseline, and unions the prune/exclude channels into matcher entries + the marker list:

```rust
// `allow(unused_mut)`: in a no-feature build all the `push`es below are cfg'd out,
// leaving `v` never-mutated. (Slim builds run `cargo build`, not clippy -D warnings,
// but keep them warning-clean anyway.)
#[allow(unused_mut)]
fn default_corpus_profiles() -> Vec<fxrank_core::CorpusProfile> {
    let mut v = vec![fxrank_core::CorpusProfile::COMMON];
    #[cfg(feature = "rust")]
    v.push(fxrank_lang_rust::CORPUS_PROFILE);
    #[cfg(feature = "ts")]
    v.push(fxrank_lang_ts::CORPUS_PROFILE);
    #[cfg(feature = "python")]
    v.push(fxrank_lang_python::CORPUS_PROFILE);
    v
}

/// Union the prune-dir + exclude-file channels into a sorted, deduped entry list
/// for `CorpusMatcher::build` (the default when `--exclude` is absent).
fn default_exclude_entries() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in default_corpus_profiles() {
        out.extend(p.prune_dirs.iter().map(|s| s.to_string()));
        out.extend(p.exclude_file_globs.iter().map(|s| s.to_string()));
    }
    out.sort();
    out.dedup();
    out
}

/// Union of content-marker file names whose presence prunes a directory.
fn default_prune_markers() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in default_corpus_profiles() {
        out.extend(p.prune_marker_files.iter().map(|s| s.to_string()));
    }
    out.sort();
    out.dedup();
    out
}
```

(These are new pure functions added together with their test — there is no meaningful red phase for a brand-new helper + its test in the same file; add both, then run green.) Add a `#[cfg(test)] mod tests` with a **union-parity** assertion (full build = all frontends enabled) that the union EXACTLY reproduces the old hardcoded `default_value` — this is the regression guard that nothing was dropped or added when the central list moved into the profiles:

```rust
#[test]
#[cfg(all(feature = "rust", feature = "ts", feature = "python"))]
fn full_build_union_equals_old_default_value() {
    // The verbatim pre-#21 default_value (spec 004 + the #14 interim Python add).
    let mut old: Vec<String> = "node_modules,.git,target,*.min.js,*.min.mjs,*.min.cjs,\
*.stories.*,mockServiceWorker.js,jest.setup.*,jest.config.*,__mocks__,.venv,venv,.tox,.nox,\
__pycache__,.eggs,build,dist,.mypy_cache,.pytest_cache,.ruff_cache,site-packages,*_pb2.py,\
*_pb2_grpc.py"
        .split(',')
        .map(|s| s.to_string())
        .collect();
    old.sort();
    old.dedup();
    assert_eq!(default_exclude_entries(), old, "union default drifted from the old --exclude list");
    assert_eq!(default_prune_markers(), vec!["pyvenv.cfg".to_string()]);
}
```

Run: `cargo test -p fxrank full_build_union` → PASS (Claude review confirmed the union reproduces the old list exactly; this locks it).

- [ ] **Step 2: Change the `--exclude` arg to detect "not provided"**

In the Scan args (main.rs:42-46) **remove the `default_value = "…"`** and make the field optional so the CLI can distinguish "absent → union default" from "provided → replace":

```rust
    /// Patterns to skip during directory scans (comma-separated; REPLACES the
    /// default union of the enabled frontends' corpus profiles when provided).
    /// Classified by `/` (spec 004): no-`/` literal prunes a dir + excludes a file;
    /// no-`/` glob excludes files only; `/`-bearing glob filters files by path.
    #[arg(long, value_delimiter = ',')]
    exclude: Option<Vec<String>>,
```

At the matcher build site (main.rs ~line 173), resolve the entries:

```rust
    let exclude_entries = exclude.unwrap_or_else(default_exclude_entries);
    let matcher = fxrank_core::CorpusMatcher::build(&exclude_entries)?; // invalid glob → JSON error, parity w/ old
```

(Replace the old `ExcludeMatcher::build(...)?` call; `matcher` keeps the same `dir_pruned`/`file_excluded` API + the `?` error path, so the walk-loop call sites at main.rs:300-329 are unchanged.)

**Also update `run_scan`'s signature + its caller.** `exclude` changed type, so `run_scan` (`main.rs:99-210`, param at line 104: `exclude: Vec<String>`) becomes `exclude: Option<Vec<String>>`, and the `main()` call site (line 82) passes the new `Option` through unchanged (clap already produces it). Without this the crate won't compile.

- [ ] **Step 3: Add the content-marker directory prune to `walk_dir`**

`walk_dir` (main.rs:250-349) currently prunes a child dir iff `matcher.dir_pruned(name)` (lines 300-306). Add a content-marker prune. **Put the check at the TOP of `walk_dir`, against the directory it is about to walk** — this covers the **scan root AND every subdirectory uniformly** (each recursion re-enters `walk_dir`), and `return;` is valid here (`walk_dir` returns `()`, unlike `collect_source_files` which returns a `Vec`). One check, no per-child duplication:

```rust
    // Content-marker prune: a dir containing e.g. `pyvenv.cfg` is a venv root,
    // regardless of its name (`myenv/`, `.env3/`). Catches arbitrarily-named venvs,
    // and (because this runs at walk_dir entry) the scan root itself.
    if markers.iter().any(|m| dir.join(m).is_file()) {
        return;
    }
```

(`dir` is `walk_dir`'s directory-path parameter; adjust to its actual name. `markers: &[String]` is a new parameter threaded through the **full chain**: `run_scan` (compute `let markers = default_prune_markers();` **always — independent of `--exclude`**, per Global Constraints) → `collect_source_files` (`main.rs:228-245`, add the param) → `walk_dir` (`main.rs:250-349`, add the param). Marker prunes, like dir prunes, are NOT counted in `skipped_excluded`. Placing the check at `walk_dir` entry rather than in the parent's child-loop is what makes the directly-scanned-venv-root case work.)

- [ ] **Step 3b: Add a CLI integration test for the marker prune** (durable; not only the Task-5 dogfood)

Add to `crates/fxrank-cli/tests/cli.rs` (or the existing integration test file) a test using a temp dir with `weirdenv/pyvenv.cfg` + a routable file inside it and a normal file outside, asserting (via the JSON output) the inside file is absent from `hotspots` AND `scope.skipped_excluded == 0` (marker prunes are not counted). Also assert scanning the venv root *directly* yields no hotspots.

- [ ] **Step 4: Delete `exclude.rs`, drop its `mod`**

Remove `crates/fxrank-cli/src/exclude.rs` and the `mod exclude;` line in `main.rs`. First **sweep for every consumer** so nothing is missed (tests/benches/examples included):

```bash
rg -n 'ExcludeMatcher|crate::exclude|mod exclude|globset|GlobSet|Glob::' crates/fxrank-cli
```

Repoint each `ExcludeMatcher` → `fxrank_core::CorpusMatcher` (the `dir_pruned`/`file_excluded` API is identical; the build now returns `Result`, so keep the `?`). After the sweep shows no remaining direct `globset`/`Glob`/`GlobSet` use in the CLI, remove `globset` from `crates/fxrank-cli/Cargo.toml` (and confirm it's not a dev-dependency in use).

- [ ] **Step 5: Build all feature combinations**

```bash
cargo build -p fxrank
cargo build -p fxrank --no-default-features --features rust
cargo build -p fxrank --no-default-features --features ts
cargo build -p fxrank --no-default-features --features python
cargo build -p fxrank --no-default-features
```
Expected: all compile. Then add a **feature-gated content test** (proves the union actually excludes the absent languages, not just that it compiles) and run it under the slim feature set:

```rust
#[test]
#[cfg(all(feature = "ts", not(feature = "rust"), not(feature = "python")))]
fn ts_only_union_excludes_other_ecosystems() {
    let e = default_exclude_entries();
    assert!(e.iter().any(|x| x == "node_modules") && e.iter().any(|x| x == ".git"));
    assert!(!e.iter().any(|x| x == "target"), "Rust default leaked into TS-only build");
    assert!(!e.iter().any(|x| x == ".venv"), "Python default leaked into TS-only build");
    assert!(default_prune_markers().is_empty(), "pyvenv.cfg leaked into TS-only build");
}
```

Run it: `cargo test -p fxrank --no-default-features --features ts ts_only_union`
Expected: PASS (a `--features ts` build references only `fxrank_lang_ts::CORPUS_PROFILE` + the `.git` common baseline).

- [ ] **Step 6: Run tests + fmt + clippy + commit**

```bash
cargo test --workspace
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
git add crates/fxrank-cli/src/main.rs crates/fxrank-cli/Cargo.toml Cargo.lock
git rm crates/fxrank-cli/src/exclude.rs
git commit -m "feat(cli): union frontend corpus profiles; pyvenv.cfg marker prune; retire hardcoded list (#21)"
```

---

### Task 4: Docs — cross-language CorpusProfile guideline + spec 004 amendment

Record the cross-language model (the user's explicit requirement) and amend the ownership spec.

**Files:**
- Create: `docs/corpus-profile-guideline.md` (mirror `docs/mutation-classification-guideline.md`'s structure)
- Modify: `docs/superpowers/specs/004-corpus-hygiene-excludes.md` (ownership model + matcher location)
- Modify: `CLAUDE.md` (the `--exclude` bullet under *Conventions & non-obvious gotchas* — defaults are now frontend-declared + CLI-unioned)

- [ ] **Step 1: Write `docs/corpus-profile-guideline.md`**

Mirror the mutation guideline's shape: **Shared model** (the four `CorpusProfile` channels; pure `&'static` data; the two-phase split — name-based in profile/applied by frontend, source-based in `analyze_unit`); **Per-language table** — illustrative; state explicitly that **the `CORPUS_PROFILE` consts (Task 2) are normative/the single source of truth and this table is descriptive** (it must agree with them but doesn't override them); **Honest per-language differences** (Rust tests are source-based → empty `test_file_globs`; TS/Python name-based; the `.git` common baseline owned by no frontend; union follows the compiled-in feature set); **CLI behavior** (union default, `--exclude` replace, dir-prune union, content-marker prune, `skipped_excluded` vs `skipped_tests` routing). Keep it descriptive.

- [ ] **Step 2: Amend spec 004**

In `docs/superpowers/specs/004-corpus-hygiene-excludes.md`, update the **ownership table** (lines ~65-79) and the **default-list section** (lines ~143-161): defaults are no longer a CLI-hardcoded string — they are **frontend-declared `CorpusProfile`s, unioned by the CLI**; the three-class matcher now lives in `fxrank-core::corpus::CorpusMatcher`; add the content-marker prune class; note `--exclude` replace is unchanged. Point to the new guideline.

- [ ] **Step 3: Update CLAUDE.md**

`CLAUDE.md` currently describes `--exclude` in the **Commands** section + the dogfooding notes (there is no standalone "`--exclude`" gotcha bullet yet — locate the most relevant spot, likely the *Conventions & non-obvious gotchas* area near the existing exclude/test-skip notes). State: the default is now the **union of the enabled frontends' `CorpusProfile`s** (frontend-declared) + a `.git` baseline, plus an always-on `pyvenv.cfg` content-marker dir-prune; `--exclude` still **replaces** the glob list (but not the marker prune). Reference `docs/corpus-profile-guideline.md`.

- [ ] **Step 4: Commit**

```bash
git add docs/corpus-profile-guideline.md docs/superpowers/specs/004-corpus-hygiene-excludes.md CLAUDE.md
git commit -m "docs: cross-language CorpusProfile guideline + spec 004 amendment (#21)"
```

---

### Task 5: Dogfood verification

Confirm the win (Python noise pruned, marker prune works) and no regression (TS/Rust unchanged, snapshots stable).

- [ ] **Step 1: Build the release binary**

Run: `cargo build --release -p fxrank`

- [ ] **Step 2: Python noise is pruned**

```bash
set -o pipefail
target/release/fxrank scan /home/caasi/GitHub/django/django \
  | jq '{files: .scope.files, leaked: [.hotspots[].path | select(test("__pycache__|/\\.venv/|/site-packages/|/\\.tox/"))]}'
```
**Concrete criterion (not "ballpark"):** `leaked` is `[]` (no `__pycache__`/`.venv`/`site-packages`/`.tox` paths in the output). Record the exact `files` count in the PR; it should equal the pre-#21 count (the interim central list already pruned these — Task 3 moved them into the Python profile, so behavior is equivalent).

- [ ] **Step 3: Content-marker prune works on an arbitrarily-named venv**

```bash
mkdir -p /tmp/cp-test/weirdenv && printf 'home = /usr\n' > /tmp/cp-test/weirdenv/pyvenv.cfg
printf 'import os\n_c={}\ndef f():\n    _c["k"]=1\n' > /tmp/cp-test/weirdenv/leak.py
printf 'def g(): pass\n' > /tmp/cp-test/app.py
set -o pipefail
target/release/fxrank scan /tmp/cp-test | jq '{paths: [.hotspots[].path], skipped_excluded: .scope.skipped_excluded}'
```
Expected: `paths` contains only `app.py` (`weirdenv/leak.py` is pruned because the dir has `pyvenv.cfg`, even though `weirdenv` is not a literal in any `prune_dirs`); **and `skipped_excluded == 0`** — a marker prune, like a dir prune, is NOT counted in `skipped_excluded` (the dir is never read). Clean up: `rm -rf /tmp/cp-test`.

- [ ] **Step 4: TS unchanged + no snapshot drift**

```bash
set -o pipefail
target/release/fxrank scan /home/caasi/GitLab/omni/114-kg-frontend/src | jq '.scope.files, .scope.skipped_tests'
cargo test --workspace
git status --porcelain crates/*/tests/snapshots/
```
Expected: TS file/skipped_tests counts match pre-#21; `cargo test --workspace` green; **no `.snap.new`** (the `Scope` JSON is unchanged). If a snapshot drifts, stop and investigate.

- [ ] **Step 5: Record results** in the PR description (django file count, marker-prune proof, TS parity).

---

## Self-Review

**1. Spec coverage** (issue #21):
- `CorpusProfile` on the `Frontend` trait, pure `&'static str` data → Task 1. ✓
- Phase-1 name-based (prune/exclude/test globs) unified; Phase-2 source-based stays in `analyze_unit` → Tasks 1–2 (test_file_globs declared in profile, applied by frontend; source-based untouched). ✓
- CLI unions enabled frontends' profiles; dir prunes unioned; slim build → only its language's defaults → Task 3. ✓
- `--exclude` replace unchanged → Task 3 Step 2. ✓
- `pyvenv.cfg` content-marker prune (user decision) → Task 1 (channel) + Task 3 Step 3. ✓
- `--exclude-add` NOT added (user decision); routing bug NOT touched (user decision). ✓ (out of scope, correctly absent)
- Interim #14 central Python list removed → Task 3 (the union replaces it; the hardcoded `default_value` is deleted). ✓
- Cross-language part recorded + implementations aligned (user requirement) → Task 4 guideline (single source of truth agreeing with the Task-2 consts) + spec 004 amendment. ✓
- `skipped_excluded` (files, not `--include-tests`) vs `skipped_tests` (units, `--include-tests`) routing preserved → Global Constraints + Task 2 (frontend keeps unit-count semantics). ✓

**2. Placeholder scan:** every code step has complete code; commands have expected output; no TBD. The matcher `corpus.rs` is given in full; per-frontend profiles are literal. ✓

**3. Type consistency:** `CorpusProfile` (4 `&'static [&'static str]` channels) and `CorpusMatcher` (`build`/`test_matcher`/`dir_pruned`/`file_excluded`/`matches_test_file`) are used identically in core (Task 1), frontends (Task 2, via `CorpusMatcher::test_matcher(CORPUS_PROFILE.test_file_globs)`), and CLI (Task 3, `CorpusMatcher::build(&entries)`). `corpus_profile(&self) -> CorpusProfile` default + per-frontend override are consistent. `CORPUS_PROFILE` is the const name in all three frontends. ✓
