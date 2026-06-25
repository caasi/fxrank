# TS cross-dialect resolution (`.tsx` ↔ `.ts`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make one `TsModuleMap` span both TS dialects so `.tsx`→`.ts` references (relative AND tsconfig-alias) resolve into cross-file propagation (issue #41).

**Architecture:** `TsFrontend::analyze` chooses each file's parse dialect **per file** from its path extension (`self.lang` becomes the stdin-only fallback); `dispatch_ts` drops the per-dialect grouping and runs **all** TS files through a single `analyze`, so the `module_map` (built over `files`) naturally covers both dialects. TS-internal only — no `Frontend`-trait or core change. See spec `docs/superpowers/specs/026-ts-cross-dialect-resolution.md`.

**Tech Stack:** Rust, `swc`. Touches `crates/fxrank-lang-ts/src/lib.rs` and `crates/fxrank-cli/src/main.rs`.

## Global Constraints

- **Own-body byte-identical.** Each file is still parsed with the same dialect as before — both the old grouping and the new per-file path call the *same* `Lang::from_extension` (`ts→Ts`, `tsx→Tsx`, `js/jsx/mjs/cjs→Js`; `mts/cts` are never routed). Effects/risks/own_score unchanged; only `resolved_target`/`propagated_*` improve.
- **React scoring preserved.** The `analyze_units` two-pass is per-file own-body — it does not consult the cross-file map; merging dialects into one batch leaves every `.tsx`'s React augmentation identical.
- **Never-guess preserved.** A `.tsx`→`.ts` reference resolves only to an in-batch module key (now visible in the unified map); else opaque.
- **Cross-language isolation unchanged.** `partition_by_language` (Rust/TS/Python) is untouched; this merges only the two TS *dialects* within the single TS partition.
- **stdin unchanged.** A stdin source (path `"stdin"`, no extension) keeps using `--lang` via the fallback dialect.
- CI gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Run cargo from the worktree. TDD throughout.

## File Structure

- `crates/fxrank-lang-ts/src/lib.rs` — **modify** `TsFrontend::analyze`: derive the parse dialect per file from `source.path`'s extension (fallback `self.lang`). Add the Task-1 test.
- `crates/fxrank-cli/src/main.rs` — **modify** `dispatch_ts` (the `#[cfg(feature = "ts")]` one): replace the `HashMap<Lang, Vec<SourceFile>>` grouping + per-group loop with a `fallback_lang` + single `analyze`. Drop the now-unused `use std::collections::HashMap;`.
- `crates/fxrank-cli/tests/cli.rs` — **modify**: add the Task-2 cross-dialect + stdin-regression CLI tests.

---

### Task 1: Per-file dialect in `TsFrontend::analyze`

**Files:**
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`analyze` + a `#[cfg(test)]` test)

**Interfaces:**
- Consumes: `Lang::from_extension(&str) -> Option<Lang>` (already in `source.rs`), `functions::parse_module(text, path, lang)`.
- Produces: no signature change — `analyze` still `fn analyze(&self, files: &[SourceFile]) -> FrontendOutput`. Behavior: a mixed-dialect `files` batch parses each file by its own extension and resolves cross-dialect refs against the single `module_map`.

- [ ] **Step 1: Write the failing test** (append inside the existing `#[cfg(test)] mod` in `lib.rs`, alongside `e2e_at_alias_resolves_with_project_tsconfig`)

```rust
#[test]
fn cross_dialect_tsx_imports_ts_resolves() {
    use fxrank_core::frontend::SourceFile;
    use fxrank_core::graph::Edge;
    use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
    // app.tsx contains JSX → it MUST parse as Tsx. TsFrontend::default() has
    // lang = Ts, so only a PER-FILE dialect (from the .tsx extension) parses it.
    // `load` is lowercase (not a PascalCase component) → no React absorption, so
    // its call to `x` stays a plain outgoing ref.
    let files = vec![
        SourceFile {
            path: "src/app.tsx".into(),
            text: "import { x } from './util';\n\
                   export function load() { x(); return (<div/>); }\n"
                .into(),
        },
        SourceFile {
            path: "src/util.ts".into(),
            text: "export function x() { return fetch('/u'); }\n".into(),
        },
    ];
    // default lang = Ts; the .tsx file only parses via the per-file dialect.
    let out = TsFrontend::default().analyze(&files);
    let load_rec = out
        .records
        .iter()
        .find(|r| r.symbol == "load")
        .expect("app.tsx must parse as Tsx per-file and yield a `load` record");
    let x_ref = load_rec
        .refs
        .iter()
        .find(|r| r.module.as_deref() == Some("./util"))
        .expect("load must have a ref with module='./util'");
    let idx = CanonicalIndex::from_records(&out.records);
    assert!(idx.adopted(), "TS partition must be adopted");
    let edge = resolve_ref_precise(x_ref, &idx, &load_rec.path);
    // `Edge` has no `Debug` — pre-bind the boolean.
    let is_resolved = matches!(edge, Some(Edge::Resolved(_)));
    assert!(
        is_resolved,
        ".tsx→.ts relative import must resolve cross-dialect (Edge::Resolved)"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p fxrank-lang-ts cross_dialect_tsx_imports_ts_resolves 2>&1 | tail -20`
Expected: FAIL — with `self.lang = Ts`, `app.tsx`'s JSX fails to parse (swc error → a diagnostic, no `load` record), so `.expect("… yield a `load` record")` panics.

- [ ] **Step 3: Implement per-file dialect**

In `analyze`, change the parse line. Replace:

```rust
        for source in files {
            match functions::parse_module(&source.text, &source.path, self.lang) {
```

with:

```rust
        for source in files {
            // Per-file dialect (#41): a file's own extension decides its parse
            // dialect, so a single analyze call handles a mixed .ts/.tsx batch and
            // the module_map (built over `files`) spans both dialects. `self.lang`
            // is the fallback only for an extension-less path (stdin).
            // `Lang::from_extension` takes the extension string (no dot).
            let dialect = std::path::Path::new(&source.path)
                .extension()
                .and_then(|e| e.to_str())
                .and_then(Lang::from_extension)
                .unwrap_or(self.lang);
            match functions::parse_module(&source.text, &source.path, dialect) {
```

(If `Lang` is not already in scope in `lib.rs`, add `use crate::source::Lang;` to the test-free top of the file — it is the type of `TsFrontend.lang`, so it is already imported; do not duplicate.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p fxrank-lang-ts cross_dialect_tsx_imports_ts_resolves` → PASS.
Then `cargo test -p fxrank-lang-ts` (all existing TS tests still green — own-body unchanged), `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank/41 add crates/fxrank-lang-ts/src/lib.rs
git -C /dev/shm/fxrank/41 commit -m "feat(ts): per-file parse dialect in analyze (#41)

analyze derives each file's dialect from its path extension (self.lang is the
stdin fallback), so one analyze call handles a mixed .ts/.tsx batch and the
single module_map spans both dialects — a .tsx→.ts import resolves. Own-body
byte-identical (same dialect per file, selected per-file not per-group)."
```

---

### Task 2: `dispatch_ts` — drop the dialect grouping, single `analyze`

**Files:**
- Modify: `crates/fxrank-cli/src/main.rs` (the `#[cfg(feature = "ts")] fn dispatch_ts`)
- Modify: `crates/fxrank-cli/tests/cli.rs` (two tests)

**Interfaces:**
- Consumes: `dispatch_ts(sources: Vec<(String, SourceFile)>, include_tests, project)` (signature unchanged — the `(ext, source)` pairs still arrive; the ext is now used only to derive the stdin fallback dialect). `TsFrontend { lang, include_tests, tsconfig }`, `Lang::from_extension`.
- Produces: `dispatch_ts` runs one `TsFrontend::analyze` over all TS files, so `.tsx` and `.ts` share one `module_map`.

- [ ] **Step 1: Write the failing tests** (append to `crates/fxrank-cli/tests/cli.rs`, mirroring the existing `assert_cmd` + `tempfile` harness used by the `project_flag_*` tests)

```rust
#[test]
fn cross_dialect_tsx_imports_ts_resolves_via_cli() {
    // A .tsx component importing a .ts util that does fetch. Before #41 the .tsx
    // and .ts landed in separate dialect module_maps → the import was opaque.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.tsx"),
        "import { load } from './util';\n\
         export function App() { load(); return (<div/>); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("util.ts"),
        "export function load() { return fetch('/u'); }\n",
    )
    .unwrap();

    let out = assert_cmd::Command::cargo_bin("fxrank")
        .unwrap()
        .arg("scan")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // App (in app.tsx) must inherit util.ts's net effect across the dialect boundary:
    // its propagated_max_class reaches the net/fs/db class (7), not its own-body class.
    let app = report["hotspots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["symbol"] == "App")
        .expect("App hotspot present");
    assert_eq!(
        app["propagated_max_class"].as_u64(),
        Some(7),
        ".tsx App must inherit .ts util's net effect across dialects"
    );
}

#[test]
fn stdin_lang_tsx_still_parses() {
    // Regression for the fallback dialect: stdin has path "stdin" (no extension),
    // so its dialect must come from --lang via dispatch_ts's fallback_lang.
    let out = assert_cmd::Command::cargo_bin("fxrank")
        .unwrap()
        .args(["scan", "--lang", "tsx", "-"])
        .write_stdin("export function load() { return (<div/>); }\n")
        .output()
        .unwrap();
    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // No parse diagnostic for the stdin source → it parsed as Tsx.
    let diags = report["diagnostics"].as_array().cloned().unwrap_or_default();
    assert!(
        diags.iter().all(|d| d["parsed"] != serde_json::json!(false)),
        "stdin --lang tsx must parse (no parse-failure diagnostic): {diags:?}"
    );
}
```

(Use whatever `serde_json` / `assert_cmd` / `tempfile` imports the existing `cli.rs` tests already use; if `propagated_max_class` is absent in the JSON for a hotspot with no propagation, the assertion still pins the resolved case — App DOES propagate here. If the field name differs in the schema, match the schema; check an existing hotspot assertion in `cli.rs`.)

- [ ] **Step 2: Run to verify they fail**

Run (two separate invocations — `cargo test` takes only one positional filter before `--`):
```
cargo test -p fxrank --test cli cross_dialect_tsx_imports_ts_resolves_via_cli 2>&1 | tail -25
cargo test -p fxrank --test cli stdin_lang_tsx_still_parses 2>&1 | tail -25
```
Expected: `cross_dialect_…` FAILS (today the `.tsx` App's import of the `.ts` `load` is opaque → `propagated_max_class` is the own class, not 7). `stdin_lang_tsx_still_parses` should PASS already (proves we don't regress it).

- [ ] **Step 3: Implement — drop the grouping**

In `dispatch_ts`, replace the grouping block. Replace:

```rust
    // Group by resolved `Lang` so each dialect runs with its own syntax. The
    // grouping key is the `Lang` (a `.ts` and a `.tsx` in one dir differ).
    let mut groups: HashMap<Lang, Vec<SourceFile>> = HashMap::new();
    for (ext, source) in sources {
        // Every collected extension is one `Lang::from_extension` recognizes.
        let lang = Lang::from_extension(&ext).unwrap_or_else(|| {
            unreachable!("route_for_path only routes extensions from_extension recognizes")
        });
        groups.entry(lang).or_default().push(source);
    }

    let mut output = FrontendOutput::default();
    for (lang, group) in groups {
        let frontend = TsFrontend {
            lang,
            include_tests,
            tsconfig: ts_cfg.clone(),
        };
        merge_output(&mut output, frontend.analyze(&group));
    }
    (output, config_errors)
```

with:

```rust
    // #41: one TsModuleMap must span both dialects, so run ALL TS files through a
    // single analyze. The parse dialect is chosen per-file (from each path's
    // extension) inside analyze. `fallback_lang` covers a stdin source (path
    // "stdin", no extension) whose dialect comes from the routed extension; for a
    // directory scan every file carries its own extension so the fallback is never
    // consulted.
    let fallback_lang = sources
        .first()
        .and_then(|(ext, _)| Lang::from_extension(ext))
        .unwrap_or_default();
    let files: Vec<SourceFile> = sources.into_iter().map(|(_, source)| source).collect();

    let mut output = FrontendOutput::default();
    let frontend = TsFrontend {
        lang: fallback_lang,
        include_tests,
        tsconfig: ts_cfg,
    };
    merge_output(&mut output, frontend.analyze(&files));
    (output, config_errors)
```

Then remove the now-unused `use std::collections::HashMap;` — the **function-local** one inside `dispatch_ts`'s `use` block ONLY. Do NOT touch the module-level `use std::collections::{HashMap, HashSet};` at the top of `main.rs` (still used by `partition_by_language`). Keep `use fxrank_lang_ts::source::Lang;` (`fallback_lang` uses it). `cargo clippy` will flag the unused import if missed.

- [ ] **Step 4: Run to verify they pass**

Run (two invocations): `cargo test -p fxrank --test cli cross_dialect_tsx_imports_ts_resolves_via_cli` and `cargo test -p fxrank --test cli stdin_lang_tsx_still_parses` → both PASS.
Then `cargo test --workspace` (ALL green — the regression guard: every existing single-dialect + CLI test passing proves own-body is unchanged), `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git -C /dev/shm/fxrank/41 add crates/fxrank-cli/src/main.rs crates/fxrank-cli/tests/cli.rs
git -C /dev/shm/fxrank/41 commit -m "feat(cli): dispatch_ts runs one analyze over all TS files (#41)

Drop the per-dialect HashMap grouping; pass all TS files to a single
TsFrontend::analyze so the module_map spans .ts + .tsx. The per-file dialect is
chosen in analyze; fallback_lang (from the first source's routed extension)
covers stdin. Cross-dialect .tsx→.ts imports now resolve; stdin --lang unchanged."
```

---

### Task 3: e2e verification + omni dogfood (the payoff)

**Files:** none changed (verification only; the functional tests live in Tasks 1–2).

- [ ] **Step 1: Full regression gate**

Run from the worktree: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test --workspace` → all green; `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`. (All existing tests passing is the byte-identical own-body guard — no snapshot exercises `dispatch_ts`, so the suite + the §4.3 mapping-table proof are the guard.)

- [ ] **Step 2: Dogfood the omni app — the #41 payoff**

```bash
cd /dev/shm/fxrank/41 && export PATH="$HOME/.cargo/bin:$PATH" && cargo build -q -p fxrank
APP=/home/caasi/GitLab/omni/114-kg-frontend
BIN=target/debug/fxrank
echo "AFTER #41 (--project):"
"$BIN" scan "$APP/src" --project "$APP" | jq '{at_alias_reaches: ([.scope.external_reaches[]|select(.specifier|startswith("@/"))]|length), inherited: ([.hotspots[]|select((.inherited|length)>0)]|length), violations: ([.hotspots[]|select(.propagated_score < .own_score)]|length)}'
```

Expected vs the spec-025-3e-Plan-5 baseline (`@/` reaches 523, inherited 34, violations 0): the `@/` cross-dialect reaches drop **sharply** (the `.tsx`→`.ts` `@/libs/utils`-class imports now resolve), `inherited` rises, `violations == 0`. Record the exact AFTER numbers and the delta from 523/34 in the report. If `@/` reaches do NOT drop much, STOP and diagnose (is `dispatch_ts` actually passing both dialects to one analyze? is the tsconfig alias still resolving?) — report the numbers, do not fudge. If `/dev/shm` is low on space the build may fail with a misleading "could not compile" — check `df -h /dev/shm`.

- [ ] **Step 3: Record results** (no commit needed unless a fixture was added)

Write the regression result + the omni before/after into the task report.

---

## Self-Review

**Spec coverage:** §4.1 (per-file dialect) → Task 1; §4.2 (drop grouping, single analyze, stdin fallback) → Task 2; §6 (unit cross-dialect, stdin regression, dogfood) → Tasks 1–3; §4.3 invariants (byte-identical, React, never-guess, cross-language) → Global Constraints + the full-suite regression gate. ✓

**Placeholder scan:** every code step has concrete code; the only "match the existing harness" notes are for `cli.rs`'s test-boilerplate imports (assert_cmd/tempfile/serde_json) and the exact schema field name for the propagated-class assertion — both verifiable in `cli.rs` at implementation time. ✓

**Type consistency:** `dialect` is a `Lang`; `fallback_lang` is a `Lang`; `parse_module(_,_, dialect)` matches the existing `lang: Lang` parameter; `Lang::from_extension(&str) -> Option<Lang>` used consistently; `dispatch_ts` signature unchanged (`Vec<(String, SourceFile)>`). ✓

**Scope:** small, TS-internal — `analyze` (1 spot) + `dispatch_ts` (1 block) + tests. No `Frontend`-trait, core, Rust, or Python change. The `Route::Ts(String)` / `RoutedSource` ext plumbing is **retained** (used for the stdin `fallback_lang`), not cleaned up — the spec's "optional cleanup" is declined because the ext is still needed.

## Execution Handoff

Plan complete. Subagent-driven execution recommended: a fresh implementer per task + per-task review (opus on Task 1/2, the resolution-critical ones) + a final whole-branch review, then dogfood. After it lands, the TS frontend resolves cross-dialect and #41 closes — leaving #37 as the last release-gate item.
