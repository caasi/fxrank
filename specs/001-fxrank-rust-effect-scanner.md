# 001 — FxRank Rust Effect Scanner (Milestone A)

## Goal

Build the first FxRank milestone: a fast, primarily **syntactic** analyzer that
scans Rust source, computes a per-function **own effect score**, ranks functions as
hotspots, and emits compact JSON to stdout for consumption by coding agents.

This milestone establishes FxRank as a *measuring instrument*. It deliberately
ships no refactoring advice and no call-graph propagation. The central, least-proven
idea it exists to validate is the **containment discount**: Rust's type system makes
some effects declared and bounded (`&mut`, `&self`, ownership), so the same effect
scores *lower* in Rust than in a language where it is invisible. We build Rust first
so we can **dogfood** — run `fxrank` on the FxRank crates themselves and check the
scores and evidence against intuition.

"Primarily syntactic" rather than "purely syntactic": the analyzer parses with
`syn` and never runs a borrow-checker or full type inference, but some signals are
inherently type-dependent (which type a `.lock()` receiver is, whether a `static`
is shared). Those are detected by **heuristics with a mandatory confidence
penalty** (see *Detectability and Confidence*), not claimed as certain. Being honest
about that boundary is a first-class design constraint.

## Scope

In scope:

- A `fxrank scan` subcommand that reads Rust source (stdin or a path) and writes
  compact JSON to stdout.
- A `syn`-based analyzer with no borrow-checker, no name resolution, and no type
  inference; type-dependent signals are explicitly marked heuristic.
- **Own-score only**: each function scored from the effects in its own body.
- The scoring model: severity classes, a convex (Fibonacci) weight map, the
  containment discount expressed as a class down-shift, three output channels
  (score / confidence / risk), and a rank key in which risk participates.
- The Rust effect catalog (see *Effect Catalog*), each signal tagged with a
  detectability tier.
- A lightweight per-file import table (from `use` statements) to improve path
  matching.
- Graceful handling of un-parseable files via a `diagnostics` array.

Out of scope (deferred to later milestones):

- **Call-graph propagation** (`inherited_score` / `total_score`) — the whole point
  of own-score-first is to get the language-specific scoring right before adding
  propagation. (See *Known Limitations* for the gaming hole this leaves open.)
- `fxrank diff`, `top`, `explain`, `suggest`, `budget` subcommands.
- The `lower-effect-score` agent skill and any `suggested_moves` / refactoring
  advice. The tool measures; deciding the move is the agent's job.
- Writing files (`.fxrank/`, `.tsugu/fxrank/`). Output goes to stdout; an agent or
  human redirects if they want a file.
- Scope modifiers `--changed`, `--since <ref>`, `--symbol <name>`, glob scopes.
- JavaScript / TypeScript frontends. The architecture leaves room for them, but no
  code is written for them here.
- Interprocedural / within-crate call summaries.
- A **semantic pass** (name/type resolution) that would upgrade the heuristic
  signals to certain ones; until then they carry a confidence penalty.
- Drop construction→effect linking (needs type resolution); only module-level
  `impl Drop` presence is detected.

## Architecture

A single Cargo **workspace**, one shipped binary, language frontends isolated as
feature-gated crates:

```text
fxrank/
  Cargo.toml                  # [workspace]
  crates/
    fxrank-core/              # lib: effect vocabulary, scoring, model, JSON, Frontend trait
    fxrank-lang-rust/         # lib: syn-based Rust frontend (behind `rust` feature)
    fxrank-cli/               # bin (package `fxrank`): arg parsing, dispatch, orchestration
```

- `fxrank-core` depends on **no** language parser. It defines the `EffectKind`
  vocabulary, the class→weight map, the discount/aggregation/rank logic, the
  hotspot data model, the JSON serialization, and the `Frontend` trait. The
  compiler enforces "core is language-neutral": `syn` only appears in
  `fxrank-lang-rust`.
- Frontends implement a normalization interface so core never sees `syn`:

  ```rust
  // fxrank-core
  pub trait Frontend {
      fn language(&self) -> Language;
      /// Parse the sources and emit per-symbol effect observations (with evidence
      /// and locations). Un-parseable files become diagnostics, not panics.
      fn analyze(&self, files: &[SourceFile]) -> FrontendOutput;
  }
  ```

- `fxrank-cli` dispatches files to a frontend by extension (`.rs` → Rust). With
  only the `rust` feature in this milestone, dispatch trivially routes `.rs`; the
  same skeleton accepts future frontends without changes to core or to the
  dispatch shape. A slim single-language build is possible via
  `cargo build --no-default-features --features rust`.
- Frontend registration is a **static match**, not a dynamic plugin registry
  (YAGNI for one frontend).

The existing single binary crate `fxrank` is restructured into this workspace as
part of this milestone (cheapest while the repo is still a scaffold). The clippy /
rustfmt / edition-2024 / toolchain conventions from spec 000 carry over to every
crate; CI continues to gate `fmt --check`, `clippy --all-targets -- -D warnings`,
and `test`.

## Scoring Model

### Severity class → convex weight

Each effect has an ordinal **severity class** `0..=8` (which *kind* of effect, the
language-neutral ladder) and a convex **weight** used for aggregation. Convexity
makes one severe effect dominate a pile of trivial ones; an ordinal class kept
alongside the weight preserves a legible low-end gradient.

```text
class:  0  1  2  3  4  5   6   7   8
weight: 0  1  2  3  5  8  13  21  34   (Fibonacci)
```

The exact curve is a **calibration parameter**, defaulting to this Fibonacci map;
dogfooding on the FxRank crates is the calibration method.

### Containment discount = class down-shift

When the type system makes an effect declared and bounded, the discount **shifts
the effect down N severity classes** (not an arbitrary point subtraction). The
emitted `weight` is then computed from the **post-discount** class (`discounted_to`),
not the base class. Shifts are per-kind, clamped, and never cross a kind boundary:

- A discount applies **only to the channel it describes** — e.g. `&mut`
  containment lowers *mutation opacity*; it never discounts a sibling IO or panic
  effect in the same function body.
- An externally observable effect never discounts below **class 1**.
- `unsafe` cancels a discount **lexically**: an `unsafe` block that lexically
  encloses the discounted mutation cancels *that mutation's* discount, and an
  `unsafe fn` cancels discounts throughout its body. An `unsafe` block elsewhere in
  the function (not enclosing the mutation) does not cancel it, and no `unsafe`
  cancels discounts on unrelated channels.

### Three channels (score / confidence / risk)

The model never forces every nuance into one scalar:

1. **score** — `class` + `weight` per effect; aggregated into `own_score`.
2. **confidence** — how sure the (sometimes type-dependent) detection is, in
   `[0, 1]` (see *Detectability and Confidence*).
3. **risk** — `risk_features` flags. **Each risk feature carries its own severity
   class**, so risk participates in ranking rather than being a sidelined axis that
   a risk-only function could hide behind:

   | risk feature | class |
   | --- | --- |
   | `transmute`, raw pointer deref, FFI `extern` call, `asm!`, volatile / `ptr::{read,write,copy_nonoverlapping}`, `MaybeUninit`, `*::from_raw`, `get_unchecked` | 7 |
   | generic `unsafe` block / `unsafe fn` / `unsafe impl` | 5 |
   | `Box::leak`, `mem::forget`, `ManuallyDrop` | 4 |
   | module-level `impl Drop` (informational; see *Module-level risk*) | 2 |

   `risk_class` = the highest risk class present; `risk_weight` = the Fibonacci
   weight of `risk_class` (mirrors `max_weight` for effects).

### Aggregation and rank key

Per function:

```text
own_score = max_weight + 0.5 × Σ(other effect weights)        # effects only
max_class = max( highest effect class , risk_class )           # risk participates
```

The `0.5` damping keeps dense effect-soup visible without letting it overpower a
single boundary effect. `own_score` is a scalar suitable for later budgets/deltas.

Because summation re-linearizes convex weights (e.g. many `log` calls can out-sum
one `fs::write`), **ranking does not use `own_score` alone**. The rank key is the
tuple, compared left to right, descending:

```text
( max_class , own_score , risk_weight , confidence )
```

`max_class` first guarantees a single class-7 IO outranks any quantity of class-4
logging. Because `risk_class` feeds `max_class`, a function whose only notable
property is `mem::forget` (`risk_class 4`) ranks at class 4 — **not** as a cheap
class-0 function. `risk_weight` and `confidence` are later tie-breakers.

**Deterministic ordering.** `own_score` is emitted as a JSON number but is always a
half-integer, so ordering uses the integer key `round(own_score × 2)`; `confidence`
ties break on `round(confidence × 100)`. Any remaining tie breaks stably on `id`.
This avoids relying on `f64: Ord` (which Rust does not provide).

## Detectability and Confidence

Every catalog signal is tagged with a **detectability tier**, and `confidence`
follows directly from it. This reconciles "we use `syn`, not types" with a catalog
that necessarily reaches for some type-level facts:

- **`exact`** — a macro name, keyword, or fully-qualified path is syntactically
  unambiguous (`println!`, `panic!`, `unsafe`, `let mut`, an explicit `&mut`
  parameter *binding*). High confidence.
- **`path`** — matched through the lightweight `use` import table (`std::fs::*`,
  `Instant::now`). Confidence is high but reduced when an alias (`use … as …`) or a
  glob import could shadow the path.
- **`heuristic`** — a receiver method-name or write-through guess that is only
  correct with type information FxRank does not compute (`.lock()`, `.borrow_mut()`,
  `.set()`, `.send()`, `.store()`, `Result::unwrap` vs a user `.unwrap()`, whether a
  `write!` target is `io::Write` or `fmt::Write`, whether `*p = …`/`p.push(…)`
  mutates an `&mut` parameter). **Always carries a confidence penalty** and may
  produce false positives/negatives.

**Numeric confidence.** Each detection starts at a base set by its tier — `exact` =
`1.0`, `path` = `0.9`, `heuristic` = `0.6` — multiplied by penalties where they
apply: an unresolved call `×0.8` (an unresolved *awaited* call uses the same
`×0.8`), an alias/glob-shadowed path `×0.9`. A function's
`confidence` is the **minimum** over its effects' per-detection confidence and over
any `unknown.macro` / unresolved-await evidence items it carries. `summary.confidence`
is the **minimum across hotspots**; parse coverage is **not** folded into it —
it is reported separately as `scope.parsed` / `scope.files`.

Further lowered by:

- Unresolved calls (target not determinable syntactically).
- Unknown macro invocations — recorded as an `unknown.macro` **effect at class 2,
  weight 2, tier `heuristic`, confidence `0.4`** (a rank floor, so effects laundered
  into a local macro are not free), e.g.
  `{ "kind": "unknown.macro", "class": 2, "weight": 2, "tier": "heuristic", "confidence": 0.4, "line": 9, "evidence": "my_macro!" }`.
  The Milestone-A pure-macro whitelist (exempt, no effect emitted) is exactly:
  `vec!`, `format!`, `matches!`, `concat!`, `stringify!`, `cfg!`, `line!`,
  `column!`, `file!`. Macros already classified elsewhere (`println!`/`panic!`/
  `assert!`/`write!`/…) are handled by their own catalog rows. Every other
  non-builtin macro is `unknown.macro`. Note the macro's *expansion* is invisible
  to `syn`, so effects generated inside it are not seen at all — see *Known
  Limitations*.
- `async_boundary: true` with awaited calls whose targets are unresolved (an async
  shell may hide IO).

## What Counts as a Function

The scored unit is, precisely:

- Free `fn` items; inherent-`impl` methods; trait-`impl` methods; trait
  default-method **bodies**. Included regardless of `const` / `unsafe` / `async`.
- **Excluded**: trait method signatures without a body (nothing to score) and
  `extern` fn declarations (no body; the FFI *call site* is what scores).
- **Closures and `async` blocks** are attributed to their enclosing function —
  their effects roll up into it — rather than scored as separate units in this
  milestone.
- **Macro-generated items are invisible** (`syn` sees unexpanded macro
  invocations); a known limitation, not silently treated as pure.

`id` is `path:line:symbol`. `symbol` includes the `impl` type where available
(`User::new`), and for **trait-impl** methods the trait path too
(`<User as Display>::fmt`), so two trait impls of the same method on one type do not
collide. `line` is the final tiebreak, making ids collision-resistant.

## Occurrence Counting

Effects are counted **per syntactic site**, not deduplicated by kind: five
`println!` call sites are five `logging` effects (this is exactly what makes the
"logging-soup vs one `fs::write`" ranking test meaningful). A site inside a loop is
**not** multiplied — the syntactic pass does not unroll. `let mut` mutation counts
once per write site.

## Effect Catalog (Rust)

Base **class** per kind, the containment discount where applicable, and the
detectability tier. `weight` is derived from the (possibly discounted) class via the
Fibonacci map.

| kind | Rust signal | class | discount | tier |
| --- | --- | --- | --- | --- |
| `net.fs.db` | `std::fs` (`read`/`write`/`File::open`/`create`/`remove_*`/`rename`/`create_dir*`/`metadata`), `std::net`, `tokio::fs`; `std::io` `Read`/`Write`, `stdin/stdout/stderr`; `write!`/`writeln!`; `reqwest`, `sqlx` | 7 | — | path; method calls + `write!` target are `heuristic` |
| `process.control` | `std::process::exit`/`abort`; `Command` `spawn`/`status`/`output` (**not** `Command::new`, which is builder setup); `Child::kill` | 6 | — | path / `heuristic` |
| `env.write` | `std::env::set_var`/`remove_var`/`set_current_dir` | 6 | — | path |
| `env.write` note | In edition 2024 `set_var`/`remove_var` require `unsafe`; the `env.write` effect and the enclosing `unsafe` risk feature **compose** (both recorded). | | | |
| `concurrency` | `thread::spawn`, `tokio::spawn`, `rayon::*`, `JoinSet`; channel `send`/`recv` (`mpsc`/`oneshot`/`broadcast`/`watch`/`crossbeam`/`flume`); `thread::sleep` / blocking inside `async` | 6 | — | path / `heuristic` (channel methods) |
| `time.read` | `Instant::now`, `SystemTime::now` | 5 | — | path |
| `random` | `rand::*`, `thread_rng` | 5 | — | path |
| `env.read` | `std::env::var`/`vars`/`args`/`current_dir`/`current_exe`/`temp_dir` | 4 | — | path |
| `logging` | `println!`/`eprintln!`/`print!`/`eprint!`/`dbg!`; `log::*`, `tracing::*` | 4 | — | exact (macros) / path |
| `panic` | macros `panic!`/`unreachable!`/`todo!`/`unimplemented!`/`assert!`/`assert_eq!`/`assert_ne!` (conditionally panicking, like any guarded `panic!`); `debug_assert*!` is debug-profile-only (`cfg`-dependent) | 4 | — | exact |
| `panic` (heuristic) | `unwrap`/`expect` — method name; cannot tell `Option`/`Result` from a graceful user `.unwrap()` | 4 | — | `heuristic` |
| `global.mutation` | write to `static mut`; mutation of a `static` interior-mut value | **6 by default**; **4** only when clearly module-private (private `static` with no visible public mutating accessor) | — | path / `heuristic` |
| `hidden.mutation` | interior-mutability mutation reached through **any shared `&` reference** (`&self`, `&Context`, `&Arc<Mutex<T>>`, …): `RefCell::borrow_mut`, `Cell::set`/`replace`, `Atomic*::store`/`swap`/`fetch_*`, a write through a `Mutex`/`RwLock` guard (std, `parking_lot`, `tokio::sync`) | 3 | **none** (flag `hidden: true`) | `heuristic` |
| `param.mutation` | write through an explicit `&mut` parameter or `&mut self` | 3 | `&mut param`: **down 2 → class 1**; `&mut self`: **down 1 → class 2** | binding is `exact`; the *write-through* is `heuristic` |
| `ambient.read` | read of a `static` / global config value; `Atomic*::load` | 2 | — | path / `heuristic` |
| `local.mutation` | a **write** (assignment, compound-assignment, or `&mut` borrow) to a binding introduced by `let mut` in the same function; the `let mut` declaration alone is not scored, and counted once per write site | 1 | **none** (kept as signal) | exact (within-function lexical binding tracking — no cross-item resolution) |

`risk_features` (flags; each carries a severity class per *Three channels*, so they
feed `max_class` and `risk_weight`): `unsafe` block / `unsafe fn` / `unsafe impl`,
raw pointer deref, `transmute`, `MaybeUninit`, `*::from_raw`, `get_unchecked`, FFI
`extern` block / `extern "C"` call, `asm!`, volatile / `ptr::{read,write,
copy_nonoverlapping}`, `Box::leak`, `mem::forget`, `ManuallyDrop`, module-level
`impl Drop`.

**Module-level risk.** Risk features that belong to an item rather than a function
body — `impl Drop`, `extern` blocks (declarations, not call sites) — are **not**
attributed to individual functions in Milestone A. They are reported in a
`scope.risk_features` list, each entry shaped
`{ kind, class, weight, path, line, evidence, tier }` (e.g.
`{ "kind": "impl.drop", "class": 2, "weight": 2, "path": "src/io.rs", "line": 30, "evidence": "impl Drop for Conn", "tier": "exact" }`).
`summary.max_class` and `summary.risk_weight` take the max over hotspots **and**
`scope.risk_features`, so a file whose only risk is an `extern` block is not
summarized as risk-free. Linking a `Drop` to the functions that construct the value
needs type resolution and is deferred.

`async_boundary` (informational flag, not an effect): set on `async fn` / functions
containing `.await`, with `await_count`. Lowers confidence when awaited targets are
unresolved (see *Detectability and Confidence*).

### Rust-specific reframes

These distinguish FxRank from a naïve purity checker and are the main thing
dogfooding validates:

1. **Returning `Result` is not an effect, and `?` is not scored as one in Milestone
   A.** Fallibility is an ADT *value*; `?` performs value-divergence control flow,
   and its `From`/`FromResidual` conversion is arbitrary code that lies *outside*
   own-body syntactic scoring. Only constructs that actually abort at runtime
   (`panic!`/`unwrap`/`expect`/…) count as `panic`.
2. **Rust has almost no import-time effects.** No top-level execution; the
   `import_time_effect` category is effectively empty (only `lazy_static`/`ctor`),
   so it is not in the MVP catalog.
3. **No `eval`/reflection.** The neutral ladder's class-8 "unknown dynamic call"
   rarely fires in Rust; its analogue is `unsafe`/FFI/`transmute`, captured as
   `risk_features`. `dyn Trait` calls are *bounded*, not unknown.
4. **`.await` / calling an `async fn` is an interface, not an effect.** The inner
   IO is what scores; the async-ness is a shell (recorded as `async_boundary`).
5. **`unsafe` is the escape hatch from the guarantees** that justify discounts, so
   it cancels the affected mutation's discount and is itself a risk flag.

### Anti-Goodhart: hidden vs. declared mutation

The discount is scoped tightly to *signature-visible* boundedness so an agent
cannot launder a worse effect into a cheaper score:

- Interior mutability reached through **any shared `&` reference** — not just
  `&self`, but also `&Context` whose field is a `RefCell`, or `&Arc<Mutex<T>>` — is
  **`hidden.mutation` (class 3, no discount, flagged)**. It scores *higher* than the
  honest `&mut self` (class 2). A naïve checker inverts this (calling `&self`
  "pure"); FxRank corrects it, and generalizing beyond `&self` closes the
  "move the mutation onto a shared parameter" escape. (Detection is `heuristic`
  without types; the confidence penalty applies.)
- Only the syntactically observed mutation channel is discounted, never the whole
  function body.
- Unresolved method calls on `&mut`/`&self` receivers lower confidence rather than
  silently passing as bounded.

## CLI Surface

Agent-first, stdin→stdout-first, minimal:

```bash
fxrank scan                 # read Rust from stdin, write compact JSON to stdout
fxrank scan <path>          # scan a file or directory (recurse for .rs)
fxrank scan <path> --limit N  # keep only the top-N hotspots by rank key
```

- Output is **compact JSON only** (no pretty-printing — agents consume it, humans
  pipe through `jq`; no `--format` flag in this milestone).
- No file-writing side effects; redirection is the caller's choice.

## Output JSON Schema

One compact JSON object on stdout (shown expanded here for readability):

```json
{
  "scope":   { "input": "crates/fxrank-core/src", "files": 2, "parsed": 1, "functions": 4, "risk_features": [] },
  "summary": { "own_score": 25.5, "max_class": 7, "risk_weight": 0, "confidence": 0.6 },
  "hotspots": [
    {
      "id": "src/user.rs:42:save_user",
      "symbol": "save_user",
      "path": "src/user.rs",
      "line": 42,
      "max_class": 7,
      "own_score": 25.5,
      "risk_weight": 0,
      "confidence": 0.6,
      "async_boundary": false,
      "effects": [
        { "kind": "net.fs.db", "class": 7, "weight": 21, "line": 44,
          "tier": "heuristic", "evidence": "db.users.insert" },
        { "kind": "time.read", "class": 5, "weight": 8, "line": 41,
          "tier": "path", "evidence": "Instant::now" },
        { "kind": "param.mutation", "class": 3, "discounted_to": 1, "weight": 1,
          "line": 41, "tier": "heuristic", "evidence": "&mut user; assigns user.created_at",
          "discount": "explicit &mut param, caller-visible" }
      ],
      "risk_features": []
    }
  ],
  "diagnostics": [
    { "path": "src/broken.rs", "parsed": false, "error": "expected `;`, line 8" }
  ]
}
```

The flagship `hidden.mutation` case (a single function) serializes like this — note
the `hidden` flag and the lower `confidence` of a heuristic detection:

```json
{
  "id": "src/store.rs:10:set_name",
  "symbol": "Store::set_name",
  "line": 10, "max_class": 3, "own_score": 3.0, "risk_weight": 0, "confidence": 0.6,
  "effects": [
    { "kind": "hidden.mutation", "class": 3, "weight": 3, "line": 11,
      "tier": "heuristic", "hidden": true,
      "evidence": "&self; *self.name.borrow_mut() = name" }
  ],
  "risk_features": []
}
```

- `hotspots` is sorted by the rank key (descending severity).
- `scope.files` counts **all** files seen; `scope.parsed` the successfully parsed
  subset (parse coverage = `parsed / files`); `scope.functions` counts only
  functions in parsed files; `scope.risk_features` carries module-level risks.
- `summary.own_score` is the **max** hotspot `own_score`; `summary.max_class` and
  `summary.risk_weight` the max over hotspots **and** `scope.risk_features`;
  `summary.confidence` the **min** (weakest-link) across hotspots. All `summary.*`
  and `scope.*` are computed over **all** scanned functions; `--limit N` truncates
  only the `hotspots` array, not the summary.
- **Zero hotspots** (empty input, or a file with only module-level risks):
  `own_score: 0.0`, `confidence: 1.0`, and `max_class` / `risk_weight` are the max
  of the `scope.risk_features` (or `0` if there are none).
- `own_score` is a JSON number derived from `f64`; whole values render with a
  trailing `.0` (e.g. `3.0`). Per-effect `confidence` is **not** in the wire format —
  confidence is computed per detection but only surfaced at the function level
  (`hotspots[].confidence`, the min); `effects[]` carry no `confidence` field.
- `effects[].weight` reflects the **post-discount** class (`discounted_to` when a
  discount applies, else `class`).
- `effects[].discount` / `discounted_to` / `tier` explain *why a score is what it
  is* (measurement transparency) — this is fact, not advice. There is no
  `suggested_moves` field; refactoring decisions belong to the agent / future skill.

## Error Handling

- **Un-parseable file**: recorded in `diagnostics` with `parsed: false` and the
  parser error; the file is excluded from scoring; **the run still succeeds and
  emits JSON for everything else**. The agent learns which file was not seen
  rather than the whole run aborting. Parse coverage is reflected in
  `scope.parsed` / `scope.files`; there is no separate `scope.confidence`, and a
  parse failure does not lower `summary.confidence` (which is the min across
  hotspots).
- **No input / empty scope**: a valid JSON object with empty `hotspots` and a
  `scope` reflecting zero files, exit code 0.
- **Unreadable path / IO error opening sources**: a `diagnostics` entry; only a
  genuinely unusable invocation (e.g. a nonexistent path argument) is a non-zero
  exit with a JSON error object.
- Internal logic is total where possible; `analyze` must not panic on malformed
  but parseable ASTs.

## Testing Strategy

- **Fixture crates / snippets** under a `tests/fixtures/` tree, one per
  interesting case: the discount pair (`&mut self` vs `&self`+`RefCell`), the
  shared-ref hidden-mutation case (`&Context`/`&Arc<Mutex<T>>`), the aggregation
  case (logging-soup vs one `fs::write`), a pure function, a risk-only function
  (`mem::forget`) confirming it ranks at its `risk_class` not class 0, `Result`/`?`
  being pure, an `async` shell, an `unsafe` discount-cancel, a `Command::new`
  builder (no effect) followed by `.spawn()` (effect), and an un-parseable file.
- **Snapshot tests** on the emitted JSON for each fixture (a snapshot crate such
  as `insta`, or hand-written expected-JSON assertions) so scoring changes are
  visible in review.
- **Unit tests** in `fxrank-core` for the class→weight map, the discount
  down-shift + clamps, `own_score` aggregation, `risk_class` participation in
  `max_class`, and the rank-key ordering (especially that `max_class` outranks a
  large damped sum, and that a risk-only function outranks a class-0 one).
- **Dogfood**: a test (or CI step) that runs `fxrank scan` over the FxRank crates
  and asserts the run succeeds and produces well-formed JSON; the scores are
  inspected manually to calibrate the weight curve and discounts.

## Verification

Milestone A is complete when all pass locally and in CI:

- `cargo build`
- `cargo test` (unit + snapshot + dogfood smoke)
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `fxrank scan crates/` emits valid compact JSON whose hotspots and evidence match
  the worked examples in this spec.

## Known Limitations

These are accepted for Milestone A and documented so they are not mistaken for
correctness:

- **Own-score-only is gameable by extract-method.** Splitting a hot function moves
  its IO into a callee; with no call-graph propagation (deferred), the caller's
  `own_score` and `max_class` drop while the effect still happens. Dogfooding
  watches for score drops that coincide with new thin wrappers.
- **Macro-generated effects are invisible.** `syn` sees unexpanded macro
  invocations, so effects produced inside a macro's expansion are not scored; an
  unknown macro is recorded as a low-confidence `unknown.macro` evidence item.
- **Type-dependent signals are heuristic.** Interior-mutability detection,
  receiver method-name effects, and `&mut` write-through all need name/type
  resolution FxRank does not compute; they carry a confidence penalty and may
  misfire. A semantic pass that upgrades them is a separate, later milestone.
- **`global.mutation` module-private downgrade is deferred.** Deciding "no visible
  public mutating accessor" needs whole-module analysis; Milestone A scores all
  detected global mutation at class 6 (the class-4 case lands with that analysis).

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| First language | Rust | Dogfooding: FxRank is itself Rust, so scores/evidence are immediately checkable. |
| Analysis depth | `syn`, primarily syntactic; type-dependent signals are heuristic | Containment discounts are mostly readable from signatures; full semantic resolution would slow iteration and is deferred, but the catalog is honest about which signals are guesses. |
| Parser | `syn` | De-facto Rust AST parser, stable API, signatures (`&mut`/`&self`/`unsafe`) visible. `ra_ap_*` is too heavy/unstable for an MVP; `tree-sitter` is coarser for Rust specifics. |
| Code structure | Workspace: core + lang-rust + cli, single binary, feature-gated frontends | Compiler-enforced "core is language-neutral"; heavy per-language parser deps isolated; slim single-language builds possible; one invocation for agents. |
| Scale | Severity class 0–8 + convex Fibonacci weight | Linear summation under-weights severe effects and mis-ranks; convex weight fixes magnitude; ordinal class preserves a legible gradient. Prime scale rejected (factorization is multiplicative, not additive; near-linear growth). |
| Discount | Class down-shift, per-kind, clamped, `unsafe`-cancelled; `weight` from post-discount class | Well-defined on any scale; reflects the thesis (declared/bounded effects cost less) without arbitrary point subtraction. |
| Risk in ranking | Each risk feature carries a severity class; `risk_class` feeds `max_class` | Otherwise a risk-only function (`mem::forget`) ranks as class 0 — cheap — contradicting intent. |
| Aggregation | `own_score = max_weight + 0.5·Σrest`; rank by `(max_class, own_score, risk_weight, confidence)`, ordered via scaled integers | Per-effect convexity is re-linearized by summation; ranking on `max_class` first guarantees one real boundary effect outranks effect-soup; integer keys avoid `f64: Ord`. |
| Detectability | Three tiers (`exact` / `path` / `heuristic`), heuristic ⇒ confidence penalty | Reconciles "no type resolution" with a catalog that needs some type facts; keeps the tool honest and confidence meaningful. |
| Function unit | Free fns + inherent/trait-impl methods + trait default bodies; closures roll up; trait sigs / `extern` decls excluded; `id = path:line:symbol` | Defines the output unit unambiguously and avoids id collisions across `impl` blocks. |
| Constructor vs effect | Score the effectful terminal call (`Command::spawn`), not the builder (`Command::new`) | Counting constructors creates false positives and rewards hiding the real effect downstream. |
| `Result` | Not an effect | Fallibility-as-value (ADT); only runtime-aborting constructs are `panic`. |
| Hidden mutation | Interior mutation through *any* shared `&` ref scored above `&mut self`, flagged | Anti-Goodhart: prevents laundering mutation into a signature-pure-looking method or shared parameter. |
| `global.mutation` default | Class 6 (process-global) unless clearly module-private | Under-counting a global mutation is the worse error; visibility alone does not prove containment. |
| Suggestions | None in the tool | The tool is a measuring instrument; refactoring decisions belong to the agent / future `lower-effect-score` skill. |
| Output | Compact JSON to stdout only | Built for agents; humans use `jq`; redirection over file-writing side effects. |
| Ill-formed files | `diagnostics` + `parsed:false`, run continues | The agent must know what was not analyzed without losing the rest of the report. |

## Open Questions

- The exact convex curve and the `0.5` aggregation damping are calibration
  parameters; dogfooding will tune them. Should the curve be configurable, or
  fixed until evidence says otherwise?
- When does the heuristic tier's false-positive rate justify building the deferred
  semantic pass (name/type resolution) to upgrade those signals to `path`/`exact`?
- Should `async_boundary` eventually carry the awaited call targets (low
  confidence) to hint at hidden IO, or stay a bare flag?
- Should low-confidence ("possibly hiding effects") functions optionally sort
  *upward* for hotspot discovery, instead of only acting as a trust tie-breaker?
