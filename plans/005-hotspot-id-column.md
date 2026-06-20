# Column-Disambiguated Hotspot IDs — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every hotspot `id` unique by changing the wire format from `path:line:symbol` to `path:line:col:symbol` in both frontends, so two anonymous functions on one source line no longer collide (issue #9).

**Architecture:** `col` is the **1-based character column** of the *same* span anchor that already produces `line` at each collection site — anchor consistency is the load-bearing invariant. TS routes both coordinates through one new `SpanLines::line_col(span)` call per site (so they cannot diverge); Rust reads `line`/`column` off the single `LineColumn` already in hand. Anonymous TS symbols additionally gain a `C{col}` suffix (`<arrow@L279C55>`). No new wire field on `Hotspot`; `col` lives only inside the `id` string, exactly as `line` does today.

**Tech Stack:** Rust, Cargo workspace; swc (`swc_common::SourceMap::lookup_char_pos`) for TS; `syn` + `proc-macro2` (`Span::start() -> LineColumn`) for Rust; `insta` for snapshot tests.

**Spec:** `specs/005-hotspot-id-column.md`. Read it first — it is source of truth for the format, the 1-based char-column semantics, and the anchor-consistency invariant.

**Branch:** Implementation is code, so it goes on a feature branch (e.g. `feat/005-hotspot-id-column`), never on `main`. Docs-only Task 5 may alternatively land on `main`, but keeping it on the same branch + PR is simpler.

---

## File map

| File | Change |
|---|---|
| `crates/fxrank-lang-ts/src/source.rs` | **Add** `SpanLines::line_col(span) -> (usize, usize)` (1-based line, 1-based char col) + unit tests. |
| `crates/fxrank-lang-ts/src/functions.rs` | `push` gains a `col` param; id format → 4-field; 9 collection sites switch `lines.line(span)` → `lines.line_col(span)`; anonymous symbol fallbacks gain `C{col}`; doc comments updated. |
| `crates/fxrank-lang-ts/tests/ts_frontend.rs` | **Add** #9 regression test; update one stale comment. |
| `crates/fxrank-lang-rust/src/functions.rs` | 3 id-format sites read `start().line` + `start().column + 1`; id format → 4-field; doc comment updated. |
| `crates/fxrank-lang-rust/tests/rust_frontend.rs` | **Add** a test pinning the `path:line:col:symbol` shape + 1-based col. |
| `specs/001-fxrank-rust-effect-scanner.md` | Amend the `id` paragraph (~line 252) and the Function-unit rationale row (~line 525). |

No `Hotspot`/`FnUnit` struct field is added. Snapshots (`*.snap`) are **not expected to churn** — their `summarize`/`summary` builders omit `id` — but Task 6 runs the full suite to confirm.

---

## Task 1: TS — `SpanLines::line_col` helper

**Files:**
- Modify: `crates/fxrank-lang-ts/src/source.rs`
- Test: `crates/fxrank-lang-ts/src/source.rs` (its `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `source.rs`. **Reuse the existing `test_file(src) -> (cm, fm)` helper and anchor spans at `fm.start_pos.0 + offset`** — exactly as the existing `spanlines_resolves` test does (source.rs ~line 89). This is load-bearing: swc assigns the first source file `start_pos.0 = 1`, so an absolute `BytePos(4)` is **not** byte offset 4. Anchoring relative to `fm.start_pos` keeps the offsets meaning "Nth byte of the source" and immune to the base. `Span`/`BytePos` are already in scope via `super::*` (imported at source.rs top) — no new `use` needed.

```rust
#[test]
fn line_col_is_one_based_for_line_and_column() {
    // `a` of `ab` is the 5th column (1-based) of line 1 ("let " = 4 chars).
    let (cm, fm) = test_file("let ab = 1;\n");
    let lines = SpanLines::new(cm);
    let pos = swc_common::BytePos(fm.start_pos.0 + 4); // byte offset of `a`
    assert_eq!(lines.line_col(Span::new(pos, pos)), (1, 5));
}

#[test]
fn line_col_counts_characters_not_display_width() {
    // A leading tab is ONE character: the `x` after it is col 2, not col 9.
    // (col_display would report 8 for the tab; we use the char column.)
    let (cm, fm) = test_file("\tx = 1;");
    let lines = SpanLines::new(cm);
    let pos = swc_common::BytePos(fm.start_pos.0 + 1); // byte offset of `x`
    assert_eq!(lines.line_col(Span::new(pos, pos)), (1, 2));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p fxrank-lang-ts --lib line_col`
Expected: FAIL — `no method named line_col found for struct SpanLines`.

- [ ] **Step 3: Add the `line_col` method**

In `impl SpanLines`, alongside `line` and `line_of`:

```rust
/// Resolve a span's start to a 1-based `(line, column)`. The column is the
/// 1-based **character** column (Unicode scalar count, not byte/UTF-16/display
/// width): swc's `CharPos` is 0-based, so add 1. Both coordinates come from a
/// single `lookup_char_pos`, so callers get an anchor-consistent `(line, col)`.
pub fn line_col(&self, span: Span) -> (usize, usize) {
    let loc = self.cm.lookup_char_pos(span.lo);
    (loc.line, loc.col.0 + 1)
}
```

(`Loc.col` is `CharPos(pub usize)`, so `.0` reads the 0-based column; `+ 1` makes it 1-based. `col_display` is the display-width variant — deliberately not used.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p fxrank-lang-ts --lib line_col`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/fxrank-lang-ts/src/source.rs
git commit -m "feat(ts): add SpanLines::line_col for 1-based char column"
```

---

## Task 2: TS — thread `col` into the id + anonymous `C{col}` suffix (#9 regression)

**Files:**
- Modify: `crates/fxrank-lang-ts/src/functions.rs`
- Test: `crates/fxrank-lang-ts/tests/ts_frontend.rs`

- [ ] **Step 1: Write the failing regression test**

Append to `ts_frontend.rs`. Uses `functions::parse_and_collect`, which takes an inline `&str` (no fixture file needed).

```rust
#[test]
fn anonymous_fns_on_same_line_get_distinct_ids() {
    // Two anonymous arrows on one physical line — issue #9.
    let src = "foo().then(() => {}).catch(() => {});";
    let units = functions::parse_and_collect(src, "t.ts", Lang::Ts).expect("parse");

    let arrows: Vec<_> = units
        .iter()
        .filter(|u| u.symbol.starts_with("<arrow@L"))
        .collect();
    assert_eq!(arrows.len(), 2, "both arrows collected");

    // The bug: identical ids. The fix: distinct (column disambiguates).
    assert_ne!(arrows[0].id, arrows[1].id, "same-line arrows must have distinct ids");

    // Every id in the report is unique.
    let ids: Vec<&String> = units.iter().map(|u| &u.id).collect();
    let unique: std::collections::HashSet<&&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "all hotspot ids are unique");

    // 4-field shape `path:line:col:symbol` and the symbol carries C{col}.
    for a in &arrows {
        assert!(a.symbol.starts_with("<arrow@L") && a.symbol.contains('C'),
            "anonymous symbol carries column: {}", a.symbol);
        assert!(a.id.starts_with("t.ts:1:"), "id is path:line:col:symbol: {}", a.id);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p fxrank-lang-ts --test ts_frontend anonymous_fns_on_same_line`
Expected: FAIL — `assert_ne!` fires (both ids are `t.ts:1:<arrow@L1>` today), and the symbol has no `C`.

- [ ] **Step 3: Change `push` to take a column and emit the 4-field id**

In `functions.rs`, update the `push` signature and id format (around line 186–206):

```rust
    fn push(
        &mut self,
        symbol: String,
        line: usize,
        col: usize,
        is_async: bool,
        is_constructor: bool,
        sig: FnSig,
        body: FnBodyOwned,
    ) {
        let id = format!("{}:{}:{}:{}", self.path, line, col, symbol);
        self.units.push(FnUnit {
            symbol,
            id,
            path: self.path.to_string(),
            line,
            is_async,
            is_constructor,
            sig,
            body,
        });
    }
```

- [ ] **Step 4: Update all 9 collection sites to compute `(line, col)` from one anchor**

For each site, replace `let line = self.lines.line(<span>);` with `let (line, col) = self.lines.line_col(<span>);` and add `col` as the new 3rd argument to `push`. The span at each site is **unchanged** — this is what guarantees `(line, col)` share an anchor:

| Method | Span (unchanged) | `push(... )` becomes |
|---|---|---|
| `visit_fn_decl` | `node.ident.span` | `self.push(symbol, line, col, f.is_async, false, sig, body_of_function(f))` |
| `visit_fn_expr` | `f.span` | `self.push(symbol, line, col, f.is_async, false, sig, body_of_function(f))` |
| `visit_arrow_expr` | `node.span` | `self.push(symbol, line, col, node.is_async, false, sig, body)` |
| `visit_method_prop` | `f.span` | `self.push(symbol, line, col, f.is_async, false, sig, body_of_function(f))` |
| `visit_getter_prop` | `node.span` | `self.push(symbol, line, col, false, false, sig, body)` |
| `visit_setter_prop` | `node.span` | `self.push(symbol, line, col, false, false, sig, body)` |
| `collect_class_method` | `f.span` | `self.push(symbol, line, col, f.is_async, false, sig, body_of_function(f))` |
| `collect_private_method` | `f.span` | `self.push(symbol, line, col, f.is_async, false, sig, body_of_function(f))` |
| `collect_constructor` | `c.span` | `self.push(symbol, line, col, false, true, sig, body)` |

- [ ] **Step 5: Add `C{col}` to the two anonymous symbol fallbacks**

These two sites build the symbol from `line`; now also use `col` (both already in scope after Step 4):

`visit_fn_expr` (was `format!("<fn@L{line}>")`):
```rust
            .unwrap_or_else(|| format!("<fn@L{line}C{col}>"));
```

`visit_arrow_expr` (was `format!("<arrow@L{line}>")`):
```rust
            .unwrap_or_else(|| format!("<arrow@L{line}C{col}>"));
```

- [ ] **Step 6: Update the doc comments to the new format**

In `functions.rs`:
- Module doc (~lines 25–27): change `<arrow@L{line}>` → `<arrow@L{line}C{col}>` and `<fn@L{line}>` → `<fn@L{line}C{col}>`.
- `FnUnit.symbol` doc (~line 86): `<arrow@L{line}>` → `<arrow@L{line}C{col}>`.
- `FnUnit.id` doc (~line 88): `Collision-resistant id: path:line:symbol.` → `Collision-resistant id: path:line:col:symbol (col is the 1-based char column).`

- [ ] **Step 7: Run the regression test + the whole TS crate**

Run: `cargo test -p fxrank-lang-ts`
Expected: PASS. In particular `collects_all_function_forms` still passes — its `starts_with("<arrow@L")` assertion holds for `<arrow@L5C…>`, and its count is unchanged.

- [ ] **Step 8: Fix the one stale comment**

In `ts_frontend.rs` ~line 72, the comment lists `<arrow@L5>`; update it to `<arrow@L5C{col}>` (e.g. `<arrow@L5C…>`) so the doc matches reality. (Comment only — no assertion change.)

- [ ] **Step 9: Commit**

```bash
git add crates/fxrank-lang-ts/src/functions.rs crates/fxrank-lang-ts/tests/ts_frontend.rs
git commit -m "feat(ts): column-disambiguate hotspot ids (path:line:col:symbol)

Two anonymous functions on one line no longer collide: id gains the
1-based char column, and anonymous symbols carry a C{col} suffix. Each
collection site routes (line, col) through one SpanLines::line_col call
so both coordinates share an anchor. Closes #9 for the TS frontend."
```

---

## Task 3: Rust — `col` field in the id

**Files:**
- Modify: `crates/fxrank-lang-rust/src/functions.rs`
- Test: `crates/fxrank-lang-rust/tests/rust_frontend.rs`

- [ ] **Step 1: Write the failing test**

Append to `rust_frontend.rs`. Reuse the file's existing collection helper if one returns `FnUnit`s with `id` (search for a `collect`/`units` helper near the top); otherwise call `fxrank_lang_rust::functions::collect` on a parsed `syn::File`. The test pins the 4-field shape and a 1-based column.

```rust
#[test]
fn id_includes_one_based_column() {
    let src = "fn foo() {}\n";
    let file = syn::parse_file(src).expect("parse");
    let units = fxrank_lang_rust::functions::collect(&file, "t.rs");
    let foo = units.iter().find(|u| u.symbol == "foo").expect("foo unit");
    // `fn foo` — the ident `foo` starts at column 4 (1-based) on line 1.
    assert_eq!(foo.id, "t.rs:1:4:foo");
}
```

(Confirm `functions::collect(&file, path)` is the public entry — it is used by `RustFrontend::analyze`. If the integration test already has a thinner helper, prefer it.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p fxrank-lang-rust --test rust_frontend id_includes_one_based_column`
Expected: FAIL — id is `t.rs:1:foo` (3-field), not `t.rs:1:4:foo`.

- [ ] **Step 3: Add the column at all three id sites**

In `functions.rs`, for each of the three sites (free fn ~line 77, impl method ~line 124, trait-default method ~line 158), read both coordinates from the single `start()` and widen the id format. Pattern (free-fn site shown; apply the same shape to the other two, using their existing `…sig.ident.span()`):

```rust
                let start = f.sig.ident.span().start();
                let line = start.line;
                let col = start.column + 1; // proc-macro2 column is 0-based
                // ...
                out.push(FnUnit {
                    id: format!("{path}:{line}:{col}:{symbol}"),
                    // ...unchanged...
```

For the impl-method and trait-default sites the local is `method.sig.ident.span()`. Keep the existing `line` field on `FnUnit` (now sourced from `start.line`); do **not** add a `col` field.

- [ ] **Step 4: Update the `FnUnit.id` doc comment**

`functions.rs` ~line 24: `Collision-resistant id: path:line:symbol.` → `Collision-resistant id: path:line:col:symbol (col is the 1-based char column).`

- [ ] **Step 5: Run the test + the whole Rust crate**

Run: `cargo test -p fxrank-lang-rust`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/fxrank-lang-rust/src/functions.rs crates/fxrank-lang-rust/tests/rust_frontend.rs
git commit -m "feat(rust): add 1-based column to hotspot ids (path:line:col:symbol)

Uniform 4-field id schema across frontends. Rust never collides (closures
roll up), but adopts the same shape so ids don't vary by frontend."
```

---

## Task 4: Spec 001 amendment (docs)

**Files:**
- Modify: `specs/001-fxrank-rust-effect-scanner.md`

- [ ] **Step 1: Amend the `id` paragraph (~line 252)**

Change `id` is `path:line:symbol` … `line` is the final tiebreak, making ids collision-resistant. to state the format is `path:line:col:symbol`, that `col` is the 1-based character column of the line anchor, and that `(line, col)` — not `line` alone — guarantees per-file uniqueness (anonymous functions can share a line). Cross-reference spec 005.

- [ ] **Step 2: Amend the Function-unit rationale row (~line 525)**

Update the `id = path:line:symbol` text in that table row to `id = path:line:col:symbol`.

- [ ] **Step 3: Commit**

```bash
git add specs/001-fxrank-rust-effect-scanner.md
git commit -m "docs: amend spec 001 id format to path:line:col:symbol (per spec 005)"
```

---

## Task 5: Full gate + dogfood

**Files:** none (verification only).

- [ ] **Step 1: Format + lint**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. (If `push` argument lists trip clippy's `too_many_arguments`, it was already at the threshold before this change — confirm the lint state is unchanged from `main`; do not add an allow without checking.)

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: PASS. If any `*.snap` unexpectedly changed, inspect the diff — only an `id`-bearing snapshot should change. Accept legitimate changes with `cargo insta review`; an unexpected non-id diff is a real regression to investigate, not to rubber-stamp.

- [ ] **Step 3: Dogfood — confirm ids are now unique on our own source**

Run: `cargo run -p fxrank -- scan crates/ | jq '[.hotspots[].id] | (length) as $n | (unique | length) as $u | {total:$n, unique:$u, dupes: ($n-$u)}'`
Expected: `dupes` is `0`.

- [ ] **Step 4: Slim builds (CI parity)**

Run: `cargo build -p fxrank --no-default-features --features rust && cargo build -p fxrank --no-default-features --features ts`
Expected: both succeed.

- [ ] **Step 5: Open the PR**

Push the feature branch and open a PR linking issue #9 (`Closes #9`). Summarize the wire-format change and that it is the closure of spec 004's deferred N2.

---

## Notes for the implementer

- **The invariant that matters:** at every TS site, `(line, col)` comes from ONE `line_col(span)` call on the *same* span the code already used for `line`. Never introduce a second span for the column. This is what makes `(line, col)` a real source position and the `<computed>`-method disambiguation work.
- **Column is 1-based and character-based** in both frontends (swc `CharPos.0 + 1`; proc-macro2 `column + 1`). Not bytes, not UTF-16, not display width — a leading tab is one column.
- **No new wire/struct field.** `col` lives only inside the `id` string. If a consumer later wants structured coordinates, that is a separate additive change (out of scope, per spec 005).
- **`proc-macro2` needs `span-locations`** (already set in `fxrank-lang-rust/Cargo.toml`) or `line`/`column` are `0` — do not remove it.
