//! Shared medium-segment store helper for integration tests.
//!
//! Included via `#[path = "support/medium_store.rs"]` by tests that need a
//! 64 KB-segment store and the standard `entity:test`/`scope:test`
//! coordinate. Every consumer of this module uses both functions, so no
//! `dead_code` escape hatch is required (see ADR-0012).

use batpak::prelude::*;
use tempfile::TempDir;

/// Open a store with 64 KB segments under the caller-supplied temp directory.
/// The caller owns the `TempDir` so a test can reopen the same data directory
/// across multiple `Store::open` calls.
pub fn medium_segment_store(dir: &TempDir) -> Store {
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(64 * 1024);
    Store::open(config).expect("open store")
}

/// The standard test coordinate: `entity:test` / `scope:test`. Shared with
/// the unified red surface so test bodies can anchor events without repeating
/// the string literals.
pub fn test_coord() -> Coordinate {
    Coordinate::new("entity:test", "scope:test").expect("coord")
}
