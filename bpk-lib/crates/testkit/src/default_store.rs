//! Shared default-config store helper for integration tests.
//!
//! Included via `#[path = "support/default_store.rs"]` by tests that just need
//! a freshly opened store with the library-default [`StoreConfig`] (no segment,
//! batch, or sync overrides). This is the sibling of `small_segment_store` /
//! `medium_segment_store` for the very common "open a default store under a
//! throwaway temp dir" idiom. Every consumer uses this single function, so no
//! `dead_code` escape hatch is required (see ADR-0012).
//!
//! The returned [`TempDir`] is placed first in the tuple so that the natural
//! `let (_tmp, store) = default_temp_store();` binding drops `store` before
//! `_tmp`: `Store::drop` sends `Shutdown` and syncs the active segment, so the
//! backing directory must still exist while the store is closing. Keep `_tmp`
//! bound (even as `_tmp`) for the lifetime of the store, or reuse it to reopen
//! the same data directory across multiple `Store::open` calls.

use batpak::prelude::*;
use tempfile::TempDir;

/// Open a store with the library-default `StoreConfig` under a fresh temp dir.
/// Returns `(TempDir, Store)` so the temp dir outlives the store under the
/// usual `let (_tmp, store) = ..;` destructuring (bindings drop in reverse).
pub fn default_temp_store() -> (TempDir, Store) {
    let dir = tempfile::tempdir().expect("create temp dir for default store");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open default store");
    (dir, store)
}
