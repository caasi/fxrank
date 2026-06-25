# 025-3e Plan 4 — Python module-map emitter (adopt the Python partition) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Python frontend emit `UnitRecord.canonical_path` + `CallSiteRef.resolved_target`, flipping the Python partition to *adopted* so `resolve_ref_precise` does path-precise resolution — fixing the false-resolve (`mod.write()` no longer resolves to a lone in-project `write`) while keeping real in-project absolute-import, relative-import, and same-module calls resolving.

**Architecture:** Python modules are files addressed by **dotted paths** rooted at a **package root** (the nearest ancestor dir without `__init__.py`, walking up through `__init__.py` dirs). Build a `PyModuleMap` once over the `SourceFile` batch that (a) maps each in-batch file to its dotted module key, and (b) resolves a dotted module string (absolute, or relative with a known dot-level) to an in-batch key. A unit's `canonical_path = [..module-segments, symbol]`; a call's `resolved_target = [..target-module-segments, name]` where the import table already retains the original name (Python advantage: `from m import n as p` → `"m.n"`, so the export name `n` survives renames). Pure string work on in-batch paths — no disk, no sys.path. canonical_path + resolved_target ship together (adoption couples them).

**Tech Stack:** Rust, `libcst` (crate `libcst`, lib `libcst_native`, `default-features=false`) — already a dep of `fxrank-lang-python`. No new dependencies. `fxrank-core` untouched (Plan 1). Rust + TS frontends untouched.

## Global Constraints

- **No disk I/O, no sys.path, no interpreter.** All module-key math is over the in-batch `SourceFile.path` strings only. (spec 025-3e §5.3)
- **Own-body output byte-identical** to pre-3e (`effects`/`risks`/`own_score`/`symbol`/`unit_id` unchanged); 3e only *adds* canonical_path/resolved_target and changes `propagated_*`. (§2)
- **Module key = dotted path rooted at the package root.** Package root = nearest ancestor dir WITHOUT `__init__.py`, walking up through dirs that HAVE `__init__.py`. `pkg/__init__.py`, `pkg/sub/__init__.py`, file `pkg/sub/mod.py` → key `pkg.sub.mod`. A top-level `foo.py` with no `__init__.py` sibling → key `foo`. `pkg/sub/__init__.py` itself → key `pkg.sub`.
- **Never-guess invariant:** only emit `resolved_target` when the target module + name are determined (absolute dotted import resolving to an in-batch key; relative import with a known dot-level resolved against the referencing module's package; a same-module bare call). Stdlib / third-party / unresolvable → `None` → opaque. Never guess a name from a coincidental match.
- **Documented misses (accepted, degrade to opaque, §9):** `from m import *` star imports (local names unknown — `imports.rs` already skips them); `importlib`/`__import__` dynamic imports; namespace packages (PEP 420, no `__init__.py`) resolved by the no-`__init__.py`-ancestor rule may differ from a real interpreter; `__init__.py` re-exports (AliasFacts deferred).
- CI gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- TDD: failing test first, minimal code, green, commit. Frequent commits.

## File Structure

- `crates/fxrank-lang-python/src/module_map.rs` — **create**: `PyModuleMap` (dotted-key normalization via `__init__.py` package roots + absolute/relative module resolution). Pure, no libcst.
- `crates/fxrank-lang-python/src/lib.rs` — **modify**: declare `pub mod module_map`; in `analyze`, build the map once and thread it into the `build_record` calls (the per-unit path AND the module-init path).
- `crates/fxrank-lang-python/src/detect/mod.rs` — **modify** `build_record`: accept the `PyModuleMap`; compute `canonical_path`; pass the owning module key into `refs::extract`.
- `crates/fxrank-lang-python/src/detect/refs.rs` — **modify** `extract`: accept the referencing module key + the `PyModuleMap` + the import table's relative-level info; compute `resolved_target`.
- `crates/fxrank-lang-python/src/imports.rs` — **modify**: retain the **relative dot-level** per relative local (today only a boolean `relative_locals`), so relative imports resolve precisely.

---

### Task 1: `PyModuleMap` — dotted module key + absolute/relative resolution

**Files:**
- Create: `crates/fxrank-lang-python/src/module_map.rs`
- Modify: `crates/fxrank-lang-python/src/lib.rs` (add `pub mod module_map;`)

**Interfaces:**
- Consumes: `fxrank_core::frontend::SourceFile` (`{ path, text }`).
- Produces:
  - `pub struct PyModuleMap` with `pub fn build(files: &[SourceFile]) -> Self`
  - `pub fn module_of(&self, file_path: &str) -> Option<Vec<String>>` — dotted module segments for an in-batch file (e.g. `["pkg","sub","mod"]`); `None` if the path is not a `.py` file in the batch.
  - `pub fn resolve_absolute(&self, dotted: &str) -> Option<Vec<String>>` — an absolute dotted module string (`"pkg.sub.mod"`) → its segments IF that module is an in-batch key, else `None`.
  - `pub fn is_package(&self, file_path: &str) -> bool` — true for a package `__init__.py`.
  - `pub fn resolve_relative(&self, referencing: &[String], is_package: bool, level: usize, dotted_suffix: &str) -> Option<Vec<String>>` — resolve a relative import: anchor at the importer's PACKAGE (key itself if `is_package`, else key minus stem), walk up `level-1` more, append `dotted_suffix`, return the segments IF in-batch else `None`.

**Algorithm (pure path strings; the batch IS the universe):**
- Build the set of in-batch module keys: for each `.py` file, compute its dotted key by walking UP from its directory collecting dir names WHILE each dir has an `__init__.py` in the batch; stop at the first dir without one (that's the package root, excluded). The key = `[outermost-package … file-dir] + file-stem` (drop `__init__` as the stem — an `__init__.py` keys to its package, no `__init__` segment).
- `module_of` returns the file's key (computed the same way).
- `resolve_absolute(dotted)` = `dotted.split('.')` if that vec is a known in-batch key, else None.
- `resolve_relative(referencing, is_package, level, suffix)`: anchor = `referencing` if `is_package` else `referencing[..len-1]` (drop the module stem); then drop `level-1` more trailing segments (Python: level 1 = the anchor package itself); `target = anchor' ++ suffix.split('.')`; return `Some(target)` iff a known in-batch key, else None. (The `is_package` bit is REQUIRED — a key like `pkg.sub` is ambiguous between regular module `pkg/sub.py` and package `pkg/sub/__init__.py`, which anchor differently.)

- [ ] **Step 1: Write the failing tests**

```rust
// crates/fxrank-lang-python/src/module_map.rs (new; tests at bottom)
#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::frontend::SourceFile;
    fn sf(p: &str) -> SourceFile { SourceFile { path: p.into(), text: String::new() } }

    fn batch() -> Vec<SourceFile> {
        vec![
            sf("pkg/__init__.py"),
            sf("pkg/sub/__init__.py"),
            sf("pkg/sub/mod.py"),
            sf("pkg/util.py"),
            sf("top.py"),            // no __init__.py sibling → top-level module
        ]
    }

    #[test]
    fn module_key_via_init_packages() {
        let m = PyModuleMap::build(&batch());
        assert_eq!(m.module_of("pkg/sub/mod.py"), Some(vec!["pkg".into(),"sub".into(),"mod".into()]));
        assert_eq!(m.module_of("pkg/util.py"), Some(vec!["pkg".into(),"util".into()]));
        assert_eq!(m.module_of("pkg/sub/__init__.py"), Some(vec!["pkg".into(),"sub".into()]));
        assert_eq!(m.module_of("top.py"), Some(vec!["top".into()]));
    }

    #[test]
    fn resolve_absolute_in_batch_only() {
        let m = PyModuleMap::build(&batch());
        assert_eq!(m.resolve_absolute("pkg.sub.mod"), Some(vec!["pkg".into(),"sub".into(),"mod".into()]));
        assert_eq!(m.resolve_absolute("pkg.util"), Some(vec!["pkg".into(),"util".into()]));
        assert_eq!(m.resolve_absolute("os.path"), None);        // stdlib, not in batch
        assert_eq!(m.resolve_absolute("pkg.missing"), None);
    }

    #[test]
    fn resolve_relative_via_package_walk() {
        let m = PyModuleMap::build(&batch());
        let mod_ref = vec!["pkg".to_string(), "sub".into(), "mod".into()]; // regular module pkg/sub/mod.py
        // from pkg.sub.mod (regular, is_package=false): `from .. import util` (level 2) →
        // anchor=pkg.sub, up=1 → pkg, + "util" = pkg.util
        assert_eq!(m.resolve_relative(&mod_ref, false, 2, "util"), Some(vec!["pkg".into(),"util".into()]));
        // `from . import mod` (level 1) from pkg.sub.mod → anchor=pkg.sub, up=0 → pkg.sub, + "mod"
        assert_eq!(m.resolve_relative(&mod_ref, false, 1, "mod"), Some(vec!["pkg".into(),"sub".into(),"mod".into()]));
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
            Some(vec!["pkg".into(),"sub".into(),"mod".into()])
        );
        // `from .. import util` (level 2) from the pkg.sub package → anchor=pkg.sub, up=1 → pkg, +util
        assert_eq!(m.resolve_relative(&pkg_ref, true, 2, "util"), Some(vec!["pkg".into(),"util".into()]));
    }

    #[test]
    fn relative_import_from_top_level_module_is_none() {
        // top.py (no parent package): `from .util import write` is invalid Python
        // ("no known parent package") — must NOT resolve to a root-level util. (P2 round 3)
        let m = PyModuleMap::build(&batch());
        assert_eq!(m.resolve_relative(&["top".into()], false, 1, "util"), None);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p fxrank-lang-python module_map 2>&1 | head -20`
Expected: compile error — `module_map`/`PyModuleMap` not found.

- [ ] **Step 3: Declare the module and implement `PyModuleMap`**

In `crates/fxrank-lang-python/src/lib.rs`, add near the other `pub mod` lines: `pub mod module_map;`

Create `crates/fxrank-lang-python/src/module_map.rs`:
```rust
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
        if self.keys.contains(&segs) { Some(segs) } else { None }
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
        if self.keys.contains(&target) { Some(target) } else { None }
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
    let dir_segs: Vec<&str> = if dir_part.is_empty() { Vec::new() } else { dir_part.split('/').collect() };
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
    let mut segs: Vec<String> = dir_segs[first_pkg..].iter().map(|s| s.to_string()).collect();
    if file_stem != "__init__" {
        segs.push(file_stem.to_string());
    }
    Some(segs)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-python module_map`
Expected: 4 tests PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-python && cargo clippy --workspace --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e-py add -A
git -C /dev/shm/fxrank/3e-py commit -m "feat(python): PyModuleMap — dotted module keys + absolute/relative resolution

Path-convention module map over the in-batch SourceFile set (no disk, no sys.path,
no libcst). module_of builds the dotted key via __init__.py package roots;
resolve_absolute matches in-batch dotted modules; resolve_relative walks the
package up `level` dots then appends the suffix. Out-of-batch (stdlib/third-party)
→ None. (025-3e §5.3)"
```

---

### Task 2: emit `canonical_path` in `build_record` (adopt the partition)

**Files:**
- Modify: `crates/fxrank-lang-python/src/functions.rs` (add an `is_module_level` flag to `FnUnit`)
- Modify: `crates/fxrank-lang-python/src/detect/mod.rs` (`build_record`)
- Modify: `crates/fxrank-lang-python/src/lib.rs` (`analyze` — build the map, thread it)

**Interfaces:**
- Consumes: `PyModuleMap`, `FnUnit.{symbol, is_module_level}` + the file `path`.
- Produces: `UnitRecord.canonical_path = module_of(path) ++ [symbol]` ONLY for a module-level `def`; empty otherwise.

**Add `FnUnit.is_module_level: bool` (the never-false-resolve guard):** Python `FnUnit.symbol` is the BARE name — a method `def write(self)` in a class has symbol `"write"`, NOT `Class.write` (unlike Rust `S::method` / TS `C.method`). So WITHOUT a flag, a method `write` would get canonical_path `[pkg,util,write]` and a `from pkg.util import write; write()` call could **false-resolve to the method**. Only a true module-level `def`/`async def` is importable as `pkg.util.<name>`. In `functions.rs::collect`, set `is_module_level = true` ONLY for a `def` directly at the module top level — `false` for methods (inside a `ClassDef`), nested defs (inside another `def`), lambdas, and the synthetic `<module>` unit. (The collect walker already tracks nesting depth/context; thread a `module_level` bool that is true only at the top frame.)

**`symbol_segments` for Python:** Python `FnUnit.symbol` is the bare name (`def foo` → `"foo"`; a method is just its name, NOT `Class.method`; plus synthetic `<lambda@…>`/`<module>`). A module-level function — the only cross-file resolution target — has a bare non-synthetic symbol. So `symbol_segments` returns `Some(vec![symbol])` for real names and **`None` for synthetic `<…>` symbols** (so the `<module>`/lambda units get an empty canonical_path and stay out of the index — I4). Do NOT split on `.` (Python symbols carry no meaningful dots). **Methods/nested defs are excluded from the index via `is_module_level` (above), not by symbol** — so a method `write` can never be a false-resolve target.

- [ ] **Step 1: Write the failing test** (append to `detect/mod.rs` tests; use the crate's existing per-unit test harness — check how the current `build_record` test builds a `FnUnit` + `Imports` + `SpanIndex`)

```rust
#[test]
fn build_record_sets_canonical_path() {
    use crate::module_map::PyModuleMap;
    use fxrank_core::frontend::SourceFile;
    // Module pkg.util with a top-level `write`.
    let mmap = PyModuleMap::build(&[
        SourceFile { path: "pkg/__init__.py".into(), text: String::new() },
        SourceFile { path: "pkg/util.py".into(), text: String::new() },
    ]);
    let src = "def write():\n    pass\n";
    // Build a FnUnit for `write` at path "pkg/util.py" using the existing harness
    // pattern in this test module (parse → collect → pick the `write` unit →
    // Imports::build → SpanIndex). Then:
    let rec = build_record(&unit, "pkg/util.py", &imports, &module_bindings, &span, &mmap);
    assert_eq!(rec.canonical_path, vec!["pkg".to_string(), "util".into(), "write".into()]);
}

#[test]
fn method_unit_gets_empty_canonical_path() {
    // A method `write` inside a class is NOT module-level → empty canonical_path
    // (so it can never be a false-resolve target for `from pkg.util import write`).
    let mmap = PyModuleMap::build(&[
        SourceFile { path: "pkg/__init__.py".into(), text: String::new() },
        SourceFile { path: "pkg/util.py".into(), text: String::new() },
    ]);
    let src = "class C:\n    def write(self):\n        pass\n";
    // Build the FnUnit for the method `write` (is_module_level == false) and a record:
    let rec = build_record(&method_unit, "pkg/util.py", &imports, &module_bindings, &span, &mmap);
    assert!(rec.canonical_path.is_empty(), "a method must not get an importable canonical_path");
}
```
(There is NO dedicated `build_record` unit-test helper today — `build_record` is exercised via `lib.rs::analyze`. Build the inline setup by mirroring the `scan_fixture_hotspots` harness in `detect/mod.rs` (it already constructs `Imports`/`module_bindings`/`SpanIndex`/`collect`); the only new thing is building a `PyModuleMap` and passing `&mmap` as the new last arg. Alternatively, assert through `lib.rs::analyze` on a 2-file batch and check `output.records[].canonical_path`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-lang-python build_record_sets_canonical 2>&1 | head`
Expected: compile error — `build_record` takes 5 args, not 6.

- [ ] **Step 3: Implement**

Add `module_map: &PyModuleMap` as the last param to `build_record`. Compute (I4: synthetic
`<module>`/`<lambda…>` units are NEVER resolution targets, so give them an empty canonical_path
rather than a junk `[..module, "<module>"]` that pollutes the index):
```rust
    // Only a module-level def is importable as `module.<name>`. Methods/nested
    // defs/lambdas/<module> get an empty canonical_path so they cannot be a
    // false-resolve target (Python symbols are bare — a method `write` would
    // otherwise collide with module-level `write`). (P2-1)
    let canonical_path = if !unit.is_module_level {
        vec![]
    } else {
        match (module_map.module_of(path), symbol_segments(&unit.symbol)) {
            (Some(mut m), Some(seg)) => { m.extend(seg); m }
            _ => vec![], // no module in scope, OR a synthetic symbol
        }
    };
```
Set `canonical_path` on the `UnitRecord`; keep `aliases: vec![]` (re-exports deferred). Add:
```rust
/// Path-meaningful segments for a Python symbol. Synthetic `<module>`/`<lambda@…>`
/// symbols are not importable → None. (Module-level-vs-method filtering is done by
/// `is_module_level` at the call site above; this only guards the synthetic forms.)
fn symbol_segments(symbol: &str) -> Option<Vec<String>> {
    if symbol.starts_with('<') {
        None
    } else {
        Some(vec![symbol.to_string()])
    }
}
```
In `lib.rs::analyze`, build `PyModuleMap::build(files)` ONCE before the per-file loop and pass `&module_map` to ALL `build_record` call sites — there are **two in `lib.rs::analyze`** (the per-unit loop AND the module-init path, ~line 191) PLUS any test harness. Missing one is a compile error (caught), but name all of them.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-python build_record` then `cargo test -p fxrank-lang-python`.
Expected: new test PASS, all existing PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-python && cargo clippy --workspace --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e-py add -A
git -C /dev/shm/fxrank/3e-py commit -m "feat(python): emit canonical_path in build_record (adopt Python partition)

canonical_path = dotted-module-of(file) ++ [symbol]. Threads PyModuleMap through
analyze. Flips the Python partition to adopted; Task 3 supplies resolved_target
so in-project calls still resolve. (025-3e §5.3)"
```

**NOTE (coupled pair):** after this task the partition is *adopted* but `resolved_target` is `None`, so qualified imported calls temporarily go opaque. Do NOT gate propagation/dogfood between Task 2 and Task 3 — gate at Task 4. `cargo test --workspace` still passes (no existing Python test feeds a multi-file in-batch import that must resolve).

---

### Task 3: retain relative dot-level in `Imports` + emit `resolved_target`

**Files:**
- Modify: `crates/fxrank-lang-python/src/imports.rs` (store the relative dot-level)
- Modify: `crates/fxrank-lang-python/src/detect/refs.rs` (`extract` + walker)
- Modify: `crates/fxrank-lang-python/src/detect/mod.rs` (`build_record` passes the owning module key + map into `refs::extract`)

**Why the Imports change:** `imports.rs` resolves a local name to a dotted path that **already includes the original imported name** (`from m import n as p` → `resolve("p") = "m.n"`), so renames resolve for free — Python needs NO export-name enhancement (unlike TS). BUT `relative_locals` is only a **boolean**; `from . import x` and `from .. import x` are indistinguishable, yet they resolve to different packages. Storing the **dot-level** (`from.relative.len()`) per relative local makes relative resolution precise and never-guess.

**Imports change:** replace `relative_locals: HashSet<String>` with `relative_levels: HashMap<String, usize>` (local → dot count). In the `ImportFrom` arm, `let level = from.relative.len();` and, when relative, `relative_levels.insert(local, level)`. Keep `is_relative(local) -> bool` (`relative_levels.contains_key(local)`) for existing callers; add `relative_level(local) -> Option<usize>`.

**Resolution rules (in `extract`, per call) — ONE unified rule that is safe for every shape (never-guess):**

Step A — **expand** `base` into a full dotted callee path, MINDING the two `Imports` encodings.
Let `R = imports.resolve(root)`. There are two cases (because `import a.b.c` maps `a`→`"a.b.c"`
— `R` is a *prefix* of the written `base` — whereas `from m import n` maps `n`→`"m.n"` — `R` is
NOT a prefix of `base`):
- **If `R` is a segment-boundary prefix of `base`** (the `import a.b.c` / `import os` form, where
  the code already spells the full module path) → `full = base` (use the written path as-is).
- **Else** (the `from m import n` form) → `full = R + base-after-root` (replace the root segment
  with its resolved dotted path, keep any trailing members).
Worked:
- `from m import n; n()` → base `"n"`, R=`"m.n"` (not a prefix of `"n"`) → full `"m.n"`.
- `import pkg.util; pkg.util.write()` → base `"pkg.util.write"`, R=`"pkg.util"` (IS a prefix) → full `"pkg.util.write"`.
- `from pkg import util; util.write()` → base `"util.write"`, R=`"pkg.util"` (not a prefix of `"util.write"`) → full `"pkg.util.write"`.
- `from m import n; n.method()` → base `"n.method"`, R=`"m.n"` (not a prefix) → full `"m.n.method"`.
- `import os; os.getcwd()` → full `"os.getcwd"`.

Step B — split the full path: `name = last segment`, `target_module = all-but-last`.

Step C — resolve `target_module` against the map; emit `[..key, name]` only on an in-batch hit:
- `imports.relative_level(root)` is `Some(level)` (a relative import) → `module_map.resolve_relative(referencing_module, referencing_is_package, level, target_module_joined)`.
- else → `module_map.resolve_absolute(target_module_joined)`.
- miss → `None` (stdlib / third-party / a value, not a module).

**Why this is safe for every shape (the never-guess proof):** the `name` is always the literal last segment of the *real* dotted call path, and an edge is emitted only if `target_module` is a genuine in-batch module. This auto-distinguishes the two syntactically-identical cases `from pkg import util; util.write()` (util is a submodule → `pkg.util` IS an in-batch module → resolves) vs `from pkg import C; C.method()` (C is a class → `pkg.C` is NOT an in-batch module → `None`). And `import m; m.sub.f()` → full `m.sub.f` → module `m.sub`, name `f` → resolves iff `m.sub` is in-batch (correct). No special-casing, no guessed names.

- `RefKind::Method` → `None` (member call on a non-imported receiver; `module` is None for these).
- `module = imports.resolve(root)` is `None` AND bare free call (no `.` in base) → same-module candidate `[..referencing_module, root]`.
- else → `None`.

(Implementer: `refs.rs` already computes `base`/`root`/`module`. Add the expand-split-resolve above; thread `referencing_module` + `referencing_is_package` (from `module_map.is_package(unit_path)`) + the map into `extract` from `build_record`.)

- [ ] **Step 1: Write the failing tests** (append to `refs.rs` tests; build a `PyModuleMap` + referencing module)

```rust
// Helper: parse `src` at `file`, build Imports + PyModuleMap over `files`, extract refs of `fn_name`
// with the referencing module derived from `file`. (Mirror the existing refs_for helper, adding the map.)

#[test]
fn absolute_in_batch_import_resolves() {
    // from pkg.util import write; write()  → ["pkg","util","write"]
    let src = "from pkg.util import write\ndef caller():\n    write()\n";
    let refs = refs_with_map(src, "pkg/app.py", "caller", &["pkg/__init__.py","pkg/app.py","pkg/util.py"]);
    let r = refs.iter().find(|r| r.base == "write").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["pkg".into(),"util".into(),"write".into()]));
}

#[test]
fn stdlib_import_stays_unresolved_for_opaque() {
    // from subprocess import run; run()  → None (not in batch → opaque, the false-resolve fix)
    let src = "from subprocess import run\ndef caller():\n    run(['ls'])\n";
    let refs = refs_with_map(src, "pkg/app.py", "caller", &["pkg/__init__.py","pkg/app.py"]);
    let r = refs.iter().find(|r| r.base == "run").unwrap();
    assert_eq!(r.resolved_target, None, "subprocess.run must be unresolved (→ opaque), never a local run");
}

#[test]
fn relative_import_resolves_with_level() {
    // in pkg.sub.mod: from .. import util ... actually call write from `from ..util import write`
    let src = "from ..util import write\ndef caller():\n    write()\n";
    let refs = refs_with_map(src, "pkg/sub/mod.py", "caller",
        &["pkg/__init__.py","pkg/sub/__init__.py","pkg/sub/mod.py","pkg/util.py"]);
    let r = refs.iter().find(|r| r.base == "write").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["pkg".into(),"util".into(),"write".into()]));
}

#[test]
fn dotted_module_member_call_resolves() {
    // import pkg.util; pkg.util.write()  → ["pkg","util","write"] (the unified expand-split rule)
    let src = "import pkg.util\ndef caller():\n    pkg.util.write()\n";
    let refs = refs_with_map(src, "pkg/app.py", "caller", &["pkg/__init__.py","pkg/app.py","pkg/util.py"]);
    let r = refs.iter().find(|r| r.base == "pkg.util.write").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["pkg".into(),"util".into(),"write".into()]));
}

#[test]
fn method_call_on_from_imported_value_is_unresolved() {
    // from pkg import Client; Client.get()  — Client is a CLASS (pkg.Client is NOT an in-batch
    // module), so the expand→ "pkg.Client.get" → module "pkg.Client" → resolve_absolute miss →
    // None. Must NOT resolve `get` to a coincidental module member (never-guess).
    let src = "from pkg import Client\ndef caller():\n    Client.get()\n";
    let refs = refs_with_map(src, "pkg/app.py", "caller", &["pkg/__init__.py","pkg/app.py"]);
    let r = refs.iter().find(|r| r.base.starts_with("Client")).unwrap();
    assert_eq!(r.resolved_target, None, "method call on a from-imported value must be opaque");
}

#[test]
fn same_module_bare_call_resolves_to_own_module() {
    let src = "def helper():\n    pass\ndef caller():\n    helper()\n";
    let refs = refs_with_map(src, "pkg/app.py", "caller", &["pkg/__init__.py","pkg/app.py"]);
    let r = refs.iter().find(|r| r.base == "helper").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["pkg".into(),"app".into(),"helper".into()]));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-lang-python absolute_in_batch 2>&1 | head`
Expected: compile error — `extract` / `Imports` signature mismatch.

- [ ] **Step 3: Implement** the `Imports` relative-level change, then `resolve_in_project` in `refs.rs` per the rules above, and thread the referencing module + map through `extract` and from `build_record`. Wire `build_record` to compute `referencing_module = module_map.module_of(path).unwrap_or_default()` and pass it (+ the map) to `refs::extract`. Keep `RefKind::Method` → `None`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-python` (new + existing green, incl. unchanged `is_relative` callers).
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-python && cargo clippy --workspace --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e-py add -A
git -C /dev/shm/fxrank/3e-py commit -m "feat(python): relative dot-level in Imports + resolved_target

Imports now retains the relative dot-level (from.relative.len()) per relative
local, so `from . import x` vs `from .. import x` resolve to the right package.
resolved_target uses the import table's dotted path (which already keeps the
original name, so renames resolve) split into module+name; absolute → resolve_
absolute, relative → resolve_relative(level), `import m; m.f()` → member name,
bare same-module → own module. Stdlib/third-party/unresolvable → None → opaque.
Completes the adoption pair with Task 2: mod.write no longer false-resolves to a
lone write. (025-3e §5.3)"
```

---

### Task 4: end-to-end adoption verification (false-resolve fixture + dogfood)

**Files:**
- Modify: `crates/fxrank-lang-python/src/lib.rs` (`#[cfg(test)]` e2e). No production change.

- [ ] **Step 1: Write the e2e test** — drive `PythonFrontend::...analyze` on a 1-file project with a local `def run` whose name **collides with the imported call** — `from subprocess import run` + `def run(): ...` + a `caller()` doing `run(...)`. (The same-named local is essential: without it the test can't catch an erroneous `Resolved`-to-local edge.) Build a `CanonicalIndex`; assert the call resolves to `Edge::Opaque` (the stdlib `subprocess.run`), NOT `Edge::Resolved` to the local `def run`. (`Edge` has no `Debug` → pre-bind `let is_opaque = matches!(...)`. Use the crate's existing frontend constructor.)

- [ ] **Step 2: Run to verify it PASSES** (`cargo test -p fxrank-lang-python false_resolve_killed`). If it Resolves, STOP + report BLOCKED.

- [ ] **Step 3: Workspace gate** — `cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`. All green.

- [ ] **Step 4: Dogfood** — scan a real Python package (the local `django` or `pytorch` from the dogfood set, OR a `/tmp` 3-file package). Confirm:
  - `[.hotspots[] | select((.inherited|length)>0)] | length` > 0 (absolute/relative in-package imports resolve),
  - `[.hotspots[] | select(.propagated_score < .own_score)] | length` == 0 (invariant),
  - sample `scope.external_reaches` — stdlib (`os`, `sys`, `subprocess`) and third-party stay opaque; in-package `from .x import y` resolve.
  Record the numbers + a one-line interpretation in the report. (django uses absolute intra-package imports heavily — good coverage signal.)

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank/3e-py add -A
git -C /dev/shm/fxrank/3e-py commit -m "test(python): e2e — subprocess.run no longer false-resolves to a local def

Drives the Python frontend through CanonicalIndex/resolve_ref_precise; asserts the
stdlib call is Opaque, not Resolved to a same-named local. The 025-3e false-resolve
fix for Python, proven end to end. (025-3e §8)"
```

---

## Self-Review

**Spec coverage (025-3e §5.3 — Python emitter):**
- Module identity = dotted path via `__init__.py` package roots → Task 1. ✓
- Absolute + relative (dot-level) import resolution → Task 1 (map) + Task 3 (Imports level + wiring). ✓
- canonical_path emission (adoption) → Task 2. ✓
- resolved_target (absolute / relative / `import m; m.f()` / same-module) → Task 3. ✓
- External (stdlib/third-party) → None → opaque (false-resolve fix) → Task 3 + Task 4 (e2e). ✓
- **Renames resolve for free** — the import table's dotted path retains the original name (no enhancement needed, unlike TS). ✓
- **Deferred (documented §9):** star imports (`imports.rs` already skips), dynamic `importlib`/`__import__`, PEP-420 namespace packages (no `__init__.py`), `__init__.py` re-exports (AliasFacts deferred). Each degrades to opaque, not wrong.
- **Methods/nested defs are EXCLUDED from the canonical index (fixed via `is_module_level`, P2-1):** Python `FnUnit.symbol` is the bare name (not `Class.method`), so without this a method `write` could false-resolve a `from pkg.util import write` call to the method. Task 2 sets `is_module_level` and only module-level `def`s get a canonical_path, so methods/nested defs/lambdas never enter the primary index — no false-resolve, and no collision dropping a real module-level function. (Two module-level `def`s of the same name in one module is a Python redefinition — last wins at runtime; we ambiguous-drop, safe.)
- **Marginal class-vs-submodule collision:** `from pkg import X; X.f()` resolves only if `pkg.X` is an in-batch module; if `X` is a class AND a same-named `pkg/X.py` module with `def f` both exist, it could mis-target — astronomically rare, accepted (resolution still requires a real in-batch function).

**Placeholder scan:** the Task 2/Task 3 test harnesses say "adapt to the existing per-unit/refs test helper" — the implementer must reuse the crate's existing `build_record`/`refs_for` test scaffolding (the exact FnUnit/Imports/SpanIndex construction); flagged, not a vague gap.

**Type consistency:** `PyModuleMap::{build, module_of, resolve_absolute, resolve_relative}` consistent Tasks 1,3. `build_record(..., &PyModuleMap)` consistent Tasks 2-4. `Imports::relative_level` added Task 3, used Task 3. `symbol_segments` Task 2.

**Coupling note (executor):** Tasks 2 and 3 are a pair — Task 2 alone adopts with resolved_target None → qualified imported calls temporarily opaque; do NOT gate between them, gate at Task 4. (Same as Plans 2/3.)

**Never-guess invariant:** every `Some(resolved_target)` name comes from the import table's dotted path (which retains the real name), the `import m; m.f()` member, or a same-module local — never guessed; out-of-batch → None.

## Execution Handoff

This is **Plan 4 of the 025-3e set** (Python emitter), the last frontend. After it: all three partitions (Rust, TS, Python) adopted → #36's false-resolve fix is complete across all frontends. (TS `tsconfig.json` `paths` aliases — Plan 5, the dogfood-proven high-value TS follow-up, reads tsconfig within the `fxrank-lang-ts` crate — remains queued.)
