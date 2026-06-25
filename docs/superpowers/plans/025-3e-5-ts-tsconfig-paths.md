# 025-3e Plan 5 — TS `tsconfig.json` `paths` alias resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve TS/JS `tsconfig.json` `compilerOptions.paths` aliases (`@/*`, `~/*`, …) so alias imports — the dominant first-party reference shape in real apps (dogfood: **969/978** first-party reaches in `omni/114-kg-frontend` were `@/` aliases, all currently opaque) — fold into cross-file propagation. Driven by a **tsc-compatible `--project` / `-p` CLI flag**.

**Architecture:** The CLI passes the `--project` path to the **`fxrank-lang-ts` crate**, which reads `tsconfig.json` from disk and parses its `baseUrl` + `paths` into an alias table (the file read + parse live in the ts crate — `fxrank-core` stays parser-free and I/O-free; this is the one sanctioned disk read, spec 025-3e §9). `TsModuleMap::resolve_import` gains an alias step: a non-relative specifier matching a `paths` pattern is expanded to a `baseUrl`-relative path, then run through the existing extension/index ladder against the in-batch keys. Builds on Plan 3's `TsModuleMap` (branch `feat/025-3e-ts-emitter`).

**Tech Stack:** Rust, `swc`. Adds: `serde_json` as a regular dep of `fxrank-lang-ts` (already in the workspace/CLI), plus a **JSONC-tolerant parse** (tsconfig allows `//`/`/* */` comments + trailing commas — see Task 1 for the dependency decision). `fxrank-core` untouched.

## Global Constraints

- **Disk reads live ONLY in `fxrank-lang-ts`** (the `tsconfig` loader). `fxrank-core` stays parser-free + I/O-free; the CLI only passes the `--project` string through. (User directive + spec 025-3e §9.)
- **tsc-compatible flag:** `--project <path>` with short `-p`, accepting **either a `tsconfig.json` file OR a directory containing one** (exactly tsc's `-p` semantics). TS/JS-only; a no-op for Rust/Python scans.
- **Own-body output byte-identical**; this only *adds* alias resolutions (more `resolved_target` hits → richer `propagated_*`), never changes own-body or removes edges.
- **Never-guess invariant (still holds):** an alias resolves only if it expands to an **in-batch** module key (via the ladder); a miss → opaque, exactly as today. No guessed targets.
- **No tsc/node invocation.** Pure file read + JSONC parse + path-string resolution.
- CI gates per commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. TDD throughout.

## File Structure

- `crates/fxrank-lang-ts/src/tsconfig.rs` — **create**: `TsConfig { base, paths }` + `parse`/`strip_jsonc`/`load` (the sanctioned disk read + JSONC parse; pure `parse`/`strip_jsonc`, thin `load`).
- `crates/fxrank-lang-ts/src/module_map.rs` — **modify**: `build` takes an optional `&TsConfig`; `resolve_import` gains the alias-expansion step.
- `crates/fxrank-lang-ts/src/lib.rs` — **modify**: `TsFrontend` gains a `tsconfig: Option<TsConfig>` field; `analyze` passes it to `TsModuleMap::build`.
- `crates/fxrank-lang-ts/Cargo.toml` — **modify**: move/add `serde_json` to `[dependencies]`; add the chosen JSONC parser dep.
- `crates/fxrank-cli/src/main.rs` — **modify**: add `--project`/`-p` to `Scan`; when set and the TS feature is on, call `fxrank_lang_ts::tsconfig::load(path)` and construct the `TsFrontend` with it.

---

### Task 1: `tsconfig.rs` — parse `baseUrl` + `paths` (JSONC)

**Files:**
- Create: `crates/fxrank-lang-ts/src/tsconfig.rs`
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (add `pub mod tsconfig;`)
- Modify: `crates/fxrank-lang-ts/Cargo.toml` (deps — see below)

**Dependency decision (JSONC) — in-crate stripper, NO new external dep (revised after review I1):** `tsconfig.json` is JSONC (`//` + `/* */` comments + trailing commas), which `serde_json` rejects. An external jsonc crate (`jsonc-parser`/`json5`) is **not installable in this offline/cached build environment** (not in the cargo cache → `cargo add` would need the network and block the downstream tasks). So implement a small **string-literal-AWARE** `strip_jsonc(&str) -> String` in the ts crate (a ~40-line state machine: track in-string state with escape handling; drop `//`→EOL and `/* … */` only when NOT in a string; remove a trailing comma before `}`/`]`), then `serde_json::from_str`. The naive caution ("don't hand-roll") is about *non-string-aware* stripping — a string-aware machine handles `"a // b"` and `"/* x */"` correctly and is unit-testable. Promote `serde_json` to a regular `[dependency]` of `fxrank-lang-ts` (currently dev-only; already in the workspace lockfile, so no fetch). Add `strip_jsonc` tests for the tricky cases (comment markers inside strings, escaped quotes, trailing commas).

**Interfaces:**
- Produces:
  - `pub struct TsConfig { pub base: String, pub paths: Vec<(String, Vec<String>)> }` — **`base` is the EFFECTIVE, cleaned base directory** that `paths` targets resolve against (revised per C1/Codex-P2): `clean_dir(config_dir + baseUrl)` if `baseUrl` is present, else `clean_dir(config_dir)` — modern TS allows `paths` without `baseUrl` (targets then relative to the config dir). Always populated. `paths` keeps declaration order (sorted by specificity at match time).
  - `pub fn parse(jsonc: &str, config_dir: &str) -> TsConfig` — pure; strips JSONC then extracts `compilerOptions.{baseUrl,paths}`; computes the effective `base`. Missing → empty `paths`, `base = clean_dir(config_dir)`.
  - `pub fn strip_jsonc(jsonc: &str) -> String` — string-aware comment + trailing-comma stripper (Task-1 dep decision).
  - `pub fn load(project: &std::path::Path) -> Result<TsConfig, String>` — the sanctioned disk read: if `project` is a dir, append `tsconfig.json`; read; `parse(text, dirname)`. Errors → strings (CLI surfaces as a diagnostic, not a panic).

- [ ] **Step 1: Write the failing tests** (pure `parse` — string in, no disk)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_shape_no_baseurl_relative_dir() {
        // The REAL omni shape (C2): NO baseUrl, target with leading `./`, and a
        // RELATIVE config_dir "." (the `scan src --project .` case).
        let jsonc = r#"{
            // project config
            "compilerOptions": {
                "paths": {
                    "@/*": ["./src/*"],
                    "@/components/*": ["./src/components/*"], // overlapping prefix (I3)
                },
            },
        }"#;
        let c = parse(jsonc, ".");
        // No baseUrl → base = clean_dir(".") = "" (root namespace, matches in-batch keys).
        assert_eq!(c.base, "");
        assert_eq!(c.paths.iter().find(|(k,_)| k == "@/*").unwrap().1, vec!["./src/*".to_string()]);
    }

    #[test]
    fn baseurl_joined_and_cleaned() {
        let c = parse(r#"{"compilerOptions":{"baseUrl":"./src","paths":{"@/*":["./*"]}}}"#, "proj");
        assert_eq!(c.base, "proj/src"); // clean_join("proj","./src")
    }

    #[test]
    fn malformed_is_empty_not_error() {
        let c = parse("{ this is not json", "proj");
        assert_eq!(c.base, "proj");
        assert!(c.paths.is_empty());
    }

    #[test]
    fn clean_join_collapses_dot_base() {
        // C1: the bug class — a `.`/`./`/"" base must NOT leak a leading `./`.
        assert_eq!(clean_join(".", "./src"), "src");
        assert_eq!(clean_join("", "src"), "src");
        assert_eq!(clean_join("proj", "."), "proj");
        assert_eq!(clean_join("/abs", "./src"), "/abs/src");
        assert_eq!(clean_join("src/comp", "../util"), "src/util");
    }

    #[test]
    fn strip_jsonc_is_string_aware() {
        // comment markers INSIDE strings must survive; real comments + trailing commas go.
        let out = strip_jsonc(r#"{"a":"x // not a comment","b":"/* also not */", /* real */ "c":1,}"#);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], "x // not a comment");
        assert_eq!(v["b"], "/* also not */");
        assert_eq!(v["c"], 1);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p fxrank-lang-ts tsconfig 2>&1 | head -20`
Expected: compile error — `tsconfig`/`parse` not found.

- [ ] **Step 3: Implement** — add `pub mod tsconfig;` to lib.rs; add deps to Cargo.toml; implement `tsconfig.rs`:

```rust
//! tsconfig.json (JSONC) loader: extracts compilerOptions.baseUrl + paths for
//! alias resolution. The ONLY disk read in the TS frontend (sanctioned, §9);
//! fxrank-core stays parser-free + I/O-free.

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct TsConfig {
    /// Effective, CLEANED base directory that `paths` targets resolve against
    /// (config_dir joined with baseUrl if present, else config_dir; `.`/`./`/`""`
    /// collapse to ""). Always populated.
    pub base: String,
    /// (pattern, targets) in declaration order, e.g. ("@/*", ["./*"]).
    pub paths: Vec<(String, Vec<String>)>,
}

/// Parse a JSONC tsconfig string. Pure — no disk, no panic (malformed → empty
/// paths, base = clean_dir(config_dir)).
pub fn parse(jsonc: &str, config_dir: &str) -> TsConfig {
    let value: serde_json::Value = match serde_json::from_str(&strip_jsonc(jsonc)) {
        Ok(v) => v,
        Err(_) => return TsConfig { base: clean_dir(config_dir), paths: vec![] },
    };
    let co = value.get("compilerOptions");
    // Effective base: config_dir + baseUrl (if any), cleaned. C1: clean_dir collapses
    // a `.`/`""`/leading-`./` base so `scan src --project .` resolves (config_dir=".").
    let base = match co.and_then(|c| c.get("baseUrl")).and_then(|b| b.as_str()) {
        Some(b) => clean_join(config_dir, b),
        None => clean_dir(config_dir),
    };
    let mut paths = Vec::new();
    if let Some(obj) = co.and_then(|c| c.get("paths")).and_then(|p| p.as_object()) {
        for (pat, targets) in obj {
            let ts: Vec<String> = targets
                .as_array()
                .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            paths.push((pat.clone(), ts));
        }
    }
    TsConfig { base, paths }
}

pub fn load(project: &Path) -> Result<TsConfig, String> {
    let file = if project.is_dir() { project.join("tsconfig.json") } else { project.to_path_buf() };
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("could not read tsconfig {}: {e}", file.display()))?;
    let dir = file.parent().and_then(|p| p.to_str()).unwrap_or("").to_string();
    Ok(parse(&text, &dir))
}

/// Collapse a directory string to the in-batch-key namespace: drop a leading
/// `./`, treat `.`/`""` as the empty (root) base, drop a trailing `/`, and
/// normalize `.`/`..` segments. So `clean_dir(".")=""`, `clean_dir("./src")="src"`,
/// `clean_dir("proj/")="proj"`. (C1: without this, `config_dir="."` poisons every
/// alias with a leading `./` and the relative-invocation case resolves nothing.)
fn clean_dir(dir: &str) -> String { clean_join(dir, "") }

/// Join a base dir with a relative segment-string and normalize, collapsing `.`/
/// `..`/empty segments on BOTH sides (unlike module_map's normalize_join, which
/// keeps a `.` base segment — the C1 bug). `clean_join(".","./src")="src"`,
/// `clean_join("proj",".")="proj"`, `clean_join("/abs","./src")="/abs/src"`.
fn clean_join(base: &str, rest: &str) -> String {
    let abs = base.starts_with('/');
    let mut segs: Vec<&str> = Vec::new();
    for part in base.split('/').chain(rest.split('/')) {
        match part {
            "" | "." => {}
            ".." => { segs.pop(); }
            other => segs.push(other),
        }
    }
    let joined = segs.join("/");
    if abs { format!("/{joined}") } else { joined }
}

/// String-aware JSONC → JSON: strip `//`/`/* */` comments (NOT inside strings,
/// honoring `\"` escapes) and trailing commas before `}`/`]`. (Task-1 dep decision.)
pub fn strip_jsonc(jsonc: &str) -> String {
    // State machine over chars: in_string (+ escaped), line_comment, block_comment.
    // Emit only code chars; after emitting, drop a comma that is followed (skipping
    // whitespace) by `}` or `]`. Implement straightforwardly; unit-tested below.
    // (Full impl is a small loop — write it in the source; the tests pin behavior.)
    todo!("string-aware stripper — see strip_jsonc tests for the exact contract")
}
```
(`clean_join`/`clean_dir` REPLACE any reuse of module_map's `normalize_join` here — that one keeps a `.` base segment, which is exactly C1. Write `strip_jsonc` as a real char-state-machine; the `todo!` is a placeholder for the loop body, fully pinned by the Step-1 stripper tests below — the implementer writes the loop, not a stub.)

- [ ] **Step 4: pass + commit**

Run: `cargo test -p fxrank-lang-ts tsconfig` (2 pass), `cargo test -p fxrank-lang-ts`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`.
```bash
git -C /dev/shm/fxrank/3e-ts2 add -A
git -C /dev/shm/fxrank/3e-ts2 commit -m "feat(ts): tsconfig.rs — parse baseUrl + paths (JSONC), sanctioned disk read

The TS frontend reads tsconfig.json (file or dir) and parses compilerOptions
baseUrl + paths, tolerating JSONC comments/trailing commas. Malformed → empty
(never crashes a scan). Disk read stays in the ts crate; core untouched. (025-3e §9)"
```

---

### Task 2: `TsModuleMap` alias resolution

**Files:**
- Modify: `crates/fxrank-lang-ts/src/module_map.rs`

**Interfaces (ADDITIVE — C3: keep `build(files)` to avoid touching its 8 existing call sites):**
- Keep `TsModuleMap::build(files: &[SourceFile]) -> Self` exactly as-is (the 8 call sites in lib.rs/detect/module_map-tests stay; they build with no aliases → Plan-3 behavior).
- Add `pub fn build_with_tsconfig(files: &[SourceFile], tsconfig: &TsConfig) -> Self` — same as `build` plus a precomputed `aliases` table; **only `lib.rs::analyze` calls this**, when `--project` was given.
- `TsModuleMap` gains a private `aliases: Vec<(String, String)>` field (pattern, base-joined-target — empty for `build(files)`).
- `resolve_import(importer_file, specifier)` — unchanged signature; adds an alias step.

**Alias-match rules (mirror tsc) — using the config's cleaned `base`:**
- Precompute the alias table once in `build_with_tsconfig`: for each `(pattern, targets)`, take the first target and store `(pattern, clean_join(&cfg.base, target))` — e.g. base `""` + target `"./src/*"` → `"src/*"`; base `"proj/src"` + `"./*"` → `"proj/src/*"`. Sort the table by pattern **prefix length descending** (longest-prefix wins, I3).
- In `resolve_import`, BEFORE the relative `./`/`../` check: if `specifier` is non-relative, try the alias table in sorted order. A wildcard pattern `P/*` matches a specifier starting with `P/` (or equal to `P`), capturing the remainder `R`; substitute `R` into the stored target's `*` → a candidate path → run it through `module_key` + the in-batch ladder (the SAME `candidate = module_key(normalize_join(...))` lookup used for relative imports). First in-batch hit wins. An exact (non-`*`) pattern matches the whole specifier.
- No alias matched (or matched but not in batch) AND not relative → `None` (real package / `node:*` / unresolvable alias — unchanged, never-guess preserved).

- [ ] **Step 1: Write the failing tests** (append to module_map tests — REAL omni shape, C2)

```rust
#[test]
fn resolves_tsconfig_path_alias_real_shape() {
    use crate::tsconfig::TsConfig;
    let files = vec![sf("src/app.ts"), sf("src/hooks/use-auth.ts"), sf("src/components/btn.ts")];
    // The real shape: NO baseUrl (base=""), targets with leading `./` → cleaned to "src/*".
    let cfg = TsConfig {
        base: "".into(),
        paths: vec![
            ("@/*".into(), vec!["./src/*".into()]),
            ("@/components/*".into(), vec!["./src/components/*".into()]), // overlap (I3)
        ],
    };
    let m = TsModuleMap::build_with_tsconfig(&files, &cfg);
    assert_eq!(m.resolve_import("src/app.ts", "@/hooks/use-auth"), Some("src/hooks/use-auth".into()));
    // Overlapping prefix: @/components/btn matches both @/* and @/components/* (longest wins) → same key here.
    assert_eq!(m.resolve_import("src/app.ts", "@/components/btn"), Some("src/components/btn".into()));
    assert_eq!(m.resolve_import("src/app.ts", "react"), None); // real package
    assert_eq!(m.resolve_import("src/app.ts", "@/missing"), None); // alias, not in batch → opaque
}

#[test]
fn no_tsconfig_means_aliases_stay_opaque() {
    let files = vec![sf("src/app.ts"), sf("src/hooks/use-auth.ts")];
    let m = TsModuleMap::build(&files); // the unchanged Plan-3 constructor
    assert_eq!(m.resolve_import("src/app.ts", "@/hooks/use-auth"), None);
}
```

- [ ] **Step 2-4:** run-fail → add the `aliases` field + `build_with_tsconfig` (precompute via `clean_join(&cfg.base, target)`, sort longest-prefix) + the alias step in `resolve_import` → run-pass → `cargo test -p fxrank-lang-ts` (the 8 `build(files)` sites untouched → still green) → fmt/clippy → commit:
```
feat(ts): resolve tsconfig paths aliases in TsModuleMap

Additive build_with_tsconfig(files, &TsConfig) (build(files) untouched, no
call-site ripple). resolve_import expands a paths-alias specifier (longest-prefix
pattern, base-cleaned target, * substitution) then runs the extension/index
ladder against in-batch keys. No tsconfig → unchanged (aliases opaque).
Never-guess preserved (resolves only to in-batch keys).
```

---

### Task 3: `--project`/`-p` CLI flag + thread tsconfig into the TS frontend

**Files:**
- Modify: `crates/fxrank-cli/src/main.rs` (`Scan` args + dispatch)
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`TsFrontend` field + `analyze` threads it)

**Interfaces:**
- `TsFrontend` gains `pub tsconfig: Option<TsConfig>` (Default `None`). `analyze` calls `TsModuleMap::build_with_tsconfig(files, cfg)` when `self.tsconfig` is `Some`, else the existing `TsModuleMap::build(files)` (C3 — additive, no other call site changes).
- CLI `Scan` gains `#[arg(long, short = 'p')] project: Option<PathBuf>` — doc: "Path to a tsconfig.json (or a directory containing one) for resolving TS `paths`/`baseUrl` aliases (tsc-compatible `-p`). TS/JS only."
- **I2 — `run_scan` threading:** `run_scan` currently has 6 positional params and **~20 in-process test callers** in `main.rs`. Add `project: Option<PathBuf>` as a 7th param; the compiler will flag every caller — add `None` to each (mechanical). Also add `project` to the `Scan { .. }` destructure in `main()` and pass it through. (Alternatively bundle args into a struct, but the `None`-sweep is simpler and compiler-guided.)
- When `project` is `Some` and a TS source is routed, the CLI calls `fxrank_lang_ts::tsconfig::load(&project)` (feature-gated behind `ts`), constructs `TsFrontend { tsconfig: Some(cfg), .. }`. A load error → a `Diagnostic { parsed: false, .. }` surfaced in the report, NOT a panic; scanning continues with aliases unresolved. In a slim non-`ts` build the flag is accepted but inert (no `fxrank_lang_ts` reference compiled).

- [ ] **Step 1: Write the failing test** (CLI integration, in `crates/fxrank-cli/tests/cli.rs` or similar — mirror the existing CLI tests)

```rust
// Scan a tiny TS project with an @/ alias import + a tsconfig, assert the alias
// resolves (the importing hotspot gains an inherited edge / the reach is no longer
// FirstPartyOutOfScope). Write the fixture to a tempdir; run the binary with
// `scan <dir> --project <dir>`; parse the JSON; assert the @/ call resolved.
```
(Adapt to the crate's existing CLI test harness — `assert_cmd`/tempdir or whatever `cli.rs` uses. The assertion: with `--project`, the `@/`-imported callee's effect is inherited by the caller; without it, it's a FirstPartyOutOfScope reach.)

- [ ] **Step 2-4:** run-fail → add the flag + thread `tsconfig::load` into the TS `Route`/frontend construction (feature-gated behind `ts`; for a slim non-TS build the flag is accepted but inert) + handle load errors as diagnostics → run-pass (`cargo test --workspace`) → fmt/clippy → commit:
```
feat(cli): --project/-p flag for tsconfig paths (tsc-compatible)

Pass a tsconfig.json (file or dir) to the TS frontend for paths/baseUrl alias
resolution. The ts crate does the disk read+parse; the CLI only forwards the
path and surfaces a load error as a diagnostic. No-op for Rust/Python.
```

---

### Task 4: e2e + dogfood (the omni @/ aliases now resolve)

**Files:** Modify `crates/fxrank-lang-ts/src/lib.rs` (`#[cfg(test)]` e2e). No production change.

- [ ] **Step 1:** e2e test — a 2-file project (`src/app.ts` does `import { x } from '@/util'; x()`, `src/util.ts` exports `x` doing `fetch`) + `TsConfig { base: "".into(), paths: vec![("@/*".into(), vec!["./src/*".into()])] }` (real shape: empty base, `./src/*` target); drive `TsFrontend { tsconfig: Some(cfg), .. }.analyze`, build `CanonicalIndex`, assert the `@/util` call **resolves** (`Edge::Resolved`, and `app` inherits `x`'s `fetch`/net effect) — whereas with `tsconfig: None` it's opaque. (Pre-bind `is_resolved` — `Edge` has no `Debug`.)

- [ ] **Step 2-3:** `cargo test -p fxrank-lang-ts` + `cargo test --workspace` + fmt + clippy — all green.

- [ ] **Step 4: Dogfood the real omni app** — the dogfood that motivated this plan:
```bash
cd /dev/shm/fxrank/3e-ts2 && export PATH="$HOME/.cargo/bin:$PATH" && cargo build -q -p fxrank
APP=/home/caasi/GitLab/omni/114-kg-frontend
BIN=target/debug/fxrank
# Before (no --project): @/ aliases opaque
"$BIN" scan "$APP/src" | jq '[.scope.external_reaches[]|select(.specifier|startswith("@/"))]|length'
# After (--project): @/ aliases resolve → far fewer @/ reaches, more inherited edges
"$BIN" scan "$APP/src" --project "$APP" | jq '{at_aliases: ([.scope.external_reaches[]|select(.specifier|startswith("@/"))]|length), inherited: ([.hotspots[]|select((.inherited|length)>0)]|length), violations: ([.hotspots[]|select(.propagated_score < .own_score)]|length)}'
```
Confirm: with `--project` the `@/` reach count drops sharply (was ~969), `inherited` rises (was ~19), `violations == 0`. Record the before/after numbers in the report — this is the payoff. (If many `@/` still don't resolve, note why: baseUrl mismatch vs the scanned path layout, `extends`, etc. — those become the documented next limits.)

- [ ] **Step 5: Commit** the e2e test (`test(ts): e2e — @/ alias import resolves with --project`).

---

## Self-Review

**Spec coverage:** closes the spec 025-3e §9 deferred item "TS tsconfig `paths` is the highest-value TS follow-up." Flag is tsc-compatible (`--project`/`-p`, file-or-dir). Disk read confined to the ts crate. Never-guess preserved (alias resolves only to in-batch keys). ✓

**Decisions flagged for the executor:**
- **JSONC handling** (Task 1) — in-crate string-aware `strip_jsonc` + `serde_json` (promoted to a regular dep; already vendored). NO external jsonc crate (offline-uninstallable). The stripper is string-literal-aware and unit-tested.
- **Base/namespace reconciliation (C1)** — `clean_join`/`clean_dir` collapse `.`/`./`/`""` so aliases land in the same namespace as the in-batch keys for BOTH `scan <proj>/src --project <proj>` (absolute) and `scan src --project .` (relative). Replaces module_map's `normalize_join` (which keeps a `.` base — the bug).
- **baseUrl / path-layout reconciliation** — tsconfig `paths`/`baseUrl` are relative to the tsconfig dir; in-batch keys are relative to how the scan was invoked. v1 assumes the scan root and tsconfig dir align (the common `scan <proj>/src --project <proj>` case). Document mismatch as a limit; the dogfood (Task 4) measures real coverage and surfaces any reconciliation gap.

**Deferred (documented):** tsconfig `extends` (inherited configs), multiple targets per pattern beyond first-resolving, `references` (project references), per-directory nested tsconfigs. Each → alias stays opaque (safe), not wrong.

**Type consistency:** `TsConfig { base: String, paths }` consistent across Tasks 1-4; `TsModuleMap::build(files)` (unchanged) + additive `build_with_tsconfig(files, &TsConfig)` consistent Tasks 2-4; `tsconfig::{parse,strip_jsonc,load}` Tasks 1,3; `clean_join`/`clean_dir` Task 1.

## Execution Handoff

Plan 5 is additive on top of Plan 3 (TS emitter). After it: alias-heavy TS codebases get real cross-file coverage. Not a #36 blocker — #36 is complete once Plans 3+4 merge; Plan 5 is the dogfood-proven TS coverage enhancement.
