//! Shared NON-test fixtures for the split `durable_frontier_waits*`
//! integration harnesses.
//!
//! Included via `#[path = "support/durable_frontier_waits.rs"]` by every
//! `durable_frontier_waits*` test binary. The harness was split out of a single
//! 620-line file (over the 500-line cap) along the seam "wait surfaces vs
//! append-gate surfaces". To avoid `dead_code` warnings under `-D warnings`
//! (where `pub` does NOT suppress dead-code in a binary crate), this module
//! holds ONLY the fixtures that EVERY split binary consumes: [`WAIT_SCOPE`],
//! [`coord`], [`kind`], [`point`] for coordinate/kind/HLC construction, and
//! [`open_store`], the default-policy store opener — all used by both families.
//!
//! Family-specific helpers (the wait-only `append_number`/`assert_wait_timeout`
//! and the R3 terminal-restart-policy opener; the gate-only gate builders and
//! `batch_item`) live inline in the binary that uses them, NOT here, precisely
//! so nothing in this module is dead in any binary.

use batpak::prelude::{Coordinate, EventKind};
use batpak::store::{HlcPoint, Store, StoreConfig};
use tempfile::TempDir;

pub const WAIT_SCOPE: &str = "scope:frontier-waits";

pub fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, WAIT_SCOPE).expect("valid wait coordinate")
}

pub fn kind() -> EventKind {
    EventKind::custom(0xF, 0x94)
}

pub fn point(entry: &batpak::store::index::IndexEntry) -> HlcPoint {
    HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    }
}

pub fn open_store(sync_every_n_events: u32) -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store =
        Store::open(StoreConfig::new(dir.path()).with_sync_every_n_events(sync_every_n_events))
            .expect("open store");
    (dir, store)
}
