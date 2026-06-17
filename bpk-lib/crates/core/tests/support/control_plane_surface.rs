//! Shared fixtures for the split `control_plane_surface*` integration harnesses.
//!
//! Included via `#[path = "support/control_plane_surface.rs"]` by every
//! `control_plane_surface*` test binary. The harness was split out of a single
//! 1049-line file (over the 500-line cap) into per-seam binaries
//! (`*_smoke`, `*_ticket`, `*_fence`, `*_pressure`). Only the items used by
//! EVERY split binary live here -- the shared `KIND_COUNTER` event kind and the
//! `test_config` store builder -- so this file never introduces a dead-code
//! surface in any binary (a `pub` item unused by one integration binary still
//! warns under `-D warnings`). Family-specific helpers (ticket-wait helpers,
//! `CounterProjection`) stay inline in the binaries that use them.

use batpak::event::EventKind;
use batpak::store::StoreConfig;
use tempfile::TempDir;

/// The custom event kind every control-plane surface test appends and projects.
pub const KIND_COUNTER: EventKind = EventKind::custom(0xF, 1);

/// Build a control-plane store config rooted at `dir` that fsyncs every event
/// so fence/ticket/pressure timing is deterministic across the split binaries.
pub fn test_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path()).with_sync_every_n_events(1)
}
