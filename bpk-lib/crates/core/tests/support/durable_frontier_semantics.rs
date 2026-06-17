//! Shared fixtures for the split `durable_frontier_semantics*` integration
//! harnesses.
//!
//! Included via `#[path = "support/durable_frontier_semantics.rs"]` by every
//! `durable_frontier_semantics*` test binary. The harness was split out of a
//! single 1044-line file (over the 500-line cap) along its lifecycle vs.
//! frontier-semantics seam.
//!
//! This module holds ONLY the helpers that EVERY split binary uses: the test
//! event [`kind`], the [`coord`] builder, and the index-entry-to-[`point`]
//! adapter. Both the lifecycle and the frontier-semantics binaries construct
//! coordinates, append the custom kind, and read HLC points back from index
//! entries, so all three items are consumed in every binary — no `dead_code`
//! surface and no escape hatch required. Family-specific machinery (lifecycle
//! forging plumbing, projection fixtures, fault-injection config) stays inline
//! in the binary that owns it.

use batpak::prelude::{Coordinate, EventKind};
use batpak::store::HlcPoint;

/// Custom event kind every frontier harness appends.
pub fn kind() -> EventKind {
    EventKind::custom(0xF, 0x90)
}

/// Build the deterministic test coordinate for `entity` under `scope:test`.
pub fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, "scope:test").expect("coord")
}

/// Read the HLC point an index entry was written at.
pub fn point(entry: &batpak::store::index::IndexEntry) -> HlcPoint {
    HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    }
}
