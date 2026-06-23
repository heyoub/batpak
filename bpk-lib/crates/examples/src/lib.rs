//! Family-wide runnable examples for the batpak family.
//!
//! This crate has no library surface of its own — it exists only to host the
//! `examples/` targets, which depend on the family crates and are compile-gated
//! by the workspace `clippy --all-targets` pass. Run one with:
//! `cargo run -p batpak-examples --example quickstart`.
//!
//! It is the single home for demos: there are no per-crate `examples/` folders.
