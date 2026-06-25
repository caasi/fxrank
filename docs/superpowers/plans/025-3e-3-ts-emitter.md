# 025-3e Plan 3 — TS/JS module-map emitter (adopt the TS partition) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the TS/JS frontend emit `UnitRecord.canonical_path` + `CallSiteRef.resolved_target`, flipping the TS partition to *adopted* so `resolve_ref_precise` does path-precise resolution — fixing the false-resolve (`fs.readFile` no longer resolves to a lone in-project `readFile`) while keeping real in-project relative-import and same-module calls resolving.

**Architecture:** TS modules ARE files. Build a `TsModuleMap` once over the whole `SourceFile` batch that (a) normalizes each in-batch file path to a canonical **module key** (strip extension; `foo/index.ts` → `foo`), and (b) resolves a **relative** import specifier (`./util`, `../x`) from an importing file against the in-batch module-key set via the **extension/index ladder** — pure string work on in-batch paths, no disk I/O, no tsconfig. A unit's `canonical_path = [module_key, ...symbol_segments]`; a call's `resolved_target = [resolved_module_key, imported_name]` for relative-import or same-module calls, else `None` (→ qualified opaque). canonical_path + resolved_target ship together (adoption couples them). The TS frontend already sets `qualified`/`first_party` correctly (`ImportTable` + `is_first_party_specifier`); this plan only adds the two new fields.

**Tech Stack:** Rust, `swc` (already a dep of `fxrank-lang-ts`). No new dependencies. `fxrank-core` is untouched (it consumes the fields, Plan 1). The Rust frontend is untouched (adopted in Plan 2).

## Global Constraints

- **No disk I/O, no tsconfig parse, no network.** All module-key math is over the in-batch `SourceFile.path` strings only. (spec 025-3e §5.2)
- **Own-body output byte-identical** to pre-3e (`effects`/`risks`/`own_score`/`symbol`/`unit_id` unchanged); 3e only *adds* canonical_path/resolved_target and changes `propagated_*`. (§2)
- **Module key = in-batch file path, extension stripped, `index.{ts,tsx,js,jsx,mts,cts,...}` collapsed to its directory.** A unit in `src/util.ts` → module key `src/util`; in `src/foo/index.ts` → `src/foo`.
- **A namespace import is an alias for the module's export set, NOT an object/unit.** `import * as util from './util'; util.fetchUser()` resolves **directly to the `fetchUser` export** (`[src/util, fetchUser]`) — a namespace binding carries no own-body effects to score, and fxrank propagates *function* effects, so descending straight to the export (the SCIP/Kythe model) is correct; a synthetic object node would add an effect-less indirection for zero benefit. Indirect aliasing (`const u = util; u.f()`) is dynamic → under-resolves to opaque.
- **Documented misses (accepted, degrade to empty/opaque, §9):** `tsconfig.json` path aliases (`@/`, `~/`) — config not in batch → opaque; bare-package & `node:` imports → opaque (correct, external); **default-import member calls** (`import c from './x'; c.get()`) → opaque (`.get` is a value method, not a module export); `export … from` barrels (AliasFacts deferred). NOTE: `import { a as b }` renames now **resolve correctly** (to the original export `a`) via the enhanced `ImportTable` — no longer a miss.
- CI gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- TDD: failing test first, minimal code, green, commit. Frequent commits.

## File Structure

- `crates/fxrank-lang-ts/src/module_map.rs` — **create**: `TsModuleMap` (module-key normalization + relative-import resolution). Pure, no swc.
- `crates/fxrank-lang-ts/src/lib.rs` — **modify**: declare `pub mod module_map`; in `analyze`, build the `TsModuleMap` once and thread it into `analyze_units` / `record_from_hotspot` / `module_init_unit` record building.
- `crates/fxrank-lang-ts/src/detect/mod.rs` — **modify** `record_from_hotspot` (+ the module-init record path): accept the `TsModuleMap`; compute `canonical_path`; pass the owning module key into `refs::extract`.
- `crates/fxrank-lang-ts/src/detect/refs.rs` — **modify** `extract`: accept the referencing module key + the `TsModuleMap`; compute `resolved_target` per call (relative-import / same-module → resolved key; external → `None`).

---

### Task 1: `TsModuleMap` — module-key normalization + relative-import resolution

**Files:**
- Create: `crates/fxrank-lang-ts/src/module_map.rs`
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (add `pub mod module_map;`)

**Interfaces:**
- Consumes: `fxrank_core::frontend::SourceFile` (`{ path, text }`).
- Produces:
  - `pub struct TsModuleMap` with `pub fn build(files: &[SourceFile]) -> Self`
  - `pub fn module_of(&self, file_path: &str) -> String` — the module key for an in-batch file (always succeeds; pure path normalization).
  - `pub fn resolve_import(&self, importer_file: &str, specifier: &str) -> Option<String>` — resolve a **relative** specifier to an in-batch module key, or `None` (non-relative, or not in batch).

**Algorithm (pure path strings):**
- `module_of(path)`: strip a known TS/JS extension (`.tsx .ts .jsx .js .mts .cts .mjs .cjs`); if the remaining stem ends with `/index`, drop the `/index` segment. (`src/util.ts`→`src/util`; `src/foo/index.ts`→`src/foo`.)
- `resolve_import(importer, spec)`: only `spec` starting with `./` or `../` is relative (else `None` — bare/`node:`/alias are out of scope). Join `dirname(importer)` + `spec`, normalize `.`/`..` segments → a candidate path prefix. Then try the **ladder** against the in-batch module-key SET: the candidate itself, and candidate + `/index`. Return the matching in-batch module key, else `None`.

- [ ] **Step 1: Write the failing tests** (in `module_map.rs`, tests at bottom)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::frontend::SourceFile;

    fn sf(p: &str) -> SourceFile { SourceFile { path: p.into(), text: String::new() } }

    #[test]
    fn module_key_strips_ext_and_index() {
        let files = vec![sf("src/util.ts"), sf("src/foo/index.tsx"), sf("src/a/b.js")];
        let m = TsModuleMap::build(&files);
        assert_eq!(m.module_of("src/util.ts"), "src/util");
        assert_eq!(m.module_of("src/foo/index.tsx"), "src/foo");
        assert_eq!(m.module_of("src/a/b.js"), "src/a/b");
    }

    #[test]
    fn resolve_relative_with_extension_and_index_ladder() {
        let files = vec![
            sf("src/app.ts"), sf("src/util.ts"),
            sf("src/comp/index.ts"), sf("src/comp/inner.ts"),
        ];
        let m = TsModuleMap::build(&files);
        // ./util from src/app.ts → src/util
        assert_eq!(m.resolve_import("src/app.ts", "./util"), Some("src/util".into()));
        // ./comp from src/app.ts → src/comp (index ladder)
        assert_eq!(m.resolve_import("src/app.ts", "./comp"), Some("src/comp".into()));
        // ../util from src/comp/inner.ts → src/util
        assert_eq!(m.resolve_import("src/comp/inner.ts", "../util"), Some("src/util".into()));
    }

    #[test]
    fn resolve_relative_with_explicit_extension() {
        // ESM/NodeNext code often writes the extension; it must still resolve to
        // the extensionless in-batch key.
        let files = vec![sf("src/app.ts"), sf("src/util.ts")];
        let m = TsModuleMap::build(&files);
        assert_eq!(m.resolve_import("src/app.ts", "./util.js"), Some("src/util".into()));
        assert_eq!(m.resolve_import("src/app.ts", "./util.ts"), Some("src/util".into()));
    }

    #[test]
    fn non_relative_and_out_of_batch_are_none() {
        let files = vec![sf("src/app.ts"), sf("src/util.ts")];
        let m = TsModuleMap::build(&files);
        assert_eq!(m.resolve_import("src/app.ts", "node:fs"), None);   // node builtin
        assert_eq!(m.resolve_import("src/app.ts", "react"), None);      // bare package
        assert_eq!(m.resolve_import("src/app.ts", "@/util"), None);     // alias (no tsconfig)
        assert_eq!(m.resolve_import("src/app.ts", "./missing"), None);  // not in batch
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p fxrank-lang-ts module_map 2>&1 | head -20`
Expected: compile error — `module_map` / `TsModuleMap` not found.

- [ ] **Step 3: Declare the module and implement `TsModuleMap`**

In `crates/fxrank-lang-ts/src/lib.rs`, add near the other `pub mod` lines:
```rust
pub mod module_map;
```

Create `crates/fxrank-lang-ts/src/module_map.rs`:
```rust
//! TS/JS module map: normalize in-batch file paths to module keys and resolve
//! relative import specifiers against the in-batch set, by path convention
//! (spec 025-3e §5.2). No disk, no tsconfig, no swc.

use std::collections::HashSet;

use fxrank_core::frontend::SourceFile;

// Longest/more-specific suffixes FIRST: strip_suffix is tried in order, so
// `.mts`/`.cts`/`.mjs`/`.cjs` must precede `.ts`/`.js` (else `a.mts` → `a.m`).
const TS_EXTS: &[&str] = &[
    ".tsx", ".mts", ".cts", ".ts", ".jsx", ".mjs", ".cjs", ".js",
];

pub struct TsModuleMap {
    keys: HashSet<String>,
}

impl TsModuleMap {
    pub fn build(files: &[SourceFile]) -> Self {
        let keys = files.iter().map(|f| module_key(&f.path)).collect();
        Self { keys }
    }

    pub fn module_of(&self, file_path: &str) -> String {
        module_key(file_path)
    }

    /// Resolve a RELATIVE specifier (`./x`, `../x`) to an in-batch module key.
    /// Non-relative specifiers (bare packages, `node:*`, aliases) → `None`.
    ///
    /// The joined candidate is run through `module_key` (the SAME normalization
    /// used to build the key set) so an explicit-extension specifier
    /// (`./util.js`), a no-extension one (`./util`), an index dir (`./comp` whose
    /// file is `comp/index.ts`), and an explicit `./comp/index` all collapse to
    /// the same key and match. (This subsumes the extension/index "ladder" into
    /// one normalize-then-lookup — no separate `/index` probe needed.)
    pub fn resolve_import(&self, importer_file: &str, specifier: &str) -> Option<String> {
        if !(specifier.starts_with("./") || specifier.starts_with("../")) {
            return None;
        }
        let dir = parent_dir(importer_file);
        let candidate = module_key(&normalize_join(dir, specifier));
        if self.keys.contains(&candidate) {
            Some(candidate)
        } else {
            None
        }
    }
}

/// Normalize a file path to a module key: strip a known TS/JS extension, then
/// drop a trailing `/index` segment.
fn module_key(path: &str) -> String {
    let mut stem = path;
    for ext in TS_EXTS {
        if let Some(s) = path.strip_suffix(ext) {
            stem = s;
            break;
        }
    }
    stem.strip_suffix("/index").unwrap_or(stem).to_string()
}

/// Parent directory of a path (no trailing slash). `"src/app.ts"` → `"src"`.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Join a base dir with a relative specifier and normalize `.`/`..` segments.
/// `("src/comp", "../util")` → `"src/util"`.
fn normalize_join(base: &str, spec: &str) -> String {
    let mut segs: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            other => segs.push(other),
        }
    }
    segs.join("/")
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-ts module_map`
Expected: 4 tests PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-ts && cargo clippy -p fxrank-lang-ts --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e-ts add -A
git -C /dev/shm/fxrank/3e-ts commit -m "feat(ts): TsModuleMap — module-key normalization + relative-import resolution

Path-convention module map over the in-batch SourceFile set (no disk, no tsconfig,
no swc). module_of strips ext + collapses index; resolve_import walks ./ ../
relative specifiers via the extension/index ladder against in-batch keys. Bare /
node: / alias specifiers → None. (025-3e §5.2)"
```

---

### Task 2: emit `canonical_path` in `record_from_hotspot` (adopt the partition)

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs` (`record_from_hotspot` + the module-init record path)
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`analyze` — build the map, thread it)

**Interfaces:**
- Consumes: `TsModuleMap`, `FnUnit.{path, symbol}`.
- Produces: `UnitRecord.canonical_path = [module_of(unit.path), ...symbol_segments(unit.symbol)]`.

**`symbol_segments` for TS:** split the display symbol on `.`: `"fetchUser"`→`["fetchUser"]`; `"C.method"`→`["C","method"]`; `"C.constructor"`→`["C","constructor"]`; `"<arrow@L..C..>"`→`["<arrow@L..C..>"]` (anonymous — never a cross-file resolution target, harmless). Getters `"C.get g"` → `["C","get g"]` (kept verbatim; methods are `RefKind::Method` and never resolution targets anyway). **Accepted miss:** a quoted string-key method (`{ "a.b"() {} }`) has symbol `"a.b"` → splits to `["a","b"]` (a wrong canonical), but such methods are `RefKind::Method` and never resolution targets, and no ref ever produces `resolved_target=[mod,"a.b"]`, so it cannot mis-resolve — harmless.

- [ ] **Step 1: Write the failing test** (append to `detect/mod.rs` tests)

```rust
#[test]
fn record_from_hotspot_sets_canonical_path() {
    use crate::module_map::TsModuleMap;
    use fxrank_core::frontend::SourceFile;
    // The existing `unit_and_ctx` helper parses at path "t.ts" → module key "t".
    let mmap = TsModuleMap::build(&[SourceFile { path: "t.ts".into(), text: String::new() }]);
    let src = "export function fetchUser() {}";
    let (units, imports, module_bindings, lines, idx) = unit_and_ctx(src, "fetchUser");
    let unit = &units[idx];
    let h = analyze_unit(unit, &imports, &lines, &module_bindings);
    let rec = record_from_hotspot(unit, &h, &imports, &lines, &[], &mmap);
    assert_eq!(rec.canonical_path, vec!["t".to_string(), "fetchUser".into()]);
}
```
(This mirrors the existing `record_from_hotspot` test in `detect/mod.rs` exactly — `unit_and_ctx` → `analyze_unit(unit, &imports, &lines, &module_bindings)` → `record_from_hotspot(..., &mmap)` — only adding the new `&mmap` argument. Reuse the in-scope bare helpers; do NOT prefix with `detect::`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-lang-ts record_from_hotspot_sets_canonical 2>&1 | head`
Expected: compile error — `record_from_hotspot` takes 5 args, not 6.

- [ ] **Step 3: Implement**

Add `module_map: &TsModuleMap` as the last param to `record_from_hotspot`. Compute and set `canonical_path`:
```rust
pub fn record_from_hotspot(
    unit: &FnUnit,
    h: &Hotspot,
    imports: &ImportTable,
    lines: &SpanLines,
    extra_refs: &[fxrank_core::record::CallSiteRef],
    module_map: &crate::module_map::TsModuleMap,
) -> fxrank_core::record::UnitRecord {
    let module_key = module_map.module_of(&unit.path);
    let mut canonical_path = vec![module_key];
    canonical_path.extend(symbol_segments(&unit.symbol));
    let mut refs = refs::extract(&unit.body, imports, lines); // Task 3 adds the module/map args
    refs.extend_from_slice(extra_refs);
    fxrank_core::record::UnitRecord {
        unit_id: h.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,
        canonical_path,
        aliases: vec![], // barrel AliasFacts deferred (§9)
        effects: h.effects.clone(),
        risks: h.risk_features.clone(),
        refs,
        async_boundary: h.async_boundary,
        await_count: h.await_count,
        language: fxrank_core::frontend::Language::Ts,
    }
}

/// Split a TS display symbol into path segments (`C.method` → ["C","method"]).
fn symbol_segments(symbol: &str) -> Vec<String> {
    symbol.split('.').map(|s| s.to_string()).collect()
}
```
In `lib.rs::analyze`, build the map once before the per-file loop and pass `&module_map` to every `record_from_hotspot` call (inside `analyze_units` and the module-init path). `analyze_units` must gain a `module_map: &TsModuleMap` parameter threaded to its `record_from_hotspot` calls; the `module_init_unit` record build also passes it. Update the `analyze_src` test harness to build + pass a `TsModuleMap`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-ts record_from_hotspot_sets_canonical` then `cargo test -p fxrank-lang-ts`.
Expected: new test PASS, all existing PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-ts && cargo clippy --workspace --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e-ts add -A
git -C /dev/shm/fxrank/3e-ts commit -m "feat(ts): emit canonical_path in record_from_hotspot (adopt TS partition)

canonical_path = [module-key, ...symbol segments]. Threads TsModuleMap through
analyze/analyze_units. This flips the TS partition to adopted; Task 3 supplies
resolved_target so in-project calls still resolve. (025-3e §5.2)"
```

**NOTE (coupled pair):** after this task the TS partition is *adopted* but `resolved_target` is still `None`, so qualified relative-import/imported calls temporarily go opaque. Do NOT dogfood-gate between Task 2 and Task 3 — gate at Task 4. `cargo test --workspace` still passes (no existing TS test feeds a multi-file in-batch relative-import that must resolve).

---

### Task 3: emit `resolved_target` for relative-import + same-module calls

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/refs.rs` (`extract` + walker)
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs` (`record_from_hotspot` passes the owning module key + map into `refs::extract`)

**Interfaces:**
- Consumes: the referencing file's module key, `ImportTable` (enhanced here to retain the import KIND + original export name), `TsModuleMap`.
- Produces: `CallSiteRef.resolved_target: Some([resolved_module_key, export_name])` for **provably-safe** in-project calls; `None` for external/unresolvable/ambiguous (→ qualified opaque).

**Why ImportTable must be enhanced (the false-resolve guard):** today `ImportTable` stores only `local → specifier`, discarding the original export name and the import kind. Guessing the export name from the call site (`root` or the member) would **false-resolve** coincidental names — e.g. `import { readFile as rf } from './util'; rf()` would emit `["src/util","rf"]`, and if some unrelated `rf` export existed it would wrongly resolve. That violates #36's never-false-resolve guarantee. So `ImportTable` is extended to carry, per local binding, an `ImportTarget`:
- `Named(export_name)` — `import { x }` (export = `x`) or `import { x as y }` (export = `x`, the ORIGINAL; swc's `ImportNamedSpecifier.imported` carries it). Resolving renames CORRECTLY (to the original export) is now possible.
- `Namespace` — `import * as ns`.
- `Default` — `import x` / `import x, {…}` default binding.

**Resolution rules (in `extract`, per `CallExpr`) — only the safe shapes resolve:**
- `RefKind::Method` (member call on a non-imported receiver) → `None`.
- Bare free call `local()` (no `.`), `module = imports.resolve(local) = Some(spec)`, `resolve_import(file, spec) = Some(key)`:
  - `ImportTarget::Named(export)` → `Some([key, export])` (handles `import {x}` and `import {x as y}` correctly).
  - `Namespace` / `Default` called as a function → `None` (the export name is unknown/ambiguous).
- Member free call `local.member()` (base has a `.`, `module = imports.resolve(local) = Some(spec)`, resolvable):
  - `ImportTarget::Namespace` → `Some([key, member])` (the member IS the module export).
  - `Named` / `Default` → `None` (it's a method call on an imported value, not a module export).
- Bare free call `local()` with `module = None` (not imported) → same-module candidate `Some([referencing_module_key, local])`.
- Everything else (non-relative spec that didn't resolve, etc.) → `None`.

Pass the **referencing FILE path** (not just the key) so `resolve_import` can compute relative joins; derive the key once for the same-module case.

- [ ] **Step 1: Write the failing tests** (append to `refs.rs` tests)

```rust
fn refs_with_map(src: &str, file: &str, fn_name: &str, files: &[&str]) -> Vec<CallSiteRef> {
    use crate::module_map::TsModuleMap;
    let sfs: Vec<fxrank_core::frontend::SourceFile> = files.iter()
        .map(|p| fxrank_core::frontend::SourceFile { path: (*p).into(), text: String::new() }).collect();
    let mmap = TsModuleMap::build(&sfs);
    let (module, cm) = crate::functions::parse_module(src, file, crate::source::Lang::Ts).unwrap();
    let lines = crate::source::SpanLines::new(cm); // SpanLines::new takes the Lrc<SourceMap> by value, 1 arg
    let imports = crate::imports::ImportTable::from_module(&module);
    let units = crate::functions::collect(&module, file, &lines);
    let unit = units.iter().find(|u| u.symbol == fn_name).unwrap();
    extract(&unit.body, &imports, &lines, file, &mmap)
}

#[test]
fn relative_import_call_resolves() {
    let src = "import { fetchUser } from './util';\nexport function caller() { fetchUser(); }";
    let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
    let r = refs.iter().find(|r| r.base == "fetchUser").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["src/util".into(), "fetchUser".into()]));
}

#[test]
fn node_import_call_stays_unresolved_for_opaque() {
    // fs.readFile from node:fs must NOT resolve to a local readFile → None → opaque.
    let src = "import fs from 'node:fs';\nexport function caller() { fs.readFile('x', cb); }";
    let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
    let r = refs.iter().find(|r| r.base.starts_with("fs")).unwrap();
    assert_eq!(r.resolved_target, None, "node:fs call must be unresolved (→ opaque), never a local readFile");
}

#[test]
fn same_module_bare_call_resolves_to_own_module() {
    let src = "function helper() {}\nexport function caller() { helper(); }";
    let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts"]);
    let r = refs.iter().find(|r| r.base == "helper").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["src/app".into(), "helper".into()]));
}

#[test]
fn namespace_import_member_resolves_to_member_name() {
    // import * as util from './util'; util.fetchUser() → ["src/util","fetchUser"]
    // (the member, NOT the namespace binding "util").
    let src = "import * as util from './util';\nexport function caller() { util.fetchUser(); }";
    let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
    let r = refs.iter().find(|r| r.base.starts_with("util")).unwrap();
    assert_eq!(r.resolved_target, Some(vec!["src/util".into(), "fetchUser".into()]));
}

#[test]
fn renamed_import_resolves_to_original_export_name() {
    // import { readFile as rf } from './util'; rf() → ["src/util","readFile"]
    // (the ORIGINAL export, not the local alias `rf`) — no false-resolve.
    let src = "import { readFile as rf } from './util';\nexport function caller() { rf(); }";
    let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
    let r = refs.iter().find(|r| r.base == "rf").unwrap();
    assert_eq!(r.resolved_target, Some(vec!["src/util".into(), "readFile".into()]));
}

#[test]
fn default_import_member_call_is_unresolved() {
    // import client from './util'; client.get() — `get` is a method on the default
    // export VALUE, NOT a module export → must NOT resolve to a coincidental `get`.
    let src = "import client from './util';\nexport function caller() { client.get(); }";
    let refs = refs_with_map(src, "src/app.ts", "caller", &["src/app.ts", "src/util.ts"]);
    let r = refs.iter().find(|r| r.base.starts_with("client")).unwrap();
    assert_eq!(r.resolved_target, None, "default-import member call must be unresolved (→ opaque)");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p fxrank-lang-ts relative_import_call_resolves 2>&1 | head`
Expected: compile error — `extract` takes 3 args, not 5.

- [ ] **Step 3a: Enhance `ImportTable` to retain export name + import kind**

In `crates/fxrank-lang-ts/src/imports.rs`, add the target enum and change the map value (keep `resolve` returning the specifier so existing callers are unaffected):
```rust
/// What a local import binding refers to in the source module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    /// `import { x }` (export = "x") or `import { x as y }` (export = "x", the original).
    Named(String),
    /// `import * as ns`.
    Namespace,
    /// `import x` / default binding.
    Default,
}

struct ImportEntry {
    specifier: String,
    target: ImportTarget,
}

pub struct ImportTable {
    map: std::collections::HashMap<String, ImportEntry>,
    has_dynamic: bool,
}
```
In `from_module`, populate `target` per specifier kind (the `Named` arm reads `s.imported` for the original export name):
```rust
                    for spec in &decl.specifiers {
                        use swc_ecma_ast::{ImportSpecifier::*, ModuleExportName};
                        let (local, target) = match spec {
                            Named(s) => {
                                let local = s.local.sym.to_string();
                                // `imported` is Some for `{ orig as local }`; None means export == local.
                                let export = match &s.imported {
                                    Some(ModuleExportName::Ident(i)) => i.sym.to_string(),
                                    Some(ModuleExportName::Str(st)) => st.value.to_atom_lossy().to_string(),
                                    None => local.clone(),
                                };
                                (local, ImportTarget::Named(export))
                            }
                            Default(s) => (s.local.sym.to_string(), ImportTarget::Default),
                            Namespace(s) => (s.local.sym.to_string(), ImportTarget::Namespace),
                        };
                        table.map.insert(local, ImportEntry { specifier: src.clone(), target });
                    }
```
Update `resolve` and `scan_var_decl` (require) to the new value type, and add an accessor:
```rust
    pub fn resolve(&self, local: &str) -> Option<&str> {
        self.map.get(local).map(|e| e.specifier.as_str())
    }
    /// The import kind + original export name for a local binding (None if not imported).
    pub fn import_target(&self, local: &str) -> Option<&ImportTarget> {
        self.map.get(local).map(|e| &e.target)
    }
```
(For `const x = require('m')`, `scan_var_decl` inserts an `ImportEntry { specifier: m, target: ImportTarget::Default }` — require is a default-style binding; member calls on it won't resolve, which is correct.) Existing `imports.rs` tests use `resolve` only and keep passing.

- [ ] **Step 3b: compute `resolved_target` in `refs::extract` using the safe shapes**

Change `extract` to accept the referencing file path + the map, thread into the walker:
```rust
pub fn extract(
    body: &FnBodyOwned,
    imports: &ImportTable,
    lines: &SpanLines,
    referencing_file: &str,
    module_map: &crate::module_map::TsModuleMap,
) -> Vec<CallSiteRef> {
    let referencing_key = module_map.module_of(referencing_file);
    let mut w = RefsWalker { imports, lines, referencing_file, referencing_key, module_map, refs: Vec::new() };
    body.visit_with(&mut w);
    w.refs
}
```
Add the fields to `RefsWalker`. In the call arm, after computing `root`/`module`/`qualified`/`kind`:
```rust
            use crate::imports::ImportTarget;
            let has_member = base.contains('.');
            let resolved_target = if matches!(kind, RefKind::Method) {
                None
            } else if let Some(spec) = &module {
                // Resolve the relative module, then pick the export name ONLY for
                // provably-safe shapes (never guess → never false-resolve).
                match self.module_map.resolve_import(self.referencing_file, spec) {
                    Some(key) => match (self.imports.import_target(root), has_member) {
                        // bare `local()` from a named import → the original export name
                        (Some(ImportTarget::Named(export)), false) => Some(vec![key, export.clone()]),
                        // `ns.member()` from a namespace import → the member is the export
                        (Some(ImportTarget::Namespace), true) => {
                            let member = base.split('.').nth(1).unwrap_or(root);
                            Some(vec![key, member.to_string()])
                        }
                        // default()/default.member()/namespace() → ambiguous → opaque
                        _ => None,
                    },
                    None => None, // non-relative / out-of-batch spec → opaque
                }
            } else if !has_member {
                // bare same-module free call → own module
                Some(vec![self.referencing_key.clone(), root.to_string()])
            } else {
                None
            };
```
and set it on the pushed `CallSiteRef`. In `detect/mod.rs::record_from_hotspot`, change the `refs::extract` call to pass `&unit.path` and `module_map`. The absorbed `extra_refs` (React two-pass) are already extracted via `refs::extract` inside `analyze_units` — update THAT call too to pass the arrow's file path (same file as the component) + the map, so absorbed refs also carry `resolved_target`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p fxrank-lang-ts relative_import_call_resolves && cargo test -p fxrank-lang-ts node_import && cargo test -p fxrank-lang-ts same_module_bare && cargo test -p fxrank-lang-ts`
Expected: new tests PASS, all existing PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt -p fxrank-lang-ts && cargo clippy --workspace --all-targets -- -D warnings
git -C /dev/shm/fxrank/3e-ts add -A
git -C /dev/shm/fxrank/3e-ts commit -m "feat(ts): resolve relative-import + same-module calls to resolved_target

Expand import-resolved relative specifiers (via TsModuleMap) and bare same-module
free calls to [module-key, name]; node:/package/alias and method calls → None →
opaque. Completes the adoption pair with Task 2: the fs.readFile→local-readFile
false-resolve is gone. Enhanced ImportTable retains export name + kind, so renames
resolve correctly and ambiguous shapes (default-member, namespace-as-fn) → opaque,
never false-resolved. tsconfig aliases stay opaque (§9). (025-3e §5.2)"
```

---

### Task 4: end-to-end adoption verification (false-resolve fixture + dogfood)

**Files:**
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`#[cfg(test)]` e2e test). No production change.

- [ ] **Step 1: Write the e2e test**

```rust
#[test]
fn false_resolve_killed_node_fs_not_resolved_to_local_readfile() {
    use fxrank_core::frontend::SourceFile;
    use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
    use fxrank_core::graph::Edge;
    // A project with a lone local `readFile` + a caller using node:fs's fs.readFile.
    let files = vec![
        SourceFile { path: "src/app.ts".into(), text:
            "import fs from 'node:fs';\n\
             export function readFile() { return 1; }\n\
             export function caller() { fs.readFile('x', () => {}); }".into() },
    ];
    let out = TsFrontend::default().analyze(&files);
    let idx = CanonicalIndex::from_records(&out.records);
    assert!(idx.adopted(), "TS partition must be adopted");
    let caller = out.records.iter().find(|r| r.symbol == "caller").unwrap();
    let fs_ref = caller.refs.iter().find(|r| r.base.starts_with("fs")).unwrap();
    let edge = resolve_ref_precise(fs_ref, &idx, &caller.path);
    // `Edge` has no `Debug` derive, so pre-bind the boolean (no `{edge:?}`).
    let is_opaque = matches!(edge, Some(Edge::Opaque(_)));
    assert!(is_opaque, "node:fs fs.readFile must be opaque, not resolved to a local readFile");
}
```
(Use the same `TsFrontend` constructor the other `lib.rs` tests use — check the existing `analyze_src`/integration tests for `TsFrontend::default()` vs a `Lang`-parameterized form, and match it.)

- [ ] **Step 2: Run to verify it PASSES**

Run: `cargo test -p fxrank-lang-ts false_resolve_killed`
Expected: PASS.

- [ ] **Step 3: Workspace gate**

Run: `cargo test --workspace && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all green.

- [ ] **Step 4: Dogfood — TS partition adopted**

The committed `crates/fxrank-lang-ts/tests/fixtures/react/*.tsx` fixtures are each self-contained (no cross-file relative imports between them), so they may yield 0 inherited edges — do NOT gate on them. Instead use a **real multi-file TS project** from the dogfood set (e.g. the local `omni` repo) OR write a tiny two-file corpus to `/tmp` with a known relative import:
```bash
mkdir -p /tmp/ts-dogfood/src
printf "export function fetchUser(){ return fetch('/u'); }\n" > /tmp/ts-dogfood/src/util.ts
printf "import { fetchUser } from './util';\nexport function load(){ return fetchUser(); }\n" > /tmp/ts-dogfood/src/app.ts
cargo run -q -p fxrank -- scan /tmp/ts-dogfood | jq '{inherited: ([.hotspots[]|select((.inherited|length)>0)]|length), violations: ([.hotspots[]|select(.propagated_score < .own_score)]|length)}'
```
Confirm `inherited > 0` (the `./util` relative import resolved and `load` inherited `fetchUser`'s `fetch` effect) and `violations == 0`. Also run a larger real TS repo if available and record a short before/after note: relative imports resolve, `node:`/package imports correctly external. Put it in the report.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank/3e-ts add -A
git -C /dev/shm/fxrank/3e-ts commit -m "test(ts): e2e — node:fs fs.readFile no longer false-resolves to local readFile

Drives TsFrontend::analyze through CanonicalIndex/resolve_ref_precise; asserts the
node:fs call is Opaque, not Resolved to a same-project readFile. The 025-3e
false-resolve fix for TS, proven end to end. (025-3e §8)"
```

---

## Self-Review

**Spec coverage (025-3e §5.2 — TS emitter):**
- Module identity = in-batch file specifier (ext-strip + index-collapse) → Task 1. ✓
- Relative import resolution via extension/index ladder → Task 1. ✓
- canonical_path emission (adoption) → Task 2. ✓
- resolved_target for relative-import + same-module → Task 3. ✓
- External (`node:`/package) → None → opaque (false-resolve fix) → Task 3 + Task 4 (proven e2e). ✓
- **Deferred (documented §9):** tsconfig path aliases (`@/`,`~/`) — config not in batch → opaque; default-import member calls (`c.get()`) → opaque (value method, not a module export); `export … from` barrels → AliasFacts deferred. Each degrades to opaque, not wrong. (Renames `{ a as b }` now RESOLVE via the enhanced ImportTable — the false-resolve risk Codex flagged is closed by resolving to the original export, not guessing.)

**Placeholder scan:** no TBD/TODO; every code step shows complete code; the test harness adaptations (`analyze_unit`/hotspot construction, `TsFrontend::default`) are flagged where the implementer must match the real per-unit API.

**Type consistency:** `TsModuleMap::build(&[SourceFile])` / `module_of(&str)->String` / `resolve_import(&str,&str)->Option<String>` consistent Tasks 1,2,3. `record_from_hotspot(..., &TsModuleMap)` consistent Tasks 2-4. `extract(body, imports, lines, &str, &TsModuleMap)` consistent Tasks 3-4. `symbol_segments` defined Task 2, used Task 2.

**Coupling note (for the executor):** Tasks 2 and 3 are a pair — Task 2 alone makes the partition adopted while resolved_target is None, so do NOT gate propagation/dogfood between them. Gate at Task 4. (Same structure as Plan 2 Tasks 3↔4.)

**Two-pass wrinkle (for the executor):** TS records come from the React two-pass (`record_from_hotspot` + absorbed `extra_refs`). Both the per-unit `refs::extract` AND the absorbed-arrow `refs::extract` call in `analyze_units` must pass the file path + map so absorbed refs carry `resolved_target`. Verify against `crates/fxrank-lang-ts/src/lib.rs` `analyze_units`.

## Execution Handoff

This is **Plan 3 of the 025-3e set** (TS emitter). After it: Rust + TS partitions adopted, Python remains non-adopted (025 behavior). Plan 4 (Python) is the last, same shape.
