//! Red-path tests keep the `unified_*_red` names for cross-surface edge cases
//! that should fail fast or prove defensive behavior across the unified store.

use batpak_testkit::red_kinds;
use batpak_testkit::red_test_coord;

use red_kinds::*;
use red_test_coord::*;

use batpak_testkit::prelude::*;
use tempfile::TempDir;

#[test]
fn group_commit_batches_under_load() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_group_commit_max_batch(32)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..32 {
        let opts = AppendOptions::new()
            .with_idempotency(batpak::id::IdempotencyKey::from(u128::from(i) + 1));
        let _ = store
            .append_with_options(&coord, kind_a(), &payload(i), opts)
            .expect("append");
    }
    assert_eq!(
        store.by_entity("entity:test").len(),
        32,
        "PROPERTY: group commit must persist all 32 events.\n\
         Investigate: src/store/write/writer.rs writer_loop batch drain."
    );
    store.close().expect("close");
}

#[test]
fn group_commit_batch_1_is_backward_compat() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_group_commit_max_batch(1);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    let _ = store.append(&coord, kind_a(), &payload(0)).expect("append");
    assert_eq!(store.by_entity("entity:test").len(), 1);
    store.close().expect("close");
}

#[test]
fn group_commit_requires_idempotency_key_when_batch_gt_1() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_group_commit_max_batch(32);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    let result = store.append(&coord, kind_a(), &payload(0));
    assert!(
        matches!(result, Err(StoreError::IdempotencyRequired)),
        "PROPERTY: group commit (batch>1) must require idempotency keys.\n\
         Got: {result:?}.\n\
         Investigate: src/store/mod.rs do_append idempotency enforcement."
    );
    store.close().expect("close");
}

#[test]
fn group_commit_mid_batch_shutdown_safe() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_group_commit_max_batch(64)
        .with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..10 {
        let opts = AppendOptions::new()
            .with_idempotency(batpak::id::IdempotencyKey::from(u128::from(i) + 1));
        let _ = store
            .append_with_options(&coord, kind_a(), &payload(i), opts)
            .expect("append");
    }
    store.close().expect("close");
    let store2 = Store::open(StoreConfig::new(dir.path())).expect("reopen");
    assert_eq!(
        store2.by_entity("entity:test").len(),
        10,
        "PROPERTY: events committed before close must survive.\n\
         Investigate: src/store/write/writer.rs shutdown drain."
    );
    store2.close().expect("close");
}
