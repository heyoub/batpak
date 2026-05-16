//! Canonical `entity:test`/`scope:test` coordinate for the unified red tests
//! that reuse the same coordinate across appends.
//!
//! Included via `#[path = "support/red_test_coord.rs"]` only by tests that
//! actually call `test_coord()`. Tests that build ad-hoc coordinates per
//! sub-case (topology parity, watch) do not include this module, so the
//! helper is never dead in any binary that loads it (see ADR-0012).

use batpak::prelude::Coordinate;

/// Canonical red-test coordinate: `entity:test` / `scope:test`.
pub fn test_coord() -> Coordinate {
    Coordinate::new("entity:test", "scope:test").expect("coord")
}
