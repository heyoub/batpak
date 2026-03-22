# Contributing to free-batteries

## Setup

1. Clone the repo:
   ```bash
   git clone <repo-url>
   cd free-b/free-batteries
   ```

2. Build:
   ```bash
   cargo build --all-features
   ```

3. Test:
   ```bash
   cargo test --all-features
   ```

## Code Style

- `cargo fmt` for formatting
- `cargo clippy --all-features -- -D warnings` for linting
- No `unwrap()`, `panic!()`, `todo!()`, `dbg!()` in production code (enforced via clippy lints)
- Synchronous API only — no async runtime dependency

## Architecture

Reading order: coordinate → event → guard → pipeline → store

See [TUNING.md](free-batteries/TUNING.md) for configuration guidance.

## Before Submitting

```bash
cargo fmt --check
cargo clippy --all-features -- -D warnings
cargo test --all-features
cargo test --no-default-features
cargo doc --all-features --no-deps
```
