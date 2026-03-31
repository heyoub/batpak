# Contributing to `batpak`

## Canonical Environment

The canonical development environment is the checked-in Dev Container in [`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json). Native Windows and Linux development are still supported, but they must pass the same integrity gates.

From the repo root:

```bash
cd batpak
cargo run --manifest-path tools/integrity/Cargo.toml -- doctor --strict
```

`doctor --strict` verifies the expected toolchain, integrity tooling, and line-ending contract before you start.

## Daily Commands

From [`batpak/`](batpak/):

```bash
just doctor
just traceability
just structural
just ci
```

The canonical full gate set is also available through [`scripts/verify-all.sh`](scripts/verify-all.sh).

## Code Style

- `cargo fmt --check`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo deny check`
- No `unwrap()`, `panic!()`, `todo!()`, or `dbg!()` in production code
- Synchronous API only; no async runtime dependency in production
- Host-specific linker or environment overrides must stay out of the repo

## Traceability And Decisions

- Requirement, invariant, flow, and proving-artifact links live under [`traceability/`](traceability/).
- Architecture decisions live under [`batpak/docs/adr/`](batpak/docs/adr/).
- If you add a new public surface, named flow, or invariant, update those registries in the same change.

## Before Submitting

Run this from [`batpak/`](batpak/):

```bash
just ci
```

That expands to:

```bash
cargo run --manifest-path tools/integrity/Cargo.toml -- doctor --strict
cargo run --manifest-path tools/integrity/Cargo.toml -- traceability-check
cargo run --manifest-path tools/integrity/Cargo.toml -- structural-check
cargo fmt --check
cargo clippy --all-features --all-targets -- -D warnings
cargo deny check
cargo nextest run --profile ci --all-features
cargo test --doc --all-features
cargo check --all-features
cargo check --no-default-features
cargo bench --no-run --all-features
```
