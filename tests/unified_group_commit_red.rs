// justifies: unified red-path group-commit tests use unwrap/panic as assertion style and narrow bounded test counters that fit within u32.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::panic)]

#[path = "support/unified_red.rs"]
mod unified_red_support;

use unified_red_support::*;

#[test]
fn group_commit_batches_under_load() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_group_commit_max_batch(32)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..32 {
        let opts = AppendOptions::new().with_idempotency(i as u128 + 1);
        store
            .append_with_options(&coord, kind_a(), &payload(i), opts)
            .expect("append");
    }
    assert_eq!(
        store.stream("entity:test").len(),
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
    store.append(&coord, kind_a(), &payload(0)).expect("append");
    assert_eq!(store.stream("entity:test").len(), 1);
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
        let opts = AppendOptions::new().with_idempotency(i as u128 + 1);
        store
            .append_with_options(&coord, kind_a(), &payload(i), opts)
            .expect("append");
    }
    store.close().expect("close");
    let store2 = Store::open(StoreConfig::new(dir.path())).expect("reopen");
    assert_eq!(
        store2.stream("entity:test").len(),
        10,
        "PROPERTY: events committed before close must survive.\n\
         Investigate: src/store/write/writer.rs shutdown drain."
    );
    store2.close().expect("close");
}
