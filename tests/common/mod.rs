//! Shared test helpers used across integration test files.
//!
//! Each test file that needs these helpers includes:
//! ```ignore
//! mod common;
//! ```
//!
//! Use `common::small_segment_store()` for tests that need to force segment
//! rotation, or `common::default_store()` for tests that just need a working
//! Store.

// justifies: shared test helpers are pulled in by multiple integration binaries; each binary only invokes a subset so dead_code is expected per-binary.
#![allow(dead_code)]

pub mod proptest;

use batpak::prelude::*;
use tempfile::TempDir;

/// Open a Store with small segments (4 KB) and per-event fsync.
/// Use this for tests that need to force rapid segment rotation.
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

/// Open a Store with 64 KB segments using a caller-supplied TempDir.
/// Use this when the test needs to keep the TempDir alive across reopens.
pub fn medium_segment_store(dir: &TempDir) -> Store {
    let mut config = StoreConfig::new(dir.path());
    config.segment_max_bytes = 64 * 1024;
    Store::open(config).expect("open store")
}

/// Standard test coordinate: `entity:test` / `scope:test`.
pub fn test_coord() -> Coordinate {
    Coordinate::new("entity:test", "scope:test").expect("coord")
}

/// Standard test event kind in the user category.
pub fn test_kind() -> EventKind {
    EventKind::custom(0xF, 1)
}
