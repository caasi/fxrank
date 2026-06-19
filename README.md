# FxRank

**An effect-cost profiler for coding agents.**

`fxrank scan <path>` analyzes Rust source and emits compact JSON ranking each function by
its **effect cost** — how much IO, mutation, panic, and risk it carries in its own body —
so an agent (or a human) can find the hotspots worth refactoring toward a purer core.

FxRank is a *measuring instrument*, not a linter. It reports facts — effect kind, severity
class, why a discount applied, the evidence, a confidence, and risk flags — and
deliberately offers **no refactoring advice**. The decision is yours.

## Why it's not just a purity checker

A binary "pure vs impure" label is too coarse to refactor against. FxRank gives a
**gradient**, and it understands that Rust's type system makes some effects safer than
others:

- A **declared `&mut`** mutation is visible and bounded at the call site, so it is
  **discounted** — it scores *lower*.
- A **`&self` method that mutates through interior mutability** (`RefCell`, `Cell`,
  `Mutex`, atomics) is **hidden** from the signature, so it scores *higher*.

So FxRank **inverts** a naïve checker: the honest `&mut self` mutation ranks *below* the
sneaky `&self` + `borrow_mut()` one. That anti-Goodhart inversion is the whole thesis.

## Install / build

Requires a stable Rust toolchain (edition 2024, Rust ≥ 1.85).

```bash
git clone https://github.com/caasi/fxrank
cd fxrank
cargo build --release        # binary at target/release/fxrank
```

## Usage

```bash
fxrank scan src/                 # scan a directory (recurses *.rs, symlink-safe)
fxrank scan src/lib.rs           # scan one file
cat foo.rs | fxrank scan         # read from stdin
fxrank scan src/ --limit 20      # keep only the top-20 hotspots
```

Output is **compact JSON on stdout** (built for agents — pipe through `jq` to read it):

```jsonc
{
  "scope":   { "input": "src", "files": 6, "parsed": 6, "functions": 37, "risk_features": [] },
  "summary": { "own_score": 42.5, "max_class": 7, "risk_weight": 0, "confidence": 0.6 },
  "hotspots": [
    {
      "id": "src/main.rs:48:run_scan",
      "symbol": "run_scan",
      "max_class": 7, "own_score": 42.5, "confidence": 0.6,
      "effects": [
        { "kind": "net.fs.db", "class": 7, "line": 72, "tier": "path",
          "evidence": "std::fs::read_to_string" },
        { "kind": "net.fs.db", "class": 7, "line": 56, "tier": "path",
          "evidence": "std::io::stdin" },
        { "kind": "net.fs.db", "class": 7, "line": 56, "tier": "heuristic",
          "evidence": ".read_to_string" },
        { "kind": "local.mutation", "class": 1, "line": 94, "tier": "exact",
          "evidence": "write to local all_diagnostics" }
      ]
    }
  ],
  "diagnostics": []
}
```

*(This is FxRank scanning its own CLI: `run_scan` is correctly flagged as the top hotspot
for mixing stdin/file IO with diagnostic accumulation — a real "extract the pure
report-building from the IO boundary" candidate.)*

## The scoring model, briefly

- **Severity class `0..=8`** rates the *kind* of effect (`0` pure … `7` net/fs/db …), each
  mapped to a convex **Fibonacci weight** so one real IO boundary outweighs a pile of
  trivial effects.
- **`own_score`** = `max_weight + 0.5 · Σ(other weights)`; functions are ranked by
  `(max_class, own_score, risk_weight, confidence)` — so a single class-7 IO always
  outranks any amount of class-4 logging.
- **Risk features** (`unsafe`, `transmute`, raw-pointer ops, `mem::forget`, …) carry their
  own class so a risk-only function isn't ranked as cheap.
- **Confidence** reflects how much a signal relied on syntax-only heuristics (FxRank does
  no type inference); the function-level value is the weakest link.

The full spec lives in [`specs/001-fxrank-rust-effect-scanner.md`](specs/001-fxrank-rust-effect-scanner.md).

## Status & roadmap

**Milestone A (this release):** a Rust-only, primarily-syntactic analyzer — effect & risk
detection, the containment discount, the hidden-mutation inversion, async/confidence
metadata, diagnostics, and the `fxrank scan` CLI.

Known limitations (accepted for Milestone A): own-score only (no call-graph propagation, so
extract-method can launder a score); type-dependent signals are heuristic; macro-generated
effects are invisible to `syn`; scanning `src/` includes inline `#[cfg(test)]` modules
(filter test functions when hunting smells).

**Next:** call-graph propagation (`inherited_score`), a JavaScript/TypeScript frontend, and
a `lower-effect-score` agent skill (the "lab protocol" for using FxRank safely).
