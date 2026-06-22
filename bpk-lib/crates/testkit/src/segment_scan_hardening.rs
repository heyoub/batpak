//! Shared frame/segment-building fixtures for the split
//! `segment_scan_hardening*` integration harnesses.
//!
//! Included via `#[path = "support/segment_scan_hardening.rs"]` by every
//! `segment_scan_hardening*` test binary. The harness was split out of a single
//! 1406-line file (over the 500-line cap) along its corruption-family seam; the
//! frame/segment plumbing every family needs — the store seeder, the segment
//! locator, the user-entry filter, and the frame-region header parser — lives
//! here so each test binary stays small while sharing one source of truth for
//! how a hardening segment is built and read.
//!
//! Only helpers reachable from EVERY split binary live here: `KIND`, `config`,
//! `segment_path`, `seed_store`, `frame_scan_header_end`, and `user_entries`
//! (`KIND` is consumed inside `seed_store`, which every binary calls). Helpers
//! used by some-but-not-all binaries stay inline in the families that use them
//! — the SIDX-stripper (`strip_sidx`, three binaries), the legacy SDX2
//! downgrade, the frame-length poisoner, the `RawFramePayload` rewrite/replace
//! machinery, and the batched seeder — so this module carries no `dead_code`
//! surface in any binary that includes it (a `pub` item in a `#[path]` module
//! still warns as dead in a binary crate that never calls it; see ADR-0012).

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::segment::SEGMENT_EXTENSION;
use batpak::store::{Store, StoreConfig};
use tempfile::TempDir;

pub const KIND: EventKind = EventKind::custom(0xE, 2);

pub fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false) // force a frame scan on reopen
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1)
}

pub fn segment_path(dir: &TempDir) -> std::path::PathBuf {
    let mut out = None;
    for entry in std::fs::read_dir(dir.path()).expect("read data dir") {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some(SEGMENT_EXTENSION) {
            assert!(
                out.is_none(),
                "test populates exactly one segment; found multiple: {path:?}"
            );
            out = Some(path);
        }
    }
    out.expect("exactly one segment must exist")
}

pub fn seed_store(dir: &TempDir, count: u32) {
    let store = Store::open(config(dir)).expect("open store");
    let coord = Coordinate::new("entity:scan", "scope:test").expect("valid coord");
    for i in 0..count {
        let _ = store
            .append(&coord, KIND, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.close().expect("clean close");
}

pub fn frame_scan_header_end(bytes: &[u8]) -> usize {
    let header_len = u32::from_be_bytes(bytes[4..8].try_into().expect("4-byte header len"));
    8 + usize::try_from(header_len).expect("segment header len fits usize")
}

pub fn user_entries(store: &Store) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect()
}
