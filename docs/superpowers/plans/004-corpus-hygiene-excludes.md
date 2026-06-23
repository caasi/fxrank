# Corpus-Hygiene Excludes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use @superpowers:subagent-driven-development (recommended) or @superpowers:executing-plans to implement this plan task-by-task. Use @superpowers:test-driven-development for every task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Grow `fxrank scan --exclude` from directory-name matching to a three-class glob matcher (literal / filename-glob / path-glob) with a richer default skip list, and report a `scope.skipped_excluded` count — so vendored bundles, Storybook stories, `jest.setup`, and generated files stop burying real signal (issue #6).

**Architecture:** A new `ExcludeMatcher` (in `crates/fxrank-cli/src/exclude.rs`) classifies each `--exclude` entry once — **by whether it contains `/`** — into three structures: a literal `HashSet` (prunes dirs **and** excludes files), a filename `GlobSet` (excludes files only, never prunes), and a path `GlobSet` (excludes files by `/`-normalized relative path). `walk_dir` consults it; a `skipped_excluded` counter is threaded out and into `core::Scope`. The core gains one serialized field; no scoring or frontend change.

**Tech Stack:** Rust, `clap` (CLI), `globset` (BurntSushi/ripgrep glob engine, **new** dep in `fxrank-cli` only — core stays parser/dep-free), `serde`/`serde_json` (wire format), `assert_cmd`/`tempfile` (CLI integration tests).

**Source of truth:** `docs/superpowers/specs/004-corpus-hygiene-excludes.md`. When this plan and the spec disagree, the spec wins — re-read it before changing matcher behavior.

---

## Execution context (read first)

- **Code goes on a feature branch, never `main`.** Per project convention, run this
  plan in a git worktree on a feature branch (e.g. `feat/004-corpus-hygiene-excludes`).
  See @superpowers:using-git-worktrees. The plan/spec docs already live on `main`.
- **Commits are grouped logically** — one commit per task below (not per step). Each
  task's commit must be green (`cargo test --workspace`) and clippy-clean
  (`cargo clippy --workspace --all-targets -- -D warnings`), so there is no dead-code
  window between tasks.
- **CI gates** (run before pushing): `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
  plus the slim builds (`--features rust`, `--features ts`, no-features).

## File Structure

| File | Create / Modify | Responsibility |
| --- | --- | --- |
| `crates/fxrank-core/src/model.rs` | Modify | Add `skipped_excluded: usize` to `Scope` (after `skipped_tests`); migrate all constructors; update the serialization-order test. |
| `crates/fxrank-cli/Cargo.toml` | Modify | Add `globset` dependency. |
| `crates/fxrank-cli/src/exclude.rs` | Create | `ExcludeMatcher`: classify + match `--exclude` entries (three classes). Self-contained, unit-tested. |
| `crates/fxrank-cli/src/main.rs` | Modify | `mod exclude;`; build the matcher in `run_scan` **(directory branch only)** (compile errors → JSON error); thread it + scan-root + `skipped_excluded` through `collect_source_files`/`walk_dir`; update the `--exclude` `default_value` + doc comment; set `Scope.skipped_excluded`. |
| `crates/fxrank-cli/tests/cli.rs` | Modify | Integration tests for the new exclude behavior. |
| `CLAUDE.md` | Modify | Document the new default exclude list + `skipped_excluded` under *Commands* / *Conventions*. |

---

## Task 1: Core — add `scope.skipped_excluded`

**Files:**
- Modify: `crates/fxrank-core/src/model.rs` (`Scope` struct ~lines 6-13, `Scope::empty` ~lines 15-25, order test ~lines 241-257, and every other `Scope { … }` in `#[cfg(test)]`)
- Modify: `crates/fxrank-lang-ts/tests/snapshots.rs` (a `Scope { … }` literal at ~line 18 — a **test** target, so `cargo build` won't catch it; only `cargo test` will)
- Modify: `crates/fxrank-cli/src/main.rs` (the `Scope { … }` in `run_scan` ~line 182 — temporary `0` here, real count wired in Task 2)

The wire format gains one field, declared **between `skipped_tests` and `risk_features`** (serde emits in declaration order — same precedent as 002's `skipped_tests`).

- [ ] **Step 1: Update the serialization-order test to expect the new field (RED)**

In `crates/fxrank-core/src/model.rs`, find the test `scope_serializes_skipped_tests_between_functions_and_risk_features`. Add `skipped_excluded` to its `Scope { … }` literal and update the substring assertion:

```rust
// in the Scope { … } literal of that test, after `skipped_tests: 3,`
skipped_excluded: 5,
// …
assert!(json.contains(
    "\"functions\":2,\"skipped_tests\":3,\"skipped_excluded\":5,\"risk_features\":"
));
```

- [ ] **Step 2: Run it to verify it fails to compile (RED)**

Run: `cargo test -p fxrank-core --lib`
Expected: compile error — `Scope` has no field `skipped_excluded`.

- [ ] **Step 3: Add the field + migrate every constructor (GREEN)**

In the `Scope` struct, after `pub skipped_tests: usize,`:

```rust
pub skipped_excluded: usize,
```

In `Scope::empty`, after `skipped_tests: 0,`:

```rust
skipped_excluded: 0,
```

Then migrate **every other** `Scope { … }` literal across the **whole workspace** so it compiles. Find ALL of them (not just model.rs — there are sibling-crate and CLI sites):

Run: `grep -rn "Scope {" crates/`
Expected sites to migrate (add `skipped_excluded: 0,` after each one's `skipped_tests:` line):
- `crates/fxrank-core/src/model.rs` — the remaining `#[cfg(test)]` literals (~170, 211, 227, 262). (`Scope::empty` already done above; the struct + order-test done in Steps 1/3.)
- `crates/fxrank-lang-ts/tests/snapshots.rs` (~line 18) — **easy to miss**; it's a test target. The `.snap` files only serialize `hotspots`+`summary` (not the scope), so no snapshot regeneration is needed.
- `crates/fxrank-cli/src/main.rs` (`run_scan`, ~line 182) — temporary `0`; Task 2 wires the real count.

- [ ] **Step 4: Run core tests (GREEN)**

Run: `cargo test -p fxrank-core`
Expected: PASS (the order test now asserts the `skipped_excluded` slot).

- [ ] **Step 5: Verify the whole workspace compiles + is clean**

A public-struct change touches **test** targets that `cargo build` does not compile, so verify with the test + clippy gates, not just a build:

Run: `cargo test --workspace` then `cargo clippy --workspace --all-targets -- -D warnings`
Expected: green — confirms every `Scope { … }` site (incl. the ts snapshot test) was migrated.

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-core/src/model.rs crates/fxrank-cli/src/main.rs \
        crates/fxrank-lang-ts/tests/snapshots.rs
git commit -m "feat(core): add scope.skipped_excluded field

Wire-format addition (between skipped_tests and risk_features) for
reporting files dropped by --exclude. CLI sets it in 004's matcher task;
0 everywhere for now."
```

---

## Task 2: CLI — the `ExcludeMatcher` + walk integration + new defaults

This is the feature, committed as one logical unit. TDD: matcher unit tests first, then the matcher; then integration tests for the wiring, then the wiring. No dead-code window — the matcher is consumed by `walk_dir` in the same commit.

**Files:**
- Modify: `crates/fxrank-cli/Cargo.toml`
- Create: `crates/fxrank-cli/src/exclude.rs`
- Modify: `crates/fxrank-cli/src/main.rs`
- Modify: `crates/fxrank-cli/tests/cli.rs`

### 2a — dependency + matcher module (unit-TDD)

- [ ] **Step 1: Add the `globset` dependency**

In `crates/fxrank-cli/Cargo.toml`, under `[dependencies]`:

```toml
globset = "0.4"
```

- [ ] **Step 2: Create the module with failing unit tests (RED)**

Two parts. **(a)** Create `crates/fxrank-cli/src/exclude.rs` containing **only** the `#[cfg(test)] mod tests` below (no `ExcludeMatcher` yet). **(b)** Declare the module so it actually compiles: add `mod exclude;` near the top of `crates/fxrank-cli/src/main.rs` (with the other items). In a **binary** crate a sibling file is not compiled until `main.rs` declares it — without this, Step 3 wouldn't even compile `exclude.rs` (no real RED). The empty-in-non-test module emits no warning. The test module references `super::ExcludeMatcher`, which doesn't exist yet, so Step 3's RED is a **compile error**. Write the tests first:

```rust
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
}
```

- [ ] **Step 3: Run unit tests to verify they fail (RED)**

Run: `cargo test -p fxrank exclude`  (`fxrank-cli` is a **binary** crate — there is no `--lib`; this filters tests named `exclude` across its targets)
Expected: compile error — `ExcludeMatcher` does not exist yet.

- [ ] **Step 4: Implement the matcher (GREEN)**

Prepend the implementation to `crates/fxrank-cli/src/exclude.rs`:

```rust
use globset::{Glob, GlobSet, GlobSetBuilder};
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
                let glob = Glob::new(entry)
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
```

- [ ] **Step 5: Run unit tests (GREEN)**

`mod exclude;` was already declared in Step 2, so the new impl is picked up.

Run: `cargo test -p fxrank exclude`
Expected: the 5 matcher unit tests PASS. (This name filter also re-runs the existing `exclude_skips_default_dirs_and_flag_overrides` integration test — expected, not noise.)

> Note: the matcher is consumed by `walk_dir` in 2b within this same task (same commit), so there is no `dead_code` window for clippy.

### 2b — wire into the walk + new defaults (integration-TDD)

- [ ] **Step 6: Write integration tests for the wiring (RED)**

Append to `crates/fxrank-cli/tests/cli.rs` (helpers `fxrank()`/`TempDir` already imported). These exercise default file-glob exclusion, the `skipped_excluded` count, and wildcard-never-prunes:

```rust
// ── Task 004: default file-glob excludes + skipped_excluded count ──
#[test]
fn default_excludes_skip_bundles_stories_and_count_them() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("mkdir");
    // one real source file (kept) + three default-excluded files
    std::fs::write(src.join("app.ts"), "export function ok() { return 1; }\n").unwrap();
    std::fs::write(src.join("vendor.min.js"), "function a(){}\n").unwrap();
    std::fs::write(src.join("Button.stories.tsx"), "export const s = () => 1;\n").unwrap();
    std::fs::write(src.join("jest.setup.js"), "globalThis.x = 1;\n").unwrap();

    let out = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(3),
        "three default-excluded files; got: {j}");
    // only app.ts contributed functions
    assert!(j["scope"]["functions"].as_u64().unwrap_or(0) >= 1);
}

// ── Task 004: a wildcard entry must NOT prune a same-named directory ──
#[test]
fn wildcard_default_does_not_prune_matching_directory() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    // directory name matches the default `*.stories.*`
    let d = root.join("x.stories.d");
    std::fs::create_dir_all(&d).expect("mkdir");
    std::fs::write(d.join("keep.ts"), "export function keep() { return 2; }\n").unwrap();

    let out = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    // keep.ts under x.stories.d/ is still scanned (wildcard files-only)
    assert!(j["hotspots"].as_array().unwrap().iter()
        .any(|h| h["symbol"].as_str() == Some("keep")),
        "x.stories.d/ must not be pruned by the `*.stories.*` default; got: {j}");
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(0));
}

// ── Task 004: invalid glob → non-zero exit + JSON error ──
#[test]
fn invalid_exclude_glob_is_startup_error() {
    let tmp = TempDir::new().expect("tmp");
    let out = fxrank().arg("scan").arg(tmp.path())
        .arg("--exclude").arg("[").output().expect("ran");
    assert!(!out.status.success(), "expected non-zero exit for bad glob");
    let j: serde_json::Value =
        serde_json::from_str(String::from_utf8(out.stdout).unwrap().trim())
        .expect("JSON error object");
    assert!(j.get("error").is_some(), "expected error key; got: {j}");
}
```

- [ ] **Step 7: Run them to verify they fail (RED)**

Run: `cargo test -p fxrank --test cli`  (runs the whole CLI test binary)
Expected: `default_excludes_skip_bundles_stories_and_count_them` and `invalid_exclude_glob_is_startup_error` FAIL (defaults are still dir-name-only; `skipped_excluded` is 0; a bad glob doesn't error yet). `wildcard_default_does_not_prune_matching_directory` already passes even pre-wiring (the old default has no `*.stories.*`, so nothing prunes `x.stories.d/`) — it serves as a regression guard that the wiring keeps it passing.

- [ ] **Step 8: Update the `--exclude` arg (default + doc comment)**

In `crates/fxrank-cli/src/main.rs`, the `Cmd::Scan` `exclude` field (~lines 35-42). Replace the doc comment and `default_value`:

```rust
/// Patterns to skip during directory scans (comma-separated; replaces the
/// default list when provided). Classified by `/`: a no-`/` literal prunes a
/// matching directory and excludes a matching file; a no-`/` glob (`*.min.js`,
/// `*.stories.*`) excludes files only; a `/`-bearing glob (`src/legacy/**`)
/// filters files by path. An entry cannot contain a comma (the list delimiter),
/// so brace alternation with commas (`*.{js,ts}`) must be split into entries.
#[arg(
    long,
    value_delimiter = ',',
    default_value = "node_modules,.git,target,*.min.js,*.min.mjs,*.min.cjs,*.stories.*,mockServiceWorker.js,jest.setup.*,jest.config.*,__mocks__"
)]
exclude: Vec<String>,
```

- [ ] **Step 9: Build the matcher in `run_scan`; thread it + root + count through the walk (GREEN)**

In `main.rs`:

1. **`main()`** (~lines 65-90): stop converting `exclude` to a `HashSet`; pass the `Vec<String>` straight to `run_scan`. **Delete** the `let exclude_set: HashSet<String> = …;` line and the `use std::collections::HashSet;` import at the top — after this change nothing else in `main.rs` uses `HashSet`, so the import is unconditionally dead and `clippy -D warnings` will fail if it remains. Update the call to `run_scan(path, limit, include_tests, lang, exclude)`.

2. **`run_scan` signature + counter scope**: change the param to `exclude: Vec<String>`. **Do not build the matcher at the top** — `--exclude` is a no-op for single-file/stdin (spec), so a bad glob must *not* error those paths. Declare the counter *before* the `let (input_label, routed) = …` binding so it is in scope for the `Scope` later and stays `0` for the file/stdin branches; build the matcher **only inside the directory branch**:

```rust
fn run_scan(
    path: Option<PathBuf>,
    limit: Option<usize>,
    include_tests: bool,
    lang: Option<String>,
    exclude: Vec<String>,
) -> Result<Report, String> {
    let mut read_errors: Vec<Diagnostic> = Vec::new();
    let mut skipped_excluded = 0usize;        // 0 for stdin/single-file (no-op)
    // … existing is_stdin / --lang checks …
    let (input_label, routed) = if is_stdin {
        // … unchanged …
    } else {
        let p = path.expect("path present when not stdin");
        if !p.exists() { /* … unchanged Err … */ }
        if p.is_file() {
            // … single-file branch UNCHANGED — it does NOT consult --exclude …
        } else {
            // DIRECTORY branch — the ONLY place the matcher is built:
            let matcher = exclude::ExcludeMatcher::build(&exclude)?;  // bad glob → JSON error, non-zero
            let label = p.to_string_lossy().into_owned();
            let routed = collect_source_files(&p, &p, &mut read_errors, &matcher, &mut skipped_excluded);
            (label, routed)
        }
    };
```

(So `fxrank scan a.ts --exclude '['` and `… scan --lang ts - --exclude '['` succeed — the matcher is never built — while a directory scan with a bad glob fails fast. The matcher line goes in the **inner** `else` (directory), never the `p.is_file()` branch.)

3. **`Scope`** (~lines 182-189): set the real count:

```rust
skipped_excluded,
```

4. **`collect_source_files` + `walk_dir`**: replace the `exclude: &HashSet<String>` param with `root: &std::path::Path`, `matcher: &exclude::ExcludeMatcher`, and `skipped_excluded: &mut usize`. In `walk_dir`:

```rust
// directory branch (replaces the HashSet `contains` prune)
if file_type.is_dir() {
    let dir_name = entry.file_name();
    if matcher.dir_pruned(&dir_name.to_string_lossy()) {
        continue;
    }
    walk_dir(&path, root, sources, read_errors, matcher, skipped_excluded);
} else if file_type.is_file() {
    if let Some(route) = route_for_path(&path) {
        // exclusion runs AFTER routing (spec 004 invariant)
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if matcher.file_excluded(&file_name, &rel_str) {
            *skipped_excluded += 1;
            continue;
        }
        match std::fs::read_to_string(&path) {
            // …unchanged…
        }
    }
}
```

(The `use std::collections::HashSet;` import was already removed in item 1.)

- [ ] **Step 10: Run the full suite (GREEN)**

Run: `cargo test -p fxrank` then `cargo test --workspace`
Expected: PASS — new tests green; existing `exclude_skips_default_dirs_and_flag_overrides` still passes (`src` is a literal that prunes; replace semantics unchanged).

- [ ] **Step 11: fmt + clippy**

Run: `cargo fmt` then `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 12: Commit (one logical feature commit)**

```bash
git add crates/fxrank-cli/Cargo.toml crates/fxrank-cli/src/exclude.rs \
        crates/fxrank-cli/src/main.rs crates/fxrank-cli/tests/cli.rs Cargo.lock
git commit -m "feat(cli): glob-aware --exclude with default skip list (issue #6)

Three-class matcher (literal / filename-glob / path-glob) classified by
presence of '/'. Literal entries prune dirs + exclude files; wildcard
filename entries exclude files only (never prune a same-named dir); path
globs filter by relative path. Richer documented default list (bundles,
stories, jest setup/config, __mocks__, MSW worker) and a skipped_excluded
count. Invalid globs are a startup error. globset (CLI-only) backs it."
```

---

## Task 3: Additional coverage + docs

**Files:**
- Modify: `crates/fxrank-cli/tests/cli.rs`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add the remaining behavioral tests**

Append to `crates/fxrank-cli/tests/cli.rs` — replace semantics, `--include-tests` orthogonality, single-file no-op, and the `files == parsed` accounting:

```rust
// ── Task 004: --exclude replaces the default (not additive) ──
#[test]
fn exclude_replaces_default_so_bundles_reappear() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("a.min.js"), "function a(){ fetch('x'); }\n").unwrap();
    // override with an unrelated pattern → a.min.js is no longer excluded
    let out = fxrank().arg("scan").arg(root).arg("--exclude").arg("*.nope")
        .output().expect("ran");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(j["scope"]["functions"].as_u64().unwrap_or(0) >= 1,
        "a.min.js should be scanned once defaults are replaced; got: {j}");
}

// ── Task 004: --exclude is a no-op for an explicitly named single file ──
#[test]
fn exclude_does_not_apply_to_single_file_target() {
    let tmp = TempDir::new().expect("tmp");
    let f = tmp.path().join("vendor.min.js");
    std::fs::write(&f, "function a(){ fetch('x'); }\n").unwrap();
    // Even though *.min.js is a default exclude, naming the file scans it. The
    // bogus `--exclude '['` ALSO proves the no-op: for a single file the matcher
    // is never built, so the invalid glob can't error (guards against building
    // the matcher at the top of run_scan).
    let out = fxrank().arg("scan").arg(&f).arg("--exclude").arg("[").output().expect("ran");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(j["scope"]["functions"].as_u64().unwrap_or(0) >= 1,
        "explicit single-file target must be honored; got: {j}");
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(0));
}

// ── Task 004: --include-tests does NOT re-include a *.stories.* file ──
#[test]
fn include_tests_does_not_reinclude_excluded_stories() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("X.stories.tsx"), "export const s = () => 1;\n").unwrap();
    let out = fxrank().arg("scan").arg(root).arg("--include-tests")
        .output().expect("ran");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(1),
        "stories stay excluded under --include-tests (exclude != test mechanism); got: {j}");
}

// ── Task 004: files accounting — excluded files are in neither files nor read_errors ──
#[test]
fn excluded_files_count_in_skipped_not_files() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("app.ts"), "export function ok() { return 1; }\n").unwrap();
    std::fs::write(root.join("vendor.min.js"), "function a(){}\n").unwrap(); // excluded by default
    let out = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // app.ts is the only file read; vendor.min.js is excluded (not counted in files)
    assert_eq!(j["scope"]["files"].as_u64(), Some(1), "files = read files only; got: {j}");
    assert_eq!(j["scope"]["parsed"].as_u64(), Some(1));
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(1));
}
```

- [ ] **Step 2: Run the new tests (verify they pass against Task 2's implementation)**

Run: `cargo test -p fxrank --test cli`
Expected: PASS. (These characterize behavior implemented in Task 2; if any fails, the spec's intended behavior is not met — fix `main.rs`/`exclude.rs`, not the test.)

- [ ] **Step 3: Update `CLAUDE.md`**

Under the *Commands* section, document the new default and field. Add near the existing `--include-tests` line:

```markdown
cargo run -p fxrank -- scan <path> --exclude 'node_modules,*.min.js,*.stories.*'  # replaces the default skip list
```

And add a *Conventions* bullet:

```markdown
- **`--exclude` is a three-class matcher** (spec 004): a no-`/` literal prunes a
  matching directory and excludes a matching file; a no-`/` glob (`*.min.js`,
  `*.stories.*`) excludes files only (never prunes a same-named dir); a `/`-bearing
  glob filters files by relative path. It **replaces** the default list when given.
  The default skips vendored bundles, Storybook stories, `jest.setup`/`jest.config`,
  `__mocks__`, and the MSW worker. Files dropped this way are counted in
  `scope.skipped_excluded` (directory prunes are not counted — they are never read).
  `--exclude` applies only to directory scans; an explicitly named file/stdin is
  always scanned.
```

- [ ] **Step 4: fmt + clippy + full suite + slim builds**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p fxrank --no-default-features --features rust
cargo build -p fxrank --no-default-features --features ts
cargo build -p fxrank --no-default-features
```
Expected: all green (globset is a CLI dep, present under every feature combo).

- [ ] **Step 5: Dogfood sanity check (output unchanged)**

Run: `cargo run -p fxrank -- scan crates/ | jq '.scope'`
Expected: `skipped_excluded` is `0` (our tree has no `*.min.js`/stories/jest files; `target` already pruned), `hotspots` unchanged — `run_scan`/`walk_dir` still surface as the IO boundaries.

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-cli/tests/cli.rs CLAUDE.md
git commit -m "test(cli): exclude edge cases + docs for --exclude defaults

Replace-semantics, single-file no-op, --include-tests orthogonality;
document the default skip list and scope.skipped_excluded."
```

---

## Dogfood the snippets (this is an effect profiler — eat our own dog food)

Before/while implementing, run the plan's own code snippets through `fxrank` to
sanity-check them. Verified during planning:

- **Matcher impl** (`exclude.rs`) piped to `fxrank scan -`: all four functions score
  **class 0–1** (`build` own_score `1.0`, the rest `0.0`) — the new production code is
  pure/low-effect, as a glob matcher should be. If a future edit makes it score high,
  that's a smell worth a second look.
- **A `*.stories.*` handler fixture** (`scan --lang ts -`): the demo handler with
  `fetch` scores **class 7** (own_score `21.0`) — empirical confirmation that stories
  carry real IO and are genuine ranking noise, i.e. the exclusion is justified, not
  cosmetic.

Re-run these on the real files once they exist (the matcher should still be ~0).

## Done criteria

- `cargo test --workspace` green; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --check` clean; all three slim builds compile.
- `fxrank scan` on a tree with `*.min.js` / `*.stories.*` / `jest.setup.js` skips them by default and reports the count in `scope.skipped_excluded`.
- A directory named `x.stories.d/` is **not** pruned by the `*.stories.*` default.
- A bad glob (`--exclude '['`) exits non-zero with a JSON `error`.
- `--exclude` replaces the default; an explicitly named file/stdin ignores it.
- Dogfood `scan crates/` output unchanged (`skipped_excluded == 0`).
- Open a PR linking issue #6 (see @superpowers:finishing-a-development-branch).
