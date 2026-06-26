# FxRank

**An effect-cost profiler for coding agents.**

`fxrank scan <path>` ranks each function in Rust, TypeScript/JavaScript, and Python
source by its **effect cost** — how much IO, mutation, panic, and risk it carries — and
emits compact JSON, so an agent (or a human) can find the hotspots worth refactoring
toward a purer core.

FxRank is a **measuring instrument**, not a linter. It reports facts — effect kind,
severity class, evidence, confidence, risk — and deliberately gives **no refactoring
advice**. The decision is yours, and the output is a signal to verify, not a verdict.

## Why it's not just a purity checker

A binary "pure vs impure" label is too coarse to refactor against. FxRank gives a
**gradient**, and it knows a language's type system makes some effects safer than others:

- A **declared `&mut`** mutation is visible and bounded at the call site, so it is
  **discounted** — it scores *lower*.
- A **`&self` method that mutates through interior mutability** (`RefCell`, `Cell`,
  `Mutex`, atomics) is **hidden** from the signature, so it scores *higher*.

So FxRank **inverts** a naïve checker: the honest `&mut self` mutation ranks *below* the
sneaky `&self` + `borrow_mut()` one. That anti-Goodhart inversion is the whole thesis.
(The TS/JS frontend applies an analogous **boundary discount** by how much of a signature
is typed.) All three frontends classify mutation against one canonical model, documented
in [`docs/mutation-classification-guideline.md`](docs/mutation-classification-guideline.md).

## Install

Needs a stable Rust toolchain (edition 2024, Rust ≥ 1.85; get one via [rustup](https://rustup.rs)).

```bash
cargo install fxrank
```

Ships all three frontends by default; for a slim single-language build, add
`--no-default-features --features rust` (or `ts`, or `python`).

## Usage

```bash
fxrank scan src/                    # a directory (by file extension; symlink-safe)
fxrank scan src/lib.rs --limit 20   # one file; keep the top 20 hotspots
cat foo.ts | fxrank scan --lang ts  # stdin (--lang: rust | ts | tsx | js | jsx | python)
fxrank scan crates/ | jq            # compact JSON on stdout — pipe to jq to read it
```

Run **`fxrank scan --help`** for the full flag set (`--include-tests`, `--exclude`,
`--no-resolve`, …) — usage lives in the binary, so it never drifts from this page.

Each hotspot `id` is `path:line:col:symbol` — a unique, **opaque** key within one report
(it encodes position, so it changes when code moves). Read the `path`, `line`, and
`symbol` fields rather than splitting the `id` string.

## Using it well

FxRank is a precision instrument, not a crawler:

- **Scan hand-written source only.** Scores are meaningless on minified, generated, or
  vendored code — and a file named explicitly is always scanned, so don't point it at a
  bundle.
- **Verify, don't trust.** Open the top hotspots and confirm them against the source; the
  JSON is a measurement, not a verdict — a signal, not gospel.

The full step-by-step "lab protocol" is moving into the planned `lower-effect-score` agent
skill.

## The scoring model, briefly

- **Severity class `0..=8`** rates the *kind* of effect (`0` pure … `7` net/fs/db), each
  mapped to a convex **Fibonacci weight**, so one real IO boundary outweighs a pile of
  trivial effects.
- **`own_score`** = `max_weight + 0.5 · Σ(other weights)`; functions rank by
  `(max_class, own_score, risk_weight, confidence)`, so a single class-7 IO always
  outranks any amount of class-4 logging.
- **`propagated_score`** folds *escaping* effects along the call graph, so pushing IO into
  a callee doesn't hide it from the caller.
- **Confidence** reflects how much a signal leaned on syntax-only heuristics (FxRank does
  no type inference); the function-level value is the weakest link.

Full detail: [spec 001](docs/superpowers/specs/001-fxrank-rust-effect-scanner.md).

## Status

Milestone A (syntactic effect/risk analysis + the containment discount) and Milestone B
(cross-file propagation — each function gets a `propagated_score` for its effect
blast-radius) ship across all three frontends: Rust (`syn`), TS/JS (`swc`), and Python
(`libcst`), each syntactic — no type checker or borrow checker.

Known limitations and the roadmap (precise module-tree resolution #36, the React fold
retrofit #37, the `lower-effect-score` skill, research in #4) live in the
[issue tracker](https://github.com/caasi/fxrank/issues).
