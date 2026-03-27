#![allow(clippy::disallowed_methods, clippy::unwrap_used)] // tests use thread::spawn for producers
//! Integration tests for SubscriptionOps: filter, take, and combined chains.
//! [SPEC:tests/subscription_ops.rs]

use batpak::prelude::*;
use batpak::store::{Notification, Store, StoreConfig};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn test_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    (store, dir)
}

#[test]
fn ops_recv_without_filters() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity:");
    let sub = store.subscribe(&region);
    let mut ops = sub.ops();

    // Spawn producer thread — recv() is blocking so append must happen concurrently.
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::spawn(move || {
        // Small delay to ensure subscriber is ready.
        thread::sleep(Duration::from_millis(20));
        let payload = serde_json::json!({"hello": "world"});
        store_w.append(&coord_w, kind, &payload).expect("append");
    });

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS RECV WITHOUT FILTERS: expected a notification, got None.\n\
         Check: src/store/subscription.rs SubscriptionOps::recv(), writer broadcast."
    );
    let notif = notif.unwrap();
    assert_eq!(
        notif.coord.entity(),
        "entity:1",
        "OPS RECV WITHOUT FILTERS: notification entity mismatch."
    );
    assert_eq!(
        notif.kind, kind,
        "OPS RECV WITHOUT FILTERS: notification kind mismatch."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_filter_passes_matching() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let target_kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity:");
    let sub = store.subscribe(&region);
    let mut ops = sub
        .ops()
        .filter(move |n: &Notification| n.kind == target_kind);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let payload = serde_json::json!({"x": 1});
        // Append an event with the matching kind.
        store_w
            .append(&coord_w, target_kind, &payload)
            .expect("append matching");
    });

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS FILTER PASSES MATCHING: filter should pass events with matching kind.\n\
         Check: src/store/subscription.rs SubscriptionOps::filter()."
    );
    let notif = notif.unwrap();
    assert_eq!(
        notif.kind, target_kind,
        "OPS FILTER PASSES MATCHING: received event has wrong kind."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_filter_rejects_non_matching() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let wanted_kind = EventKind::custom(0xF, 1);
    let unwanted_kind = EventKind::custom(0xF, 2);

    let region = Region::entity("entity:");
    let sub = store.subscribe(&region);
    // Filter only passes wanted_kind. Take 1 so the test terminates.
    let mut ops = sub
        .ops()
        .filter(move |n: &Notification| n.kind == wanted_kind)
        .take(1);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let payload = serde_json::json!({"x": 1});
        // First: append non-matching event — should be rejected by filter.
        store_w
            .append(&coord_w, unwanted_kind, &payload)
            .expect("append unwanted");
        // Second: append matching event — should pass through.
        store_w
            .append(&coord_w, wanted_kind, &payload)
            .expect("append wanted");
    });

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS FILTER REJECTS: should have received the matching event after the non-matching one."
    );
    let notif = notif.unwrap();
    assert_eq!(
        notif.kind, wanted_kind,
        "OPS FILTER REJECTS: the non-matching event should have been filtered out,\n\
         but received kind {:?} instead of {:?}.",
        notif.kind, wanted_kind
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_take_limits_count() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity:");
    let sub = store.subscribe(&region);
    let mut ops = sub.ops().take(2);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let payload = serde_json::json!({"x": 1});
        for _ in 0..5 {
            store_w.append(&coord_w, kind, &payload).expect("append");
        }
    });

    // Should receive exactly 2.
    let first = ops.recv();
    assert!(first.is_some(), "OPS TAKE: first recv should return Some.");
    let second = ops.recv();
    assert!(
        second.is_some(),
        "OPS TAKE: second recv should return Some."
    );
    let third = ops.recv();
    assert!(
        third.is_none(),
        "OPS TAKE: third recv should return None after take(2), but got Some.\n\
         Check: src/store/subscription.rs SubscriptionOps::take() limit enforcement."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_filter_and_take_combined() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let wanted_kind = EventKind::custom(0xF, 1);
    let other_kind = EventKind::custom(0xF, 2);

    let region = Region::entity("entity:");
    let sub = store.subscribe(&region);
    // Filter for wanted_kind only, take 2.
    let mut ops = sub
        .ops()
        .filter(move |n: &Notification| n.kind == wanted_kind)
        .take(2);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let payload = serde_json::json!({"x": 1});
        // Interleave matching and non-matching events.
        store_w
            .append(&coord_w, other_kind, &payload)
            .expect("append other");
        store_w
            .append(&coord_w, wanted_kind, &payload)
            .expect("append wanted 1");
        store_w
            .append(&coord_w, other_kind, &payload)
            .expect("append other");
        store_w
            .append(&coord_w, wanted_kind, &payload)
            .expect("append wanted 2");
        store_w
            .append(&coord_w, wanted_kind, &payload)
            .expect("append wanted 3");
    });

    // Should receive exactly 2 matching events, skipping the non-matching ones.
    let first = ops.recv();
    assert!(
        first.is_some(),
        "OPS COMBINED: first recv should return Some."
    );
    assert_eq!(
        first.unwrap().kind,
        wanted_kind,
        "OPS COMBINED: first event should be wanted_kind."
    );

    let second = ops.recv();
    assert!(
        second.is_some(),
        "OPS COMBINED: second recv should return Some."
    );
    assert_eq!(
        second.unwrap().kind,
        wanted_kind,
        "OPS COMBINED: second event should be wanted_kind."
    );

    let third = ops.recv();
    assert!(
        third.is_none(),
        "OPS COMBINED: third recv should return None (take(2) exhausted), but got Some.\n\
         Check: src/store/subscription.rs filter + take interaction."
    );

    producer.join().expect("producer thread");
}
