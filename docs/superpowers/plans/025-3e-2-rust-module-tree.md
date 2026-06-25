# 025-3e Plan 2 — Rust module-tree emitter (adopt the Rust partition) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Rust frontend emit canonical fully-qualified paths (`UnitRecord.canonical_path`) and import-resolved call targets (`CallSiteRef.resolved_target`), flipping the Rust partition to *adopted* so `resolve_ref_precise` runs path-precise resolution — fixing the false-resolve (`std::fs::write` no longer resolves to a lone `Foo::write`) while keeping real in-crate cross-file calls resolving.

**Architecture:** Build a `ModuleTree` once over the whole `SourceFile` batch from **filesystem-path convention** (crate root = `lib.rs`/`main.rs`/`src/bin/*`; `foo.rs`→module `foo`, `foo/mod.rs`→`foo`, `foo/bar.rs`→`foo::bar`) — pure string work on in-batch paths, no AST mod-walking, no disk I/O, no cargo. Thread it through collection (inline-`mod` nesting) and ref extraction (path expansion). canonical_path + resolved_target ship together (adoption couples them: once adopted, every qualified ref resolves canonically or goes opaque).

**Tech Stack:** Rust, `syn` (already a dep of `fxrank-lang-rust`). No new dependencies. `fxrank-core` is untouched (it already consumes the fields, Plan 1).

## Global Constraints

- **No `cargo` invocation, no disk reads, no network.** All module-path math is over the in-batch `SourceFile.path` strings only. Verbatim from spec 025-3e §5.1.
- **Own-body output byte-identical** to pre-3e (`effects`/`risks`/`own_score`/`symbol`/`unit_id` unchanged). 3e only *adds* canonical_path/resolved_target and changes `propagated_*` as resolution sharpens. (spec 025-3e §2)
- **Crate root = `lib.rs`/`main.rs`/`src/bin/*.rs`** in the batch; its items are at module path `crate` with NO root-module segment (`crate::Bar`, not `crate::main::Bar`). (spec 025-3e §5.1; the root module is anonymous.)
- **mod-rs vs non-mod-rs directory ownership:** `mod.rs`/`lib.rs`/`main.rs` own their own directory; `foo.rs` owns sibling dir `foo/`. (spec 025-3e §5.1)
- **Documented misses (accepted, degrade to empty canonical_path):** `#[path]` attributes, inline-`#[path]`, macro-generated mods, files with no crate root in scope, `pub use` re-exports (no AliasFacts in this plan — deferred). A unit with empty canonical_path does not enter the primary index; its refs follow the §4.3 unqualified/qualified rules.
- CI gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- TDD: failing test first, minimal code, green, commit. Frequent commits.

---

## File Structure

- `crates/fxrank-lang-rust/src/module_tree.rs` — **create**: `ModuleTree` (file path → crate-relative module segments), built from `&[SourceFile]` by path convention. Pure, no AST.
- `crates/fxrank-lang-rust/src/lib.rs` — **modify**: declare `pub mod module_tree`; in `analyze`, build the `ModuleTree` once and thread it into collection + record building.
- `crates/fxrank-lang-rust/src/functions.rs` — **modify**: thread an inline-`mod` nesting accumulator through `collect_items`; add `mod_path: Vec<String>` to `FnUnit`.
- `crates/fxrank-lang-rust/src/detect/mod.rs` — **modify** `build_record`: accept the `ModuleTree` + the unit's module context; compute `canonical_path` = file-module ++ inline-nesting ++ symbol-segments; set it on the `UnitRecord`.
- `crates/fxrank-lang-rust/src/detect/refs.rs` — **modify** `extract`: accept the referencing unit's full module path + the `ModuleTree`; expand each call path to `resolved_target` (in-crate paths resolve; external → `None`).

---

### Task 1: `ModuleTree` — file path → crate-relative module segments

**Files:**
- Create: `crates/fxrank-lang-rust/src/module_tree.rs`
- Modify: `crates/fxrank-lang-rust/src/lib.rs` (add `pub mod module_tree;`)

**Interfaces:**
- Consumes: `fxrank_core::frontend::SourceFile` (`{ path: String, text: String }`).
- Produces:
  - `pub struct ModuleTree` with `pub fn build(files: &[SourceFile]) -> Self`
  - `pub fn module_of(&self, file_path: &str) -> Option<Vec<String>>` — the crate-relative module segments for a file (e.g. `src/util/config.rs` → `["util","config"]`; a crate root → `[]`). `None` if the file has no discoverable crate root in the batch.

**Algorithm (path-convention, no AST):**
1. Identify crate roots: any in-batch path whose file name is `lib.rs` or `main.rs`, or that matches `**/src/bin/*.rs` / `**/src/bin/*/main.rs`. The crate's **source root directory** is the root file's parent dir (for `lib.rs`/`main.rs`: their dir; conventionally `<crate>/src`).
2. For a non-root file `F` under a source root `R`: take the path of `F` relative to `R`, drop the `.rs`, split on `/`. Apply: a trailing `mod` segment (from `…/mod.rs`) contributes NO segment (the directory already named it); a `foo.rs` contributes `foo`; intermediate directories contribute their names. So `R/util/config.rs` → `["util","config"]`; `R/util/mod.rs` → `["util"]`; `R/lib.rs` (the root itself) → `[]`.
3. A file under no source root → not in any crate → `module_of` returns `None`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/fxrank-lang-rust/src/module_tree.rs  (new file, tests at bottom)
#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::frontend::SourceFile;

    fn sf(path: &str) -> SourceFile {
        SourceFile { path: path.into(), text: String::new() }
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
        assert_eq!(mt.module_of("crates/foo/src/util.rs"), Some(vec!["util".into()]));
        assert_eq!(mt.module_of("crates/foo/src/util/config.rs"), Some(vec!["util".into(), "config".into()]));
        assert_eq!(mt.module_of("crates/foo/src/net/mod.rs"), Some(vec!["net".into()]));
        assert_eq!(mt.module_of("crates/foo/src/net/http.rs"), Some(vec!["net".into(), "http".into()]));
    }

    #[test]
    fn binary_root_and_bin_dir() {
        let files = vec![sf("app/src/main.rs"), sf("app/src/cli.rs"), sf("app/src/bin/tool.rs")];
        let mt = ModuleTree::build(&files);
        assert_eq!(mt.module_of("app/src/main.rs"), Some(vec![]));
        assert_eq!(mt.module_of("app/src/cli.rs"), Some(vec!["cli".into()]));
        assert_eq!(mt.module_of("app/src/bin/tool.rs"), Some(vec![]), "a bin file is its own crate root");
    }

    #[test]
    fn no_crate_root_in_scope_returns_none() {
        // A subdirectory scan with no lib.rs/main.rs in the batch.
        let files = vec![sf("crates/foo/src/util/config.rs"), sf("crates/foo/src/util.rs")];
        let mt = ModuleTree::build(&files);
        assert_eq!(mt.module_of("crates/foo/src/util/config.rs"), None);
        assert_eq!(mt.module_of("crates/foo/src/util.rs"), None);
    }

    #[test]
    fn separate_workspace_crates_are_independent() {
        let files = vec![
            sf("crates/a/src/lib.rs"), sf("crates/a/src/x.rs"),
            sf("crates/b/src/lib.rs"), sf("crates/b/src/x.rs"),
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
        assert_eq!(mt.module_of("app/src/main.rs"), Some(vec![]), "src/main.rs IS the root");
        assert_eq!(mt.module_of("app/src/cli/main.rs"), Some(vec!["cli".into(), "main".into()]));
        assert_eq!(mt.module_of("app/src/cli/parse.rs"), Some(vec!["cli".into(), "parse".into()]),
            "sibling must be crate::cli::parse, not crate::parse");
    }

    #[test]
    fn multifile_binary_main_is_its_own_root() {
        // src/bin/<name>/main.rs is a multi-file binary crate root (module []).
        let files = vec![sf("app/src/lib.rs"), sf("app/src/bin/tool/main.rs"), sf("app/src/bin/tool/helper.rs")];
        let mt = ModuleTree::build(&files);
        assert_eq!(mt.module_of("app/src/bin/tool/main.rs"), Some(vec![]), "bin/<name>/main.rs is its own root");
        assert_eq!(mt.module_of("app/src/bin/tool/helper.rs"), Some(vec!["helper".into()]));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p fxrank-lang-rust module_tree 2>&1 | head -20`
Expected: compile error — `module_tree` module / `ModuleTree` not found.

- [ ] **Step 3: Declare the module and implement `ModuleTree`**

In `crates/fxrank-lang-rust/src/lib.rs`, add near the other `pub mod` lines:
```rust
pub mod module_tree;
```

Create `crates/fxrank-lang-rust/src/module_tree.rs`:
```rust
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
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-rust module_tree`
Expected: 6 tests PASS.

- [ ] **Step 5: fmt + clippy + commit**

Run: `cargo fmt -p fxrank-lang-rust && cargo clippy -p fxrank-lang-rust --all-targets -- -D warnings`
```bash
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(rust): ModuleTree — file path → crate-relative module segments

Path-convention reconstruction over the in-batch SourceFile set (no AST, no
disk, no cargo). Crate roots = lib.rs/main.rs/src/bin/*; mod.rs owns its dir,
foo.rs owns foo/. #[path] is a documented miss. (025-3e §5.1)"
```

---

### Task 2: thread inline-`mod` nesting into `FnUnit`

**Files:**
- Modify: `crates/fxrank-lang-rust/src/functions.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `FnUnit.mod_path: Vec<String>` — the inline-`mod` nesting of the unit WITHIN its file (e.g. a fn inside `mod a { mod b { .. } }` has `["a","b"]`; a top-level fn has `[]`). Impl/trait method symbols are unaffected (the type/trait stays in `symbol`).

- [ ] **Step 1: Write the failing test** (append to `functions.rs` tests)

```rust
#[test]
fn inline_module_nesting_recorded_in_mod_path() {
    let src = r#"
        fn top() {}
        mod a {
            fn mid() {}
            mod b {
                fn deep() {}
            }
        }
    "#;
    let file = syn::parse_file(src).unwrap();
    let units = collect(&file, "x.rs");
    let by = |name: &str| units.iter().find(|u| u.symbol == name).unwrap();
    assert_eq!(by("top").mod_path, Vec::<String>::new());
    assert_eq!(by("mid").mod_path, vec!["a".to_string()]);
    assert_eq!(by("deep").mod_path, vec!["a".to_string(), "b".to_string()]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-lang-rust inline_module_nesting 2>&1 | head`
Expected: compile error — `FnUnit` has no field `mod_path`.

- [ ] **Step 3: Add `mod_path` and thread it through `collect_items`**

In `FnUnit`, add the field:
```rust
    /// Inline-`mod` nesting within the file (`["a","b"]` for `mod a { mod b { fn } }`).
    /// Empty for a top-level item. Combined with the file's module path to form
    /// the canonical path. (025-3e)
    pub mod_path: Vec<String>,
```

Change `collect` / `collect_items` / `collect_from_impl` / `collect_from_trait` to thread a `mod_path: &[String]` slice. `collect` starts with `&[]`; the `Item::Mod` inline arm pushes the module ident before recursing:

```rust
pub fn collect(file: &syn::File, path: &str) -> Vec<FnUnit> {
    let mut units = Vec::new();
    collect_items(&file.items, path, false, &[], &mut units);
    units
}

fn collect_items(items: &[Item], path: &str, in_cfg_test: bool, mod_path: &[String], out: &mut Vec<FnUnit>) {
    for item in items {
        match item {
            Item::Fn(f) => {
                let symbol = f.sig.ident.to_string();
                let start = f.sig.ident.span().start();
                let line = start.line;
                let col = start.column + 1;
                let is_test = in_cfg_test || has_test_attr(&f.attrs);
                out.push(FnUnit {
                    id: format!("{path}:{line}:{col}:{symbol}"),
                    symbol,
                    path: path.to_string(),
                    line,
                    col,
                    sig: f.sig.clone(),
                    block: *f.block.clone(),
                    is_test,
                    mod_path: mod_path.to_vec(),
                });
            }
            Item::Impl(impl_block) => collect_from_impl(impl_block, path, in_cfg_test, mod_path, out),
            Item::Trait(trait_item) => collect_from_trait(trait_item, path, in_cfg_test, mod_path, out),
            Item::Mod(m) => {
                if let Some((_, nested_items)) = &m.content {
                    let nested_in_cfg_test = in_cfg_test || is_cfg_test(&m.attrs);
                    let mut child = mod_path.to_vec();
                    child.push(m.ident.to_string());
                    collect_items(nested_items, path, nested_in_cfg_test, &child, out);
                }
            }
            _ => {}
        }
    }
}
```

Add the `mod_path: &[String]` parameter to `collect_from_impl` and `collect_from_trait` and set `mod_path: mod_path.to_vec()` on the `FnUnit`s they push (the type/trait name stays in `symbol`, unchanged). Update the existing `#[cfg(test)]` helpers / any other `FnUnit { … }` literal in this file to add `mod_path: vec![]`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-rust` (the new test + all existing frontend tests green).
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-rust && cargo clippy -p fxrank-lang-rust --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(rust): record inline-mod nesting on FnUnit.mod_path

Thread a module-path accumulator through collect_items so a unit inside
mod a { mod b { .. } } carries [\"a\",\"b\"]. Top-level items get []. Feeds the
canonical path. (025-3e §5.1)"
```

---

### Task 3: emit `canonical_path` in `build_record` (adopt the partition)

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/mod.rs` (`build_record`)
- Modify: `crates/fxrank-lang-rust/src/lib.rs` (`analyze` — build the tree, pass it in)

**Interfaces:**
- Consumes: `ModuleTree`, `FnUnit.{path, mod_path, symbol}`.
- Produces: `UnitRecord.canonical_path` populated for units whose file has a module path; empty otherwise.

**canonical_path formula:** `module_tree.module_of(unit.path)` (None → empty canonical_path, degrade) `++ unit.mod_path ++ symbol_segments(unit.symbol)`, where `symbol_segments` splits the display symbol into path-meaningful parts: `"free_fn"` → `["free_fn"]`; `"S::method"` → `["S","method"]`; `"<S as T>::method"` → `["S","method"]` (the inherent-type form; the trait qualifier is dropped for path identity). Prefix everything with `"crate"` so a free fn `f` in `lib.rs` is `["crate","f"]` and `helpers::write` (file `helpers.rs`) is `["crate","helpers","write"]`.

- [ ] **Step 1: Write the failing test** (append to `detect/mod.rs` tests; use a small `ModuleTree`)

```rust
#[test]
fn build_record_sets_canonical_path_from_module_tree() {
    use crate::module_tree::ModuleTree;
    use fxrank_core::frontend::SourceFile;

    let files = vec![SourceFile { path: "src/helpers.rs".into(), text: String::new() }];
    let mt = ModuleTree::build(&[
        SourceFile { path: "src/lib.rs".into(), text: String::new() },
        files[0].clone(),
    ]);
    let src = "fn write() {}";
    let file = syn::parse_file(src).unwrap();
    let unit = &functions::collect(&file, "src/helpers.rs")[0];
    let imports = ImportTable::from_file(&file);
    let statics = std::collections::HashSet::new();
    let rec = build_record(unit, &imports, &statics, &mt);
    assert_eq!(rec.canonical_path, vec!["crate".to_string(), "helpers".into(), "write".into()]);
}

#[test]
fn build_record_empty_canonical_when_no_root() {
    use crate::module_tree::ModuleTree;
    use fxrank_core::frontend::SourceFile;
    // No lib.rs/main.rs in the batch → no module path → empty canonical_path.
    let mt = ModuleTree::build(&[SourceFile { path: "src/helpers.rs".into(), text: String::new() }]);
    let file = syn::parse_file("fn write() {}").unwrap();
    let unit = &functions::collect(&file, "src/helpers.rs")[0];
    let imports = ImportTable::from_file(&file);
    let statics = std::collections::HashSet::new();
    let rec = build_record(unit, &imports, &statics, &mt);
    assert!(rec.canonical_path.is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-lang-rust build_record_sets_canonical 2>&1 | head`
Expected: compile error — `build_record` takes 3 args, not 4.

- [ ] **Step 3: Implement**

In `detect/mod.rs`, change `build_record`'s signature to accept the tree and compute the path:
```rust
pub fn build_record(
    unit: &FnUnit,
    imports: &ImportTable,
    statics: &HashSet<String>,
    module_tree: &crate::module_tree::ModuleTree,
) -> fxrank_core::record::UnitRecord {
    let effects = gather(unit, imports, statics);
    let risks = risk::detect_fn_risks(&unit.block, &unit.sig, &unit.path);
    let call_refs = refs::extract(&unit.block, imports);
    let await_count = count_awaits(&unit.block);
    let async_boundary = unit.sig.asyncness.is_some() || await_count > 0;
    let canonical_path = canonical_path_of(unit, module_tree);

    fxrank_core::record::UnitRecord {
        unit_id: unit.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,
        canonical_path,
        aliases: vec![], // pub-use AliasFacts deferred (025-3e §9)
        effects,
        risks,
        refs: call_refs,
        async_boundary,
        await_count,
        language: fxrank_core::frontend::Language::Rust,
    }
}

/// canonical_path = ["crate"] ++ file-module ++ inline-mod nesting ++ symbol segments.
/// Returns empty when the file has no module path (no crate root in scope).
fn canonical_path_of(unit: &FnUnit, module_tree: &crate::module_tree::ModuleTree) -> Vec<String> {
    let Some(file_mod) = module_tree.module_of(&unit.path) else {
        return vec![];
    };
    let mut segs = vec!["crate".to_string()];
    segs.extend(file_mod);
    segs.extend(unit.mod_path.iter().cloned());
    segs.extend(symbol_segments(&unit.symbol));
    segs
}

/// Split a display symbol into path-meaningful segments:
/// `"f"` → ["f"]; `"S::method"` → ["S","method"];
/// `"<S as T>::method"` → ["S","method"] (trait qualifier dropped for identity).
///
/// Relies on `functions.rs` already normalizing generics out of the type name
/// (`last_segment_ident` returns the bare ident), so the LHS is `S`, `<S as T>`,
/// never `S<u32>`. The `<`/`>`/` as ` peeling below is robust to those forms.
fn symbol_segments(symbol: &str) -> Vec<String> {
    if let Some((lhs, method)) = symbol.rsplit_once("::") {
        // lhs is "S" (inherent) or "<S as T>" (trait impl). Strip the angle
        // brackets and the trait qualifier, leaving the self-type ident.
        let ty = lhs
            .trim_start_matches('<')
            .trim_end_matches('>')
            .split(" as ")
            .next()
            .unwrap_or(lhs)
            .trim();
        vec![ty.to_string(), method.to_string()]
    } else {
        vec![symbol.to_string()]
    }
}

#[cfg(test)]
mod symbol_segments_tests {
    use super::symbol_segments;

    #[test]
    fn segments_free_inherent_and_trait_impl() {
        assert_eq!(symbol_segments("free_fn"), vec!["free_fn".to_string()]);
        assert_eq!(symbol_segments("S::method"), vec!["S".to_string(), "method".into()]);
        // The trait-impl form: angle brackets + trait qualifier both stripped.
        assert_eq!(symbol_segments("<S as T>::method"), vec!["S".to_string(), "method".into()]);
    }
}
```

In `lib.rs::analyze`, build the tree once before the per-file loop and pass it to every `build_record` call:
```rust
fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
    let mut output = FrontendOutput::default();
    let module_tree = module_tree::ModuleTree::build(files);
    for source in files {
        // … existing parse + imports + statics + collect …
        // every `detect::build_record(unit, &imports, &statics)` becomes:
        //   detect::build_record(unit, &imports, &statics, &module_tree)
    }
    output
}
```
(Update BOTH `build_record` call sites in the `include_tests` true/false branches.)

- [ ] **Step 4: Run to verify pass + adoption smoke**

Run: `cargo test -p fxrank-lang-rust build_record` then `cargo test -p fxrank-lang-rust`.
Expected: new tests PASS, all existing PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-rust && cargo clippy --workspace --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(rust): emit canonical_path in build_record (adopt Rust partition)

canonical_path = crate ++ file-module (ModuleTree) ++ inline-mod ++ symbol segs.
Empty when no crate root in scope (degrade). This flips the Rust partition to
adopted; Task 4 supplies resolved_target so in-crate calls still resolve. (025-3e §5.1)"
```

**NOTE for the reviewer/controller:** after this task the Rust partition is *adopted* but `resolved_target` is still `None` on every ref, so under `resolve_ref_precise` every QUALIFIED in-crate call now goes opaque — propagation temporarily regresses until Task 4. This is an intermediate state; do NOT dogfood-gate between Task 3 and Task 4. Tasks 3 and 4 are a coupled pair (see plan Architecture).

---

### Task 4: emit `resolved_target` for in-crate call paths

**Files:**
- Modify: `crates/fxrank-lang-rust/src/detect/refs.rs` (`extract` + `RefsWalker`)
- Modify: `crates/fxrank-lang-rust/src/detect/mod.rs` (`build_record` passes the referencing module path into `refs::extract`)

**Interfaces:**
- Consumes: the referencing unit's full module path (`["crate","helpers"]` for a fn in `helpers.rs`), `ImportTable`, `ModuleTree`.
- Produces: `CallSiteRef.resolved_target: Some(Vec<String>)` for calls that resolve to an in-crate canonical path; `None` for external/unresolvable (→ qualified opaque, the false-resolve fix).

**Expansion rules (best-effort, in-crate only):**
- Written path starts with `crate::` → `["crate", …rest]`.
- `self::x` → referencing-module ++ `["x"]`.
- A LEADING RUN of `super` → pop one module per `super` (`super::super::x` pops two), then ++ rest. If popping escapes the crate root (result no longer starts with `"crate"`), or the referencing module is empty (root-less file), → `None`.
- Bare name resolved via `ImportTable` to a path starting `crate::`/`self::`/`super::` → expand as above.
- Anything else (`std::…`, other-crate `serde::…`, an unresolved bare name) → `None`.

- [ ] **Step 1: Write the failing tests** (append to `refs.rs` tests)

```rust
fn refs_with_ctx(src: &str, referencing_mod: &[&str]) -> Vec<CallSiteRef> {
    let file = syn::parse_file(src).unwrap();
    let imports = ImportTable::from_file(&file);
    // Find the FIRST fn item — the fixture may start with a `use` (so items[0]
    // is not a fn). Mirrors the existing `refs_of` helper pattern in this file.
    let block = file
        .items
        .iter()
        .find_map(|it| match it {
            syn::Item::Fn(f) => Some((*f.block).clone()),
            _ => None,
        })
        .expect("fixture must contain a fn item");
    let rmod: Vec<String> = referencing_mod.iter().map(|s| s.to_string()).collect();
    extract(&block, &imports, &rmod)
}

#[test]
fn resolves_crate_self_super_paths() {
    let src = r#"
        fn caller() {
            crate::helpers::write();
            self::local();
            super::sibling();
            std::fs::write();
        }
    "#;
    // referencing module = crate::net (so super:: → crate)
    let refs = refs_with_ctx(src, &["crate", "net"]);
    let t = |b: &str| refs.iter().find(|r| r.base == b).unwrap().resolved_target.clone();
    assert_eq!(t("crate::helpers::write"), Some(vec!["crate".into(), "helpers".into(), "write".into()]));
    assert_eq!(t("self::local"), Some(vec!["crate".into(), "net".into(), "local".into()]));
    assert_eq!(t("super::sibling"), Some(vec!["crate".into(), "sibling".into()]));
    // std:: is external → None (→ qualified miss → opaque; the false-resolve fix)
    assert_eq!(t("std::fs::write"), None);
}

#[test]
fn bare_import_resolved_to_crate_path() {
    let src = r#"
        use crate::helpers::write;
        fn caller() { write(); }
    "#;
    let refs = refs_with_ctx(src, &["crate"]);
    let w = refs.iter().find(|r| r.base == "write").unwrap();
    assert_eq!(w.resolved_target, Some(vec!["crate".into(), "helpers".into(), "write".into()]));
}

#[test]
fn super_super_walks_up_two_modules() {
    let src = r#"fn caller() { super::super::sibling(); }"#;
    // referencing module crate::a::b → super::super → crate
    let refs = refs_with_ctx(src, &["crate", "a", "b"]);
    let t = refs.iter().find(|r| r.base == "super::super::sibling").unwrap();
    assert_eq!(t.resolved_target, Some(vec!["crate".into(), "sibling".into()]));
}

#[test]
fn relative_paths_unanchorable_in_rootless_file_are_none() {
    // Root-less file → empty referencing module → self/super cannot anchor → None
    // (must NOT fabricate a ["crate"] target that could false-resolve).
    let src = r#"fn caller() { self::local(); super::up(); }"#;
    let refs = refs_with_ctx(src, &[]); // empty referencing module
    assert_eq!(refs.iter().find(|r| r.base == "self::local").unwrap().resolved_target, None);
    assert_eq!(refs.iter().find(|r| r.base == "super::up").unwrap().resolved_target, None);
}

#[test]
fn super_past_crate_root_is_none() {
    // super from a unit directly at the crate root pops "crate" → escapes → None.
    let src = r#"fn caller() { super::x(); }"#;
    let refs = refs_with_ctx(src, &["crate"]);
    assert_eq!(refs.iter().find(|r| r.base == "super::x").unwrap().resolved_target, None);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-lang-rust resolves_crate_self_super 2>&1 | head`
Expected: compile error — `extract` takes 2 args, not 3.

- [ ] **Step 3: Implement**

Change `extract` to accept the referencing module and thread it into the walker; compute `resolved_target` in `visit_expr_call`:
```rust
pub fn extract(block: &syn::Block, imports: &ImportTable, referencing_mod: &[String]) -> Vec<CallSiteRef> {
    let mut walker = RefsWalker { imports, referencing_mod, refs: Vec::new() };
    walker.visit_block(block);
    walker.refs
}

struct RefsWalker<'a> {
    imports: &'a ImportTable,
    referencing_mod: &'a [String],
    refs: Vec<CallSiteRef>,
}
```
In `visit_expr_call`, after computing `base`/`module`/`qualified`/`first_party`, add:
```rust
            let resolved_target = resolve_in_crate(&base, self.imports, self.referencing_mod);
```
and set it on the pushed `CallSiteRef`. The method-call arm keeps `resolved_target: None`. Add the helper:
```rust
/// Expand a written call path to an in-crate canonical segment vector, or `None`
/// for external / unresolvable targets. (025-3e §5.1)
///
/// `referencing_mod` is the module the calling unit lives in (e.g.
/// `["crate","net"]`), already anchored at `"crate"`. An EMPTY `referencing_mod`
/// means the calling file has no crate root in scope — then `self`/`super` cannot
/// be anchored and MUST return `None` (anchoring at a fabricated `["crate"]` could
/// false-resolve in an adopted mixed batch — the exact bug 3e kills).
fn resolve_in_crate(base: &str, imports: &ImportTable, referencing_mod: &[String]) -> Option<Vec<String>> {
    // Resolve a bare leading name through the import table first.
    let head = base.split("::").next().unwrap_or(base);
    let effective: String = match imports.resolve(head) {
        Some(full) => {
            // replace the head with its imported full path
            let rest = base.strip_prefix(head).unwrap_or("");
            format!("{full}{rest}")
        }
        None => base.to_string(),
    };
    let segs: Vec<String> = effective.split("::").map(|s| s.to_string()).collect();
    match segs.first().map(String::as_str) {
        Some("crate") => Some(segs),
        Some("self") | Some("super") => {
            if referencing_mod.is_empty() {
                return None; // cannot anchor a relative path with no module context
            }
            let mut module = referencing_mod.to_vec();
            // Consume the LEADING run of self/super, walking up for each super.
            let mut rest = segs.into_iter().peekable();
            while let Some(seg) = rest.peek() {
                match seg.as_str() {
                    "self" => {
                        rest.next();
                    }
                    "super" => {
                        rest.next();
                        module.pop(); // up one module
                    }
                    _ => break,
                }
            }
            // After walking up, the module must still be anchored at `crate`;
            // otherwise the path escaped the crate root → not in-crate → None.
            if module.first().map(String::as_str) != Some("crate") {
                return None;
            }
            module.extend(rest);
            Some(module)
        }
        _ => None, // std::, other crates, unresolved bare names → external/opaque
    }
}
```
In `detect/mod.rs::build_record`, compute the referencing module and pass it:
```rust
    let referencing_mod = module_of_unit(&canonical_path, &unit.symbol);
    let call_refs = refs::extract(&unit.block, imports, &referencing_mod);
```
(`canonical_path` is the value already computed earlier in `build_record` via
`canonical_path_of`; reuse it rather than recomputing. **Executor note:** Task 3's
`build_record` computes `let call_refs = refs::extract(...)` BEFORE `let canonical_path`;
when you add the `referencing_mod` argument here, MOVE the `let canonical_path` line
above the `call_refs` line so `canonical_path` is in scope — otherwise use-before-def
compile error.) Add a helper in
`detect/mod.rs` to strip the symbol segments from a unit's canonical path,
leaving its module:
```rust
/// The module a unit lives in = its canonical_path minus the symbol segments.
/// Returns an EMPTY vec for an empty canonical_path (a root-less file): callers
/// pass it to `resolve_in_crate`, which returns `None` for self/super when the
/// module is empty — so we never fabricate a `["crate"]` anchor that could
/// false-resolve. (025-3e §6)
fn module_of_unit(canonical: &[String], symbol: &str) -> Vec<String> {
    if canonical.is_empty() {
        return vec![];
    }
    let drop = symbol_segments(symbol).len();
    canonical[..canonical.len().saturating_sub(drop)].to_vec()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-rust resolves_crate_self_super && cargo test -p fxrank-lang-rust bare_import && cargo test -p fxrank-lang-rust`
Expected: new tests PASS, all existing PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-rust && cargo clippy --workspace --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "feat(rust): resolve in-crate call paths to resolved_target

Expand crate::/self::/super:: and import-resolved bare names to canonical
segment vectors; external (std::, other crates, unresolved) → None → qualified
opaque. Completes the adoption pair with Task 3: in-crate calls resolve, the
std::fs::write→Foo::write false-resolve is gone. (025-3e §5.1)"
```

---

### Task 5: end-to-end adoption verification (the false-resolve fixture + dogfood)

**Files:**
- Create: `crates/fxrank-lang-rust/tests/fixtures/false_resolve.rs` (a fixture exercising the bug) — OR an inline integration test in `crates/fxrank-lang-rust/src/lib.rs` tests using `analyze` directly.
- Modify: none (verification only).

**Interfaces:**
- Consumes: `RustFrontend::analyze`, `fxrank_core` resolve/fold (the full pipeline) — easiest via the CLI on a temp fixture, or assert on `FrontendOutput.records[*].{canonical_path, refs[*].resolved_target}` directly.

- [ ] **Step 1: Write the end-to-end false-resolve test** (in `lib.rs` `#[cfg(test)]`)

```rust
#[test]
fn false_resolve_killed_std_write_not_resolved_to_local_write() {
    use fxrank_core::frontend::SourceFile;
    use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
    use fxrank_core::graph::Edge;

    // Crate with a lone `Foo::write` and a caller that calls std::fs::write.
    let files = vec![
        SourceFile { path: "src/lib.rs".into(), text:
            "pub struct Foo;\n\
             impl Foo { pub fn write(&self) {} }\n\
             pub fn caller() { std::fs::write(\"a\", b\"b\").unwrap(); }".into() },
    ];
    let out = RustFrontend::default().analyze(&files);
    let idx = CanonicalIndex::from_records(&out.records);
    assert!(idx.adopted(), "Rust partition must be adopted (canonical_path set)");

    // Find the caller's std::fs::write ref and resolve it.
    let caller = out.records.iter().find(|r| r.symbol == "caller").unwrap();
    let std_ref = caller.refs.iter().find(|r| r.base.contains("fs") && r.base.ends_with("write")).unwrap();
    let edge = resolve_ref_precise(std_ref, &idx, &caller.path);
    // MUST be opaque (external), NOT Resolved to Foo::write.
    assert!(matches!(edge, Some(Edge::Opaque(_))), "std::fs::write must be opaque, not resolved to Foo::write; got {edge:?}");
}
```

- [ ] **Step 2: Run to verify it PASSES** (this is the headline outcome)

Run: `cargo test -p fxrank-lang-rust false_resolve_killed`
Expected: PASS. (If it fails with `Resolved`, the adoption/resolution is wrong — STOP.)

- [ ] **Step 3: Workspace gate**

Run: `cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all green.

- [ ] **Step 4: Dogfood — the Rust partition is now adopted**

Run:
```bash
cargo run -q -p fxrank -- scan crates/fxrank-core/src/ | jq '[.hotspots[] | select(.propagated_score>0)] | length'
```
Expected: a number > 0 (propagation still works). Compare the top hotspots to a pre-3e `--no-resolve`-vs-resolve sanity: `run_scan`/`walk_dir` (the real IO boundaries) should still be the top hotspots. Capture a short before/after note of any `propagated_score` shifts and confirm each is explained by a now-correctly-opaque external call or a now-correctly-resolved in-crate call (NOT a lost in-crate edge). Record this in the task report.

- [ ] **Step 5: Commit the verification**

```bash
git -C /dev/shm/fxrank/3e add -A
git -C /dev/shm/fxrank/3e commit -m "test(rust): e2e — std::fs::write no longer false-resolves to a local write

Drives RustFrontend::analyze through CanonicalIndex/resolve_ref_precise on a
crate with a lone Foo::write + a std::fs::write caller; asserts the call is
Opaque, not Resolved. The 025-3e false-resolve fix, proven end to end. (025-3e §8)"
```

---

## Self-Review

**Spec coverage (025-3e §5.1 — Rust emitter):**
- Crate root (lib.rs/main.rs/src/bin), anonymous root module → Task 1. ✓
- mod-rs/non-mod-rs directory ownership (foo.rs→foo, foo/mod.rs→foo, foo/bar.rs→foo::bar) → Task 1. ✓
- No-root-in-scope degrade → Task 1 (`module_of` None) + Task 3 (empty canonical_path). ✓
- Inline `mod` nesting → Task 2. ✓
- canonical_path emission (adoption) → Task 3. ✓
- `use`/`crate`/`self`/`super` path expansion to resolved_target → Task 4. ✓
- External (std::, other crates) → None → opaque (the false-resolve fix) → Task 4 + Task 5 (proven e2e). ✓
- **Deferred (documented, §9):** `#[path]` / inline-`#[path]` (empty canonical_path), `pub use` AliasFacts (re-exported calls under-resolve to opaque — safe), full edition-2015 path semantics. Each is a degrade-to-safe miss, not a wrong result.

**Placeholder scan:** no TBD/TODO; every code step shows complete code; the only "documented misses" are explicit degrade-to-empty behaviors with tests (Task 1 `no_crate_root_in_scope_returns_none`).

**Type consistency:** `ModuleTree::build(&[SourceFile])` / `module_of(&str) -> Option<Vec<String>>` consistent across Tasks 1,3,4. `build_record(unit, imports, statics, &ModuleTree)` consistent Tasks 3-5. `extract(block, imports, &[String])` consistent Tasks 4-5. `canonical_path_of` / `symbol_segments` / `module_of_unit` defined in Task 3-4 and reused. `FnUnit.mod_path: Vec<String>` defined Task 2, consumed Task 3.

**Coupling note (carried for the executor):** Tasks 3 and 4 are a pair — Task 3 alone makes the partition adopted while resolved_target is still None, so do NOT run a propagation/dogfood gate between them. Gate at Task 5.

## Execution Handoff

This is **Plan 2 of the 025-3e set** (Rust emitter). After it: the Rust partition is adopted, the false-resolve is fixed and proven e2e, TS/Python remain non-adopted (025 behavior, unchanged). Plans 3 (TS) and 4 (Python) follow with the same shape.
