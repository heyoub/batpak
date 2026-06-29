//! Red-path tests keep the `unified_*_red` names for cross-surface edge cases
//! that should fail fast or prove defensive behavior across the unified store.
//!
//! PROVES: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST

use batpak_testkit::red_kinds;
use batpak_testkit::red_test_coord;

use red_kinds::*;
use red_test_coord::*;

use batpak_testkit::prelude::*;
use tempfile::TempDir;

#[test]
fn sidx_cold_start_uses_footer() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(512);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..50 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    store.sync().expect("sync");
    store.close().expect("close");
    let store2 =
        Store::open(StoreConfig::new(dir.path()).with_segment_max_bytes(512)).expect("reopen");
    assert_eq!(
        store2.by_entity("entity:test").len(),
        50,
        "PROPERTY: cold start via SIDX footer must recover all events.\n\
         Investigate: src/store/segment/scan.rs SIDX-aware scan_segment_index."
    );
    store2.close().expect("close");
}

#[test]
fn checkpoint_write_load_roundtrip() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_enable_checkpoint(true);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..100 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    store.sync().expect("sync");
    store.close().expect("close writes checkpoint");
    let store2 = Store::open(StoreConfig::new(dir.path()).with_enable_checkpoint(true))
        .expect("reopen from checkpoint");
    assert_eq!(
        store2.by_entity("entity:test").len(),
        100,
        "PROPERTY: checkpoint roundtrip must preserve all events.\n\
         Investigate: src/store/cold_start/checkpoint.rs write_checkpoint + try_load_checkpoint."
    );
    store2.close().expect("close");
}

#[test]
fn stale_checkpoint_falls_back_to_full_rebuild() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_enable_checkpoint(true);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..20 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    store.sync().expect("sync");
    store.close().expect("close");
    let ckpt_path = dir.path().join("index.ckpt");
    if ckpt_path.exists() {
        std::fs::write(&ckpt_path, b"CORRUPT").expect("corrupt checkpoint");
    }
    let store2 = Store::open(StoreConfig::new(dir.path()).with_enable_checkpoint(true))
        .expect("reopen with corrupt checkpoint");
    assert_eq!(
        store2.by_entity("entity:test").len(),
        20,
        "PROPERTY: corrupt checkpoint must fall back to full rebuild.\n\
         Investigate: src/store/cold_start/checkpoint.rs try_load_checkpoint -> None."
    );
    store2.close().expect("close");
}

#[test]
fn post_compact_checkpoint_valid() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(512)
        .with_enable_checkpoint(true);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..50 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    store.sync().expect("sync");
    let (_result, _report) = store
        .compact(&CompactionConfig::default())
        .expect("compact");
    store.close().expect("close writes post-compact checkpoint");
    let store2 = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_enable_checkpoint(true),
    )
    .expect("reopen");
    assert_eq!(
        store2.by_entity("entity:test").len(),
        50,
        "PROPERTY: post-compact checkpoint must be valid.\n\
         Investigate: src/store/lifecycle.rs compact writes checkpoint."
    );
    store2.close().expect("close");
}

#[test]
fn interner_roundtrip() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("intern:entity", "intern:scope").expect("coord");
    let _ = store.append(&coord, kind_a(), &payload(0)).expect("append");
    let entries = store.by_entity("intern:entity");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].coord().entity(), "intern:entity");
    assert_eq!(entries[0].coord().scope(), "intern:scope");
    store.close().expect("close");
}
