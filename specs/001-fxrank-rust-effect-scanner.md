# 001 — FxRank Rust Effect Scanner (Milestone A)

## Goal

Build the first FxRank milestone: a fast, purely **syntactic** analyzer that scans
Rust source, computes a per-function **own effect score**, ranks functions as
hotspots, and emits compact JSON to stdout for consumption by coding agents.

This milestone establishes FxRank as a *measuring instrument*. It deliberately
ships no refactoring advice and no call-graph propagation. The central, least-proven
idea it exists to validate is the **containment discount**: Rust's type system makes
some effects declared and bounded (`&mut`, `&self`, ownership), so the same effect
scores *lower* in Rust than in a language where it is invisible. We build Rust first
so we can **dogfood** — run `fxrank` on the FxRank crates themselves and check the
scores and evidence against intuition.

## Scope

In scope:

- A `fxrank scan` subcommand that reads Rust source (stdin or a path) and writes
  compact JSON to stdout.
- A purely syntactic analyzer built on `syn` (no borrow-checker, no name
  resolution, no type inference).
- **Own-score only**: each function scored from the effects in its own body.
- The scoring model: severity classes, a convex (Fibonacci) weight map, the
  containment discount expressed as a class down-shift, three output channels
  (score / confidence / risk_features), and a rank key.
- The Rust effect catalog (see *Effect Catalog*).
- A lightweight per-file import table (from `use` statements) to improve path
  matching.
- Graceful handling of un-parseable files via a `diagnostics` array.

Out of scope (deferred to later milestones):

- **Call-graph propagation** (`inherited_score` / `total_score`) — the whole point
  of own-score-first is to get the language-specific scoring right before adding
  propagation.
- `fxrank diff`, `top`, `explain`, `suggest`, `budget` subcommands.
- The `lower-effect-score` agent skill and any `suggested_moves` / refactoring
  advice. The tool measures; deciding the move is the agent's job.
- Writing files (`.fxrank/`, `.tsugu/fxrank/`). Output goes to stdout; an agent or
  human redirects if they want a file.
- Scope modifiers `--changed`, `--since <ref>`, `--symbol <name>`, glob scopes.
- JavaScript / TypeScript frontends. The architecture leaves room for them, but no
  code is written for them here.
- Interprocedural / within-crate call summaries.
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
  vocabulary, the class→weight map, the discount/aggregation logic, the hotspot
  data model, the JSON serialization, and the `Frontend` trait. The compiler
  enforces "core is language-neutral": `syn` only appears in `fxrank-lang-rust`.
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
the effect down N severity classes** (not an arbitrary point subtraction). Shifts
are per-kind, clamped, and never cross a kind boundary:

- A discount applies **only to the channel it describes** — e.g. `&mut`
  containment lowers *mutation opacity*; it never discounts a sibling IO or panic
  effect in the same function body.
- An externally observable effect never discounts below **class 1**.
- `unsafe` anywhere in a function **cancels that function's discounts** (the code
  has stepped outside the guarantees the discount assumes).

### Three channels

The model never forces every nuance into one scalar:

1. **score** — `severity_class` + `weight` per effect; aggregated into `own_score`.
2. **confidence** — how sure the syntactic pass is (see *Confidence*).
3. **risk_features** — independent flags for danger a scalar would hide.

### Aggregation and rank key

Per function:

```text
own_score = max_weight + 0.5 × Σ(other effect weights)
```

The `0.5` damping keeps dense effect-soup visible without letting it overpower a
single boundary effect. `own_score` is a scalar suitable for later budgets/deltas.

Because summation re-linearizes convex weights (e.g. many `log` calls can out-sum
one `fs::write`), **ranking does not use `own_score` alone**. The rank key is the
tuple, compared left to right:

```text
( max_severity_class , own_score , risk_weight , confidence )
```

`max_severity_class` first guarantees a single class-7 IO outranks any quantity of
class-4 logging. `risk_weight` (derived from `risk_features`) is in the key so a
function whose only notable property is `mem::forget` / `Box::leak` is not ranked
as cheap.

## Effect Catalog (Rust, syntactic)

Base **class** per kind, with the containment discount where applicable. `weight`
is derived from class via the Fibonacci map.

| kind | Rust syntactic signal | class | discount |
| --- | --- | --- | --- |
| `net.fs.db` | `std::fs`, `std::net`, `tokio::fs`; `std::io` `Read`/`Write`, `File`, `OpenOptions`, `stdin/stdout/stderr`, `write!`/`writeln!`; `reqwest`, `sqlx` | 7 | — |
| `process.control` | `std::process::exit`/`abort`, `Command::new`, `Child::kill` | 6 | — |
| `env.write` | `std::env::set_var`/`remove_var` | 6 | — |
| `concurrency` | `thread::spawn`, `tokio::spawn`, `rayon::*`, `JoinSet`; channel `send`/`recv` (`mpsc`/`oneshot`/`broadcast`/`watch`/`crossbeam`/`flume`) | 6 | — |
| `time.read` | `Instant::now`, `SystemTime::now` | 5 | — |
| `random` | `rand::*`, `thread_rng` | 5 | — |
| `env.read` | `std::env::var`/`vars` | 4 | — |
| `logging` | `println!`/`eprintln!`/`print!`/`eprint!`/`dbg!`, `log::*`, `tracing::*` | 4 | — |
| `panic` | `panic!`/`unwrap`/`expect`/`unreachable!`/`todo!`/`unimplemented!`/`assert!`/`assert_eq!`/`assert_ne!`/`debug_assert*!` | 4 | — |
| `global.mutation` | write to `static mut`; mutation of a `static` interior-mut value (process-global unless clearly module-private) | 4 (process-global **6**) | — |
| `hidden.mutation` | `&self` + interior mutability mutation: `RefCell::borrow_mut`, `Cell::set`/`replace`, `Atomic*::store`/`swap`/`fetch_*`, observed write through a `Mutex`/`RwLock` guard (std, `parking_lot`, `tokio::sync`) | 3 | **none** (flag `hidden: true`) |
| `param.mutation` | write through an explicit `&mut` parameter or `&mut self` | 3 | `&mut param`: **down 2 → class 1**; `&mut self`: **down 1 → class 2** |
| `ambient.read` | read of a `static` / global config value; `Atomic*::load` | 2 | — |
| `local.mutation` | `let mut` local variable mutation | 1 | **none** (kept as signal) |

`risk_features` (independent flags; contribute `risk_weight` to the rank key, not
to `own_score`): `unsafe` block / `unsafe fn` / `unsafe impl`, raw pointer deref,
`transmute`, `MaybeUninit`, `*::from_raw`, `get_unchecked`, FFI `extern` block /
`extern "C"` call, `Box::leak`, `mem::forget`, `ManuallyDrop`, module-level
`impl Drop`.

`async_boundary` (informational flag, not an effect): set on `async fn` / functions
containing `.await`, with `await_count`. Prevents async shells with unresolved
awaited calls from looking deceptively pure.

### Rust-specific reframes

These distinguish FxRank from a naïve purity checker and are the main thing
dogfooding validates:

1. **Returning `Result` is not an effect.** Fallibility is an ADT *value*, not a
   control-flow side effect; `?` propagates a value and is pure. Only constructs
   that actually abort at runtime (`panic!`/`unwrap`/`expect`/…) count as `panic`.
2. **Rust has almost no import-time effects.** No top-level execution; the
   `import_time_effect` category is effectively empty (only `lazy_static`/`ctor`),
   so it is not in the MVP catalog.
3. **No `eval`/reflection.** The neutral ladder's class-8 "unknown dynamic call"
   rarely fires in Rust; its analogue is `unsafe`/FFI/`transmute`, captured as
   `risk_features`. `dyn Trait` calls are *bounded*, not unknown.
4. **`.await` / calling an `async fn` is an interface, not an effect.** The inner
   IO is what scores; the async-ness is a shell (recorded as `async_boundary`).
5. **`unsafe` is the escape hatch from the guarantees** that justify discounts, so
   it cancels a function's discounts and is itself a risk flag.

### Anti-Goodhart: hidden vs. declared mutation

The discount is scoped tightly to *signature-visible* boundedness so an agent
cannot launder a worse effect into a cheaper score:

- `&self` + interior mutability is **`hidden.mutation` (class 3, no discount,
  flagged)** — it scores *higher* than the honest `&mut self` (class 2). A naïve
  checker inverts this (calling `&self` "pure"); FxRank corrects it.
- Only the syntactically observed mutation channel is discounted, never the whole
  function body.
- Unresolved method calls on `&mut`/`&self` receivers lower confidence rather than
  silently passing as bounded.

## Confidence

A per-effect and per-scope `confidence` in `[0, 1]`, lowered by the limits of a
syntactic pass:

- Calls whose target cannot be resolved syntactically.
- Method-name heuristics (`.lock()`, `.set()`, `.send()`, `.write()`), which are
  ambiguous without types.
- Path matches that depend on `use ... as` aliases not captured by the import
  table.
- Unknown macro invocations (an unknown macro adds a low-confidence unknown-effect
  signal rather than a confident score).

Scope confidence is additionally lowered when files fail to parse (see *Error
Handling*).

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
  "scope":   { "input": "stdin", "files": 1, "functions": 4 },
  "summary": { "own_score": 25.5, "max_class": 7, "confidence": 0.9 },
  "hotspots": [
    {
      "id": "src/user.rs:save_user",
      "symbol": "save_user",
      "path": "src/user.rs",
      "line": 12,
      "max_class": 7,
      "own_score": 25.5,
      "risk_weight": 0,
      "confidence": 0.9,
      "async_boundary": false,
      "effects": [
        { "kind": "net.fs.db", "class": 7, "weight": 21, "line": 14,
          "evidence": "db.users.insert" },
        { "kind": "time.read", "class": 5, "weight": 8, "line": 13,
          "evidence": "Instant::now" },
        { "kind": "param.mutation", "class": 3, "discounted_to": 1, "weight": 1,
          "line": 13, "evidence": "&mut user; assigns user.created_at",
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

- `hotspots` is sorted by the rank key (descending severity).
- `effects[].discount` and `discounted_to` explain *why a score is what it is*
  (measurement transparency) — this is fact, not advice. There is no
  `suggested_moves` field; refactoring decisions belong to the agent/skill.
- `hidden: true` appears on `hidden.mutation` effects.

## Error Handling

- **Un-parseable file**: recorded in `diagnostics` with `parsed: false` and the
  parser error; the file is excluded from scoring; **the run still succeeds and
  emits JSON for everything else**. The agent learns which file was not seen
  rather than the whole run aborting. Scope `confidence` is lowered proportionally.
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
  aggregation case (logging-soup vs one `fs::write`), a pure function, a
  risk-only function (`mem::forget`), `Result`/`?` being pure, an `async` shell,
  an `unsafe` discount-cancel, and an un-parseable file.
- **Snapshot tests** on the emitted JSON for each fixture (a snapshot crate such
  as `insta`, or hand-written expected-JSON assertions) so scoring changes are
  visible in review.
- **Unit tests** in `fxrank-core` for the class→weight map, the discount
  down-shift + clamps, `own_score` aggregation, and the rank-key ordering
  (especially that `max_class` outranks a large damped sum).
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

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| First language | Rust | Dogfooding: FxRank is itself Rust, so scores/evidence are immediately checkable. |
| Analysis depth | Purely syntactic (`syn`) | Containment discounts are readable from signatures; semantic resolution is unneeded for own-score and would slow iteration. |
| Parser | `syn` | De-facto Rust AST parser, stable API, signatures (`&mut`/`&self`/`unsafe`) visible. `ra_ap_*` is too heavy/unstable for an MVP; `tree-sitter` is coarser for Rust specifics. |
| Code structure | Workspace: core + lang-rust + cli, single binary, feature-gated frontends | Compiler-enforced "core is language-neutral"; heavy per-language parser deps isolated; slim single-language builds possible; one invocation for agents. |
| Scale | Severity class 0–8 + convex Fibonacci weight | Linear summation under-weights severe effects and mis-ranks; convex weight fixes magnitude; ordinal class preserves a legible gradient. Prime scale rejected (factorization is multiplicative, not additive; near-linear growth). |
| Discount | Class down-shift, per-kind, clamped, `unsafe`-cancelled | Well-defined on any scale; reflects the thesis (declared/bounded effects cost less) without arbitrary point subtraction. |
| Aggregation | `own_score = max_weight + 0.5·Σrest`; rank by `(max_class, own_score, risk_weight, confidence)` | Per-effect convexity is re-linearized by summation; ranking on `max_class` first guarantees one real boundary effect outranks effect-soup. |
| `Result` | Not an effect | Fallibility-as-value (ADT); only runtime-aborting constructs are `panic`. |
| Hidden mutation | `&self`+interior-mut scored above `&mut self`, flagged | Anti-Goodhart: prevents laundering mutation into a signature-pure-looking method. |
| Suggestions | None in the tool | The tool is a measuring instrument; refactoring decisions belong to the agent / future `lower-effect-score` skill. |
| Output | Compact JSON to stdout only | Built for agents; humans use `jq`; redirection over file-writing side effects. |
| Ill-formed files | `diagnostics` + `parsed:false`, run continues | The agent must know what was not analyzed without losing the rest of the report. |

## Open Questions

- The exact convex curve and the `0.5` aggregation damping are calibration
  parameters; dogfooding will tune them. Should the curve be configurable, or
  fixed until evidence says otherwise?
- `global.mutation` "process-global unless clearly module-private" — how is
  "module-private" decided syntactically (visibility + not exported), and what is
  the default when unsure?
- Should `async_boundary` eventually carry the awaited call targets (low
  confidence) to hint at hidden IO, or stay a bare flag?
