//! Integration-test support helpers for batpak.
//!
//! These modules exercise only batpak's public API and are shared across the
//! `batpak` integration-test suite as a real dev-dependency crate. Because the
//! `pub use` re-exports here are genuine re-exports (not `#[path]`-included into
//! a test binary), `unused_imports` fires honestly and needs no escape hatch.

pub mod bounded_blocking;
pub mod bounded_writer_reply;
pub mod chaos_testing;
pub mod control_plane_surface;
pub mod cursor_durability;
pub mod default_store;
pub mod durable_frontier_semantics;
pub mod durable_frontier_waits;
pub mod fuzz_chaos_feedback;
pub mod medium_store;
pub mod prelude;
pub mod projection_cache;
pub mod raw_projection_mode;
pub mod red_counters;
pub mod red_kind_b;
pub mod red_kinds;
pub mod red_test_coord;
pub mod red_versioned_counters;
pub mod segment_scan_hardening;
pub mod small_store;
pub mod store_error_contract;
