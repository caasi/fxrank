# 000 — Rust Project Scaffold

## Goal

Initialize the `fxrank` repository as a minimal, well-tooled Rust binary crate: a
"Hello, world!" entry point backed by a real unit test, with formatter, linter,
toolchain pinning, and CI configured from the start. The result is a clean
baseline that future feature work can build on without revisiting tooling.

## Scope

In scope:

- A binary crate named `fxrank` targeting **edition 2024**.
- `main()` printing `Hello, world!`, plus a pure, testable helper function.
- A passing unit test using Rust's **built-in** `#[test]` harness (no extra deps).
- Formatter configuration (`rustfmt`).
- Linter configuration (`clippy`) via the `Cargo.toml` `[lints]` table.
- Toolchain configuration (`rust-toolchain.toml`, channel `stable`).
- Git initialization with a Rust `.gitignore`.
- A GitHub Actions CI workflow gating format, lint, and tests.

Out of scope:

- Any application logic beyond the greeting.
- External testing/assertion crates (built-in harness only).
- Publishing, release automation, or dependency management beyond what the
  scaffold needs.

## Components

### `Cargo.toml`

- `[package]`: name `fxrank`, `edition = "2024"`, a starting `version` of `0.1.0`.
- `[lints.clippy]`: enable `all` and `pedantic` at `warn` level. This is the
  modern, centralized way to configure clippy (replaces scattered
  `#![warn(...)]` crate attributes).

### `src/main.rs`

- `fn greeting() -> String` returning `"Hello, world!"` — a pure function so the
  behavior is unit-testable (capturing `println!` output is not clean).
- `fn main()` that prints `greeting()`.
- A `#[cfg(test)]` module with one test asserting `greeting()` equals
  `"Hello, world!"`.

### `rustfmt.toml`

- `edition = "2024"`.
- `max_width = 100`.
- `newline_style = "Unix"`.

Only stable rustfmt options are used so `cargo fmt` works on the pinned stable
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
  1. Checkout.
  2. `cargo fmt --check`.
  3. `cargo clippy --all-targets -- -D warnings`.
  4. `cargo test`.
- Relies on `rust-toolchain.toml` for toolchain + component resolution.

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
| Edition | 2024 | Latest stable edition; toolchain (1.96.0) supports it. |
| Testing | Built-in harness | Zero dependencies; sufficient for current scope. |
| Toolchain pin | `channel = "stable"` | Auto-tracks latest stable; no manual version bumps. |
| Clippy config | `[lints.clippy]` in `Cargo.toml` | Centralized, modern; `-D warnings` in CI enforces it. |
| Repo/CI | git + `.gitignore` + GitHub Actions | Reproducible baseline with format/lint/test gating. |
