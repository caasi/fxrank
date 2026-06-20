# 005 — Column-Disambiguated Hotspot IDs

## Goal

A hotspot `id` must be **unique** — FxRank is built for agents, which key hotspots
by `id` to address them, cache references, and track them across edits. Today the
`id` is `path:line:symbol`, and that is **not unique**: two anonymous functions on
the same physical line of the same file get the same symbol fallback
(`<arrow@L279>`), so the report emits the same `id` more than once with different
`own_score`s (issue #9).

```
…/routes/workspaces.js:279:<arrow@L279>   ← .then(()=>{})
…/routes/workspaces.js:279:<arrow@L279>   ← .catch(()=>{})
```

This is **not** a minification artifact. The collision is caused by two anonymous
arrows/functions sharing a line — idiomatic in hand-written code. Confirmed across
two production repos in the issue #6 field test, on plain `.ts`/`.tsx`/`.js`:

| Pattern | Example | The two anonymous fns |
|---|---|---|
| `.then().catch()` | `….then(()=>{}).catch(()=>{});` | `.then` cb + `.catch` cb |
| Nested JSX handler | `<button onClick={() => setCount((c) => c + 1)}>` | outer handler + inner updater |
| Chained predicate | `(node) => !nodes.find((n) => n.uid === node.uid)` | outer predicate + `.find` predicate |

Distinct ids emitted more than once, **excluding minified files**: Repo D = 15,
Repo C = 10 — all `.then().catch()`, nested JSX handlers, or chained
`.map()/.filter()/.find()`. These are bread-and-butter patterns, exactly the code
FxRank exists to help write. Spec 004 deferred this as "N2"; this spec closes that
deferral.

`path` cannot break the tie (the colliding pair is in the same file), and `line`
cannot either (same line) — so the fix must add a finer coordinate.

## Scope

In scope:

- Change the `id` wire format from `path:line:symbol` to **`path:line:col:symbol`**
  — a uniform 4-field structure — in **both** the Rust (`syn`) and TS/JS (`swc`)
  frontends.
- `col` is the **1-based character column** of the same span anchor that already
  produces `line`, so `(line, col)` always points at one consistent location.
- Anonymous symbol fallbacks additionally carry a `C{col}` suffix:
  `<arrow@L279C55>`, `<fn@L279C55>`. (Named functions/methods keep their existing
  symbol; only the new structural `col` field is added to their `id`.)
- Amend spec 001's `id` contract (it currently claims `line` makes ids
  collision-resistant — now false for anonymous TS functions).

Out of scope (YAGNI / deferred):

- **Structured `line`/`col` wire fields on `Hotspot`.** `line` is not a separate
  field today (it lives only inside `id` and in evidence); `col` follows suit. If a
  consumer later needs machine-parseable coordinates without splitting the `id`
  string, that is a separate, additive change.
- **Call-graph propagation / `inherited_score`**, FFI call-site detection, and the
  other Milestone-B deferrals from spec 001 — untouched.
- **Content/density heuristics for unnamed minified bundles** (spec 004's deferred
  item) — orthogonal; column uniqueness fixes the *id*, not the decision to scan a
  bundle at all.

## Why `path:line:col:symbol` (uniform 4-field), not symbol-only

Two distinct functions can never share a `(line, col)` start position, so
`line + col` is a **guaranteed-unique** key — strictly stronger than a per-line
occurrence index (`#1`, `#2`), which is positional and renumbers when an earlier
arrow is inserted, making it a poor key for an agent caching ids across edits.

The column is promoted to its own colon-delimited **structural field** (rather than
hidden only inside the anonymous symbol) so the `id` schema is **uniform and
machine-parseable**: every `id`, from either frontend and for named or anonymous
functions alike, has the shape `path:line:col:symbol`. A consumer splitting on the
last-three-from-the-right delimiters gets `(line, col, symbol)` without needing to
know whether the function was anonymous or which language produced it. The Rust
frontend never actually collides (closures roll up; it emits no anonymous units),
but it adopts the same 4-field shape so the wire format does not vary by frontend.

The anonymous symbol keeps a redundant `C{col}` suffix (`<arrow@L279C55>`) so the
**`symbol` field is self-sufficient** too: a human skimming `symbol` can tell two
same-line arrows apart, not only their ids.

## Column semantics

- **1-based.** Matches the existing 1-based `line` and the issue's implied `C55`
  notation. Both parsers expose the column **0-based**; the frontends add 1.
- **Character column** — counts Unicode scalar values, **not** bytes, **not**
  UTF-16 code units, **not** display width. Tabs and wide characters therefore do
  not shift the column. This is the stable, parser-native coordinate:
  - **TS/swc:** `cm.lookup_char_pos(pos).col` is a `CharPos` newtype, not a bare
    integer — unwrap it before adding 1. The 1-based char column is
    `cm.lookup_char_pos(pos).col.to_usize() + 1` (equivalently `.col.0 + 1`, since
    `CharPos` is a public tuple struct). `col_display` is the *display-width*
    variant and is deliberately **not** used.
  - **Rust/proc-macro2:** `span.start().column + 1` — `LineColumn.column` is a bare
    0-based, character-based `usize`. (Requires the `span-locations` feature,
    already set in `fxrank-lang-rust/Cargo.toml` and load-bearing for non-zero
    line/col.)
- **Anchor consistency (the load-bearing invariant).** `col` MUST be taken from the
  **exact same `span` / `BytePos` already passed to the line lookup** at each
  collection site — never a second, different span. `(line, col)` is one real source
  position only if both coordinates share an anchor. Concretely: do not anchor `line`
  to a function/property span while anchoring `col` to the method *key* span. The
  TS frontend derives `line` from several different anchors depending on the function
  kind (identifier span for `fn_decl`; the `Function`/arrow span `f.span`/`node.span`
  for fn-exprs, arrows, class methods, private methods, and method-props; the whole
  *property* span `node.span` for getters/setters; `c.span` for constructors), and
  the Rust frontend uses `…ident.span()` for fns/methods/trait-default methods. The
  rule is anchor-agnostic: whatever span feeds `line` at a site, the same span feeds
  `col` at that site. Implementation note: prefer a combined
  `SpanLines::line_col(span) -> (usize, usize)` (parallel to today's `line`/`line_of`
  in `source.rs`) so each site does **one** `lookup_char_pos`, not two.

## Behavior

- **Anonymous TS arrows/fns on the same line** now receive distinct ids and distinct
  symbols:

  ```
  …/routes/workspaces.js:279:55:<arrow@L279C55>   ← .then(()=>{})
  …/routes/workspaces.js:279:71:<arrow@L279C71>   ← .catch(()=>{})
  ```

- **`<computed>`-keyed methods** sharing a line (`[a]() {} [b]() {}`) — which today
  also collide on the `<computed>` fallback — are disambiguated by the new `col`
  field, **provided** the method sites obey the anchor-consistency rule above (their
  `col` comes from the same `f.span` that feeds `line`); it is "free" only as a
  consequence of that rule, not independently of it.
- **Named functions/methods** (both frontends) keep their `symbol` unchanged; their
  `id` gains the `col` field (e.g. `src/user.rs:42:1:save_user`,
  `src/store.rs:10:5:Store::set_name`).
- **Stable tiebreak.** Spec 001's final ranking tiebreak "on `id`" still holds and is
  now strictly more discriminating; no ranking logic changes.

## Spec-001 amendment

Spec 001's id paragraph (currently: "`id` is `path:line:symbol` … `line` is the
final tiebreak, making ids collision-resistant") and the Function-unit row of its
design-rationale table are updated to state the format is `path:line:col:symbol`,
that `col` is the 1-based character column of the line anchor, and that
`(line, col)` — not `line` alone — is what guarantees per-file uniqueness
(anonymous functions can share a line). The amendment cross-references this spec.

## Acceptance

- Scanning `foo().then(() => {}).catch(() => {});` (TS) emits **two hotspots with
  distinct `id`s** (regression for issue #9); today they are identical.
- Every emitted `id`, in both frontends, matches `path:line:col:symbol`; no two
  hotspots in one report share an `id`.
- `col` is 1-based and character-based: a function whose name/anchor starts at the
  first column of its line reports `col = 1`; a leading tab counts as one character
  (`col` unaffected by tab display width).
- Anonymous symbols render as `<arrow@L{line}C{col}>` / `<fn@L{line}C{col}>`; named
  symbols are unchanged.
- The inline `FnUnit.id` doc comments in **both** frontends (currently
  "Collision-resistant id: `path:line:symbol`" in `fxrank-lang-ts/src/functions.rs`
  and `fxrank-lang-rust/src/functions.rs`) are updated to the `path:line:col:symbol`
  format, so the code's own documentation matches the wire format.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -D warnings`,
  and `cargo fmt --check` all pass; insta snapshots are regenerated to include the
  `col` field.
