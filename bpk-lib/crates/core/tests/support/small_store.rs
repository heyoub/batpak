//! Shared small-segment store helper for integration tests.
//!
//! Included via `#[path = "support/small_store.rs"]` by any test that needs a
//! 4 KB-segment store with per-event fsync. Every consumer uses this single
//! function, which keeps the `dead_code` surface honest (see ADR-0012).

use batpak::prelude::*;
use std::path::Path;
use tempfile::TempDir;

/// Store configuration used by [`small_segment_store`] and reopen tests.
pub fn small_segment_store_config(data_dir: &Path) -> StoreConfig {
    StoreConfig::new(data_dir)
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1)
}

/// Open a store with 4 KB segments and per-event fsync, under a fresh temp
/// directory. Returns `(TempDir, Store)` so the natural
/// `let (_dir, store) = small_segment_store()?;` binding drops `store` before
/// the `TempDir`: `Store::drop` syncs the active segment during shutdown, so the
/// backing directory must outlive the store. Reuse the `TempDir` to reopen the
/// same data directory across `Store::open` calls.
pub fn small_segment_store() -> Result<(TempDir, Store), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let config = small_segment_store_config(dir.path());
    let store = Store::open(config)?;
    Ok((dir, store))
}
