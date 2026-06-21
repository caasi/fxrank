# Python Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `libcst`-based Python frontend (`fxrank-lang-python`, feature `python`) so `fxrank scan` profiles the own-body effect cost of `.py` functions, at parity with the Rust/TS frontends.

**Architecture:** A new workspace crate mirroring `fxrank-lang-ts`: `source` (parse + position recovery) → `functions` (collect `FnUnit`s) → `imports` (resolution table) → `coverage` (annotation slots) → `detect/{calls,mutation,risk}` walkers orchestrated by `detect::analyze_unit` (the single scoring owner) → `FrontendOutput`. **Parity-first: reuses existing `EffectKind`/`RiskKind` only — zero `fxrank-core` vocabulary changes** (`ThisMutation`/`DynamicCode`/`TypeEscape` already exist from spec 003).

**Tech Stack:** Rust, `libcst` 1.8.6 (`default-features = false` — pure Rust, no PyO3), `insta` snapshots, `assert_cmd` CLI tests.

## Global Constraints

- **Spec is source of truth:** `specs/006-fxrank-python-frontend.md`. When code and spec disagree, the spec wins.
- **No new core vocabulary.** Every signal maps to an existing `EffectKind`/`RiskKind`. `fxrank-core` is NOT modified. Never hand-write wire strings — use `kind.wire()`.
- **libcst dependency:** `libcst = { version = "1.8.6", default-features = false }`. The default `py` feature pulls PyO3 `extension-module` and breaks the binary — `default-features = false` is load-bearing.
- **Primarily syntactic:** no `mypy`/`pyright`. Type-dependent signals are `Tier::Heuristic` and take a confidence penalty.
- **CI gates (run before every push):** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and the slim builds `--features python` / `--features rust` / `--features ts` / no-features.
- **The libcst API names in this plan are from research and are UNVERIFIED.** Task 1 is a blocking spike that proves them against the pinned crate; if a name differs, adjust at that point. If positioning for **named** units or the pure-Rust build proves impossible, take the **tree-sitter-python off-ramp** (spec §"Parser off-ramp") *before* writing detectors. If only the **lambda** anchor is unworkable, the narrower fallback is to defer lambda units (spec §Review gate 3b). The spec's blocking review gate (Copilot re-confirms the libcst API) must clear before Task 6.
- **Deferred (do NOT implement — issues filed):** receiver-state gradient (#14), `StdioRead`/conditional-effect vocab (#20), frontend-owned `CorpusProfile` interface (#21). Python `--exclude` defaults are placed in the central CLI list as an interim step (#21).

---

## File structure

```
crates/fxrank-lang-python/
  Cargo.toml                       # libcst dep, fxrank-core dep, insta dev-dep
  src/
    lib.rs                         # PythonFrontend (impl core::Frontend) + spike test
    source.rs                      # parse + SpanIndex (byte→1-based char line:col) + lambda token anchor
    functions.rs                   # FnUnit collection (def/async def/method/nested def/lambda)
    imports.rs                     # import / from-import / import-as table
    coverage.rs                    # annotation slot coverage (None/Partial/Full) + Any/decorator analysis
    detect/
      mod.rs                       # analyze_unit: gather → fold (single scoring owner) + recursion driver
      calls.rs                     # world-effect call detection
      mutation.rs                  # global/nonlocal/self/param/local + escape (contained) analysis
      risk.rs                      # eval/exec/pickle/shell=True/Any → dynamic.code/type.escape
  tests/
    fixtures/*.py                  # analyze_fixture() reads these (subdir; not compiled as targets)
    python_frontend.rs             # assert_cmd end-to-end CLI test
```

Modified: root `Cargo.toml` (workspace member), `crates/fxrank-cli/Cargo.toml` (feature), `crates/fxrank-cli/src/main.rs` (`.py` routing, `--lang python`, dispatch, `--exclude` default string), `crates/fxrank-core/src/frontend.rs` (`Language::Python`).

## Test-harness conventions (define once in Task 3, reused by all later test modules)

Add these `#[cfg(test)]` helpers to the relevant unit-test modules as they are first needed (mirroring the Rust/TS frontends' shared `analyze_fixture`). Each is a thin view over the same pipeline — define the signature once and reuse:

- `fn parse_fixture(name: &str) -> (String, libcst_native::Module)` — reads `tests/fixtures/{name}.py`, returns owned source + parsed module. (The `Module` borrows the `String`, so both are returned and must outlive the borrow.)
- `fn scan_fixture_hotspots(name: &str) -> Vec<core::model::Hotspot>` — runs the full `PythonFrontend::analyze` on the fixture and returns `output.functions`. Used wherever a test asserts final `own_score`/`max_class`/`id`/`risk_features`.
- `fn analyze_fixture(name: &str) -> std::collections::HashMap<String, Vec<(EffectKind, u8)>>` — maps each unit symbol → its `(kind, class)` effect pairs (a pre-fold view for detector tests).
- `fn mutation_effects(name: &str) -> HashMap<String, Vec<(EffectKind, bool)>>` — symbol → `(kind, contained)` pairs (mutation-escape tests).
- `fn risk_features(name: &str) -> HashMap<String, Vec<String>>` — symbol → risk-kind wire strings.
- `fn analyze_files(paths: &[&str], include_tests: bool) -> core::FrontendOutput` — builds `SourceFile`s and runs `PythonFrontend { include_tests }` (test-skip tests).

Each helper is ~3–6 lines over `PythonFrontend`/`functions::collect`/`detect::analyze_unit`; introduce the first ones in Task 3, the scoring ones in Task 7, the mutation/risk ones in Tasks 8/10. Treat them as scaffolding folded into the task that first needs them (per the skill's right-sizing rule).

---

## Task 1: libcst spike — prove parse + pure-Rust build + BOTH position anchors

**This is the make-or-break task. Do not proceed to any detector work until every assertion here passes against the real pinned crate.** It de-risks the three unverified libcst facts the whole frontend rests on.

**Files:**
- Modify: root `Cargo.toml` (add member)
- Create: `crates/fxrank-lang-python/Cargo.toml`
- Create: `crates/fxrank-lang-python/src/lib.rs` (spike test only for now)

**Interfaces:**
- Produces: a proven, pinned libcst baseline; the confirmed API names (`parse_module`, `Module`, `tokenize`, the `Token` position accessors, `FunctionDef.name.value`, `Lambda`) that all later tasks consume.

- [ ] **Step 1: Create the feature branch.**

```bash
git -C /home/caasi/GitHub/fxrank checkout -b feat/006-python-frontend main
```

- [ ] **Step 2: Add the crate to the workspace.** In root `Cargo.toml`, add `"crates/fxrank-lang-python"` to `members`.

- [ ] **Step 3: Create `crates/fxrank-lang-python/Cargo.toml`.** Pin libcst with `default-features = false` (NOT a guessed feature set):

```toml
[package]
name = "fxrank-lang-python"
description = "Python (libcst-based) frontend for FxRank's effect-cost profiler."
edition.workspace = true
version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
rust-version.workspace = true
keywords.workspace = true
categories.workspace = true

[dependencies]
fxrank-core = { path = "../fxrank-core", version = "0.1.1" }
libcst = { version = "1.8.6", default-features = false }

[dev-dependencies]
insta = { version = "1", features = ["json"] }
serde_json = "1"

[lints]
workspace = true
```

- [ ] **Step 4: Write the spike test** in `crates/fxrank-lang-python/src/lib.rs`. It must prove FOUR things: (a) the crate builds with no PyO3/C linkage, (b) Python source parses to a `Module`, (c) a **named** function's `name.value` is a subslice of the source so pointer arithmetic yields its byte offset, (d) `tokenize()` exposes per-token line/col and a `lambda` keyword token.

```rust
//! Python frontend (spike phase). The libcst API names below are confirmed here.
#[cfg(test)]
mod spike {
    // NOTE: the crate's lib name is `libcst_native` even though the Cargo dep is `libcst`.
    use libcst_native::{parse_module, tokenize};

    const SRC: &str = "def greet(name):\n    return name\n\nf = lambda x: x + 1\n";

    #[test]
    fn parses_and_anchors_named_and_lambda() {
        // (b) parse
        let module = parse_module(SRC, None).expect("python parses");

        // (c) named-unit pointer-trick: find the FunctionDef, prove name.value borrows SRC.
        // Walk module.body for the first FunctionDef; read its `name.value` (&str).
        // Confirm pointer arithmetic lands on byte offset 4 (`greet` starts at col 5, line 1).
        let name = first_funcdef_name(&module); // helper below, adjust to real node enums
        let off = name.as_ptr() as usize - SRC.as_ptr() as usize;
        assert_eq!(off, 4, "name.value must be a borrowed subslice of SRC");
        assert_eq!(&SRC[off..off + name.len()], "greet");

        // (d) tokenize: every Token has line/col; a `lambda` keyword token exists.
        let tokens = tokenize(SRC).expect("tokenizes");
        let lambda_tok = tokens
            .iter()
            .find(|t| t.string == "lambda")
            .expect("a lambda keyword token");
        // line_number() is 1-based; char_column_number() is 0-based.
        assert_eq!(lambda_tok.start_pos.line_number(), 4);
        assert_eq!(lambda_tok.start_pos.char_column_number() + 1, 5); // 1-based col of `lambda`
    }

    // Helper: descend module.body to the first FunctionDef and return its name &str.
    // The exact enum path (Statement::Compound(CompoundStatement::FunctionDef) etc.)
    // is what this spike CONFIRMS — adjust to the real pinned API.
    fn first_funcdef_name(module: &libcst_native::Module) -> &str {
        for stmt in &module.body {
            if let Some(name) = funcdef_name_of(stmt) {
                return name;
            }
        }
        panic!("no FunctionDef found");
    }
    fn funcdef_name_of(_stmt: &libcst_native::Statement) -> Option<&str> {
        // Fill in the real match against Statement::Compound(...FunctionDef { name, .. })
        // returning name.value. This is the API-shape proof.
        unimplemented!("confirm node enum path during the spike")
    }
}
```

- [ ] **Step 5: Run, iterating against the real API until green.** Run: `cargo test -p fxrank-lang-python spike -- --nocapture`. Expected: PASS. While iterating, also confirm the pure-Rust build: `cargo build -p fxrank-lang-python --no-default-features` must succeed with **no** C/Python linker step.

- [ ] **Step 6: BLOCKING — clear the spec review gate.** Confirm with the Copilot pass (spec §Review gate) that (1) the crate/version is right, (2) the pure-Rust `default-features = false` build is clean, (3a) the named-unit pointer-trick holds, (3b) the lambda token ordinal anchor is obtainable. If 3a or the build fails → tree-sitter off-ramp. If only 3b fails → defer lambdas. **Do not start Task 6 until this clears.**

- [ ] **Step 7: Commit the proven baseline.**

```bash
git add Cargo.toml Cargo.lock crates/fxrank-lang-python/
git commit -m "feat(python): libcst spike — pin 1.8.6 (pure-Rust), prove parse + name/lambda anchors"
```

---

## Task 2: `source.rs` — parse wrapper + `SpanIndex` + anchors

**Files:**
- Create: `crates/fxrank-lang-python/src/source.rs`
- Modify: `crates/fxrank-lang-python/src/lib.rs` (add `pub mod source;`)

**Interfaces:**
- Produces:
  - `struct SpanIndex` with `fn new(src: &str) -> Self` and `fn line_col(&self, byte_off: usize) -> (u32, u32)` (1-based line, 1-based **char** col).
  - `fn anchor_of_subslice(src: &str, sub: &str) -> usize` — byte offset of a borrowed subslice via pointer arithmetic.
  - `fn lambda_anchors(src: &str) -> Vec<(u32, u32)>` — the (line, 1-based char col) of each `lambda` keyword token, in source order (the k-th entry anchors the k-th `Lambda` node in pre-order).

- [ ] **Step 1: Write failing tests** in `source.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_chars_not_bytes() {
        let src = "x = 'é'\ndef f():\n    pass\n"; // 'é' is 2 bytes, 1 char
        let idx = SpanIndex::new(src);
        let byte_off = src.find("def").unwrap();
        assert_eq!(idx.line_col(byte_off), (2, 1)); // line 2, char col 1
    }

    #[test]
    fn anchor_of_subslice_is_exact() {
        let src = "def greet():\n    pass\n";
        let name = &src[4..9]; // "greet"
        assert_eq!(anchor_of_subslice(src, name), 4);
    }

    #[test]
    fn lambda_anchors_in_source_order() {
        let src = "a = lambda: 1\nb = lambda y: y\n";
        let anchors = lambda_anchors(src);
        assert_eq!(anchors, vec![(1, 5), (2, 5)]); // both `lambda` at char col 5
    }
}
```

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python source`. Expected: FAIL (undefined items).

- [ ] **Step 3: Implement `source.rs`.**

```rust
use libcst_native::tokenize;

/// Precomputed line-start byte offsets for O(log n) byte→(line, char-col) mapping.
pub struct SpanIndex<'a> {
    src: &'a str,
    line_starts: Vec<usize>, // byte offset of the start of each line (line 1 = index 0)
}

impl<'a> SpanIndex<'a> {
    pub fn new(src: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SpanIndex { src, line_starts }
    }

    /// 1-based line, 1-based **char** column for a byte offset.
    pub fn line_col(&self, byte_off: usize) -> (u32, u32) {
        let line_idx = match self.line_starts.binary_search(&byte_off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx];
        let col_chars = self.src[line_start..byte_off].chars().count();
        ((line_idx + 1) as u32, (col_chars + 1) as u32)
    }
}

/// Byte offset of a `&str` that is a subslice of `src` (pointer arithmetic).
pub fn anchor_of_subslice(src: &str, sub: &str) -> usize {
    sub.as_ptr() as usize - src.as_ptr() as usize
}

/// (line, 1-based char col) of each `lambda` keyword token, in source order.
pub fn lambda_anchors(src: &str) -> Vec<(u32, u32)> {
    tokenize(src)
        .map(|toks| {
            toks.iter()
                .filter(|t| t.string == "lambda")
                .map(|t| (t.start_pos.line_number() as u32, (t.start_pos.char_column_number() + 1) as u32))
                .collect()
        })
        .unwrap_or_default()
}
```

- [ ] **Step 4: Run — expect PASS** (adjust `Token`/`Pos` accessor names to the spike-confirmed API). Run: `cargo test -p fxrank-lang-python source`.

- [ ] **Step 5: Commit.**

```bash
git add crates/fxrank-lang-python/src/
git commit -m "feat(python): source.rs — SpanIndex (byte→char line:col) + subslice/lambda anchors"
```

---

## Task 3: `functions.rs` — collect `FnUnit`s (named + lambda)

**Files:**
- Create: `crates/fxrank-lang-python/src/functions.rs`
- Modify: `crates/fxrank-lang-python/src/lib.rs` (`pub mod functions;`)

**Interfaces:**
- Consumes: `source::{SpanIndex, anchor_of_subslice, lambda_anchors}`.
- Produces:
  - `struct FnUnit<'a> { symbol: String, line: u32, col: u32, is_async: bool, decorators: Vec<&'a Expression>, params: &'a Parameters, body: FnBody<'a> }` (exact field types per the spike API).
  - `fn collect<'a>(module: &'a Module, src: &'a str) -> Vec<FnUnit<'a>>` — every `def`/`async def`/method/nested `def` (named, anchored via `name.value`) and every `lambda` (anchored via the k-th `lambda_anchors` entry, symbol `<lambda@L{line}C{col}>`). Nested `def`/`lambda` are their **own** units.

- [ ] **Step 1: Add the `analyze_fixture` helper + a fixture.** Create `crates/fxrank-lang-python/tests/fixtures/functions.py`:

```python
def top():
    return 1

class C:
    def method(self):
        pass

async def fetcher():
    pass

g = lambda x: x * 2
h = lambda: 0
```

- [ ] **Step 2: Write the failing test** in `functions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collects_all_named_and_lambda_units() {
        let src = std::fs::read_to_string("tests/fixtures/functions.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let units = collect(&module, &src);
        let symbols: Vec<&str> = units.iter().map(|u| u.symbol.as_str()).collect();
        assert!(symbols.contains(&"top"));
        assert!(symbols.contains(&"method"));
        assert!(symbols.contains(&"fetcher"));
        assert!(units.iter().any(|u| u.symbol.starts_with("<lambda@L")));
        assert!(units.iter().find(|u| u.symbol == "fetcher").unwrap().is_async);
        // two lambdas, distinct anchors
        let lambdas: Vec<&str> = symbols.iter().filter(|s| s.starts_with("<lambda@L")).cloned().collect();
        assert_eq!(lambdas.len(), 2);
        assert_ne!(lambdas[0], lambdas[1]);
    }
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python functions`.

- [ ] **Step 4: Implement `functions.rs`.** Recursively walk `module.body`. For each `FunctionDef` (and async variant): symbol = `name.value`; byte offset = `anchor_of_subslice(src, name.value)`; `(line, col) = SpanIndex::new(src).line_col(off)`. **Recurse into the body** to collect nested `def`s/lambdas as their own units (each gets its own anchor). For `Lambda` nodes: collect them in pre-order; zip with `lambda_anchors(src)` by index (the k-th `Lambda` ↔ k-th `lambda` token), symbol `format!("<lambda@L{line}C{col}>", ...)`. Build `SpanIndex` once per file and pass it down (don't rebuild per unit).

- [ ] **Step 5: Run — expect PASS.** Run: `cargo test -p fxrank-lang-python functions`.

- [ ] **Step 6: Commit.**

```bash
git add crates/fxrank-lang-python/
git commit -m "feat(python): functions.rs — collect named + lambda FnUnits with anchors"
```

---

## Task 4: `PythonFrontend` skeleton + end-to-end CLI (walking skeleton)

**Files:**
- Modify: `crates/fxrank-core/src/frontend.rs` (add `Language::Python`)
- Create: `crates/fxrank-lang-python/src/detect/mod.rs` (minimal `analyze_unit`)
- Modify: `crates/fxrank-lang-python/src/lib.rs` (`PythonFrontend`)
- Modify: `crates/fxrank-cli/Cargo.toml`, `crates/fxrank-cli/src/main.rs`
- Create: `crates/fxrank-lang-python/tests/python_frontend.rs`

**Interfaces:**
- Consumes: `functions::collect`, `core::{Frontend, FrontendOutput, SourceFile, Language}`, `core::model::Hotspot`.
- Produces: `struct PythonFrontend { include_tests: bool }` implementing `Frontend`; `fn analyze_unit(unit: &FnUnit, ...) -> Hotspot` (minimal: empty effects).

- [ ] **Step 1: Add `Language::Python`** to the enum in `crates/fxrank-core/src/frontend.rs` (this is the ONLY core change — adding an enum variant, not vocabulary). Run `cargo test -p fxrank-core` to confirm green.

- [ ] **Step 2: Minimal `analyze_unit`** in `detect/mod.rs`: build a `Hotspot` from a `FnUnit` with empty `effects`/`risk_features`, `own_score: 0.0`, `max_class: 0`, `confidence: 1.0`, `async_boundary: unit.is_async`, `await_count: 0`. Construct the `id` as `format!("{path}:{line}:{col}:{symbol}")`. (Real gather/fold arrive in Task 7.)

- [ ] **Step 3: Implement `PythonFrontend`** in `lib.rs`, mirroring `TsFrontend::analyze`: for each `SourceFile`, `parse_module(&file.text, None)`; on `Err` push `Diagnostic { parsed: false, .. }`; on `Ok` run `functions::collect` → `analyze_unit` per unit → push to `output.functions`. `language()` returns `Language::Python`.

- [ ] **Step 4: CLI wiring** in `crates/fxrank-cli/src/main.rs`:
  - extension routing: `"py" => Some(Route::Python)`;
  - stdin `--lang`: `"python" => Route::Python` (single value; unknown value error message lists `python` alongside the TS dialects);
  - `.pyi` is NOT routed (excluded — type-only);
  - a feature-gated `run_python(...)` dispatch mirroring `run_ts`, behind `#[cfg(feature = "python")]` with a `#[cfg(not(feature = "python"))]` "no frontend" diagnostic.

- [ ] **Step 5: CLI manifest** — in `crates/fxrank-cli/Cargo.toml`:

```toml
# under [dependencies]
fxrank-lang-python = { path = "../fxrank-lang-python", version = "0.1.1", optional = true }
# under [features]
python = ["dep:fxrank-lang-python"]
# add "python" to default:
default = ["rust", "ts", "python"]
```

- [ ] **Step 6: Write the end-to-end integration test** in `crates/fxrank-lang-python/tests/python_frontend.rs` (using `assert_cmd`):

```rust
use assert_cmd::Command;

#[test]
fn scans_python_stdin_fragment() {
    let mut cmd = Command::cargo_bin("fxrank").unwrap();
    cmd.args(["scan", "--lang", "python", "-"])
        .write_stdin("def f():\n    pass\n")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"language\""));
}
```

- [ ] **Step 7: Run the whole thing.** Run: `cargo test -p fxrank-lang-python && cargo run -p fxrank -- scan --lang python - <<< 'def f(): pass'`. Expected: valid JSON with a `f` hotspot at `own_score 0.0`.

- [ ] **Step 8: Commit the runnable skeleton.**

```bash
git add crates/ Cargo.toml
git commit -m "feat(python): walking skeleton — PythonFrontend + .py/--lang python CLI wiring"
```

---

## Task 5: `imports.rs` — import / from-import / import-as table

**Files:**
- Create: `crates/fxrank-lang-python/src/imports.rs`
- Modify: `lib.rs` (`pub mod imports;`)

**Interfaces:**
- Produces: `struct Imports` with `fn build(module: &Module) -> Self`, `fn resolve(&self, local: &str) -> Option<&str>` (local name → module path), and `fn has_dynamic(&self) -> bool` (set on `importlib`/`__import__` presence — the confidence-penalty analog).

- [ ] **Step 1: Write failing tests** in `imports.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn build_str(src: &str) -> Imports {
        Imports::build(&libcst_native::parse_module(src, None).unwrap())
    }
    #[test]
    fn resolves_import_forms() {
        let i = build_str("import os\nimport numpy as np\nfrom subprocess import run\n");
        assert_eq!(i.resolve("os"), Some("os"));
        assert_eq!(i.resolve("np"), Some("numpy"));
        assert_eq!(i.resolve("run"), Some("subprocess.run"));
    }
}
```

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python imports`.

- [ ] **Step 3: Implement.** Walk `module.body` for `Import` (`import a.b as c` → local `c` → `"a.b"`; `import os` → `os` → `"os"`) and `ImportFrom` (`from m import n` → local `n` → `"m.n"`; `from m import n as p` → `p` → `"m.n"`). Set `has_dynamic` when `importlib` or `__import__` is imported/used.

- [ ] **Step 4: Run — expect PASS.** Run: `cargo test -p fxrank-lang-python imports`.

- [ ] **Step 5: Commit.**

```bash
git add crates/fxrank-lang-python/
git commit -m "feat(python): imports.rs — import/from/as resolution table"
```

---

## Task 6: `detect/mod.rs` recursion driver + `detect/calls.rs` (world effects)

**BLOCKING:** the Task-1 review gate must have cleared.

**Files:**
- Modify: `crates/fxrank-lang-python/src/detect/mod.rs` (the recursion driver)
- Create: `crates/fxrank-lang-python/src/detect/calls.rs`
- Create: `crates/fxrank-lang-python/tests/fixtures/calls.py`

**Interfaces:**
- Consumes: `imports::Imports`, `source::SpanIndex`, `core::effect::{Effect, EffectKind, Tier}`, `core::score::weight_for_class`.
- Produces:
  - In `detect/mod.rs`: `fn walk_own_body(node, sink)` — the recursion driver that **descends into** the body, f-string format exprs, eager comprehension element+iterable, `with`-items, decorator + parameter-default exprs; and **does NOT descend into** nested `def`/`lambda` bodies or a lazy generator-expression element body.
  - `calls::detect(body, imports, span) -> Vec<Effect>`.

- [ ] **Step 1: Fixture** `calls.py`:

```python
import os, subprocess, requests, logging, random, time

def io_boundary(path):
    data = open(path).read()
    logging.info("read")
    return requests.get("http://x").text

def env_and_rng():
    subprocess.run(["ls"], shell=True)
    return os.getenv("X"), random.random(), time.time()

def reads_stdin():
    return input("name? ")

def db_write(session):
    session.commit()
```

- [ ] **Step 2: Write the failing test** (assert the set of `(kind, class)` pairs):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::effect::EffectKind::*;
    // analyze_fixture returns Vec<(symbol, Vec<(EffectKind, u8)>)>; helper in the test module.
    #[test]
    fn detects_world_effects() {
        let by_fn = analyze_fixture("calls"); // see helper
        let io: Vec<_> = by_fn["io_boundary"].clone();
        assert!(io.contains(&(NetFsDb, 7)));   // open + requests.get
        assert!(io.contains(&(Logging, 4)));   // logging.info
        let env = &by_fn["env_and_rng"];
        assert!(env.contains(&(ProcessControl, 6))); // subprocess.run
        assert!(env.contains(&(EnvRead, 4)));        // os.getenv
        assert!(env.contains(&(Random, 5)) && env.contains(&(TimeRead, 5)));
        assert!(by_fn["reads_stdin"].contains(&(EnvRead, 4)));      // input()
        assert!(by_fn["db_write"].contains(&(NetFsDb, 7)));        // session.commit() heuristic
    }
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python detects_world_effects`.

- [ ] **Step 4: Implement the recursion driver + the call detector.** The driver (`detect/mod.rs`) centralizes own-body traversal per the spec's attribution rules (descend into eager wrappers + decorators/defaults; stop at nested `def`/`lambda` bodies and lazy genexp element bodies). `calls::detect` classifies, using `imports.resolve`:
  - `open`, `pathlib.*.read_text/write_text`, `os` fs, `shutil`, `tempfile`, `json.load/dump`, `csv`, `pandas.read_csv`/`read_excel`; `requests`/`httpx`/`urllib`/`socket`/`aiohttp`; `sqlite3`/SQLAlchemy → `NetFsDb`(7). Method-name heuristics (`.commit()`/`.save()`/`.objects.create()`/`cursor.execute()`/`.to_sql()`/`.to_csv()`) → `NetFsDb`(7) `Tier::Heuristic`.
  - `subprocess.*`/`os.system`/`sys.exit` → `ProcessControl`(6) `Path`.
  - `os.environ[...] =`/`os.putenv`/`dotenv.load_dotenv()` → `EnvWrite`(6).
  - `threading`/`multiprocessing`/`asyncio` primitives → `Concurrency`(6).
  - `time.*`/`datetime.now` → `TimeRead`(5); `random.*`/`secrets.*` → `Random`(5).
  - `os.getenv`/`os.environ.get` → `EnvRead`(4) `Heuristic`; **`input()`** → `EnvRead`(4) `Exact`, evidence `"input() — interactive stdin read"`.
  - `logging.*`/`print()` → `Logging`(4); `sys.argv` → `AmbientRead`(2).
  - bare `assert`/`raise` → `Panic`(4) `Exact`; `assert` evidence appends `" — stripped under -O"`.
  Each effect: `class = kind.base_class()`, `discounted_to: None`, `weight = weight_for_class(class)`, line via `span.line_col(anchor_of_subslice(src, <node-name &str>))`.

- [ ] **Step 5: Run — expect PASS.** Run: `cargo test -p fxrank-lang-python detects_world_effects`.

- [ ] **Step 6: Commit.**

```bash
git add crates/fxrank-lang-python/
git commit -m "feat(python): detect/calls.rs + own-body recursion driver — world effects"
```

---

## Task 7: `analyze_unit` — gather + fold + async (real scores)

**Files:**
- Modify: `crates/fxrank-lang-python/src/detect/mod.rs`

**Interfaces:**
- Consumes: `calls::detect`, `core::score::{own_score, max_class, rank_key}`, `core::confidence`.
- Produces: a real `analyze_unit` computing `own_score`, `max_class`, function `confidence` (weakest-link min), `async_boundary`, `await_count`.

- [ ] **Step 1: Write the failing test** — `io_boundary` should now score `max_class 7`, `own_score == 24.0` for the `open`+`requests`(21) + `logging`(5) + (if any local) combination per the fixture; `fetcher` async sets `async_boundary`.

```rust
#[test]
fn analyze_unit_scores_world_effects() {
    let h = scan_fixture_hotspots("calls"); // returns Vec<Hotspot>
    let io = h.iter().find(|x| x.id.ends_with(":io_boundary")).unwrap();
    assert_eq!(io.max_class, 7);
    assert!(io.own_score >= 21.0);
}
```

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python analyze_unit_scores_world_effects`.

- [ ] **Step 3: Implement.** Mirror the Rust/TS `analyze_unit`: `gather` runs `calls::detect` over the own-body (via the driver) (and, after later tasks, `mutation::detect` / `risk::detect`); `fold` collects weights/classes/confidences → `own_score(&weights)`, `max_class(&classes, risk_class)`, function confidence = min of per-effect confidences; `await_count` via a driver counter over the body; `async_boundary = unit.is_async || await_count > 0`. Leave `// TODO(Task 8): mutation`, `// TODO(Task 9): boundary discount`, `// TODO(Task 10): risk` markers.

- [ ] **Step 4: Run — expect PASS.** Run: `cargo test -p fxrank-lang-python analyze_unit_scores_world_effects`.

- [ ] **Step 5: Battle-test checkpoint + commit.** Dogfood: `cargo run -p fxrank -- scan crates/fxrank-lang-python/tests/fixtures/ --lang python` (single files) — confirm `io_boundary` surfaces. Commit:

```bash
git commit -am "feat(python): analyze_unit — gather/fold/async real scoring"
```

---

## Task 8: `detect/mutation.rs` — mutation + escape (contained) analysis

**Files:**
- Create: `crates/fxrank-lang-python/src/detect/mutation.rs`
- Create: `crates/fxrank-lang-python/tests/fixtures/mutation.py`

**Interfaces:**
- Produces: `mutation::detect(unit, span) -> Vec<(Effect, bool)>` where the `bool` is the per-effect `contained` flag (true only for `LocalMutation` on a locally-created binding).

- [ ] **Step 1: Fixture** `mutation.py`:

```python
g = 0

def uses_global():
    global g
    g = 1

class Counter:
    def __init__(self):
        self.n = 0          # __init__ → local.mutation (contained)
    def bump(self):
        self.n += 1         # this.mutation (escaping)

def mutates_param(items):
    items.append(1)         # param.mutation (escaping)

def builds_local():
    acc = []
    acc.append(1)           # local.mutation (contained)
    return acc
```

- [ ] **Step 2: Write the failing test** asserting `(kind, contained)` per function:

```rust
#[test]
fn classifies_mutation_by_escape() {
    let m = mutation_effects("mutation"); // Vec<(symbol, Vec<(EffectKind, bool)>)>
    assert!(m["uses_global"].contains(&(GlobalMutation, false)));
    assert!(m["bump"].contains(&(ThisMutation, false)));
    assert!(m["mutates_param"].contains(&(ParamMutation, false)));
    assert!(m["builds_local"].contains(&(LocalMutation, true)));
    // __init__ self.n = 0 is local.mutation (contained), NOT this.mutation
    assert!(m["__init__"].contains(&(LocalMutation, true)));
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python classifies_mutation_by_escape`.

- [ ] **Step 4: Implement.** Track parameter names + locally-assigned names per unit. Classify write targets:
  - `global x` declared then assigned → `GlobalMutation`(6), contained=false.
  - `nonlocal x` write, and `self.attr =` / `self.attr.x =` in a **non-`__init__`** method → `ThisMutation`(3), contained=false. (evidence carries `"nonlocal x"` / `"self.x = … (instance state)"`.)
  - `self.attr =` inside `__init__` → `LocalMutation`(1), contained=**true**.
  - mutation of a **parameter** (attr/item/`.append()` etc. where the base is a param) → `ParamMutation`(3), contained=false.
  - mutation of a **local** binding (`.append()`/`d[k]=`/`.add()`/`+=`) → `LocalMutation`(1), contained=true.

- [ ] **Step 5: Wire into gather** (mutation effects join calls effects), **run — expect PASS.** Run: `cargo test -p fxrank-lang-python classifies_mutation_by_escape`.

- [ ] **Step 6: Commit.**

```bash
git commit -am "feat(python): detect/mutation.rs — global/nonlocal/self/param/local + escape analysis"
```

---

## Task 9: `coverage.rs` + apply the boundary discount (incl. `Any` poison, decorator confidence)

**Files:**
- Create: `crates/fxrank-lang-python/src/coverage.rs`
- Modify: `crates/fxrank-lang-python/src/detect/mod.rs` (apply the discount in `analyze_unit`)
- Create: `crates/fxrank-lang-python/tests/fixtures/coverage.py`

**Interfaces:**
- Consumes: `core::score::{BoundaryCoverage, apply_boundary_discount}`.
- Produces: `coverage::of(unit) -> Coverage` where `Coverage { boundary: BoundaryCoverage, any_in_body: bool, unknown_decorator: bool }`.

- [ ] **Step 1: Fixture** `coverage.py`:

```python
from typing import Any, cast

def fully_typed(x: int) -> list:
    acc = []
    acc.append(x)        # local.mutation, contained → discount to 0 under Full
    return acc

def has_any(x: Any) -> list:   # Any slot → can't reach Full + type.escape
    return []

def body_any(x: int) -> list:
    y = cast(Any, x)     # body Any → voids discount + type.escape
    acc = []
    acc.append(y)
    return acc
```

- [ ] **Step 2: Write failing tests:**

```rust
#[test]
fn boundary_discount_zeros_contained_local_when_typed() {
    let h = scan_fixture_hotspots("coverage");
    let ft = h.iter().find(|x| x.id.ends_with(":fully_typed")).unwrap();
    assert_eq!(ft.own_score, 0.0); // local.mutation class 1 → 0 under Full coverage
}

#[test]
fn any_emits_type_escape_and_blocks_discount() {
    let h = scan_fixture_hotspots("coverage");
    let ba = h.iter().find(|x| x.id.ends_with(":body_any")).unwrap();
    assert!(ba.risk_features.iter().any(|r| r.kind == "type.escape"));
    assert!(ba.own_score >= 1.0); // discount voided → local.mutation stays class 1
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python boundary`.

- [ ] **Step 4: Implement `coverage::of`.** Slots = each parameter (excluding `self`/`cls`) + return; `*args`/`**kwargs` each one slot; a slot is typed iff it has an explicit annotation whose top-level type is not `Any`. → `None`/`Partial`/`Full`. Set `any_in_body` on a `cast(Any,...)` / `Any`-annotated local. Set `unknown_decorator` when a decorator is outside the pure allowlist (`property`, `staticmethod`, `classmethod`, `dataclass`, `functools.wraps`, framework route decorators). In `analyze_unit`: for each effect with `contained == true`, `effect.discounted_to = Some(apply_boundary_discount(class, coverage_or_None_if_any_in_body, true))` and recompute weight; emit a `type.escape`(3) risk when `Any` present (signature slot OR body); subtract a confidence step when `unknown_decorator`.

- [ ] **Step 5: Run — expect PASS.** Run: `cargo test -p fxrank-lang-python boundary`.

- [ ] **Step 6: Commit.**

```bash
git commit -am "feat(python): coverage.rs + boundary discount (Any poison + type.escape, decorator confidence)"
```

---

## Task 10: `detect/risk.rs` — dynamic code + shell=True + Any

**Files:**
- Create: `crates/fxrank-lang-python/src/detect/risk.rs`
- Create: `crates/fxrank-lang-python/tests/fixtures/risk.py`

**Interfaces:**
- Produces: `risk::detect(unit, imports, span) -> Vec<RiskFeature>`.

- [ ] **Step 1: Fixture** `risk.py`:

```python
import pickle, subprocess

def dyn(code):
    eval(code)                          # dynamic.code exact
    exec(code)

def deserialize(b):
    return pickle.loads(b)              # dynamic.code path

def shell(cmd):
    subprocess.run(cmd, shell=True)     # process.control + dynamic.code
```

- [ ] **Step 2: Write the failing test:**

```rust
#[test]
fn detects_dynamic_code_and_shell() {
    let r = risk_features("risk"); // Vec<(symbol, Vec<String>)> of risk kinds
    assert!(r["dyn"].contains(&"dynamic.code".to_string()));
    assert!(r["deserialize"].contains(&"dynamic.code".to_string()));
    assert!(r["shell"].contains(&"dynamic.code".to_string())); // shell=True
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python detects_dynamic_code`.

- [ ] **Step 4: Implement.** `eval`/`exec`/`compile`/`__import__` → `DynamicCode`(7) `Exact`; `pickle.load/loads`, unsafe `yaml.load` (not `safe_load`), `importlib.import_module` → `DynamicCode`(7) `Path`; `setattr` on an imported module/class (monkey-patch) → `DynamicCode`(7) `Heuristic` (plain `getattr`/`setattr` attribute access NOT flagged); `subprocess(..., shell=True)` → `DynamicCode`(7) `Path` with evidence `"subprocess(shell=True) — shell-injection surface"` (the `ProcessControl` effect comes from `calls`). Wire `risk::detect` into `gather`; `risk_class` feeds `max_class`/`rank_key`.

- [ ] **Step 5: Run — expect PASS.** Run: `cargo test -p fxrank-lang-python detects_dynamic_code`.

- [ ] **Step 6: Commit.**

```bash
git commit -am "feat(python): detect/risk.rs — dynamic.code (eval/pickle/shell=True) + Any type.escape"
```

---

## Task 11: Test-code skipping (path + source) + `--include-tests`

**Files:**
- Modify: `crates/fxrank-lang-python/src/functions.rs` (source-based test detection) and `lib.rs` (path-based skip + `skipped_tests` counting), plus CLI passes `include_tests`.
- Create: `crates/fxrank-lang-python/tests/fixtures/test_sample.py`

**Interfaces:**
- Consumes: `PythonFrontend { include_tests }`.
- Produces: `skipped_tests` populated; default scan skips test code, `--include-tests` re-includes it.

- [ ] **Step 1: Fixture** `test_sample.py`:

```python
def test_one():        # test_* function → skipped by default
    assert True

class TestThing:       # Test* class → its methods skipped
    def test_method(self):
        assert 1 == 1

import unittest
class MyCase(unittest.TestCase):
    def test_case(self):
        self.assertTrue(True)
```

- [ ] **Step 2: Write the failing test:**

```rust
#[test]
fn skips_test_code_by_default_and_counts() {
    // file named test_sample.py → path-based file skip
    let out = analyze_files(&["tests/fixtures/test_sample.py"], /*include_tests=*/false);
    assert_eq!(out.functions.len(), 0);
    assert!(out.skipped_tests >= 1);
    let inc = analyze_files(&["tests/fixtures/test_sample.py"], /*include_tests=*/true);
    assert!(inc.functions.len() >= 3);
}
```

- [ ] **Step 3: Run — expect FAIL.** Run: `cargo test -p fxrank-lang-python skips_test_code`.

- [ ] **Step 4: Implement.** When `!include_tests`: (a) **path-based** — skip a whole file whose base name matches `test_*.py` / `*_test.py` / `conftest.py`, or whose path contains a `tests/` segment (count units skipped in `skipped_tests`); (b) **source-based** — within an otherwise-scanned file, skip a unit that is a `test_*` function, a method of a `Test*` class, or a method of a `unittest.TestCase` subclass. `--include-tests` (already a CLI flag) disables both. (The CLI passes `include_tests` into `PythonFrontend`, mirroring Rust/TS.)

- [ ] **Step 5: Run — expect PASS.** Run: `cargo test -p fxrank-lang-python skips_test_code`.

- [ ] **Step 6: Commit.**

```bash
git commit -am "feat(python): test-code skipping (path + source-based) + --include-tests"
```

---

## Task 12: Interim Python corpus-hygiene `--exclude` defaults + `--help` string

**Files:**
- Modify: `crates/fxrank-cli/src/main.rs` (the `default_value` string) and `crates/fxrank-cli/src/exclude.rs` if needed.
- Create: `crates/fxrank-cli/tests/` exclude integration assertion (or extend an existing CLI test).

**Interfaces:**
- Produces: the enlarged cross-ecosystem default exclude union; `--help` prints it verbatim.

- [ ] **Step 1: Write the failing test** — a directory scan over a temp tree containing `.venv/x.py` (effectful) is NOT scanned by default, and `.venv` does not appear in output; a `*_pb2.py` is excluded and counted.

```rust
#[test]
fn prunes_python_noise_by_default() {
    // build temp dir: pkg/app.py (real), pkg/.venv/lib/dep.py (noise), pkg/svc_pb2.py (generated)
    // scan pkg/ → only app.py's functions appear; skipped_excluded counts svc_pb2.py.
}
```

- [ ] **Step 2: Run — expect FAIL.** Run: `cargo test -p fxrank prunes_python_noise`.

- [ ] **Step 3: Implement.** Extend the `default_value` string in `main.rs:45` to the union (append, verbatim, so `--help` documents it):

```
node_modules,.git,target,*.min.js,*.min.mjs,*.min.cjs,*.stories.*,mockServiceWorker.js,jest.setup.*,jest.config.*,__mocks__,.venv,venv,.tox,.nox,__pycache__,.eggs,build,dist,.mypy_cache,.pytest_cache,.ruff_cache,site-packages,*_pb2.py,*_pb2_grpc.py
```

The matcher (spec 004) already classifies these: bare literals prune dirs (`.venv`, `__pycache__`, …); `*_pb2.py` globs exclude files only. No matcher change needed.

- [ ] **Step 4: Run — expect PASS.** Run: `cargo test -p fxrank prunes_python_noise`.

- [ ] **Step 5: Commit.**

```bash
git commit -am "feat(cli): interim Python corpus-hygiene --exclude defaults (#21) + --help string"
```

---

## Task 13: Dogfood, snapshot, slim builds, finalize

**Files:**
- Create: `crates/fxrank-lang-python/tests/snapshots/` (insta), a representative `tests/fixtures/dogfood.py`.

- [ ] **Step 1: Add an `insta` snapshot test** over a representative fixture (world + state + risk + typed boundary + lambda + test-skip), asserting whole-`Report` shape. Run `cargo insta review` to accept.

- [ ] **Step 2: Dogfood end-to-end.** Run: `cargo run -p fxrank -- scan crates/fxrank-lang-python/tests/fixtures/ --lang python | jq`. Confirm: world-effect functions surface at class 6–7; the fully-typed local-mutation function scores 0; `self.x =` is NOT discounted; `test_*` files skipped; `.venv` (if added to a fixture tree) pruned.

- [ ] **Step 3: Run all CI gates locally.**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p fxrank --no-default-features --features python
cargo build -p fxrank --no-default-features --features rust
cargo build -p fxrank --no-default-features --features ts
cargo build -p fxrank --no-default-features
```

Expected: all green; all slim builds compile (feature-gate hygiene).

- [ ] **Step 4: Add the CI dogfood line** — extend `.github/workflows/ci.yml` with a Python dogfood `scan` over the committed fixtures (mirroring the TS dogfood gate).

- [ ] **Step 5: Final commit + open the PR.**

```bash
git commit -am "test(python): insta snapshot + CI python dogfood; finalize Milestone A"
gh pr create --fill --base main
```

The PR triggers the spec's blocking **Copilot review gate** (re-confirm libcst API) if not already cleared in Task 1, plus the `review-loop` local gate. Link issues #14/#20/#21 in the PR body as the deferred follow-ups.

---

## Self-review notes (coverage map)

- Spec §Effect vocabulary → Tasks 6 (world/`input`/`assert`), 8 (mutation), 10 (risk).
- Spec §Boundary discount (coverage, `Any` poison, decorator confidence) → Task 9.
- Spec §Architecture (libcst, no-visitor recursion driver, borrowed-pass, positions) → Tasks 1, 2, 6.
- Spec §Lambda anchoring (ordinal bijection) → Tasks 2, 3.
- Spec §Own-body attribution (descend eager wrappers/decorators/defaults; stop at nested def/lambda/genexp bodies) → Task 6 driver.
- Spec §Test skipping → Task 11. Spec §Corpus hygiene → Task 12.
- Spec §CLI (.py, `--lang python`, `.pyi` excluded) → Task 4. Spec §Output schema (no new kinds; richer evidence) → Tasks 6/9/10.
- Spec §Review gate → Task 1 Step 6 (blocking) + Task 13 Step 5.
- Parity-first (no core vocab) → only core change is `Language::Python` (Task 4 Step 1), not vocabulary.
