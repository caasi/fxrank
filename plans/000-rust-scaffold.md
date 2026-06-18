# Rust Project Scaffold Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scaffold the `fxrank` repository as a minimal, well-tooled Rust binary crate — a tested "Hello, world!" with formatter, linter, toolchain config, and CI.

**Architecture:** A single binary crate (edition 2024). `main()` prints the return value of a pure `greeting()` function, which is covered by one built-in unit test. Tooling (rustfmt, clippy, toolchain channel, CI) is layered on in separate commits so each piece is verifiable on its own.

**Tech Stack:** Rust (edition 2024, stable channel), cargo built-in test harness, rustfmt, clippy, GitHub Actions.

**Paired spec:** `specs/000-rust-scaffold.md`

**Implementation note:** Per project rules, runtime code must NOT be committed to a primary branch — implement on a feature branch (ideally a worktree). The plan/spec docs themselves may live on the primary branch.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `Cargo.toml` | Package manifest; declares crate name/edition and the `[lints.clippy]` config. |
| `src/main.rs` | Entry point: `greeting()` (pure), `main()` (prints it), and the `#[cfg(test)]` test module. |
| `rustfmt.toml` | Formatter configuration. |
| `rust-toolchain.toml` | Toolchain channel + components contributors/CI resolve. |
| `.gitignore` | Excludes the cargo build directory. |
| `.github/workflows/ci.yml` | CI gate: fmt-check → clippy → test. |

---

## Task 1: Crate skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `.gitignore`

- [ ] **Step 1: Create `.gitignore`**

```gitignore
/target
```

- [ ] **Step 2: Create `Cargo.toml`** (manifest only; clippy lints come in Task 4)

```toml
[package]
name = "fxrank"
version = "0.1.0"
edition = "2024"
```

- [ ] **Step 3: Create a minimal `src/main.rs`** so the crate compiles

```rust
fn main() {}
```

- [ ] **Step 4: Verify the crate builds**

Run: `cargo build`
Expected: compiles successfully; creates `Cargo.lock` and `target/`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs .gitignore
git commit -m "feat: initialize fxrank cargo crate skeleton"
```

---

## Task 2: `greeting()` function (TDD)

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test** — append a test module to `src/main.rs` that references the not-yet-existing `greeting()`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_returns_hello_world() {
        assert_eq!(greeting(), "Hello, world!");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test`
Expected: FAIL — compile error `cannot find function 'greeting' in this scope`.

- [ ] **Step 3: Implement `greeting()` and wire it into `main()`** — replace the `fn main() {}` line so the top of `src/main.rs` reads:

```rust
fn greeting() -> &'static str {
    "Hello, world!"
}

fn main() {
    println!("{}", greeting());
}
```

(The `#[cfg(test)]` module from Step 1 stays at the bottom of the file.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test`
Expected: PASS — `test tests::greeting_returns_hello_world ... ok`, `1 passed`.

- [ ] **Step 5: Verify the binary prints the greeting**

Run: `cargo run`
Expected: stdout `Hello, world!`.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat: add tested greeting function"
```

---

## Task 3: Formatter (rustfmt)

**Files:**
- Create: `rustfmt.toml`

- [ ] **Step 1: Create `rustfmt.toml`**

```toml
edition = "2024"
max_width = 100
newline_style = "Unix"
```

- [ ] **Step 2: Format the code, then verify it is clean**

Run: `cargo fmt && cargo fmt --check`
Expected: `cargo fmt --check` exits 0 with no diff output.

- [ ] **Step 3: Commit** (include `src/main.rs` only if formatting changed it)

```bash
git add rustfmt.toml src/main.rs
git commit -m "chore: configure rustfmt"
```

---

## Task 4: Linter (clippy)

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the `[lints.clippy]` table to `Cargo.toml`** (append below the `[package]` table):

```toml
[lints.clippy]
all = "warn"
```

- [ ] **Step 2: Run clippy as CI will, treating warnings as errors**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS — `Finished` with no warnings (the scaffold code is clippy-clean).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: enable clippy all-group lints"
```

---

## Task 5: Toolchain configuration

**Files:**
- Create: `rust-toolchain.toml`

- [ ] **Step 1: Create `rust-toolchain.toml`**

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 2: Verify the toolchain file is honored and the build still works**

Run: `cargo build`
Expected: compiles successfully (rustup resolves the `stable` channel + components).

- [ ] **Step 3: Commit**

```bash
git add rust-toolchain.toml
git commit -m "chore: pin toolchain channel and components"
```

---

## Task 6: CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Create `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
  pull_request:

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test
```

- [ ] **Step 2: Sanity-check the YAML parses**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('ok')"`
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add fmt, clippy, and test workflow"
```

---

## Task 7: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full local gate — the same four checks CI runs**

```bash
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Expected: all four exit 0 — build succeeds, `1 passed`, no fmt diff, no clippy warnings.

- [ ] **Step 2: Confirm the working tree is clean**

Run: `git status --porcelain`
Expected: empty output (everything committed).
