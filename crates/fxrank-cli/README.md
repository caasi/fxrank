# fxrank

**An effect-cost profiler for coding agents.**

`fxrank scan <path>` analyzes Rust and TypeScript/JavaScript source and emits compact
JSON ranking each function by its **effect cost** — how much IO, mutation, panic, and
risk it carries in its own body — so an agent (or a human) can find the hotspots worth
refactoring toward a purer core.

FxRank is a *measuring instrument*, not a linter. It reports facts — effect kind,
severity class, why a discount applied, the evidence, a confidence, and risk flags — and
deliberately offers **no refactoring advice**. The decision is yours.

## Why it's not just a purity checker

A binary "pure vs impure" label is too coarse to refactor against. FxRank gives a
**gradient**, and it understands that a language's type system makes some effects safer
than others: a **declared `&mut`** mutation is visible and bounded, so it's *discounted*;
a **`&self` method that mutates through interior mutability** is *hidden*, so it scores
*higher*. So FxRank **inverts** a naïve checker — the honest `&mut self` ranks *below* the
sneaky `&self` + `borrow_mut()`. The TS/JS frontend applies an analogous **boundary
discount** driven by how much of a signature is typed.

## Install

Requires a stable Rust toolchain (edition 2024, Rust ≥ 1.85; install via
[rustup](https://rustup.rs)).

```bash
cargo install fxrank
```

By default the binary ships **both** frontends (Rust + TS/JS). For a slimmer build:

```bash
cargo install fxrank --no-default-features --features rust  # Rust only
cargo install fxrank --no-default-features --features ts    # TS/JS only
```

## Usage

```bash
fxrank scan src/                 # scan a directory (recurses by extension, symlink-safe)
fxrank scan src/lib.rs           # scan one Rust file
fxrank scan app/                 # .rs → Rust; .ts/.tsx/.js/.jsx → TS/JS frontend
fxrank scan src/ --limit 20      # keep only the top-20 hotspots
cat foo.rs | fxrank scan         # read Rust from stdin
cat foo.ts | fxrank scan --lang ts   # read TS/JS from stdin (--lang: ts, tsx, js, jsx)
```

Test code is excluded by default (pass `--include-tests` to score it). `--exclude`
controls corpus hygiene for directory scans (vendored bundles, stories, `jest.setup`,
etc. are skipped by default); see the repository for the full matcher semantics.

Output is **compact JSON on stdout** (built for agents — pipe through `jq`):

```jsonc
{
  "scope":   { "input": "src", "files": 6, "parsed": 6, "functions": 37, "skipped_tests": 0, "skipped_excluded": 0, "risk_features": [] },
  "summary": { "own_score": 42.5, "max_class": 7, "risk_weight": 0, "confidence": 0.6 },
  "hotspots": [
    {
      "id": "src/main.rs:48:4:run_scan",
      "symbol": "run_scan",
      "max_class": 7, "own_score": 42.5, "confidence": 0.6,
      "effects": [
        { "kind": "net.fs.db", "class": 7, "line": 72, "tier": "path", "evidence": "std::fs::read_to_string" }
      ]
    }
  ],
  "diagnostics": []
}
```

## Documentation & source

Full documentation, the scoring model, and the design specs live in the repository:
<https://github.com/caasi/fxrank>.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/caasi/fxrank/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/caasi/fxrank/blob/main/LICENSE-MIT) at your option.
