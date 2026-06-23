# TypeScript / JavaScript Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `swc`-based, syntactic JS/TS frontend (`fxrank-lang-ts`) that ranks functions by own-body effect cost, where a fully-typed boundary discounts contained interior effects ("types lower the score").

**Architecture:** Mirror `fxrank-lang-rust` exactly — a feature-gated crate with `functions` / `imports` / `detect/{calls,mutation,risk}` modules, orchestrated by `detect::analyze_unit`, with `swc` standing in for `syn` and a `SourceMap`-backed span→line resolver standing in for `proc-macro2` span-locations. A new **language-neutral** boundary-containment discount in `fxrank-core` floors contained (non-escaping) state effects at class 0. Built **walking-skeleton-first**: after Task 2 the tool runs end-to-end; after Task 6 it emits real IO-hotspot signal for battle-testing; mutation, discount, and risk layer on after.

**Tech Stack:** Rust 2024, `swc_ecma_parser` / `swc_ecma_ast` / `swc_ecma_visit` / `swc_common`, `clap`, `serde`, `insta`.

**Spec:** `docs/superpowers/specs/003-fxrank-typescript-frontend.md` (source of truth — read it before starting; when code and spec disagree, the spec wins).

**Conventions (from `CLAUDE.md` + spec):**
- Core depends on **no parser** — `swc` must never appear in `fxrank-core`'s deps (compiler-enforced).
- Centralize vocabulary: new effect/risk kinds go in `EffectKind` / `RiskKind` with `wire()` + class; never hand-write wire strings at call sites.
- Each detector is pure (returns `Vec<Effect>` / risks); assembly lives in `analyze_unit`.
- Each detector walker always calls the default `swc_ecma_visit::visit_*` so nested expressions are still visited (same rule as the syn detectors).
- Implementation goes on a **feature branch** (never commit code to `main`). Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` before pushing.

---

## File structure

| File | Responsibility |
| --- | --- |
| `Cargo.toml` (workspace) | add `crates/fxrank-lang-ts` to `members` |
| `crates/fxrank-lang-ts/Cargo.toml` | new crate manifest; swc deps |
| `crates/fxrank-lang-ts/src/lib.rs` | `TsFrontend` + `Frontend` impl; per-file parse → collect → analyze; owns the `SourceMap` |
| `crates/fxrank-lang-ts/src/source.rs` | `Syntax` selection from a `Lang` enum; `SpanLines` span→line resolver |
| `crates/fxrank-lang-ts/src/functions.rs` | `FnUnit` collection from the swc module AST (decls, methods, arrows, getters/setters) |
| `crates/fxrank-lang-ts/src/imports.rs` | `ImportTable` for ES `import` + `require` |
| `crates/fxrank-lang-ts/src/coverage.rs` | signature typed-slot coverage + `any`-presence for the boundary gate |
| `crates/fxrank-lang-ts/src/detect/mod.rs` | `analyze_unit` (gather → fold → boundary discount → async) |
| `crates/fxrank-lang-ts/src/detect/calls.rs` | world-effect call/member detection |
| `crates/fxrank-lang-ts/src/detect/mutation.rs` | mutation detection + escape analysis (local/param/this/hidden/global) |
| `crates/fxrank-lang-ts/src/detect/risk.rs` | `type.escape` / `dynamic.code` / `proto.pollution` / `html.injection` |
| `crates/fxrank-lang-ts/tests/fixtures/*.{ts,tsx,js}` | fixtures (subdir, not compiled as test targets) |
| `crates/fxrank-lang-ts/tests/ts_frontend.rs` | `analyze_fixture` helper + unit/integration tests |
| `crates/fxrank-core/src/effect.rs` | add `EffectKind::ThisMutation`; add 4 `RiskKind`s |
| `crates/fxrank-core/src/score.rs` | add `apply_boundary_discount` (floor 0) + `BoundaryCoverage` |
| `crates/fxrank-cli/src/main.rs` | JS/TS extension discovery; `--lang` flag; feature-gated `ts` dispatch |
| `crates/fxrank-cli/Cargo.toml` | `ts` feature → optional `fxrank-lang-ts` dep |

**Detector template:** Task 6 (`calls`) establishes the swc walker shape (a `swc_ecma_visit::Visit` impl with a `push` helper building `Effect`s). Tasks 9 and 11 mirror that shape; the plan gives their classification tables + deltas rather than repeating boilerplate.

---

## Task 0: Branch + core vocabulary (no parser involved)

Do the parser-free core additions first — they unblock everything and carry zero swc risk.

**Files:**
- Create branch
- Modify: `crates/fxrank-core/src/effect.rs`
- Modify: `crates/fxrank-core/src/score.rs`

- [ ] **Step 1: Create the feature branch**

```bash
git checkout -b feat/ts-frontend main
```

- [ ] **Step 2: Write failing tests for the new vocabulary** in `crates/fxrank-core/src/effect.rs` (extend the existing `mod tests`)

```rust
#[test]
fn ts_vocabulary_metadata() {
    assert_eq!(EffectKind::ThisMutation.wire(), "this.mutation");
    assert_eq!(EffectKind::ThisMutation.base_class(), 3);
    assert_eq!(RiskKind::TypeEscape.wire(), "type.escape");
    assert_eq!(RiskKind::TypeEscape.class(), 3);
    assert_eq!(RiskKind::DynamicCode.class(), 7);
    assert_eq!(RiskKind::ProtoPollution.class(), 4);
    assert_eq!(RiskKind::HtmlInjection.class(), 5);
}
```

- [ ] **Step 3: Run it — expect FAIL** (`ThisMutation` etc. undefined)

Run: `cargo test -p fxrank-core ts_vocabulary_metadata`
Expected: compile error / FAIL.

- [ ] **Step 4: Add the kinds.** In `EffectKind`, add `ThisMutation` variant; in `wire()` add `ThisMutation => "this.mutation"`; in `base_class()` add `ThisMutation => 3`. In `RiskKind`, add `TypeEscape, DynamicCode, ProtoPollution, HtmlInjection`; in `wire()` add their strings (`"type.escape"`, `"dynamic.code"`, `"proto.pollution"`, `"html.injection"`); in `class()` map `TypeEscape => 3`, `DynamicCode => 7`, `ProtoPollution => 4`, `HtmlInjection => 5`.

- [ ] **Step 5: Run — expect PASS**

Run: `cargo test -p fxrank-core ts_vocabulary_metadata`
Expected: PASS.

- [ ] **Step 6: Write failing test for the boundary discount** in `crates/fxrank-core/src/score.rs`

```rust
#[test]
fn boundary_discount_floors_contained_at_zero() {
    use BoundaryCoverage::*;
    // contained effects: floor 0 (unlike apply_discount's floor 1)
    assert_eq!(apply_boundary_discount(1, Partial, true), 0); // local.mutation, some typing → free
    assert_eq!(apply_boundary_discount(1, Full, true), 0);
    assert_eq!(apply_boundary_discount(1, None, true), 1);    // no typing → unchanged
    // the latent gradient: only visible on class >= 2 contained inputs
    assert_eq!(apply_boundary_discount(3, Partial, true), 2);
    assert_eq!(apply_boundary_discount(3, Full, true), 1);
    // escaping effects: never shifted, regardless of coverage
    assert_eq!(apply_boundary_discount(3, Full, false), 3);
    assert_eq!(apply_boundary_discount(1, Full, false), 1);
}
```

- [ ] **Step 7: Run — expect FAIL**

Run: `cargo test -p fxrank-core boundary_discount`
Expected: FAIL (undefined).

- [ ] **Step 8: Implement** in `crates/fxrank-core/src/score.rs`

```rust
/// How much of a function's signature is explicitly typed (the boundary gate).
/// `None` = nothing typed (or `any`-poisoned); `Partial` = some slots typed;
/// `Full` = every slot typed. See spec 003 "The boundary-containment discount".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryCoverage {
    None,
    Partial,
    Full,
}

/// Boundary-containment discount: a class down-shift applied ONLY to **contained**
/// (non-escaping) state effects, floored at **class 0** (a contained effect is not
/// observable, so it may discount to truly free — unlike `apply_discount`, whose
/// floor is 1 for externally observable effects).
///
/// Escaping effects (`contained == false`) are never shifted. The depth is latent
/// for class-1 inputs (Partial and Full both floor to 0); it only separates on a
/// contained input of class >= 2 (none exist in the JS/TS milestone — kept for
/// future/Rust use).
pub fn apply_boundary_discount(base_class: u8, coverage: BoundaryCoverage, contained: bool) -> u8 {
    if !contained {
        return base_class;
    }
    let shift = match coverage {
        BoundaryCoverage::Full => 2,
        BoundaryCoverage::Partial => 1,
        BoundaryCoverage::None => 0,
    };
    base_class.saturating_sub(shift) // floor 0 (no `.max(1)`)
}
```

- [ ] **Step 9: Run — expect PASS**

Run: `cargo test -p fxrank-core boundary_discount`
Expected: PASS.

- [ ] **Step 10: Verify the whole core still passes, then commit**

```bash
cargo test -p fxrank-core && cargo clippy -p fxrank-core --all-targets -- -D warnings
git add crates/fxrank-core/src/effect.rs crates/fxrank-core/src/score.rs
git commit -m "feat(core): add JS/TS effect/risk kinds and boundary-containment discount"
```

---

## Task 1: swc spike — pin versions, prove parse + span→line

De-risk the one real unknown (swc API/version) before building on it. This is a throwaway-ish unit test that becomes the seed of `source.rs`.

**Files:**
- Modify: `Cargo.toml` (workspace `members`)
- Create: `crates/fxrank-lang-ts/Cargo.toml`
- Create: `crates/fxrank-lang-ts/src/lib.rs` (temporary spike test)

- [ ] **Step 1: Add the crate to the workspace.** In root `Cargo.toml`, add `"crates/fxrank-lang-ts"` to `members`.

- [ ] **Step 2: Create `crates/fxrank-lang-ts/Cargo.toml`** and pin the swc set with `cargo add` (they version together — do NOT guess versions):

```bash
# from repo root
cargo add --package fxrank-lang-ts swc_common swc_ecma_parser swc_ecma_ast swc_ecma_visit 2>/dev/null || true
```

Then hand-edit to match the house manifest style:

```toml
[package]
name = "fxrank-lang-ts"
edition.workspace = true
version.workspace = true
[dependencies]
fxrank-core = { path = "../fxrank-core" }
swc_common = "<pinned by cargo add>"
swc_ecma_parser = "<pinned by cargo add>"
swc_ecma_ast = "<pinned by cargo add>"
swc_ecma_visit = "<pinned by cargo add>"
[dev-dependencies]
insta = { version = "1", features = ["json"] }
serde_json = "1"
[lints]
workspace = true
```

> If `cargo add` resolves an incompatible mix, consult docs.rs / context7 for the latest mutually-compatible `swc_*` versions. They share a release train.

- [ ] **Step 3: Write the spike test** in `crates/fxrank-lang-ts/src/lib.rs`. It must prove three things: a TS string parses, we get a module AST, and a node's span resolves to a 1-based line.

```rust
#[cfg(test)]
mod spike {
    use swc_common::{BytePos, FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

    #[test]
    fn parses_ts_and_resolves_line() {
        let cm: Lrc<SourceMap> = Default::default();
        let src = "function f(): void {\n  fetch('x');\n}\n";
        let fm = cm.new_source_file(FileName::Custom("t.ts".into()).into(), src.into());
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax::default()),
            Default::default(),
            StringInput::from(&*fm),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let module = parser.parse_module().expect("parse");
        assert!(!module.body.is_empty());
        // span->line: the `fetch` call sits on line 2.
        let line = cm.lookup_char_pos(BytePos(src.find("fetch").unwrap() as u32 + fm.start_pos.0)).line;
        assert_eq!(line, 2);
    }
}
```

> The exact constructor signatures (`new_source_file`, `lookup_char_pos`, `start_pos`) are version-sensitive. If they differ, adjust to the resolved version's API — the *responsibility* (parse a module, map a `BytePos`→line via the `SourceMap`) is fixed.

- [ ] **Step 4: Run — iterate until it parses and asserts line 2**

Run: `cargo test -p fxrank-lang-ts spike`
Expected: PASS (fix API mismatches here, while the surface is tiny).

- [ ] **Step 5: Commit the pinned, proven baseline**

```bash
git add Cargo.toml Cargo.lock crates/fxrank-lang-ts/Cargo.toml crates/fxrank-lang-ts/src/lib.rs
git commit -m "feat(ts): scaffold fxrank-lang-ts crate; pin swc; prove parse + span->line"
```

---

## Task 2: `source.rs` — `Lang` → `Syntax`, and `SpanLines`

Turn the spike into reusable pieces: choose the swc `Syntax` from a language tag, and resolve any `Span` to a line.

**Files:**
- Create: `crates/fxrank-lang-ts/src/source.rs`
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`pub mod source;`, drop the spike)

- [ ] **Step 1: Write failing tests** in `source.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lang_from_extension() {
        assert_eq!(Lang::from_extension("ts"), Some(Lang::Ts));
        assert_eq!(Lang::from_extension("tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_extension("mjs"), Some(Lang::Js));
        assert_eq!(Lang::from_extension("rs"), None);
    }
    #[test]
    fn spanlines_resolves() {
        let (cm, fm) = test_file("a;\nb;\n");
        let lines = SpanLines::new(cm);
        // a span at the start of line 2:
        let pos = swc_common::BytePos(fm.start_pos.0 + 3);
        assert_eq!(lines.line(swc_common::Span::new(pos, pos)), 2);
    }
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p fxrank-lang-ts source`

- [ ] **Step 3: Implement `source.rs`**

```rust
//! Language selection and span→line resolution for the swc-based frontend.
use swc_common::{BytePos, FileName, SourceFile, SourceMap, Span, sync::Lrc};
use swc_ecma_parser::{EsSyntax, Syntax, TsSyntax};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang { Ts, Tsx, Js, Jsx }

impl Lang {
    /// Map a file extension (no dot) to a language. `.js`/`.mjs`/`.cjs`/`.jsx`
    /// all enable JSX (JSX-in-`.js` is common); `.ts` is TS without JSX; `.tsx` is TS+JSX.
    pub fn from_extension(ext: &str) -> Option<Lang> {
        match ext {
            "ts" => Some(Lang::Ts),
            "tsx" => Some(Lang::Tsx),
            "js" | "mjs" | "cjs" | "jsx" => Some(Lang::Js),
            _ => None,
        }
    }
    /// Parse a `--lang` flag value.
    pub fn from_flag(s: &str) -> Option<Lang> {
        match s { "ts" => Some(Lang::Ts), "tsx" => Some(Lang::Tsx),
                  "js" => Some(Lang::Js), "jsx" => Some(Lang::Jsx), _ => None }
    }
    pub fn syntax(self) -> Syntax {
        match self {
            Lang::Ts  => Syntax::Typescript(TsSyntax { tsx: false, ..Default::default() }),
            Lang::Tsx => Syntax::Typescript(TsSyntax { tsx: true,  ..Default::default() }),
            Lang::Js  => Syntax::Es(EsSyntax { jsx: true, ..Default::default() }),
            Lang::Jsx => Syntax::Es(EsSyntax { jsx: true, ..Default::default() }),
        }
    }
}

/// Resolves swc `Span`s to 1-based line numbers via the `SourceMap`.
pub struct SpanLines { cm: Lrc<SourceMap> }
impl SpanLines {
    pub fn new(cm: Lrc<SourceMap>) -> Self { SpanLines { cm } }
    pub fn line(&self, span: Span) -> usize { self.cm.lookup_char_pos(span.lo).line }
    pub fn line_of(&self, pos: BytePos) -> usize { self.cm.lookup_char_pos(pos).line }
}

#[cfg(test)]
fn test_file(src: &str) -> (Lrc<SourceMap>, Lrc<SourceFile>) {
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(FileName::Custom("t".into()).into(), src.into());
    (cm, fm)
}
```

- [ ] **Step 4: Run — expect PASS** (adjust `syntax()` field names to the pinned swc API if needed)

Run: `cargo test -p fxrank-lang-ts source`

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/source.rs crates/fxrank-lang-ts/src/lib.rs
git commit -m "feat(ts): Lang->Syntax selection and SpanLines span->line resolver"
```

---

## Task 3: `functions.rs` — collect `FnUnit`s

Collect every function form as a unit. Each `FnUnit` retains the body node (owned swc AST) and the symbol/id/line. Mirror `fxrank-lang-rust/src/functions.rs`.

**Files:**
- Create: `crates/fxrank-lang-ts/src/functions.rs`
- Test: `crates/fxrank-lang-ts/tests/ts_frontend.rs` (+ fixtures)

- [ ] **Step 1: Add the `analyze_fixture` test helper + a fixture.** Create `crates/fxrank-lang-ts/tests/fixtures/functions.ts`:

```ts
function topLevel(): void {}
const arrowConst = (): void => {};
class C { method(): void {} get g(): number { return 1; } }
export function exported(): void {}
[1].map(x => x);
```

In `tests/ts_frontend.rs`, a helper that parses a fixture and returns collected unit symbols (full `TsFrontend` wiring lands in Task 5; for now call `functions::collect` directly).

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn collects_all_function_forms() {
    let symbols = collect_symbols("functions.ts"); // helper: parse + functions::collect
    assert!(symbols.contains(&"topLevel".to_string()));
    assert!(symbols.contains(&"arrowConst".to_string()));
    assert!(symbols.contains(&"C.method".to_string()));
    assert!(symbols.contains(&"C.g".to_string()));
    assert!(symbols.contains(&"exported".to_string()));
    assert!(symbols.iter().any(|s| s.starts_with("<arrow@L"))); // the inline x => x
}
```

- [ ] **Step 3: Run — expect FAIL**

Run: `cargo test -p fxrank-lang-ts collects_all_function_forms`

- [ ] **Step 4: Implement `functions.rs`.** Define:

```rust
pub struct FnUnit {
    pub symbol: String,      // `topLevel`, `C.method`, `C.g`, `<arrow@L12>`
    pub id: String,          // `path:line:symbol`
    pub path: String,
    pub line: usize,
    pub is_async: bool,
    pub function: FnBody,    // owned swc node(s) for detectors to walk: body + signature params
}
```

Walk the parsed `Module` with a `swc_ecma_visit::Visit` collector. Emit a `FnUnit` for: `FnDecl`, `FnExpr`, `ArrowExpr`, class `ClassMethod` / `PrivateMethod` (incl. getters/setters via `MethodKind`), object methods, and arrows/fn-exprs bound to a `const`/`let`/`var` name (use the binding ident as the symbol; otherwise `<arrow@L{line}>`). Use `SpanLines` for `line`. `is_async` from the node's `is_async` flag. **Nested functions are their own units** — the visitor naturally recurses; do not roll child effects into the parent.

> `FnBody` holds what detectors need: the function/arrow body (`BlockStmt` or expression body) and the parameter list / signature for coverage + mutation seeding. Keep it owned (clone out of the AST) so the `SourceMap`/module can drop, exactly as `FnUnit` clones `syn::Block` today.

- [ ] **Step 5: Run — expect PASS**

Run: `cargo test -p fxrank-lang-ts collects_all_function_forms`

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-ts/src/functions.rs crates/fxrank-lang-ts/tests/
git commit -m "feat(ts): collect function units (decls, arrows, methods, getters)"
```

---

## Task 4: `TsFrontend` skeleton + end-to-end CLI (the walking skeleton)

Wire a minimal `TsFrontend` that parses → collects → emits **zero-effect** `Hotspot`s, and hook it into the CLI so `echo 'function f(){}' | fxrank scan --lang ts -` returns a valid report. **After this task the tool runs.**

**Files:**
- Modify: `crates/fxrank-lang-ts/src/lib.rs` (`TsFrontend`)
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs` (create — minimal `analyze_unit` returning a zero-effect `Hotspot`)
- Modify: `crates/fxrank-cli/Cargo.toml` (+ `ts` feature)
- Modify: `crates/fxrank-cli/src/main.rs` (`--lang`, extension discovery, dispatch)
- Modify: `crates/fxrank-core/src/frontend.rs` (add `Language::Ts`)

- [ ] **Step 1: Add `Language::Ts`** to the enum in `frontend.rs`.

- [ ] **Step 2: Minimal `analyze_unit`** in `detect/mod.rs`: build a `Hotspot` from a `FnUnit` with empty `effects`/`risk_features`, `own_score: 0.0`, `max_class: 0`, `confidence: 1.0`, `async_boundary: unit.is_async`, `await_count: 0`. (Real gather/fold arrive in Task 7.)

- [ ] **Step 3: Implement `TsFrontend`** in `lib.rs`, mirroring `RustFrontend::analyze`: it holds a `Lang` (and later `include_tests`), creates a `SourceMap` per `analyze` call, and for each `SourceFile` parses with the spike's lexer/parser using `lang.syntax()`. On parse error → `Diagnostic { parsed: false }`. On success → `functions::collect` → `analyze_unit` per unit → push to `output.functions`.

```rust
#[derive(Default)]
pub struct TsFrontend { pub lang: Lang, pub include_tests: bool }
// impl Default for Lang: Ts
```

- [ ] **Step 4: CLI wiring** in `main.rs`:
  - Add `#[arg(long)] lang: Option<String>` to `Cmd::Scan`.
  - Stdin (`path` is `None` **or** `-`): require `--lang`; map via `Lang::from_flag`; error "`--lang {ts,tsx,js,jsx}` required for stdin" if absent.
  - File: pick frontend by extension (`.rs` → Rust as today; `.ts/.tsx/.js/.jsx/.mjs/.cjs` → TS via `Lang::from_extension`).
  - Directory walk: collect both `.rs` and the JS/TS extensions; group sources by frontend and dispatch each group (generalize `collect_rs_files` to an extension set; keep symlink-skip logic verbatim).
  - Feature-gate `dispatch` for `ts` exactly like `rust` (the `#[cfg(feature = "ts")]` / `#[cfg(not)]` pair).

- [ ] **Step 5: CLI manifest** — in `crates/fxrank-cli/Cargo.toml` add:

```toml
fxrank-lang-ts = { path = "../fxrank-lang-ts", optional = true }
# under [features]
ts = ["dep:fxrank-lang-ts"]
# add "ts" to default:
default = ["rust", "ts"]
```

- [ ] **Step 6: Write the end-to-end integration test** in `tests/ts_frontend.rs` (using `assert_cmd` like the CLI tests):

```rust
#[test]
fn cli_scans_ts_fragment_from_stdin() {
    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("fxrank").unwrap();
    let out = cmd.args(["scan", "--lang", "ts", "-"])
        .write_stdin("function f(): void {}\n").assert().success();
    let json: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(json["scope"]["functions"], 1);
}
```

> `assert_cmd` is a `fxrank` (CLI crate) dev-dependency; put this CLI test under `crates/fxrank-cli/tests/` if cross-crate binary resolution is cleaner. Mirror the existing CLI integration tests' location.

- [ ] **Step 7: Run the whole thing**

```bash
cargo build -p fxrank
echo 'function f(): void {}' | cargo run -p fxrank -- scan --lang ts -
cargo test -p fxrank cli_scans_ts_fragment_from_stdin
```

Expected: a valid JSON report with `scope.functions == 1`, empty hotspots-of-substance.

- [ ] **Step 8: Commit the runnable skeleton**

```bash
git add -A
git commit -m "feat(ts): end-to-end skeleton — parse, collect, scan --lang ts from stdin"
```

---

## Task 5: `imports.rs` — ES import + require table

Mirror `fxrank-lang-rust/src/imports.rs`: resolve a local name to its module source, flag namespace/`require` uncertainty (the swc analog of glob).

**Files:**
- Create: `crates/fxrank-lang-ts/src/imports.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn resolves_named_default_namespace() {
    let t = table("import { readFile } from 'node:fs'; import fs from 'node:fs'; import * as os from 'node:os';");
    assert_eq!(t.resolve("readFile"), Some("node:fs"));
    assert_eq!(t.resolve("fs"), Some("node:fs"));
    assert_eq!(t.resolve("os"), Some("node:os"));
}
```

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-ts imports`

- [ ] **Step 3: Implement.** Walk `Module` items for `ImportDecl`: map each specifier (`Named`, `Default`, `Namespace`) local name → the import `src` string. Also scan for `const x = require('mod')` (a `VarDecl` whose init is a `require(...)` call) → map `x` → `'mod'`. Provide `resolve(&str) -> Option<&str>` and a `has_dynamic()` flag (set when a dynamic `import(expr)` / `require(expr)` with a non-literal arg appears — the confidence-penalty analog of `has_glob`).

- [ ] **Step 4: Run — expect PASS.** Run: `cargo test -p fxrank-lang-ts imports`

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/imports.rs
git commit -m "feat(ts): ES import + require resolution table"
```

---

## Task 6: `detect/calls.rs` — world effects (the high-signal detector)

This is the detector that makes the tool **useful for battle-testing**: IO/time/random/env/logging/throw. It also establishes the swc walker template for Tasks 9 and 11.

**Files:**
- Create: `crates/fxrank-lang-ts/src/detect/calls.rs`
- Test fixture: `crates/fxrank-lang-ts/tests/fixtures/calls.ts`

- [ ] **Step 1: Fixture** `calls.ts`:

```ts
async function io(): Promise<void> {
  await fetch('https://x');     // net.fs.db (7)
  console.log('hi');            // logging (4)
  const t = Date.now();         // time.read (5)
  const r = Math.random();      // random (5)
  const e = process.env.HOME;   // env.read (4)
  if (!e) throw new Error('x'); // panic (4)
}
```

- [ ] **Step 2: Write the failing test** (assert the set of `(kind, class)` pairs detected)

```rust
#[test]
fn detects_world_effects() {
    let kinds = effect_kinds("calls.ts", "io"); // helper: analyze unit, return wire kinds
    for k in ["net.fs.db","logging","time.read","random","env.read","panic"] {
        assert!(kinds.contains(&k.to_string()), "missing {k}");
    }
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-ts detects_world_effects`

- [ ] **Step 4: Implement the walker.** A `swc_ecma_visit::Visit` impl, `CallWalker { imports, lines, effects, .. }`, with a `push(kind, tier, line, evidence)` helper that builds an `Effect` exactly like `calls.rs` in the Rust frontend (class from `kind.base_class()`, `discounted_to: None`, weight from `weight_for_class`, confidence from `detection_confidence`). Detect:
  - `visit_call_expr`: a callee that is a member/ident — render it (e.g. `Date.now`, `Math.random`, `process.exit`, `crypto.randomUUID`) and resolve bare idents through `imports`; classify via a `classify_call` table (the spec's *World effects* table). `fetch` is a bare global ident; `console.*` is a member on `console`; `process.env.X` read is a member-of-member; node `fs`/`child_process` resolved via imports.
  - `visit_throw_stmt`: emit `panic` (class 4, `exact`).
  - Always call the default `visit_*` afterward (the syn rule: keep nested expressions visited; for the manual-recursion callee case, mirror `calls.rs`'s pattern).
  - Member-name signals with unknown receiver (`.query`, `.execute`, `.read_to_string` analog) → `heuristic` tier.

> Build the `classify_call` and `classify_member` tables from the spec's *World effects* table. Keep them as `match`/`if` ladders like `classify_path_call` — readable, not clever.
>
> **Bare globals must work without imports.** `fetch`, `Date`, `Math`, `console`, `process` are ambient globals — they are NOT in the import table. The resolver (mirror Rust's `resolve`) must return the rendered name unchanged on a lookup miss, so `fetch(...)` classifies even when a stdin fragment has no `import` context (the spec's "fragment degrades to heuristic, still detects" case). Test this with a fixture that has zero imports.

- [ ] **Step 5: Run — expect PASS.** Run: `cargo test -p fxrank-lang-ts detects_world_effects`

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-ts/src/detect/calls.rs crates/fxrank-lang-ts/tests/fixtures/calls.ts
git commit -m "feat(ts): world-effect call/member detection (fetch, console, Date, throw, ...)"
```

---

## Task 7: `analyze_unit` — gather + fold + async (real scores)

Replace the Task-4 stub. Wire `calls::detect` into a gather step and fold into a scored `Hotspot`, mirroring `fxrank-lang-rust/src/detect/mod.rs`. Coverage/mutation/risk are added by later tasks; leave clearly marked extension points.

**Files:**
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs`

- [ ] **Step 1: Write the failing test** — the `io` fixture function should now score (max_class 7 from `fetch`, async_boundary true).

```rust
#[test]
fn analyze_unit_scores_world_effects() {
    let h = analyze_fixture_unit("calls.ts", "io");
    assert_eq!(h.max_class, 7);
    assert!(h.own_score >= 21.0);
    assert!(h.async_boundary);
    assert!(h.await_count >= 1);
}
```

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-ts analyze_unit_scores_world_effects`

- [ ] **Step 3: Implement.** Mirror the Rust `analyze_unit`: `gather` runs `calls::detect` (and, after later tasks, `mutation::detect`); fold collects weights/classes/confidences, computes `max_class` / `own_score` / `function_confidence`, counts `await` (a `Visit` counter over the body, like `count_awaits`), sets `async_boundary = unit.is_async || await_count > 0`, pushes a `0.8` confidence entry when `await_count > 0` (same "unresolved awaited call" approximation as Rust). Leave `// TODO(Task 8): mutation`, `// TODO(Task 10): risk`, `// TODO(Task 9): boundary discount` markers at the gather/fold sites.

- [ ] **Step 4: Run — expect PASS.** Run: `cargo test -p fxrank-lang-ts analyze_unit_scores_world_effects`

- [ ] **Step 5: Battle-test checkpoint + commit**

```bash
cargo run -p fxrank -- scan crates/fxrank-lang-ts/tests/fixtures | jq '.hotspots[0]'
git add crates/fxrank-lang-ts/src/detect/mod.rs
git commit -m "feat(ts): analyze_unit folds world effects into scored hotspots"
```

---

## Task 8: `detect/mutation.rs` — mutation + escape analysis

Classify each write site as local / param / `this` / hidden (closure or imported) / global, and tag each emitted effect with a `contained` flag (only `local.mutation`, incl. constructor `this`-init, is contained). Mirror the binding-seeding approach of the Rust `mutation.rs`, adapted to JS scoping.

**Files:**
- Create: `crates/fxrank-lang-ts/src/detect/mutation.rs`
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs` (add to gather)
- Fixture: `crates/fxrank-lang-ts/tests/fixtures/mutation.ts`

- [ ] **Step 1: Fixture** `mutation.ts`:

```ts
function buildLocal(): number[] {
  const acc: number[] = [];
  acc.push(1);                 // local.mutation (contained)
  return acc;
}
function mutParam(xs: number[]): void { xs.push(1); }     // param.mutation (escaping)
let counter = 0;
function viaClosure(): void { counter += 1; }             // hidden.mutation (captured module binding)
class Box { v = 0; set(n: number): void { this.v = n; } } // this.mutation (escaping)
function viaGlobal(): void { (globalThis as any).z = 1; } // global.mutation
```

- [ ] **Step 2: Write the failing test** asserting `(kind, contained)` per function (use a helper exposing the raw effects + a per-effect `contained` flag, or assert via the resulting class after a known coverage in Task 9; here assert kinds).

```rust
#[test]
fn classifies_mutation_by_escape() {
    assert!(kinds("mutation.ts","buildLocal").contains(&"local.mutation".into()));
    assert!(kinds("mutation.ts","mutParam").contains(&"param.mutation".into()));
    assert!(kinds("mutation.ts","viaClosure").contains(&"hidden.mutation".into()));
    assert!(kinds("mutation.ts","set").contains(&"this.mutation".into()));
    assert!(kinds("mutation.ts","viaGlobal").contains(&"global.mutation".into()));
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-ts classifies_mutation_by_escape`

- [ ] **Step 4: Implement.** A `MutationWalker` that seeds:
  - **locals** = parameter binding idents + `const`/`let`/`var` idents declared *within* this function body (track as the visitor descends).
  - **captured** = idents used as write targets that are NOT locals and NOT module-globals → resolve against the enclosing scope: a binding from an outer function/module `let`/`var` → `hidden.mutation` (flagged `hidden: true`, `contained: false`). (Milestone A does not split enclosing-local vs module — both are `hidden`; recorded as a deferred refinement.)
  - **this-writes**: `this.x = …` in a **non-constructor** method → `this.mutation` (class 3, `contained: false`, not hidden). In a **constructor** → `local.mutation` (`contained: true`).
  - **global**: write to `globalThis`/`window`/an imported binding → `global.mutation` (class 6).
  - **local**: write/array-mutator (`push`/`splice`/`sort`/…)/`Map.set`/`Set.add`/`Object.assign`/`delete` on a **local** binding declared in this body → `local.mutation` (class 1, `contained: true`, `exact`).

  Write sites: `AssignExpr` (`=`, `+=`, …), `UpdateExpr` (`++`/`--`), member-mutator method calls (resolve receiver base ident like Rust's `base_ident`). Emit effects carrying a `contained: bool` (extend the detector's internal effect representation; `analyze_unit` consumes the flag in Task 9 — for now store it alongside, e.g. return `Vec<(Effect, bool)>` from this detector and have gather thread it).

> Constructors: a `ClassMethod` with `MethodKind::Method` named `constructor`, or swc's dedicated `Constructor` node — check the pinned API.
>
> **Highest-friction integration point — slow down here.** Rust's `gather` returns a plain `Vec<Effect>`; this detector must additionally carry a per-effect `contained: bool` so Task 9 can decide eligibility. Pick ONE representation up front (e.g. this detector returns `Vec<(Effect, bool)>` and `gather` threads the flag; or add a private `contained` field to a frontend-local effect wrapper) and use it consistently across `mutation::detect` → `gather` → `analyze_unit`. Do not infer containment from `EffectKind` in `analyze_unit` — the mutation detector is the single source of truth.

- [ ] **Step 5: Wire into gather** (mutation effects join calls effects), **run — expect PASS.**

Run: `cargo test -p fxrank-lang-ts classifies_mutation_by_escape`

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-ts/src/detect/mutation.rs crates/fxrank-lang-ts/src/detect/mod.rs crates/fxrank-lang-ts/tests/fixtures/mutation.ts
git commit -m "feat(ts): mutation detection with escape analysis (local/param/this/hidden/global)"
```

---

## Task 9: `coverage.rs` + apply the boundary discount in `analyze_unit`

Compute signature coverage + `any`-presence, then shift **contained** effects via `apply_boundary_discount`.

**Files:**
- Create: `crates/fxrank-lang-ts/src/coverage.rs`
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs`
- Fixture: `crates/fxrank-lang-ts/tests/fixtures/coverage.ts`

- [ ] **Step 1: Fixture** `coverage.ts`:

```ts
function fullyTyped(xs: number[]): number[] { const a: number[] = []; a.push(1); return a; } // c=1
function partlyTyped(xs: number[]) { const a: number[] = []; a.push(1); return a; }          // c=2/3? params typed, return not
function untyped(xs) { const a = []; a.push(1); return a; }                                   // c=0
function poisoned(xs: number[]): number[] { const a = xs as any; a.push(1); return a; }       // any in body → voided + risk
```

- [ ] **Step 2: Write failing tests.** Coverage counting:

```rust
#[test]
fn coverage_counts_typed_slots_and_any() {
    assert_eq!(coverage("coverage.ts","fullyTyped"), (BoundaryCoverage::Full, false));   // (coverage, has_any)
    assert_eq!(coverage("coverage.ts","partlyTyped"), (BoundaryCoverage::Partial, false));
    assert_eq!(coverage("coverage.ts","untyped"), (BoundaryCoverage::None, false));
    assert_eq!(coverage("coverage.ts","poisoned").1, true);
}
#[test]
fn boundary_discount_zeros_contained_local_mutation() {
    // fullyTyped: local.mutation (class 1, contained) → discounted_to 0
    let e = effects("coverage.ts","fullyTyped");
    let lm = e.iter().find(|e| e.kind == EffectKind::LocalMutation).unwrap();
    assert_eq!(lm.effective_class(), 0);
    // untyped: stays class 1
    let e2 = effects("coverage.ts","untyped");
    assert_eq!(e2.iter().find(|e| e.kind == EffectKind::LocalMutation).unwrap().effective_class(), 1);
    // poisoned: discount voided (stays 1) AND a type.escape risk is present
    let h = analyze_fixture_unit("coverage.ts","poisoned");
    assert!(h.risk_features.iter().any(|r| r.kind == RiskKind::TypeEscape));
    assert!(h.effects.iter().any(|e| e.kind == EffectKind::LocalMutation && e.effective_class() == 1));
}
```

- [ ] **Step 3: Run — expect FAIL.**

- [ ] **Step 4: Implement `coverage.rs`.** Given a function's params + return annotation:
  - `S` = params + (1 if the form has a return slot; constructors have none).
  - a param slot is **typed** iff it has an explicit `TsTypeAnn` whose top-level type is not the `any` keyword; the return slot is typed iff there is an explicit return `TsTypeAnn` not `any`.
  - `has_any` = any signature slot's top-level type is `any` **OR** the body contains an `any`-family token (`any` annotation, `as any`, `as unknown as`, `@ts-ignore`/`@ts-expect-error` comment — comments come from the swc comments side-table; if not threaded, restrict M-A body-check to `as any` / `: any` AST nodes and record the comment-directive case as deferred).
  - return `BoundaryCoverage`: `has_any` → `None` (the gate is voided); else `t==S` → `Full`, `t>0` → `Partial`, else `None`.
  Return `(BoundaryCoverage, has_any)` so `analyze_unit` can also emit the `type.escape` risk when `has_any`.

- [ ] **Step 5: Apply in `analyze_unit`.** After gather, compute `(coverage, has_any)`. For each `(effect, contained)`: set `effect.discounted_to = Some(apply_boundary_discount(effect.class, coverage, contained))` when `contained` and `coverage != None`, set `effect.discount = Some(format!("contained by typed boundary (coverage {t}/{s})"))` (match the spec's JSON example wording exactly — e.g. `"contained by fully-typed boundary (coverage 3/3)"` for the Full case; if Task 11's snapshot pins this string, keep them byte-identical), and `effect.sync_weight()`. When `has_any`, push a `type.escape` `RiskFeature`. Recompute `max_class`/`own_score` from `effective_class()`/synced weights.

- [ ] **Step 6: Run — expect PASS.**

- [ ] **Step 7: Commit**

```bash
git add crates/fxrank-lang-ts/src/coverage.rs crates/fxrank-lang-ts/src/detect/mod.rs crates/fxrank-lang-ts/tests/fixtures/coverage.ts
git commit -m "feat(ts): signature coverage + boundary-containment discount with any-poison"
```

---

## Task 10: `detect/risk.rs` — type.escape / dynamic.code / proto.pollution / html.injection

**Files:**
- Create: `crates/fxrank-lang-ts/src/detect/risk.rs`
- Modify: `crates/fxrank-lang-ts/src/detect/mod.rs`
- Fixture: `crates/fxrank-lang-ts/tests/fixtures/risk.ts`

- [ ] **Step 1: Fixture** `risk.ts`:

```ts
function dyn(s: string): unknown { return eval(s); }                       // dynamic.code (7)
function proto(o: object): void { Object.setPrototypeOf(o, null); }        // proto.pollution (4)
function html(el: HTMLElement, s: string): void { el.innerHTML = s; }      // html.injection (5)
function cast(x: unknown): number { return (x as any).n; }                 // type.escape (3)
```

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn detects_risks() {
    assert!(risks("risk.ts","dyn").contains(&"dynamic.code".into()));
    assert!(risks("risk.ts","proto").contains(&"proto.pollution".into()));
    assert!(risks("risk.ts","html").contains(&"html.injection".into()));
    assert!(risks("risk.ts","cast").contains(&"type.escape".into()));
}
```

- [ ] **Step 3: Run — expect FAIL.**

- [ ] **Step 4: Implement** a `Visit` walker returning `Vec<RiskFeature>` (path-carrying, like the Rust `risk.rs`): `eval`/`new Function`/`with` → `DynamicCode`; `Object.setPrototypeOf` and `__proto__` assignment → `ProtoPollution`; assignment to `.innerHTML`/`.outerHTML` and `.insertAdjacentHTML`/`document.write` calls → `HtmlInjection`; `as any` / `: any` / `as unknown as` / non-null `!` (`TsNonNull`) → `TypeEscape`. **Dedupe with Task 9:** `analyze_unit` already emits one `type.escape` when `has_any`; let `coverage.has_any` own the `as any`/`: any` signal and let this detector own `!` and the non-`any` risks, OR emit here and dedupe in `analyze_unit` by `(kind, line)`. Pick one and note it in a comment.

  > **Implemented dedup strategy (recorded post-implementation):** `coverage` owns the
  > `any`-family `type.escape` (from `as any`, `: any` in sig/body); `risk::detect` owns
  > the non-null `!` (`TsNonNull`) and the non-`any` dangers (`dynamic.code`,
  > `proto.pollution`, `html.injection`). The test fixture functions are `nonNull` (for
  > `!`) and `pureAsAny` (asserting `risk::detect` does NOT double-emit `type.escape` for
  > `as any` — that is `coverage`'s job), not the plan's original `cast`.

- [ ] **Step 5: Wire into `analyze_unit`'s risk assembly, run — expect PASS.**

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-ts/src/detect/risk.rs crates/fxrank-lang-ts/src/detect/mod.rs crates/fxrank-lang-ts/tests/fixtures/risk.ts
git commit -m "feat(ts): risk detection (dynamic.code, proto.pollution, html.injection, type.escape)"
```

---

## Task 11: core unit test for the latent gradient + insta snapshot + slim builds

Cover the latent Partial-vs-Full depth (no JS/TS fixture can), add a whole-report snapshot, and verify feature-gate hygiene.

**Files:**
- Modify: `crates/fxrank-core/src/score.rs` (the class-≥2 case is already in Task 0's test — verify it asserts Partial≠Full)
- Create: `crates/fxrank-lang-ts/tests/snapshots.rs` + `crates/fxrank-lang-ts/tests/fixtures/worked.ts`

- [ ] **Step 1: Confirm the core test from Task 0** asserts `apply_boundary_discount(3, Partial, true) == 2` and `(3, Full, true) == 1` (the latent gradient). If missing, add it.

- [ ] **Step 2: Add an insta snapshot test** over a small `worked.ts` mixing a world-effect function, a contained-mutation function (typed → score 0), and an `any`-poisoned function. Run `cargo insta review` to accept.

- [ ] **Step 3: Verify slim builds** (CI parity):

```bash
cargo build -p fxrank --no-default-features --features ts
cargo build -p fxrank --no-default-features --features rust
```

Both must compile.

- [ ] **Step 4: Full gates**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(ts): latent-gradient core test, insta snapshot, slim-build parity"
```

---

## Task 12: CI + dogfood + docs

**Files:**
- Modify: `.github/workflows/ci.yml` (add the `ts` slim build + a TS dogfood scan)
- Modify: `CLAUDE.md` (note the new `ts` frontend in *Workspace layout* + *Commands*)

- [ ] **Step 1: Add to CI:** `cargo build -p fxrank --no-default-features --features ts`, and a dogfood `cargo run -p fxrank -- scan <ts fixtures dir>` step. Mirror the existing Rust dogfood job.

- [ ] **Step 2: Update `CLAUDE.md`** *Workspace layout* (add `fxrank-lang-ts`) and *Commands* (add `--lang` / stdin fragment example, the `ts` slim build). Keep edits minimal and factual.

- [ ] **Step 3: Final gates + push**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A && git commit -m "ci+docs: gate ts slim build + dogfood; document ts frontend"
git push -u origin feat/ts-frontend
```

- [ ] **Step 4: Open the PR** linking spec 003.

```bash
gh pr create --title "feat: TypeScript/JavaScript frontend (spec 003)" --body "Implements docs/superpowers/specs/003-fxrank-typescript-frontend.md. Walking-skeleton-first; types lower the score via the boundary-containment discount."
```

---

## Notes for the implementer

- **swc API drift is the main risk.** Task 1 pins versions and proves parse + span→line; every later swc call builds on that proven surface. If a node/field name differs from this plan, trust the pinned API and adjust — the *responsibility* of each piece is fixed, the exact swc symbol is not.
- **DRY the walker boilerplate:** Tasks 6/8/10 share the `Visit` + `push` shape. Factor a tiny `push_effect`/`push_risk` helper if it reduces repetition, but don't over-abstract across detectors with different outputs.
- **`contained` is the load-bearing flag.** The discount is meaningless without it. Keep the mutation detector the single source of truth for whether a state effect escapes; never infer containment in `analyze_unit` from the kind alone (a future kind could break that).
- **Stay syntactic.** No `tsc`, no type resolution. Inferred types are invisible by design; only explicit annotations count toward coverage. Don't "improve" this — it's the spec's thesis.
- **Deferred (do NOT build):** JSDoc comment types, closure-capture local-vs-module tiering, full DOM catalog, call-graph propagation, return-slot weighting, scheduling effects, Rust `unsafe`-discount revisit. See spec 003 *Deferred / Future work*.
