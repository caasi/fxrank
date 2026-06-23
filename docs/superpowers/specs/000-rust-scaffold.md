# 000 — Rust Project Scaffold

## Goal

Initialize the `fxrank` repository as a minimal, well-tooled Rust binary crate: a
"Hello, world!" entry point backed by a real unit test, with formatter, linter,
toolchain channel configuration, and CI configured from the start. The result is a clean
baseline that future feature work can build on without revisiting tooling.

## Scope

In scope:

- A binary crate named `fxrank` targeting **edition 2024**.
- `main()` printing `Hello, world!`, plus a pure, testable helper function.
- A passing unit test using Rust's **built-in** `#[test]` harness (no extra deps).
- Formatter configuration (`rustfmt`).
- Linter configuration (`clippy`) via the `Cargo.toml` `[lints]` table.
- Toolchain configuration (`rust-toolchain.toml`, channel `stable`).
- A Rust `.gitignore` (the repository is already git-initialized).
- A GitHub Actions CI workflow gating format, lint, and tests.

Out of scope:

- Any application logic beyond the greeting.
- External testing/assertion crates (built-in harness only).
- Publishing, release automation, or dependency management beyond what the
  scaffold needs.

## Components

### `Cargo.toml`

- `[package]`: name `fxrank`, `edition = "2024"`, a starting `version` of `0.1.0`.
- `[lints.clippy]`: enable the `all` group at `warn` level. This is the modern,
  centralized way to configure clippy (replaces scattered `#![warn(...)]` crate
  attributes). `all` covers the correctness, suspicious, style, complexity, and
  perf lint groups — a pragmatic baseline. The noisier `pedantic` group is
  intentionally omitted to avoid CI friction as the code grows (CI runs
  `clippy -- -D warnings`, so any enabled group becomes a hard error).
- Note for future edits: if individual per-lint overrides are later added to the
  same table, the `all` group entry must become
  `all = { level = "warn", priority = -1 }` so the specific lints take
  precedence over the group; otherwise Cargo errors.

### `src/main.rs`

- `fn greeting() -> &'static str` returning `"Hello, world!"` — a pure function so
  the behavior is unit-testable (capturing `println!` output is not clean).
  Returning `&'static str` avoids a needless allocation.
- `fn main()` that prints `greeting()`.
- A `#[cfg(test)]` module with one test asserting `greeting()` equals
  `"Hello, world!"`.

### `rustfmt.toml`

- `edition = "2024"` (must match the `edition` in `Cargo.toml`).
- `max_width = 100`.
- `newline_style = "Unix"`.

Only stable rustfmt options are used so `cargo fmt` works on the stable
toolchain without nightly features.

### `rust-toolchain.toml`

- `[toolchain]` with `channel = "stable"`.
- `components = ["rustfmt", "clippy"]` so contributors and CI auto-install the
  tooling the project depends on.

### `.gitignore`

- Ignore the Cargo build directory: `/target`.

### `.github/workflows/ci.yml`

- Triggers: `push` and `pull_request`.
- Single job on `ubuntu-latest`:
  1. Checkout (`actions/checkout`).
  2. Install the toolchain explicitly via `dtolnay/rust-toolchain@stable` with
     `components: rustfmt, clippy`. Relying solely on `rust-toolchain.toml`
     auto-install is fragile on hosted runners, so the workflow pins the setup
     step.
  3. `cargo fmt --check`.
  4. `cargo clippy --all-targets -- -D warnings`.
  5. `cargo test`.

## Data Flow

`main()` → calls `greeting()` → prints the returned string to stdout. The test
module calls `greeting()` directly and asserts on its return value. There is no
I/O, state, or external dependency.

## Error Handling

No fallible operations exist in this scaffold; `main()` returns `()`. The
greeting helper is total (cannot fail). Error-handling conventions are
intentionally deferred to the first feature that needs them.

## Testing Strategy

- One unit test in the `#[cfg(test)]` module of `src/main.rs` verifying
  `greeting()` returns `"Hello, world!"`.
- The built-in harness is exercised via `cargo test`.

## Verification

The scaffold is considered complete only when all of the following pass locally:

- `cargo build`
- `cargo test`
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| Edition | 2024 | Latest stable edition (stable since Rust 1.85). |
| Testing | Built-in harness | Zero dependencies; sufficient for current scope. |
| Toolchain channel | `channel = "stable"` | Auto-tracks latest stable; no manual version bumps. Not a version pin. |
| Clippy config | `[lints.clippy] all = "warn"` | Centralized, modern; pragmatic baseline; `-D warnings` in CI enforces it. |
| Repo/CI | git + `.gitignore` + GitHub Actions | Reproducible baseline with format/lint/test gating. |
