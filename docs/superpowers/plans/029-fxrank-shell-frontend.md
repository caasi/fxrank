# FxRank Shell Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fourth language frontend, `fxrank-lang-shell`, that scores Bash/POSIX `.sh`/`.bash` source by own-body effect cost and participates in cross-file propagation, per spec `docs/superpowers/specs/029-fxrank-shell-frontend.md`.

**Architecture:** A feature-gated crate `crates/fxrank-lang-shell` parses each file with **brush-parser** (pure-Rust Bash AST), collects one `FnUnit` per function plus a synthetic `<script>` unit, and runs pure detectors (`calls`/`mutation`/`risk`/`refs`) orchestrated by `detect::analyze_unit` into scored `Hotspot`s and neutral `UnitRecord`s. `fxrank-core` stays parser-free; the only core additions are `Language::Shell` and two `RiskKind`s. Shell is a `BoundaryCoverage::None` contrast case: no containment discount; nearly every command is a world boundary.

**Tech Stack:** Rust 2024, `brush-parser` (MIT), `serde`, `insta` (snapshot tests), clap (CLI).

## Confirmed brush-parser 0.4.0 API (verified against the shipped crate source)

Use these; the Task-0 spike only re-confirms them on the pinned version:
- Entry: `brush_parser::tokenize_str(&str) -> Result<Vec<Token>, TokenizerError>` then `brush_parser::parse_tokens(tokens: &[Token], options: &ParserOptions) -> Result<ast::Program, ParseError>` — **two args** (`SourceInfo` is only for the feature-gated winnow path; do **not** pass it).
- AST tree shape: `Program.complete_commands: Vec<CompleteCommand>`; `CompleteCommand = CompoundList = Vec<CompoundListItem>`; `CompoundListItem(AndOrList, SeparatorOperator)`; `AndOrList.first: Pipeline` (+ `.additional`); `Pipeline.seq: Vec<Command>`, `Pipeline.timed: Option<PipelineTimed>` (this is `time`); background `&` = `SeparatorOperator::Async`.
- Locations: every node implements the `SourceLocation` trait → `.location() -> Option<SourceSpan>`; `ast::Word { value: String, loc: Option<SourceSpan> }`. Derive 1-based `line`/`col` from `SourceSpan`. **Caveat: `IoRedirect::location()` is a TODO returning `None` in 0.4.0** — for a redirect effect, take the location from the redirect's target `Word` (if it has a `loc`), else fall back to the enclosing command's span. Never rely on a redirect node's own span.
- `(( … ))` = `ast::CompoundCommand::Arithmetic(ArithmeticCommand)` (NOT a `SimpleCommand`). `${x:=word}` is inside a `Word.value` string — parse with the `brush_parser::word` sub-parser (`WordPiece::ParameterExpansion`) or a targeted scan; it is not in the top-level command AST.
- Function body: `FunctionDefinition.body: FunctionBody(CompoundCommand, …)` (usually a `BraceGroupCommand` wrapping a `CompoundList`).

## Global Constraints

- Rust edition 2024, `rust-version = 1.85` (workspace-inherited). One line each, copied from the workspace manifest.
- License `MIT OR Apache-2.0` — **`brush-parser` is MIT (compatible)**; **never** add a GPL dep (yash-syntax is banned).
- `fxrank-core` must stay **parser-free** — `brush-parser` may appear **only** in `crates/fxrank-lang-shell/Cargo.toml`.
- `analyze` must **never panic** — an un-parseable file becomes a `Diagnostic { parsed: false, .. }`.
- Every effect/risk carries `line`/`col` **1-based** (the `Hotspot.id` is `path:line:col:symbol`).
- Frontends always emit `is_root: false`; the CLI sets roots.
- CI gates (all must pass): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, slim build `cargo build -p fxrank --no-default-features --features shell`.
- Own-body output for the other three frontends must be **byte-identical** after this change (only additive).
- Effect/risk classes are the spec's, verified against `fxrank-core`: `net.fs.db`=7, `process.control`/`env.write`/`concurrency`/`global.mutation`=6, `logging`=2, `local.mutation`=1, `hidden`(flag) evidentiary only; `DynamicCode`=7, `PrivilegeEscalation`=6, `DestructiveFs`=5. Fibonacci `CLASS_WEIGHTS=[0,1,2,3,5,8,13,21,34]`.

---

## Task 0: Parser spike + crate scaffold + feature wiring

Resolve the one open risk (brush-parser's parse entry + `SourceSpan` access) *before* building detectors, and stand up a crate that builds and parses a trivial script.

**Files:**
- Create: `crates/fxrank-lang-shell/Cargo.toml`
- Create: `crates/fxrank-lang-shell/src/lib.rs` (temporary smoke shell — replaced in Task 12)
- Modify: `Cargo.toml` (workspace `members`)
- Test: inline `#[cfg(test)]` in `crates/fxrank-lang-shell/src/lib.rs`

**Interfaces:**
- Produces: a `parse(text: &str) -> Result<brush_parser::ast::Program, String>` helper the later tasks call; and a documented note (in `lib.rs` doc comment) of **how to obtain `line`/`col`** for an AST node (the spike's finding).

- [ ] **Step 1: Add the crate to the workspace and pin the dep**

`Cargo.toml` (workspace root) — add the member:

```toml
members = ["crates/fxrank-core", "crates/fxrank-lang-rust", "crates/fxrank-cli", "crates/fxrank-lang-ts", "crates/fxrank-lang-python", "crates/fxrank-lang-shell"]
```

`crates/fxrank-lang-shell/Cargo.toml`:

```toml
[package]
name = "fxrank-lang-shell"
description = "Shell (Bash/POSIX) frontend for fxrank"
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
fxrank-core = { path = "../fxrank-core", version = "0.4.1" }
brush-parser = "=0.4.0"   # exact pin — the plan's AST-shape claims are verified against 0.4.0

[dev-dependencies]
insta = { version = "1", features = ["json"] }
serde_json = "1"   # parity with sibling crates; used if a JSON-snapshot test is added

[lints]
workspace = true
```

- [ ] **Step 2: Write the failing spike test**

`crates/fxrank-lang-shell/src/lib.rs`:

```rust
//! Shell (Bash/POSIX) frontend for fxrank — SPIKE scaffold (Task 0).

/// Parse a shell script into a brush-parser AST, or a diagnostic string.
pub fn parse(text: &str) -> Result<brush_parser::ast::Program, String> {
    // NOTE(spike): confirm exact entry signature in Step 4 and adjust.
    unimplemented!("spike: wire tokenize + parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_script_into_a_program() {
        let prog = parse("echo hi\nfoo() { rm -rf /tmp/x; }\n").expect("should parse");
        // A Program should expose its top-level command list; assert non-empty.
        // (Exact accessor confirmed in the spike — see Step 4.)
        assert!(format!("{prog:?}").contains("foo") || format!("{prog:?}").contains("echo"));
    }

    #[test]
    fn unparseable_input_is_an_err_not_a_panic() {
        // An obviously broken construct must return Err, never panic.
        let _ = parse("if then fi fi )("); // must not panic
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell -- --nocapture`
Expected: FAIL (`not yet implemented` panic from `unimplemented!`).

- [ ] **Step 4: Spike — confirm the real API and implement `parse`**

Inspect the installed crate's surface, then wire it:

Run: `cargo doc -p brush-parser --no-deps` then open the generated docs, **or** `cargo tree -p brush-parser` + read `~/.cargo/registry/src/**/brush-parser-*/src/lib.rs`. Confirm three things and record them in the `lib.rs` module doc comment:
1. The tokenize→parse entry — confirmed **2-arg** `parse_tokens(tokens: &[Token], options: &ParserOptions)` (no `SourceInfo`); re-confirm on the pinned 0.4.0 and find the `tokenize_str` fn + the `Program.complete_commands` accessor.
2. How a `SourceSpan`/`SourcePosition` is obtained for a command/word/redirect node (direct field vs. via token index) — **this is the load-bearing spike finding.**
3. How `time` (reserved word over a pipeline) and redirect lists on compound commands appear in the AST.

Implement `parse` using the confirmed API, e.g. (adjust to the real signatures):

```rust
pub fn parse(text: &str) -> Result<brush_parser::ast::Program, String> {
    let opts = brush_parser::ParserOptions::default();
    let tokens = brush_parser::tokenize_str(text).map_err(|e| format!("{e}"))?;
    brush_parser::parse_tokens(&tokens, &opts).map_err(|e| format!("{e}"))  // 2 args
}
```

Also add a `pub fn span(node: &impl brush_parser::ast::SourceLocation) -> (usize, usize)` helper that maps a `SourceSpan` to 1-based `(line, col)` (used by every detector). Replace the test's `assert!` with the confirmed accessor (`prog.complete_commands.len() > 0`).

- [ ] **Step 5: Run tests to verify they pass, then commit**

Run: `cargo test -p fxrank-lang-shell` → Expected: PASS. Run: `cargo build -p fxrank-lang-shell` → Expected: builds.

```bash
git add Cargo.toml Cargo.lock crates/fxrank-lang-shell/
git commit -m "feat(shell): crate scaffold + brush-parser spike (parse + span access) (refs #15)"
```

---

## Task 1: Core vocabulary — `Language::Shell` + two `RiskKind`s

Additive core changes. Parser-free.

**Files:**
- Modify: `crates/fxrank-core/src/frontend.rs` (`enum Language`)
- Modify: `crates/fxrank-core/src/effect.rs` (`enum RiskKind` + `wire()`/`class()`/`escapes()` + tests)

**Interfaces:**
- Produces: `Language::Shell`; `RiskKind::DestructiveFs` (class 5), `RiskKind::PrivilegeEscalation` (class 6), both `escapes() == true`.

- [ ] **Step 1: Write the failing core test**

Add to `crates/fxrank-core/src/effect.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn shell_vocabulary_metadata() {
    assert_eq!(RiskKind::DestructiveFs.wire(), "destructive.fs");
    assert_eq!(RiskKind::DestructiveFs.class(), 5);
    assert!(RiskKind::DestructiveFs.escapes());
    assert_eq!(RiskKind::PrivilegeEscalation.wire(), "privilege.escalation");
    assert_eq!(RiskKind::PrivilegeEscalation.class(), 6);
    assert!(RiskKind::PrivilegeEscalation.escapes());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-core shell_vocabulary_metadata`
Expected: FAIL (variants don't exist → compile error).

- [ ] **Step 3: Implement the additions**

`frontend.rs`: add `Shell` to `enum Language { Rust, Ts, Python, Shell }`.

`effect.rs` — add the two variants and extend the three matches:

```rust
// in enum RiskKind { … }
    DestructiveFs,
    PrivilegeEscalation,
```
```rust
// wire()
    DestructiveFs => "destructive.fs",
    PrivilegeEscalation => "privilege.escalation",
// class()
    PrivilegeEscalation => 6,          // above DestructiveFs/UnsafeBlock(5), below DynamicCode(7)
    DestructiveFs => 5,
// escapes() — add to the matches!(…) capability list:
    DynamicCode | FfiCall | HtmlInjection | ProtoPollution | EffectInRender
        | DestructiveFs | PrivilegeEscalation
```

(Class 6 is otherwise unused by `RiskKind::class()` — a gap in the scale, intentional.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-core` → Expected: PASS (new + existing).

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-core/src/frontend.rs crates/fxrank-core/src/effect.rs
git commit -m "feat(core): Language::Shell + DestructiveFs/5 + PrivilegeEscalation/6 risks (refs #15)"
```

---

## Task 2: `functions.rs` + `walk.rs` — `FnUnit`, `collect`, and the ONE shared descent

**Files:**
- Create: `crates/fxrank-lang-shell/src/functions.rs`
- Create: `crates/fxrank-lang-shell/src/walk.rs` — the **single** recursive descent every detector shares (defined here, before Task 3, so `bindings`/`calls`/`mutation`/`risk` all call it — no duplicate copies, which was the root cause of the control-flow-blindness bug)
- Create: `crates/fxrank-lang-shell/tests/fixtures/functions.sh`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- `functions.rs` produces:
  - `pub enum FnBody<'a> { Script(Vec<&'a ast::CompoundListItem>), Func(&'a ast::FunctionBody) }` and `pub struct FnUnit<'a> { pub symbol: String, pub id: String, pub path: String, pub line: usize, pub col: usize, pub body: FnBody<'a>, pub is_script: bool }`. A function body is `FunctionBody(pub CompoundCommand, pub Option<RedirectList>)` whose `.0` can be **any** `CompoundCommand` variant (not always a flat `CompoundList`), so `Func` holds `&FunctionBody` (compound `.0` for the descent + `.1` redirects for `f(){…} >out`). `Script` holds **all** top-level `CompoundListItem`s **unfiltered** — the walk skips `Command::Function` nodes itself (a single item can mix a fn-def and a command, e.g. `f(){:;} && rm -rf /x`, so item-level filtering would wrongly drop `rm`).
  - `pub fn collect`, `pub fn defined_function_names` (as before).
- `walk.rs` produces the **shared descent** (Codex/Sonnet round-2: exactly one copy):
  - `pub struct CmdSite<'a> { pub sc: &'a ast::SimpleCommand, pub subshell: bool, pub redirs: Vec<&'a ast::IoRedirect>, pub stdin_is_here: bool, pub subst: bool }` — `subst` is `true` when the site was reached by descending a **process substitution** `<(…)`/`>(…)` (distinct from a plain `( )` subshell), so calls/**mutation** (which own `Effect`s — risk carries no confidence) apply spec §6's `-0.1` substitution-context confidence delta. (Command-substitution `$()`/backtick effects come from the effect-level recursion in Task 7/8, which applies the same `-0.1` to its merged inner effects directly.)
  - `pub enum Site<'a> { Command(CmdSite<'a>), Arithmetic(&'a ast::ArithmeticCommand, /*subshell*/ bool), FnDefine(&'a ast::FunctionDefinition), Concurrency(/*span*/ (usize,usize)), ForVar(&'a str, /*span*/ (usize,usize)), Pipeline(&'a ast::Pipeline, /*subshell*/ bool), Redirect(&'a ast::IoRedirect, /*subshell*/ bool) }` — a complete site set so **every** consumer works off the one descent: `Command` (calls/mutation), `Arithmetic` + `FnDefine` (mutation, Task 8), `Concurrency` (calls emits `concurrency`/6, Task 7), `ForVar` (bindings `local_names`, Task 3 — the `ForClauseCommand.variable_name`), `Pipeline` (risk `curl|sh` adjacency, Task 9), `Redirect` (calls emits fs effects, Task 7 — for redirects attached at the **`Command` level**: `Command::Compound(CompoundCommand, Option<RedirectList>)` like `while …; done >out`, `Command::ExtendedTest(ExtendedTestExprCommand, Option<RedirectList>)` like `[[ x ]] >out`, and `FnBody::Func(&FunctionBody).1`; SimpleCommand-own redirects stay on `CmdSite.redirs`).
    - **`FnDefine` semantics:** emitted for a `FunctionDefinition` encountered **inside a function body** (→ mutation.rs charges the enclosing unit a `fn-define` `global.mutation`/6); the walk does **not** descend into its body (it is its own `FnUnit` from `collect`). A **top-level** (`FnBody::Script`) function definition is **skipped entirely** (not a `<script>` mutation).
    - **`Command` variants (all 4, verified 0.4.0 — note the variant is `Simple`, wrapping a type `SimpleCommand`):** `Command::Simple(sc)` → `Site::Command`; `Command::Compound(cc, redirs)` → descend `cc` + `Site::Redirect` for each of `redirs`; `Command::Function(def)` → `Site::FnDefine` (or skip at top level); `Command::ExtendedTest(_, redirs)` → `Site::Redirect` for each of `redirs` (the `[[ … ]]` test itself has no command effect).
  - `pub fn walk<'a>(body: &FnBody<'a>, visit: &mut impl FnMut(Site<'a>))` — the one recursive descent. On a `FnBody::Func(fb)` it first emits `Site::Redirect` for each redirect in `fb.1` (the function-body redirect list, `f(){…} >out`) **before** descending `fb.0`; on `FnBody::Script(items)` it descends the items directly. It computes `subshell` context structurally and covers **every** `CompoundCommand` variant (verified 0.4.0, all 10): `Arithmetic` → `Site::Arithmetic`; `ForClause`(`.variable_name`, `.body: DoGroupCommand`) → **`visit(Site::ForVar(&fc.variable_name, fc.loc))`** (clause-level span — `variable_name` has no separate `loc`) then descend the do-group's list; `ArithmeticForClause`(`.body: DoGroupCommand`) → descend the do-group's list; `BraceGroup`(list) → descend same-context; `Subshell`(list) → descend **subshell**; `IfClause`{`condition: CompoundList`, `then: CompoundList`, `elses: Option<Vec<ElseClause>>`} → descend `condition` + `then`, then **for each `ElseClause` descend its `condition: Option<CompoundList>` AND `body: CompoundList`** (so an `elif`'s condition, e.g. `elif curl x; then …`, is not skipped), same-context; `WhileClause`/`UntilClause`(`WhileOrUntilClauseCommand(CompoundList, DoGroupCommand, _)`) → descend cond + do-group same-context; `CaseClause`{`cases: Vec<CaseItem>`} → descend each `CaseItem.cmd: Option<CompoundList>` same-context; `Coprocess`(`.body: Box<Command>`) → `Site::Concurrency` + descend **subshell**. Per `Command` (the enum is `Command::{Simple(SimpleCommand), Compound(CompoundCommand, Option<RedirectList>), Function(FunctionDefinition), ExtendedTest(ExtendedTestExprCommand, Option<RedirectList>)}`): `Command::Simple(sc)` → `Site::Command`; `Command::Compound(cc, redirs)` → `Site::Redirect` per redir + descend `cc`; `Command::ExtendedTest(_, redirs)` → `Site::Redirect` per redir; `Command::Function(_)` inside a function body → `Site::FnDefine` (don't descend), at top level → skipped (its own unit). Per `CompoundListItem`: `AndOrList.first`(+`.additional`) → `Pipeline.seq`; a background `&` (`SeparatorOperator::Async` on the item) and a multi-stage `Pipeline.seq.len() > 1` set subshell; emit `Site::Pipeline(pipe, subshell)` per `Pipeline` and `Site::Concurrency` for a background job. Process substitution (`CommandPrefixOrSuffixItem::ProcessSubstitution(_, SubshellCommand)` on a `Command::Simple` arg, `IoFileRedirectTarget::ProcessSubstitution(_, SubshellCommand)` on a redirect) → descend the inner `SubshellCommand.list` **subshell**.
  - `pub fn walk_commands<'a>(unit: &FnUnit<'a>) -> Vec<CmdSite<'a>>` — thin filter collecting only `Site::Command` (for calls/risk that only classify `SimpleCommand`s). `mutation`/`bindings` call `walk` directly for the full `Site` set.
  - Command-substitution helpers (shared so calls.rs/mutation.rs each recurse their own half without a forward dep): `pub fn subst_words<'a>(sc: &'a ast::SimpleCommand) -> Vec<&'a ast::Word>` (command word + args + `VAR=val` prefix `AssignmentValue` values) and `pub fn subst_programs(word: &ast::Word) -> Vec<(SourceSpan /*enclosing Word.loc to re-anchor to*/, ast::Program)>` (word-parse each `CommandSubstitution`/`BackquotedCommandSubstitution` piece → `tokenize_str`+`parse_tokens` a locally-owned inner `Program`).
- Consumers: Task 3 `bindings`, Task 4 `calls`, Task 8 `mutation`, Task 9 `risk` all **call `walk`/`walk_commands`** — never re-implement the descent.
  - `pub fn collect<'a>(prog: &'a ast::Program, path: &str) -> Vec<FnUnit<'a>>` — one unit per `FunctionDefinition` (both `name() {}` and `function name {}` forms), including nested definitions, **plus** a synthetic `<script>` unit (symbol `"<script>"`, line 1, col 1) when the top level has executable (non-definition) statements.
  - `pub fn defined_function_names(prog: &ast::Program) -> std::collections::HashSet<String>` — same-file function name set (Tasks 4 & 10 consume it).

- [ ] **Step 1: Write the fixture**

`crates/fxrank-lang-shell/tests/fixtures/functions.sh`:

```bash
#!/usr/bin/env bash
GREETING=hello        # top-level → forces a <script> unit

greet() { echo "$GREETING"; }

function deploy {
  outer() { rm -rf /tmp/x; }   # nested definition
  greet
}
```

- [ ] **Step 2: Write the failing test**

`functions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn units(name: &str) -> Vec<String> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap();
        let prog = parse(&src).unwrap();
        collect(&prog, name).into_iter().map(|u| u.symbol).collect()
    }

    #[test]
    fn collects_functions_nested_defs_and_script_unit() {
        let syms = units("functions.sh");
        assert!(syms.contains(&"greet".to_string()));
        assert!(syms.contains(&"deploy".to_string()));
        assert!(syms.contains(&"outer".to_string()));      // nested def is its own unit
        assert!(syms.contains(&"<script>".to_string()));   // top-level GREETING=hello forces it
    }

    #[test]
    fn no_script_unit_when_only_definitions() {
        let src = "greet() { echo hi; }\n";
        let prog = parse(src).unwrap();
        let syms: Vec<_> = collect(&prog, "x.sh").into_iter().map(|u| u.symbol).collect();
        assert!(!syms.contains(&"<script>".to_string()));
        assert_eq!(syms, vec!["greet".to_string()]);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell functions` → Expected: FAIL (compile error — `collect` missing).

- [ ] **Step 4: Implement `FnUnit`, `collect`, `defined_function_names`**

Walk `prog.complete_commands` (each a `CompoundList = Vec<CompoundListItem>`). For each `FunctionDefinition` node, emit a `FnUnit` with `symbol = name`, `line`/`col` from the name node's span (per the Task-0 spike), and `body = FnBody::Func(&def.body)` (the whole `&FunctionBody` — `.0` the `CompoundCommand`, `.1` the redirect list; Task 4's `walk_commands` descends `.0` and Task 7 attributes `.1` to the unit). Recurse into the body to find nested `FunctionDefinition`s (each its own unit). For the `<script>` unit, `body = FnBody::Script(items)` where `items` = **all** top-level `CompoundListItem`s (the shared `walk` skips `Command::Function` nodes itself — item-level filtering would mis-handle a mixed item like `f(){:;} && rm -rf /x`); emit it if any item contains a non-`Function` command. Build `id = format!("{path}:{line}:{col}:{symbol}")`. `defined_function_names` returns the union of all function `symbol`s (top-level + nested).

Use a small recursive visitor over `ast::Command`/`ast::CompoundCommand` (match the real variant names from the spike). Keep it a plain recursion — no `unimplemented!`.

- [ ] **Step 5: Run tests, then commit**

Run: `cargo test -p fxrank-lang-shell functions` → Expected: PASS.

```bash
git add crates/fxrank-lang-shell/src/functions.rs crates/fxrank-lang-shell/tests/fixtures/functions.sh
git commit -m "feat(shell): FnUnit + collect (functions, nested defs, <script> unit) (refs #15)"
```

---

## Task 3: `bindings.rs` — local-name pre-scan + script-top binding set

The scope model that `mutation.rs` (Task 8) consults for declaration-vs-hidden.

**Files:**
- Create: `crates/fxrank-lang-shell/src/bindings.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub fn local_names(unit: &FnUnit) -> HashSet<String>` — names declared **local in this function**: `local`, `declare`/`typeset` **without `-g`**, and `for` loop vars (`ForClauseCommand.variable_name`). **`readonly` is excluded** (not local scope) and **`select`/`read`/`mapfile` are excluded** (no `select` AST variant in 0.4.0; `read`/`mapfile` are deliberate non-local writes, spec §7).
  - `pub fn script_top_names(prog: &ast::Program) -> HashSet<String>` — names assigned at the script top level.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, functions::collect};

    #[test]
    fn local_names_include_local_declare_but_not_readonly() {
        let src = "f(){ local a=1; declare b=2; readonly c=3; declare -g d=4; for e in x; do :; done; }\n";
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh").into_iter().find(|u| u.symbol == "f").unwrap();
        let names = local_names(&unit);
        assert!(names.contains("a") && names.contains("b") && names.contains("e"));
        assert!(!names.contains("c"), "readonly does not create local scope");
        assert!(!names.contains("d"), "declare -g is global, not local");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell bindings` → Expected: FAIL (compile error).

- [ ] **Step 3: Implement**

Call the shared `crate::walk::walk` (Task 2) so `local`s nested in `if`/`for`/`while`/`case` bodies are found — collecting names from `Site::Command`s whose first word is `local`/`declare`/`typeset` (assigned names, unless a `-g` flag) plus `for` iteration variables (from the descent). Return the set. `script_top_names` walks the program's top-level `Assignment`s (and pure `VAR=val` `SimpleCommand`s with no command). **Do not re-implement the descent** — one shared `walk` (spec §7 / round-2 review: duplicate copies caused the original control-flow blindness).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell bindings` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/bindings.rs
git commit -m "feat(shell): local-name pre-scan + script-top binding set (readonly excluded) (refs #15)"
```

---

## Task 4: `detect/calls.rs` — command classifier core (name → effect)

**Files:**
- Create: `crates/fxrank-lang-shell/src/detect/mod.rs` (module decls only, for now)
- Create: `crates/fxrank-lang-shell/src/detect/calls.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub fn detect(unit: &FnUnit, fns: &HashSet<String>) -> Vec<Effect>` — classifies each `SimpleCommand` in the unit body (Tasks 5–7 extend this same fn). **Consults `fns`: a command word matching a same-file function name (and not under a function-bypass wrapper — Task 6) emits NO command effect** (it's a call ref, handled in Task 10) — spec §4 function-vs-command precedence.
  - `pub enum Cls { NoEffect, Effect(EffectKind, u8), Unknown }` and `fn classify_command(name: &str, has_v: bool) -> Cls` — a **tri-state** name classifier so recognized-but-effectless names (`tr`, filter tools, `read`/`mapfile`, declaration builtins, `MUT_OWNED` mutation builtins) do **not** fall through to the `Unknown → process.control/6` spawn branch. `has_v` gates `printf` (with `-v` it's a mutation, owned by mutation.rs).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, functions::collect};
    use fxrank_core::effect::EffectKind;
    use std::collections::HashSet;

    fn kinds_fns(src: &str, fns: &HashSet<String>) -> Vec<(EffectKind, u8)> {
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh").into_iter().find(|u| u.is_script).unwrap();
        detect(&unit, fns).into_iter().map(|e| (e.kind, e.class)).collect()
    }
    fn kinds(src: &str) -> Vec<(EffectKind, u8)> { kinds_fns(src, &HashSet::new()) }

    #[test]
    fn classifies_core_command_categories() {
        assert!(kinds("rm -rf /x\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("curl http://x\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("docker ps\n").contains(&(EffectKind::ProcessControl, 6)));
        assert!(kinds("frobnicate --wat\n").contains(&(EffectKind::ProcessControl, 6))); // unknown => spawn
        assert!(kinds("echo hi\n").contains(&(EffectKind::Logging, 2)));
        assert!(kinds(": ; true\n").is_empty()); // pure builtins → no effect
        // export/cd/set/… are MUT_OWNED: calls.rs emits NOTHING (mutation.rs owns them, Task 8) —
        // this prevents the double-emit. printf -v is likewise NoEffect here.
        assert!(kinds("export FOO=1\n").is_empty());
        assert!(kinds("cd /x\n").is_empty());
        assert!(kinds("printf -v out fmt\n").is_empty());
    }

    #[test]
    fn walk_recurses_into_control_flow_bodies() {
        // a command nested in if/for/while must NOT be invisible
        assert!(kinds("if true; then rm -rf /x; fi\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("for f in a b; do curl http://$f; done\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("while read l; do rm -rf /x; done\n").contains(&(EffectKind::NetFsDb, 7)));
        // C-style arithmetic for (ArithmeticForClause) — a distinct CompoundCommand variant
        assert!(kinds("for ((i=0;i<3;i++)); do rm -rf /x; done\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("case $x in a) curl http://y ;; esac\n").contains(&(EffectKind::NetFsDb, 7)));
    }

    #[test]
    fn same_file_function_suppresses_command_classification() {
        // A script defining greet() and calling greet must NOT get a process.control
        // spawn for `greet` — it's a same-file function call.
        let fns: HashSet<String> = ["greet".to_string()].into_iter().collect();
        assert!(kinds_fns("greet\n", &fns).is_empty());
        // but an unrecognized non-function word still spawns
        assert!(kinds_fns("frobnicate\n", &fns).contains(&(EffectKind::ProcessControl, 6)));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell calls` → Expected: FAIL (compile error).

- [ ] **Step 3: Implement `classify_command` + the walk**

`detect/mod.rs`: `pub mod calls;` (add `mutation`/`risk`/`refs` in later tasks).

`detect/calls.rs` — the const tables (verbatim from spec §4) + the classifier:

```rust
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use crate::functions::FnUnit;
use std::collections::HashSet;

const PURE: &[&str] = &[":", "true", "false", "test", "[", "[[", "return", "break", "continue"];
const FS_ALWAYS: &[&str] = &["cp","mv","rm","mkdir","rmdir","touch","ln","chmod","chown","dd",
    "truncate","install","shred","mktemp","stat","readlink","ls","find"];
const NET: &[&str] = &["curl","wget","ssh","scp","sftp","rsync","nc","telnet","ftp"];
const DB: &[&str] = &["psql","mysql","sqlite3","mongo","redis-cli"];
const DEPLOY: &[&str] = &["docker","kubectl","helm","terraform","ansible","aws","gcloud","az","systemctl","service"];
const CONCURRENCY: &[&str] = &["wait","jobs","disown"];  // job-control BUILTINS (concurrency/6); coproc is NOT here — it's CompoundCommand::Coprocess, detected in Task 7

// Names recognized as effectless in CALLS (owned elsewhere), so they must NOT fall through
// to the Unknown → process.control spawn branch. SINGLE OWNERSHIP (no double-emit):
//  - FILTER / read / mapfile: fs decided by Task 5's classify_conditional
//  - tr: never fs
//  - DECL declaration/assignment builtins AND MUT_OWNED (cd/set/export/…): owned by mutation.rs (Task 8)
const FILTER: &[&str] = &["cat","grep","sed","awk","head","tail","sort","uniq","wc","cut","rev","tee"];
const NEVER_FS: &[&str] = &["tr"];
const DECL: &[&str] = &["local","declare","typeset","readonly","let","getopts","shift","read","mapfile","readarray"];
// Builtins whose ENTIRE effect is a mutation (env.write / global.mutation) — mutation.rs is the
// sole owner; calls.rs must return NoEffect for them or they'd double-emit with Task 8.
const MUT_OWNED: &[&str] = &["export","unset","cd","pushd","popd","set","shopt","umask","ulimit"];

pub enum Cls { NoEffect, Effect(EffectKind, u8), Unknown }

/// Tri-state name classifier. NoEffect = recognized & effectless-in-calls; Unknown = spawn.
/// `has_v` = the command carries a `-v` flag (for `printf -v`, which is a mutation, not logging).
pub fn classify_command(name: &str, has_v: bool) -> Cls {
    if PURE.contains(&name) || DECL.contains(&name) || NEVER_FS.contains(&name)
        || FILTER.contains(&name) || MUT_OWNED.contains(&name) {
        return Cls::NoEffect;   // FILTER fs (Task 5) / MUT_OWNED mutation (Task 8) owned elsewhere
    }
    if name == "echo" { return Cls::Effect(EffectKind::Logging, 2); }
    if name == "printf" { return if has_v { Cls::NoEffect } else { Cls::Effect(EffectKind::Logging, 2) }; } // printf -v → mutation.rs owns
    if FS_ALWAYS.contains(&name) || NET.contains(&name) || DB.contains(&name) {
        return Cls::Effect(EffectKind::NetFsDb, 7);
    }
    if CONCURRENCY.contains(&name) { return Cls::Effect(EffectKind::Concurrency, 6); }
    if name == "source" || name == "." { return Cls::Effect(EffectKind::ProcessControl, 6); } // opaque exec (ref: Task 10)
    if DEPLOY.contains(&name) { return Cls::Effect(EffectKind::ProcessControl, 6); } // known → 0.9 conf
    Cls::Unknown  // any other word → a spawn (0.7 conf)
}

pub fn detect(unit: &FnUnit, fns: &HashSet<String>) -> Vec<Effect> {
    let mut out = Vec::new();
    for site in walk::walk_commands(unit) {         // shared recursive descent from Task 2's walk.rs
        let Some(name) = command_word(site.sc) else { continue };
        // Task 6 will wrap this in wrapper-stripping + resolution mode; in Task 4 the
        // resolution mode is Normal, so consult the same-file function set directly.
        if fns.contains(&name) { continue; }        // same-file fn call → ref only (Task 10), no effect
        // Task 5 inserts classify_conditional(&name, &operands) here and, if it returns
        // Some, emits that and `continue`s. Task 4 handles the name-only tri-state:
        match classify_command(&name, has_flag(site.sc, "-v")) {
            Cls::Effect(kind, class) => out.push(mk_effect(kind, class, site.sc, Tier::Heuristic, 0.9, &name)),
            Cls::Unknown => out.push(mk_effect(EffectKind::ProcessControl, 6, site.sc, Tier::Heuristic, 0.7, &name)),
            Cls::NoEffect => {}
        }
    }
    out
}

fn mk_effect(kind: EffectKind, class: u8, sc: &SimpleCommand, tier: Tier, confidence: f64, ev: &str) -> Effect {
    let (line, col) = span_of(sc);  // from the Task-0 spike
    Effect { kind, class, discounted_to: None, weight: weight_for_class(class), line, col,
        tier, hidden: false, contained: false, evidence: ev.to_string(), discount: None,
        subreason: None, confidence }
}
```

Implement here (Task 7 adds redirect/concurrency/substitution effect emission over the same walk):
- Task 4 **consumes** the shared `crate::walk::{walk, walk_commands, CmdSite, Site}` defined in Task 2's `walk.rs` (the single recursive descent covering all control-flow/compound variants + subshell context). `calls::detect` classifies each `Site::Command`'s `CmdSite.sc` (and in Task 7 also handles `Site::Concurrency`/redirs/substitutions). Do **not** re-implement the descent here — that duplication is what let the original control-flow-blindness bug slip in.
- `command_word(sc)` (the first literal word, else `None`), `span_of(sc)` (via the Task-0 `span` helper), `has_flag(sc, "-v")` (true if any arg `Word` equals the flag — for the `printf -v` gate).

Confidence: literal known 0.9 (the `Cls::Effect` arm), unknown spawn 0.7 (the `Cls::Unknown` arm) — already encoded in the `mk_effect` calls above.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell calls` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/
git commit -m "feat(shell): command classifier core — fs/net/db/deploy/env/global + unknown=spawn (refs #15)"
```

---

## Task 5: `detect/calls.rs` — stream-filter rule (file-operand) + `tr` + `read` input boundary

**Files:**
- Modify: `crates/fxrank-lang-shell/src/detect/calls.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `detect`, `command_word`, `span_of` (Task 4).
- Produces: extended `detect` handling the conditional-fs commands + `read`/`mapfile`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn stream_filter_rule() {
    // grep with a file operand → fs read; bare (stdin) grep → no fs effect.
    assert!(kinds("grep pat f.txt\n").contains(&(EffectKind::NetFsDb, 7)));
    assert!(kinds("grep pat\n").is_empty());
    // tr never fs, regardless of args
    assert!(kinds("tr a b\n").is_empty());
    // file via option counts
    assert!(kinds("grep -f pats\n").contains(&(EffectKind::NetFsDb, 7)));
    // read from real input is class 7; here-string fed read is NOT (Task 7 wires <<<; here bare read)
    assert!(kinds("read x\n").contains(&(EffectKind::NetFsDb, 7)));
}

#[test]
fn unquoted_var_in_destructive_lowers_effect_confidence() {
    use std::collections::HashSet;
    let effect = |src: &str| {
        let prog = parse(src).unwrap();
        let unit = crate::functions::collect(&prog, "x.sh").into_iter().find(|u| u.is_script).unwrap();
        detect(&unit, &HashSet::new()).into_iter().find(|e| e.kind == EffectKind::NetFsDb).unwrap()
    };
    // rm -rf $DIR (unquoted var) → the net.fs.db effect confidence is reduced (−0.1) vs a literal path
    assert!(effect("rm -rf $DIR\n").confidence < effect("rm -rf /tmp/x\n").confidence);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell stream_filter_rule` → Expected: FAIL.

- [ ] **Step 3: Implement the conditional-fs classification**

`FILTER`/`NEVER_FS` are already declared in Task 4. Add the operand logic + wire `classify_conditional` into `detect` **before** the `classify_command` match. **Wrapper awareness:** Task 6 adds `strip_wrappers`; once it lands, `classify_conditional`/`has_file_operand` take the **peeled** `CommandView.head`/`CommandView.args` (the inner command + inner operands), not the raw `site.sc` — so `sudo grep -f pats` / `command grep pat f.txt` compute arity against the real operands, never the wrapper words. In Task 5 (before Task 6) the call passes `&name` + the raw command's arg `Word`s (`operands`) from `site.sc`; Task 6 revisits the call sites to pass `view.head`/`&view.args` (same forward-reference pattern as Task 4's same-file-fn guard). The signatures below take an operand slice + name so both forms work:

```rust
// option flags that take a FILE argument, per tool (best-effort):
fn file_taking_option(name: &str, flag: &str) -> bool {
    matches!((name, flag), ("grep","-f") | ("sed","-f") | ("awk","-f") | ("sort","-o"))
}
// index of the first POSITIONAL that is a file (earlier positionals are pattern/program):
fn first_file_positional(name: &str) -> usize {
    match name { "grep" | "sed" | "awk" => 1, _ => 0 } // grep PAT files… / sed SCRIPT files… / awk PROG files…
}

/// Some(effect) for a FILTER command iff it names a real file operand.
/// Operand-based (NOT `&SimpleCommand`) so wrapped commands can pass peeled operands:
/// Task 5 passes the raw command's arg `Word`s; Task 6 switches the call sites to `view.args`.
fn classify_conditional(name: &str, args: &[&ast::Word]) -> Option<(EffectKind, u8, /*ambiguous*/ bool)> {
    if !FILTER.contains(&name) { return None; }         // tr/others handled by Task 4's tri-state
    match has_file_operand(name, args) {                 // returns Option<bool>: None = undecidable
        Some(true) => Some((EffectKind::NetFsDb, 7, false)),
        Some(false) => None,
        None => Some((EffectKind::NetFsDb, 7, true)),    // ambiguous ($vars) → emit, but mark ambiguous
    }
}
```

`has_file_operand(name: &str, args: &[&ast::Word]) -> Option<bool>`: `Some(true)` if a positional `Word` at index `>= first_file_positional(name)` exists (so `grep pat` — only positional is the pattern — is `Some(false)`; `grep pat f.txt` is `Some(true)`), **or** a `-x file` where `file_taking_option(name, "-x")`; `None` when arity can't be decided (all positionals are `$var`s) → `classify_conditional` returns `ambiguous = true` and the emitted **effect's** confidence is lowered 0.1 (there is no risk confidence). `read`/`mapfile` → `net.fs.db`/7 **input-boundary IO** unless `CmdSite` marks the command's stdin as a here-string/here-doc (Task 7 sets that; until then bare `read` is class 7). Wire the `detect` loop: after the same-file-fn check, `if let Some((k,c,amb)) = classify_conditional(&name, &operands) { emit(k,c, conf 0.9 - if amb {0.1} else {0.0}); continue; }` then `if is_input_reader(&name) { emit read/mapfile per stdin source; continue; }` then the Task-4 `classify_command` match. In Task 5 `operands` = the raw command's arg `Word`s (from `site.sc`); **Task 6 switches this to `view.args`** (peeled inner operands) so `sudo grep -f pats` counts correctly.

**Unquoted-var-in-destructive confidence delta (owned here, spec §6).** When emitting the `net.fs.db`/7 effect for a **destructive** fs command (`rm` with `-r`/`-rf`/`-R`, `chmod -R`, `chown -R`, `dd`, `shred`) whose operands include an **unquoted** variable expansion (a `Word` that is a bare `$VAR`/`${VAR}` without surrounding quotes), subtract 0.1 from that effect's confidence. This lives in `calls.rs` (not `risk.rs`) because `RiskFeature` carries no confidence and function confidence is computed from effects only. (Task 9's `DestructiveFs` risk is emitted separately; it does not carry this.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell calls stream_filter_rule` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/calls.rs
git commit -m "feat(shell): stream-filter rule (file-operand) + tr-never-fs + read input boundary (refs #15)"
```

---

## Task 6: `detect/calls.rs` — command-prefix wrappers + resolution mode

**Files:**
- Modify: `crates/fxrank-lang-shell/src/detect/calls.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub enum ResMode { Normal, FunctionBypass, BuiltinOnly }`
  - `pub struct CommandView<'a> { pub kinds: Vec<String>, pub mode: ResMode, pub head: Option<String>, pub args: Vec<&'a ast::Word>, pub prefix_assignments: Vec<&'a ast::Assignment>, pub span: (usize, usize) }` — the wrapper-peeled view of a `SimpleCommand`: `head` = the inner command word (after peeling wrappers + their options + `VAR=val` prefixes), `args` = the inner command's operands only (wrapper options excluded), `prefix_assignments` = the `VAR=val` prefixes.
  - `pub fn strip_wrappers<'a>(sc: &'a ast::SimpleCommand) -> CommandView<'a>`.
  - **`ResMode` is NOT stored on `CallSiteRef`** (that core struct has no such field) — **Tasks 5, 9, and 10 call `strip_wrappers` themselves** and consume `CommandView.{head, args, kinds, mode}` (so stream-filter arity, destructive flags, and source refs see the *inner* operands, never wrapper options). `detect` uses it to classify the *wrapped* command and union the wrapper's own effect.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn wrappers_recurse_into_argv() {
    // sudo rm -rf keeps the fs effect (Task 9 adds the PrivilegeEscalation risk)
    assert!(kinds("sudo rm -rf /x\n").contains(&(EffectKind::NetFsDb, 7)));
    // command rm -rf recurses to rm
    assert!(kinds("command rm -rf /x\n").contains(&(EffectKind::NetFsDb, 7)));
    // exec layers its own process.control atop the wrapped command
    let k = kinds("exec rm -rf /x\n");
    assert!(k.contains(&(EffectKind::NetFsDb, 7)) && k.contains(&(EffectKind::ProcessControl, 6)));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell wrappers_recurse` → Expected: FAIL.

- [ ] **Step 3: Implement wrapper stripping**

```rust
#[derive(Clone, Copy, PartialEq)]
pub enum ResMode { Normal, FunctionBypass, BuiltinOnly }

const WRAP_EXTERNAL: &[&str] = &["sudo","su","doas","env","nice","nohup","exec","command"]; // NOT time (reserved word => Pipeline.timed)
// exec also adds its own process.control/6; sudo/su/doas add PrivilegeEscalation (Task 9).
```

Implement `strip_wrappers` returning a `CommandView`: peel leading wrapper words (and their options / `VAR=val` prefixes) while the head is in `WRAP_EXTERNAL` or `builtin`; collect `kinds`, `args` (inner operands only), `prefix_assignments`; `mode` = `BuiltinOnly` if `builtin` seen, else `FunctionBypass` if any `WRAP_EXTERNAL` seen, else `Normal`. In `detect`, call `strip_wrappers` per site, then:
- **same-file-fn precedence only when `mode == Normal`** (a `FunctionBypass`/`BuiltinOnly` wrapped word is never a same-file function call). Update the Task-4 guard to `view.mode == ResMode::Normal && fns.contains(head)`.
- classify the inner `head` word (Tasks 4–5), **except in `BuiltinOnly` mode**: `builtin foo` must classify `foo` as a builtin only — an inner word that is **not** a recognized builtin/effect name yields **no effect** (do **not** fall through to the `Unknown => process.control/6` external-spawn branch, since `builtin` never runs an external program). So in `BuiltinOnly` mode, map `Cls::Unknown => {}` (skip) instead of emitting a spawn.
- if `exec` is among `kinds`, additionally push a `ProcessControl`/6 effect for the exec itself.

Also **revisit Task 5's `classify_conditional`/`has_file_operand` call sites** to pass `view.head`/`view.args` (the peeled inner command + operands) instead of `site.sc`/`name`, so a wrapped stream-filter (`sudo grep -f pats`) computes file-operand arity against the real operands. Add a test: `sudo grep pat f.txt` → `net.fs.db`/7, and `sudo grep pat` → no fs effect.

**`ResMode` is not persisted** — Task 9 and Task 10 re-call `strip_wrappers`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell wrappers` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/calls.rs
git commit -m "feat(shell): command-prefix wrapper recursion + resolution mode (refs #15)"
```

---

## Task 7: `detect/calls.rs` — redirections, substitutions, subshell-context tagging

**Files:**
- Modify: `crates/fxrank-lang-shell/src/detect/calls.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Enriches the recursive `walk_commands`/`CmdSite` from Task 4: populates `subshell` (true inside `(…)` subshell, `&`, a multi-stage `Pipeline.seq.len()>1`, and process-substitution inner commands), `redirs`, and `stdin_is_here` (stdin fed by a here-string/here-doc → `read`/`mapfile` emit no fs IO, Task 5). `detect` emits redirect fs effects and a **`concurrency`/6 escaping effect** for launching a background job (`SeparatorOperator::Async`) or a `coproc` (`CompoundCommand::Coprocess`) — **NOT** for a plain multi-stage pipeline (bounded/joined). `walk_commands` is the shared walk `mutation.rs` (Task 8) and `risk.rs` (Task 9) also consume.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn redirections_and_here_strings() {
    assert!(kinds("cat > out\n").contains(&(EffectKind::NetFsDb, 7)));   // output redirect = write
    assert!(kinds("grep pat < in\n").contains(&(EffectKind::NetFsDb, 7))); // input redirect = read
    assert!(kinds("read x <<< \"$s\"\n").is_empty());                    // here-string: no fs
}

#[test]
fn command_substitution_recurses_as_subshell() {
    // $() in an AssignmentValue (not an arg Word) — inner curl still counts
    assert!(kinds("x=$(curl http://y)\n").contains(&(EffectKind::NetFsDb, 7)));
}

#[test]
fn process_substitution_inner_command_counts() {
    // <(...) is a borrowed SubshellCommand AST node — walk it; inner curl counts,
    // and the pseudo-file is NOT an fs operand for the outer grep.
    assert!(kinds("grep pat <(curl http://y)\n").contains(&(EffectKind::NetFsDb, 7)));
}

#[test]
fn background_launch_is_concurrency_but_a_plain_pipeline_is_not() {
    assert!(kinds("sleep 1 &\n").contains(&(EffectKind::Concurrency, 6)));   // background job escapes
    // a plain multi-stage pipeline is bounded/joined — NO concurrency effect (only the stages' own effects)
    assert!(!kinds("a | b\n").iter().any(|(k, _)| *k == EffectKind::Concurrency));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell redirections command_substitution` → Expected: FAIL.

- [ ] **Step 3: Emit redirect / concurrency / substitution effects over the shared walk**

The structural descent (control-flow recursion, `subshell` tagging, `redirs`, `stdin_is_here`, process-substitution descent) already lives in `walk.rs` (Task 2) — Task 7 does **not** re-walk; it consumes `walk`/`walk_commands` and emits the effects that depend on that structure. (`subshell` is set by `walk` for `( )`/`&`/`coproc`/`Pipeline.seq.len()>1`/process-sub; a lone single-stage command's wrapping `Pipeline` is **not** a subshell.)

In `detect`, emit redirect fs effects from **both** each `Site::Command`'s `CmdSite.redirs` (SimpleCommand-own redirects) **and** each **`Site::Redirect(io, _)`** (command-level redirects on `Command::Compound`/`Command::ExtendedTest` and the `FnBody::Func` function-redirect list — e.g. `while …; done >out`, `[[ x ]] >out`, `f(){…} >out`): output redirect to a file → `net.fs.db`/7 write; input redirect `< file` → `net.fs.db`/7 read; fd-dups / here-docs / here-strings → no fs effect (set `stdin_is_here` so Task 5's `read`/`mapfile` yields no fs). **Redirect location:** `IoRedirect::location()` is `None` (0.4.0) — take the span from the redirect's target `Word` if it has a `loc`, else the enclosing command span. `<(…)`/`>(…)` operands are **not** file operands for the outer command.

**Concurrency:** `calls::detect` iterates the full `walk(&unit.body, …)` `Site` stream (not just `walk_commands`); for each **`Site::Concurrency(span)`** (surfaced by the walk for a background `&` `SeparatorOperator::Async` or a `coproc` `CompoundCommand::Coprocess` — the latter is **not** a `SimpleCommand` word, so it is NOT in Task 4's `CONCURRENCY` const) emit a **`concurrency`/6** escaping effect at that span. A plain **multi-stage pipeline does NOT emit a concurrency effect** (bounded/joined — spec §4). The `wait`/`jobs`/`disown` builtins still come through `Site::Command` → Task 4's `CONCURRENCY` const.

**Two DIFFERENT substitution shapes — handle each with the right mechanism:**

- **Command substitution `$(…)` / backticks — text, re-parsed at EFFECT level, each detector owns its half.** These are `WordPiece::CommandSubstitution(String)` / `WordPiece::BackquotedCommandSubstitution(String)` inside a `Word.value` string (there is **no** `ProcessSubstitution` word-piece). `walk.rs` (Task 2) exposes a shared helper `pub fn subst_words(sc) -> Vec<&Word>` (all Words of a `SimpleCommand`: command word, args, **and `VAR=val` prefix `AssignmentValue::Scalar/Array` values** — where `x=$(curl …)` lives) and `pub fn subst_programs(word) -> Vec<(SourceSpan /*enclosing Word.loc to re-anchor to*/, Program)>` (word-parse each piece, `tokenize_str`+`parse_tokens` into locally-owned inner `Program`s). **Each detector recurses its own half** (avoids the Task-7-can't-call-Task-8 forward dep): `calls::detect` (Task 7) runs `calls::detect` on the inner program's items for **world effects**; `mutation::detect` (Task 8) runs `mutation::detect` on them for **mutations** and forces `contained = true` (subshell). Both **re-anchor each inner effect's `line`/`col` to the enclosing `Word.loc`** (inner spans are substring-relative), **subtract 0.1 from each merged inner effect's `confidence`** (spec §6 substitution-context delta), and merge the **owned** effects (the inner `Program` drops in-call → no `&'a` leak). This is why Task 8's own `x=$(cd /y && pwd)` test passes when calling `mutation::detect` directly (its own substitution recursion), independent of `calls.rs`. For **process substitution** (walked in place), `calls::detect`/`mutation::detect` likewise subtract 0.1 from effects emitted for any `CmdSite` with `subst == true`.
- **Process substitution `<(…)` / `>(…)` — a BORROWED AST subtree, walked in place (NOT text).** In 0.4.0 these are `CommandPrefixOrSuffixItem::ProcessSubstitution(ProcessSubstitutionKind, SubshellCommand)` (as a command argument) and `IoFileRedirectTarget::ProcessSubstitution(ProcessSubstitutionKind, SubshellCommand)` (as a redirect target) — each carrying a real, already-parsed `SubshellCommand { list: CompoundList, .. }` with the **same `'a` lifetime** as the rest of `unit.body`. So **descend its `list` through the normal `walk_commands` recursion in subshell context** (exactly like a plain `( )` subshell) — do **not** run `word::parse` on it (it is not word text; `grep pat <(curl …)` would otherwise miss the inner `curl`). The pseudo-file operand itself is **not** an fs operand for the outer command (spec §4).

**Redirects on compound commands & function bodies.** A redirect list can hang off a `CompoundCommand` or off the `FunctionBody` (`f(){ … } >out` → `FnBody::Func(&FunctionBody)`'s `.1: Option<RedirectList>`). Attribute these to the **enclosing unit** (emit their fs effects on the unit, using the target `Word` span / enclosing command span per the redirect-location fallback) — `walk_commands`/`detect` must read `unit.body`'s `FnBody::Func(.1)` and any compound-command redirect lists, not only per-`SimpleCommand` redirs.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell` (calls suite) → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/calls.rs
git commit -m "feat(shell): redirections + substitution recursion + subshell-context tagging (refs #15)"
```

---

## Task 8: `detect/mutation.rs` — declaration-vs-hidden model

**Files:**
- Create: `crates/fxrank-lang-shell/src/detect/mutation.rs`
- Modify: `crates/fxrank-lang-shell/src/detect/mod.rs` (add `pub mod mutation;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `bindings::{local_names, script_top_names}`, and the shared `crate::walk::{walk, subst_words, subst_programs}` (Task 2) — mutation.rs iterates the **full `Site` stream**, not just `Site::Command`.
- Produces: `pub fn detect(unit: &FnUnit, top: &HashSet<String>) -> Vec<(Effect, bool)>` — each effect paired with its `contained` flag (mirroring the Python frontend's tuple return). Handles assignments, `cd`/`set`/`shopt`/`umask`/`ulimit`, `export`/`unset`, `shift`/`getopts`, `printf -v`/`read`-into, `${x:=…}`, **assignment prefixes (`VAR=v cmd`)**, and **nested function definitions**.
- **Access paths (real AST):** mutation.rs iterates the shared `crate::walk::walk(&unit.body, …)` **full `Site` stream** (Task 2) — so mutations inside control flow (`if …; then x=1; fi`) are covered, and the three non-`SimpleCommand` sites arrive as their own `Site` variants: (a) `Site::Arithmetic(&ArithmeticCommand, subshell)` for `(( x++ ))`/`(( x=… ))` — its `.expr` is an `UnexpandedArithmeticExpr` (**raw string**) → extract the lvalue with a targeted scan (leading `NAME` before `=`/`++`/`--`/`+=`…) or the `brush_parser::arithmetic` sub-parser. (b) `${x:=word}` lives in a `Word.value` string on a `Site::Command`'s word → `brush_parser::word::parse` (`ParameterExpr::AssignDefaultValues`). (c) `Site::FnDefine(&FunctionDefinition)` for a nested definition → the `fn-define` `global.mutation`/6 on the enclosing unit.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, functions::collect, bindings::script_top_names};
    use fxrank_core::effect::EffectKind;
    use std::collections::HashSet;

    fn muts(src: &str, sym: &str) -> Vec<(EffectKind, u8, bool)> {
        let prog = parse(src).unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh").into_iter().find(|u| u.symbol == sym).unwrap();
        detect(&unit, &top).into_iter().map(|(e, c)| (e.kind, e.class, c)).collect()
    }

    #[test]
    fn declaration_vs_hidden_ladder() {
        // top-level FOO=bar → declaration → no mutation effect
        let prog = parse("FOO=bar\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh").into_iter().find(|u| u.is_script).unwrap();
        assert!(detect(&unit, &top).is_empty());
        // local x=1 → local.mutation/1 contained
        assert!(muts("f(){ local x=1; }\n", "f").contains(&(EffectKind::LocalMutation, 1, true)));
        // bare non-local write in a function → global.mutation/6 escaping
        assert!(muts("f(){ y=1; }\n", "f").contains(&(EffectKind::GlobalMutation, 6, false)));
        // export → env.write/6
        assert!(muts("f(){ export Z=1; }\n", "f").contains(&(EffectKind::EnvWrite, 6, false)));
        // cd → global.mutation/6
        assert!(muts("f(){ cd /x; }\n", "f").contains(&(EffectKind::GlobalMutation, 6, false)));
    }

    #[test]
    fn subshell_mutation_is_contained() {
        // cd inside $(...) does not escape → contained
        let v = muts("f(){ x=$(cd /y && pwd); }\n", "f");
        assert!(v.iter().any(|(k, _, c)| *k == EffectKind::GlobalMutation && *c == true));
    }

    #[test]
    fn indirect_write_is_global_hidden_not_below_named() {
        let prog = parse("f(){ printf -v \"$n\" x; }\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh").into_iter().find(|u| u.symbol == "f").unwrap();
        let e = &detect(&unit, &top)[0].0;
        assert_eq!((e.kind, e.class, e.hidden), (EffectKind::GlobalMutation, 6, true));
    }

    #[test]
    fn nested_function_definition_is_global_mutation() {
        // defining inner() inside outer() installs it into the GLOBAL fn namespace
        let v = muts("outer(){ inner(){ :; }; }\n", "outer");
        assert!(v.iter().any(|(k, _cls, contained)| *k == EffectKind::GlobalMutation && !*contained));
    }

    #[test]
    fn assignment_prefix_is_scoped_env_write() {
        // VAR=v cmd → temporary env for that command (env.write), NOT a script mutation
        let v = muts("f(){ FOO=1 curl http://x; }\n", "f");
        assert!(v.iter().any(|(k, _, _)| *k == EffectKind::EnvWrite));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell mutation` → Expected: FAIL.

- [ ] **Step 3: Implement the mutation model**

**Iterate the full `walk::walk(&unit.body, …)` `Site` stream** (NOT just `walk_commands`), handling each variant:
- `Site::Arithmetic(ac, subshell)` → extract the lvalue (raw-string scan) and apply the assignment rules below (with `contained` forced by `subshell`).
- `Site::FnDefine(_)` → emit `global.mutation`/6 (`contained = false`, subreason `fn-define`) on this unit (only fires for defs *inside* a function body; the walk suppresses top-level ones).
- `Site::Command(site)` → the per-command rules below.
- **Command substitution:** for each `Site::Command`, for each `word` in `walk::subst_words(site.sc)`, for each `(anchor_span, inner_prog)` in `walk::subst_programs(word)`, run `mutation::detect` on a transient `FnUnit { body: FnBody::Script(inner items), … }`, **force `contained = true`** on the results (subshell), re-anchor their `line`/`col` to `anchor_span`, and merge. (This is what makes Task 8's `x=$(cd /y && pwd)` test pass **standalone**, calling `mutation::detect` directly — no dependency on `calls.rs`.)

For each `Site::Command`'s `CmdSite`:
- Determine the written name(s) and the write kind (assignment, `cd`/`set`/…, `export`/`unset`, `shift`/`getopts`, `printf -v`/`read`, `${x:=…}`).
- Compute `locals = local_names(unit)`.
- Apply spec §7:
  - `unit.is_script` **and** the write declares a top name — a plain assignment / `readonly` / `let` / `(( x=… ))` / `${x:=…}`, **and also** a top-level `read x` / `mapfile x` / `getopts … x` **target** (the variable write is a declaration of the script's own global) → **no mutation effect** for the write (skip). Note: a top-level `read`/`mapfile` still emits its `net.fs.db`/7 **input-boundary** effect from `calls.rs` — only the *variable-write* half is a declaration. The hidden-non-local-write rules below apply **only inside a function**.
  - `local`/`declare`/`typeset` (no `-g`/`-x`) / `shift` / `set --` → `LocalMutation`/1, `contained = true`.
  - `local -x`/`declare -x`/`typeset -x` → both a `LocalMutation`/1 **and** an `EnvWrite`/6.
  - name **not** in `locals`, inside a function (bare `x=`, `x+=`, `let x=`, `(( x++ ))`, `${x:=…}`, `declare -g`, `readonly x=`, `read x`, `mapfile x`, `getopts … x`) → `GlobalMutation`/6, `contained = false`.
  - computed target (`printf -v "$var"`, `read "$name"`) → `GlobalMutation`/6, `contained = false`, **`hidden = true`**, `subreason = "indirect-assign"`.
  - `cd`/`pushd`/`popd`/`set`/`shopt`/`umask`/`ulimit` → `GlobalMutation`/6.
  - `export X=`/`export -n`/`declare -x`/`unset X`(exported/global) → `EnvWrite`/6; `unset x`(local) → `LocalMutation`/1.
  - `unset -f name` → `GlobalMutation`/6.
- **Assignment prefix** (`VAR=v cmd` — a `SimpleCommand` with assignments **and** a command word): emit `EnvWrite`/6 for the prefix (scoped, temporary env for that command), `contained = false`; do **not** also treat it as a script mutation. A `SimpleCommand` with assignments and **no** command word is a real assignment → the rules above.
- **Nested function definition**: a `FunctionDefinition` node inside this unit's body → emit `GlobalMutation`/6 on this unit, `contained = false`, `subreason = "fn-define"` (bash installs it into the global fn namespace; spec §8).
- If `CmdSite.subshell` is true, force `contained = true` on mutation effects (world effects unaffected — those come from `calls.rs`).
- **Effect confidence (spec §6):** every emitted mutation `Effect` carries a `confidence` (Task 11 folds them into `function_confidence`): a **literal-target** write (`x=…`, `local x`, `cd`, `export X`, `printf -v x`) → **0.9**; a **computed/indirect target** (`printf -v "$var"`, `read "$name"`, `${!ref}`) → **0.5**; then a substitution-context delta (`-0.1`) — both from the command-substitution recursion above **and** for any mutation emitted on a `CmdSite` with `subst == true` (process substitution). Do not leave `confidence` at the struct default.
- **No discount**: run each contained effect through `apply_boundary_discount(class, BoundaryCoverage::None, contained)` (a no-op) so the "None boundary" is explicit; do not set `discounted_to`. *(This call site is required by spec §7 for auditability but is score-invariant, so no test can observe its presence — keep it in code deliberately.)*

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell mutation` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/mutation.rs crates/fxrank-lang-shell/src/detect/mod.rs
git commit -m "feat(shell): declaration-vs-hidden mutation model + subshell containment (refs #15)"
```

---

## Task 9: `detect/risk.rs` — DestructiveFs, PrivilegeEscalation, DynamicCode

**Files:**
- Create: `crates/fxrank-lang-shell/src/detect/risk.rs`
- Modify: `crates/fxrank-lang-shell/src/detect/mod.rs` (add `pub mod risk;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces: `pub fn detect(unit: &FnUnit, path: &str) -> Vec<RiskFeature>`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, functions::collect};
    use fxrank_core::effect::RiskKind;

    fn risks(src: &str) -> Vec<RiskKind> {
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh").into_iter().find(|u| u.is_script).unwrap();
        detect(&unit, "x.sh").into_iter().map(|r| r.kind).collect()
    }

    #[test]
    fn detects_shell_risks() {
        assert!(risks("rm -rf /x\n").contains(&RiskKind::DestructiveFs));
        assert!(risks("chmod -R 777 /x\n").contains(&RiskKind::DestructiveFs));
        assert!(risks("sudo rm -rf /x\n").contains(&RiskKind::PrivilegeEscalation));
        assert!(risks("eval \"$cmd\"\n").contains(&RiskKind::DynamicCode));
        assert!(risks("curl http://x | sh\n").contains(&RiskKind::DynamicCode));
        assert!(risks("source \"$dir/x\"\n").contains(&RiskKind::DynamicCode)); // computed path
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell risk` → Expected: FAIL.

- [ ] **Step 3: Implement the risk detectors**

Walk `walk_commands(unit)`; for each site call `calls::strip_wrappers` to get the wrapper `kinds` and the inner command:
- inner command `rm` with `-r`/`-rf`/`-R`, `chmod -R`, `chown -R`, `dd`, `shred` → `DestructiveFs`/5.
- **wrapper `kinds` contains `sudo`/`su`/`doas`** → `PrivilegeEscalation`/6 (consume the wrapper kinds from `strip_wrappers` — do **not** inspect the peeled head word, which is the *wrapped* command).
- `eval` (any args), `source`/`.` with a **computed** path arg, and **download-piped-to-shell** → `DynamicCode`/7. **Mechanism:** the pipe form needs pipeline-**adjacency**, so `risk::detect` iterates the shared `walk::walk(&unit.body, …)` for each **`Site::Pipeline(pipe, _)`** (surfaced by the walk, incl. pipelines nested in `if`/`for`/`while`/`case`, so `if …; then curl … | sh; fi` is caught) and inspects `pipe.seq` for a net-fetch stage (`curl`/`wget`/…) immediately followed by a shell interpreter stage (`sh`/`bash`/`zsh`/`dash`). The substitution form (`sh -c "$(curl …)"` / `bash -c \`curl …\``) is caught via `walk::subst_programs` on a shell-interpreter command's `-c` argument `Word` (a command-substitution piece containing a net-fetch). (Grammar per spec §5; downloaded-temp-file-then-exec is out.)
- Unquoted variable in a destructive command → the `-0.1` confidence delta is applied to the paired `net.fs.db` **effect** in **`calls.rs` (Task 5, already implemented + tested there)**, **not** on the risk — `RiskFeature` has no confidence field. Task 9 emits only the `DestructiveFs` risk; it does **not** touch confidence or `calls.rs`.

Build each `RiskFeature { kind, class: kind.class(), weight: weight_for_class(kind.class()), path, line, col, evidence, tier: Tier::Heuristic }`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell risk` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/risk.rs crates/fxrank-lang-shell/src/detect/mod.rs
git commit -m "feat(shell): DestructiveFs + PrivilegeEscalation + DynamicCode risk detectors (refs #15)"
```

---

## Task 10: `detect/refs.rs` — same-file call refs + `source` reach + `canonical_path`

**Files:**
- Create: `crates/fxrank-lang-shell/src/detect/refs.rs`
- Modify: `crates/fxrank-lang-shell/src/detect/mod.rs` (add `pub mod refs;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub fn canonical_path(unit: &FnUnit) -> Vec<String>` — `[path, "fn", name]` for a function, `[path, "<script>"]` for the script unit (unique per file).
  - `pub fn refs(unit: &FnUnit, fns: &HashSet<String>) -> Vec<CallSiteRef>` — per command site, call `calls::strip_wrappers` to get the resolution mode; when `ResMode::Normal` **and** the inner head word matches a **same-file** function name: a `CallSiteRef` with `resolved_target = Some([path,"fn",name])`, `qualified: false`, `first_party: true`. When the mode is `FunctionBypass`/`BuiltinOnly` (`sudo docker`, `command docker`), **do not** resolve to a same-file function. For a `source`/`.` with a **literal path arg** (`./x.sh`, `/abs/x.sh`, or a bare name): a `CallSiteRef` with `base = <the literal path string>` (so the fold's reach specifier = `module.unwrap_or(base)` is **path-keyed**, not `"source"`), `module: None`, `resolved_target: None`, `qualified: true`, `first_party: false` — this makes `resolve_ref_precise` return `Edge::Opaque`, and the **core fold synthesizes a `ThirdParty` `external.unresolved`/2 reach** from it (there is **no** `UnitRecord` external-reach side channel — reaches come only from unresolved qualified refs; `first_party` stays `false` because a pass-1 frontend has no scanned-file set, so **all** source reaches are uniformly `ThirdParty` in Milestone A — precise `FirstPartyOutOfScope` is M-B, spec §9). For a `source` with a **computed path** (`"$dir/x"`) **emit NO `CallSiteRef`** (the `base` would be garbage) — the `DynamicCode` risk (Task 9) + the own `process.control`/6 effect already represent it. The own `process.control`/6 effect for `source`/`.` is emitted in `calls.rs` (the explicit `Cls::Effect(ProcessControl, 6)` arm), giving spec §15's deliberate divergence: own `process.control`/6 **plus** the fold's `external.unresolved`/2.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, functions::{collect, defined_function_names}};

    #[test]
    fn same_file_function_call_is_resolved_target() {
        let src = "greet(){ echo hi; }\nmain(){ greet; }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh").into_iter().find(|u| u.symbol == "main").unwrap();
        let r = refs(&main, &fns);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].resolved_target, Some(vec!["x.sh".into(),"fn".into(),"greet".into()]));
        assert!(r[0].first_party && !r[0].qualified);
    }

    #[test]
    fn source_is_opaque_path_keyed_ref() {
        let src = "main(){ source ./lib.sh; }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh").into_iter().find(|u| u.symbol == "main").unwrap();
        let r = refs(&main, &fns);
        // opaque + qualified + base is the PATH (so the fold reach is path-keyed, not "source")
        assert!(r.iter().any(|x| x.qualified && x.resolved_target.is_none() && x.base == "./lib.sh"));
    }

    #[test]
    fn wrapped_command_word_is_not_a_same_file_function() {
        // spec §4: `sudo docker` / `command docker` must NOT resolve to a same-file docker()
        let src = "docker(){ :; }\nmain(){ sudo docker ps; command docker ps; }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh").into_iter().find(|u| u.symbol == "main").unwrap();
        let r = refs(&main, &fns);
        assert!(!r.iter().any(|x| x.resolved_target == Some(vec!["x.sh".into(),"fn".into(),"docker".into()])));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell refs` → Expected: FAIL.

- [ ] **Step 3: Implement `canonical_path` + `refs`**

Per the interface. For a call word under a non-`Normal` resolution mode (Task 6), **skip** same-file function resolution (it's an external/builtin call). For `source`/`.`: emit the opaque `CallSiteRef` (`resolved_target: None, qualified: true`) so `resolve_ref_precise` returns `Edge::Opaque`. Build `CallSiteRef` with the real `RefKind::Free`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell refs` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/refs.rs crates/fxrank-lang-shell/src/detect/mod.rs
git commit -m "feat(shell): same-file call refs + source opaque ref + canonical_path (refs #15)"
```

---

## Task 11: `detect/mod.rs` — `analyze_unit` + `build_record`

Assemble effects+risks into a scored `Hotspot` and a neutral `UnitRecord`.

**Files:**
- Modify: `crates/fxrank-lang-shell/src/detect/mod.rs`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `calls::detect`, `mutation::detect`, `risk::detect`, `refs::{refs, canonical_path}`.
- Produces:
  - `pub fn analyze_unit(unit: &FnUnit, fns: &HashSet<String>, top: &HashSet<String>) -> Hotspot`
  - `pub fn build_record(unit: &FnUnit, fns: &HashSet<String>, top: &HashSet<String>) -> UnitRecord`
  - **No reach side channel.** `external_reaches` are **not** carried on `UnitRecord`; they are produced downstream by the CLI's cross-file fold (`resolve_ref_precise` → `synthesize_opaque_effect`) from the **opaque qualified `source` refs** that `build_record` puts in `UnitRecord.refs` (Task 10). `build_record`'s only job here is to populate `refs` (incl. the source opaque refs) and `canonical_path`; the fold does the rest — same as the other frontends.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, functions::{collect, defined_function_names}, bindings::script_top_names};

    #[test]
    fn sudo_rm_outranks_bare_rm_on_risk_weight() {
        let one = analyze("f(){ rm -rf /x; }\n", "f");
        let two = analyze("f(){ sudo rm -rf /x; }\n", "f");
        assert_eq!((one.max_class, two.max_class), (7, 7));
        assert_eq!(one.risk_weight, 8);   // weight_for_class(5)
        assert_eq!(two.risk_weight, 13);  // weight_for_class(6) — PrivilegeEscalation wins
        assert!(two.risk_weight > one.risk_weight);
    }

    fn analyze(src: &str, sym: &str) -> fxrank_core::model::Hotspot {
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh").into_iter().find(|u| u.symbol == sym).unwrap();
        analyze_unit(&unit, &fns, &top)
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell sudo_rm_outranks` → Expected: FAIL.

- [ ] **Step 3: Implement `analyze_unit` + `build_record`**

Mirror `crates/fxrank-lang-rust/src/detect/mod.rs::analyze_unit` (read it as the worked example): `gather` = `calls::detect` + `mutation::detect` (wire the `contained` bool onto each `Effect` exactly as the Python frontend does) + `risk::detect`; then `risk_class = risks.iter().map(|r| r.class).max().unwrap_or(0)`, `risk_weight = if risks.is_empty() {0} else {weight_for_class(risk_class)}`, `max_class = max_class(&effect_classes, risk_class)`, `own_score = own_score(&weights)`, `confidence = function_confidence(&effect_confidences)` (weakest-link min; add a synthetic entry only if you model awaits — shell has none). Build the `Hotspot` with `id`/`symbol`/`path`/`line`/`risk_weight`/`confidence`/`effects`/`risk_features` and `..Hotspot::own_seed(own_score, max_class)`. `build_record` fills `UnitRecord` (`unit_id`, `path`, `line`, `col`, `symbol`, `is_root: false`, `canonical_path: refs::canonical_path(unit)`, `aliases: vec![]`, `effects`, `risks`, `refs: refs::refs(unit, fns)`, `async_boundary: false`, `await_count: 0`, `language: Language::Shell`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank-lang-shell` → Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/detect/mod.rs
git commit -m "feat(shell): analyze_unit + build_record assembly (refs #15)"
```

---

## Task 12: `lib.rs` — `CORPUS_PROFILE` + `Frontend` impl (analyze loop)

Replace the Task-0 smoke shell with the real frontend.

**Files:**
- Modify: `crates/fxrank-lang-shell/src/lib.rs`
- Create: `crates/fxrank-lang-shell/tests/fixtures/deploy.sh`, `.../pipeline.sh`
- Test: inline `#[cfg(test)]` + an `insta` snapshot test

**Interfaces:**
- Produces: `pub struct ShellFrontend { pub include_tests: bool }` impl `Frontend`; `pub const CORPUS_PROFILE`.

- [ ] **Step 1: Write the failing test + fixtures**

`deploy.sh` (a destructive/deploy fixture) and a snapshot test:

```rust
#[test]
fn analyze_never_panics_on_garbage() {
    let out = ShellFrontend::default().analyze(&[SourceFile{ path: "g.sh".into(), text: "if then )(".into() }]);
    assert_eq!(out.diagnostics.len(), 1);
    assert!(!out.diagnostics[0].parsed);
}

#[test]
fn deploy_fixture_snapshot() {
    let src = std::fs::read_to_string("tests/fixtures/deploy.sh").unwrap();
    let out = ShellFrontend::default().analyze(&[SourceFile{ path: "deploy.sh".into(), text: src }]);
    let syms: Vec<_> = out.functions.iter().map(|h| (h.symbol.clone(), h.max_class)).collect();
    insta::assert_debug_snapshot!(syms);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank-lang-shell analyze_never_panics` → Expected: FAIL.

- [ ] **Step 3: Implement the frontend**

Mirror `crates/fxrank-lang-rust/src/lib.rs`:

```rust
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &[],
    exclude_file_globs: &[],
    test_file_globs: &["*_test.sh", "test_*.sh"],
    prune_marker_files: &[],
};

#[derive(Default)]
pub struct ShellFrontend { pub include_tests: bool }

impl Frontend for ShellFrontend {
    fn language(&self) -> Language { Language::Shell }
    fn corpus_profile(&self) -> CorpusProfile { CORPUS_PROFILE }
    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
        let mut output = FrontendOutput::default();
        for source in files {
            // File-name test-skip (shell has no in-file marker).
            if !self.include_tests && is_test_file(&source.path) { output.skipped_tests += 1; continue; }
            match parse(&source.text) {
                Err(e) => output.diagnostics.push(Diagnostic { path: source.path.clone(), parsed: false, error: e }),
                Ok(prog) => {
                    let fns = functions::defined_function_names(&prog);
                    let top = bindings::script_top_names(&prog);
                    for unit in functions::collect(&prog, &source.path) {
                        output.functions.push(detect::analyze_unit(&unit, &fns, &top));
                        output.records.push(detect::build_record(&unit, &fns, &top));
                    }
                }
            }
        }
        output
    }
}
```

`is_test_file` mirrors the sibling call pattern (`fxrank-lang-python/src/lib.rs`, `-ts/src/lib.rs`): build the matcher from `CORPUS_PROFILE.test_file_globs` and call `matches_test_file(&source.path)` — which takes the **full path** and extracts the basename internally (do not pre-basename it). **Semantic note:** shell increments `skipped_tests` by **1 per skipped file** (it doesn't parse them), whereas Python parses and counts skipped *units*; this per-language divergence in the same wire field is intentional (a shell test file's unit count is unknown without parsing) — record it in the corpus guideline (Task 15). Declare the modules (`pub mod functions; pub mod bindings; pub mod detect;`).

- [ ] **Step 4: Run tests to verify they pass, accept the snapshot**

Run: `cargo test -p fxrank-lang-shell` then `cargo insta review` (accept the `deploy_fixture_snapshot`).
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-shell/src/lib.rs crates/fxrank-lang-shell/tests/
git commit -m "feat(shell): CORPUS_PROFILE + Frontend impl (analyze loop, file-name test-skip) (refs #15)"
```

---

## Task 13: CLI wiring

**Files:**
- Modify: `crates/fxrank-cli/Cargo.toml` (optional dep + feature)
- Modify: `crates/fxrank-cli/src/main.rs` (`Route`, `route_for_path`, `--lang`, `dispatch_shell`, `default_corpus_profiles`, help strings)
- Test: inline `#[cfg(test)]` in `main.rs`

**Interfaces:**
- Consumes: `fxrank_lang_shell::{ShellFrontend, CORPUS_PROFILE}`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn routes_sh_and_bash_to_shell() {
    assert!(matches!(route_for_path(std::path::Path::new("a.sh")), Some(Route::Shell)));
    assert!(matches!(route_for_path(std::path::Path::new("a.bash")), Some(Route::Shell)));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p fxrank routes_sh_and_bash` → Expected: FAIL.

- [ ] **Step 3: Implement CLI wiring**

`Cargo.toml`:
```toml
fxrank-lang-shell = { path = "../fxrank-lang-shell", version = "0.4.1", optional = true }
# in [features]:
shell = ["dep:fxrank-lang-shell"]
default = ["rust", "ts", "python", "shell"]
```

`main.rs`:
- add `Shell` to `enum Route`; in `route_for_path`, `"sh" | "bash" => Some(Route::Shell)`.
- `--lang`: accept `"shell"` → `Route::Shell` for stdin; extend the unknown-`--lang` error text and the `about`/`--lang` help strings to list shell.
- `dispatch`: `Route::Shell => shell_sources.push(r.source);` then `merge_output(&mut output, dispatch_shell(shell_sources, include_tests));`
- `#[cfg(feature = "shell")] fn dispatch_shell(sources, include_tests) { ShellFrontend { include_tests }.analyze(&sources) }`.
- `#[cfg(not(feature = "shell"))] fn dispatch_shell(sources, _) -> FrontendOutput` must **emit a `Diagnostic { parsed: false, error: "shell feature not enabled" }` per source** (mirror the existing `dispatch_python` `#[cfg(not)]` arm in `main.rs` — do **not** return `FrontendOutput::default()`, which would silently drop routed `.sh` files and skew the parsed/file counts).
- `default_corpus_profiles`: `#[cfg(feature="shell")] out.push(fxrank_lang_shell::CORPUS_PROFILE);`
- **Publishing (spec §11.6) is deliberately out of scope for this feature branch.** The new crate's internal pin tracks the current `[workspace.package].version` (`0.4.1`); the actual version **bump + adding `fxrank-lang-shell` to the ordered publish list** (before the `fxrank` binary, after the other libs) happens at **release time** per `CLAUDE.md`'s "Releasing to crates.io", not here. Note this in the crate's `Cargo.toml` proximity but do not bump in this branch.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p fxrank` and `cargo run -p fxrank -- scan crates/fxrank-lang-shell/tests/fixtures/deploy.sh | jq .` → Expected: routed, valid JSON.

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-cli/Cargo.toml crates/fxrank-cli/src/main.rs
git commit -m "feat(cli): route .sh/.bash + --lang shell + dispatch_shell + corpus profile (refs #15)"
```

---

## Task 14: CI — slim build + dogfood scan

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add the slim-build + dogfood lines**

Mirror the existing per-language entries: add
`cargo build -p fxrank --no-default-features --features shell`
and a dogfood step `cargo run -p fxrank -- scan crates/fxrank-lang-shell/tests/fixtures/ > /dev/null`.

- [ ] **Step 2: Verify locally**

Run: `cargo build -p fxrank --no-default-features --features shell` → Expected: builds.
Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` → Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: shell slim build + dogfood scan (refs #15)"
```

---

## Task 15: Dogfood + shared-knowledge doc updates

**Files:**
- Modify: `docs/mutation-classification-guideline.md`, `docs/corpus-profile-guideline.md`, `docs/cross-file-resolution-guideline.md`, `docs/adding-a-language-frontend.md`, `CLAUDE.md`
- Create: `docs/superpowers/plans/029-dogfood-deltas.md`

- [ ] **Step 1: Dogfood on a real shell corpus**

Run: `cargo run -p fxrank -- scan <a real shell/dotfiles repo> | jq '.hotspots[0:10]'`
Sanity-check: destructive/deploy/network functions surface high (class 6–7), echo-only helpers low (class ≤2), the `<script>` unit present. Record intentional deltas / false-positives in `029-dogfood-deltas.md` (mirror `008-dogfood-deltas.md`).

- [ ] **Step 2: Add the Shell column/bullet to each guideline**

Per spec §15's per-language **honesty notes** (copy them): mutation guideline (declaration-vs-hidden line, subshell containment, `hidden:true`+`global.mutation` first use, `global.mutation` vs `env.write` axes), corpus guideline (file-name-based test-skip), cross-file guideline (same-file-only via `canonical_path`, `source` as path-keyed opaque `process.control`/6 diverging from `external.unresolved`/2), authoring guide (add Shell as a worked example), and note the two new `RiskKind`s in CLAUDE.md's vocabulary discussion.

- [ ] **Step 3: Full gate + commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` → Expected: pass.

```bash
git add docs/ CLAUDE.md
git commit -m "docs(shell): dogfood deltas + shared-knowledge guideline updates (refs #15)"
```

---

## Self-Review (author checklist — completed)

- **Spec coverage:** §3 parser → T0; §5 risks + core vocab → T1; §8 units + nested-fn → T2/T8; §7 mutation/bindings (declaration-vs-hidden, subshell, prefixes, indirect, arithmetic/word paths) → T3/T8; §4 classifier/stream-filter/wrappers+resolution-mode/redirection/subshell/concurrency → T4–T7; §5 risk detectors (wrapper-kind-driven privilege) → T9; §9 cross-file same-file-precedence + `source` path-keyed opaque + `canonical_path` → T4/T10/T11; scoring assembly → T11; §10 corpus + §1 frontend → T12; §11 wiring (incl. `cfg(not)` diagnostics, publishing-deferred note) → T13; CI → T14; §12 dogfood + §15 docs → T15. All spec sections map to a task.
- **API accuracy:** the "Confirmed brush-parser 0.4.0 API" block (verified from crate source) pins the 2-arg `parse_tokens`, `SourceLocation::location()`, the nested `Program` tree (`FnBody::{Script(Vec<&CompoundListItem>), Func(&FunctionBody)}`), all 10 `CompoundCommand` variants incl. `ArithmeticForClause`, `Pipeline.timed`/`SeparatorOperator::Async`, `CommandPrefixOrSuffixItem::ProcessSubstitution`, and the `ArithmeticCommand`/`Word`-text access paths. Core-API usages verified against `fxrank-core` (no `UnitRecord` reach side channel — reaches come from the fold; `RiskFeature` has no confidence — unquoted-var lowers the effect's).
- **Placeholder scan:** the only `unimplemented!` is the Task-0 spike's RED state (removed in T0 Step 4). No "TBD"/"handle edge cases"/"similar to Task N".
- **Type consistency:** `FnBody`/`FnUnit`, the **single shared** `walk`/`walk_commands`/`CmdSite`/`Site` (Task 2 `walk.rs`, consumed by Tasks 3/4/8/9 — no duplicate descent), `strip_wrappers`/`CommandView`/`ResMode` (returned, not stored on `CallSiteRef`), `Cls`/`classify_command(name, has_v)`, `local_names`/`script_top_names`, `canonical_path`/`refs`, `analyze_unit`/`build_record` are named identically across producing/consuming tasks; no forward dependency (`walk` defined in T2 before all consumers); command-substitution recursion is **effect-level** via `walk::subst_programs` (each detector recurses its own half); classes (`DestructiveFs`/5, `PrivilegeEscalation`/6) match Task 1 and the core-verified spec.
