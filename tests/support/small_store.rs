//! Shared small-segment store helper for integration tests.
//!
//! Included via `#[path = "support/small_store.rs"]` by any test that needs a
//! 4 KB-segment store with per-event fsync. Every consumer uses this single
//! function, which keeps the `dead_code` surface honest (see ADR-0012).

use batpak::prelude::*;
use batpak::store::SyncConfig;
use tempfile::TempDir;

/// Open a store with 4 KB segments and per-event fsync, under a fresh temp
/// directory. The returned `TempDir` must stay alive for the lifetime of the
/// `Store`; drop either before the other only if the test deliberately
/// exercises that shape.
pub fn small_segment_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    (store, dir)
}
