// justifies: INV-SUBSCRIPTION-STATE-MACHINE; tests intentionally exercise blocking subscription APIs under bounded producer/timeout probes.
#![allow(clippy::disallowed_methods)]
//! Advanced Store subscription delivery and SubscriptionOps integration tests.

mod support;
use batpak::store::Store;
use std::sync::Arc;
use support::prelude::*;
use tempfile::TempDir;

#[path = "support/small_store.rs"]
mod small_store_support;

fn test_store() -> (TempDir, Store) {
    small_store_support::small_segment_store().expect("small segment store")
}

#[test]
fn subscription_receives_matching_events() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:sub", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity:sub");
    let sub = store.subscribe_lossy(&region);

    // Write from another thread so recv doesn't deadlock
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::Builder::new()
        .name("store-advanced-sub-recv-writer".into())
        .spawn(move || {
            for i in 0..3 {
                store_w
                    .append(&coord_w, kind, &serde_json::json!({"i": i}))
                    .expect("append");
            }
        })
        .expect("spawn subscription recv writer thread");
    writer.join().expect("writer");

    // Should receive 3 matching notifications
    let mut count = 0;
    // Use try_recv in a loop since channel is bounded and events already sent
    let rx = sub.receiver();
    while let Ok(notif) = rx.try_recv() {
        if region.matches_event(notif.coord.entity(), notif.coord.scope(), notif.kind) {
            count += 1;
        }
    }
    assert_eq!(
        count, 3,
        "PROPERTY: subscription must deliver exactly 3 notifications for 3 matching appends.\n\
         Investigate: src/store/delivery/subscription.rs, src/store/mod.rs writer broadcast.\n\
         Common causes: broadcast channel dropped before all events sent, region filter too narrow.\n\
         Run: cargo test --test store_subscription_behaviorsubscription_receives_matching_events"
    );

    store.sync().expect("sync");
}

#[test]
fn subscription_filters_by_region() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let kind = EventKind::custom(0xF, 1);

    // Subscribe only to entity:a
    let region = Region::entity("entity:a");
    let sub = store.subscribe_lossy(&region);

    let store_w = Arc::clone(&store);
    let writer = std::thread::Builder::new()
        .name("store-advanced-sub-filter-writer".into())
        .spawn(move || {
            let coord_a = Coordinate::new("entity:a", "scope:test").expect("valid coord");
            let coord_b = Coordinate::new("entity:b", "scope:test").expect("valid coord");
            store_w
                .append(&coord_a, kind, &serde_json::json!({"target": "a"}))
                .expect("append a");
            store_w
                .append(&coord_b, kind, &serde_json::json!({"target": "b"}))
                .expect("append b");
            store_w
                .append(&coord_a, kind, &serde_json::json!({"target": "a2"}))
                .expect("append a2");
        })
        .expect("spawn subscription filter writer thread");
    writer.join().expect("writer");

    // Raw receiver gets all events, but region filter should match only entity:a
    let rx = sub.receiver();
    let mut matching = 0;
    while let Ok(notif) = rx.try_recv() {
        if region.matches_event(notif.coord.entity(), notif.coord.scope(), notif.kind) {
            matching += 1;
        }
    }
    assert_eq!(
        matching, 2,
        "PROPERTY: subscription filtered to entity:a must match exactly 2 of 3 appended events.\n\
         Investigate: src/store/delivery/subscription.rs region filter, src/store/mod.rs broadcast.\n\
         Common causes: region predicate not applied, entity prefix match too broad or too narrow.\n\
         Run: cargo test --test store_subscription_behaviorsubscription_filters_by_region"
    );

    store.sync().expect("sync");
}

// --- SubscriptionOps::map ---
#[test]
fn subscription_ops_map_transforms_notifications() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:map", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:map");

    let sub = store.subscribe_lossy(&region);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-map-writer".into())
        .spawn(move || {
            store_w
                .append(&coord_w, kind, &serde_json::json!({"v": 1}))
                .expect("append");
        })
        .expect("spawn subscription ops map writer thread")
        .join()
        .expect("writer");

    // Use map to transform: change the kind to a custom marker
    let marker_kind = EventKind::custom(0xA, 0xBB);
    let mut ops = sub.ops().map(move |n| {
        let mut transformed = n.clone();
        transformed.kind = marker_kind;
        Some(transformed)
    });

    // Use try-based approach: events are already sent
    let rx_result = std::thread::Builder::new()
        .name("store-advanced-sub-ops-map-recv".into())
        .spawn(move || ops.recv())
        .expect("spawn subscription ops map recv thread")
        .join()
        .expect("join subscription ops map recv thread");

    assert!(
        rx_result.is_some(),
        "PROPERTY: SubscriptionOps::map must pass through transformed notifications.\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::map and recv.\n\
         Common causes: map_fn not applied in recv loop, map returns None.\n\
         Run: cargo test --test store_subscription_behaviorsubscription_ops_map_transforms_notifications"
    );
    let notif = rx_result.expect("mapped notification should be Some per preceding assert");
    assert_eq!(
        notif.kind, marker_kind,
        "PROPERTY: SubscriptionOps::map must apply the transformation function to notifications.\n\
         Investigate: src/store/delivery/subscription.rs recv map_fn application.\n\
         Common causes: map_fn ignored, original notification returned instead.\n\
         Run: cargo test --test store_subscription_behaviorsubscription_ops_map_transforms_notifications"
    );

    store.sync().expect("sync");
}

// --- SubscriptionOps::filter chains ---
// Intentional: inner `ops.recv()` exhaustion probes are bounded by the outer
// mpsc `recv_timeout` assertions below.

#[test]
fn subscription_ops_filter_chains_correctly() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let kind1 = EventKind::custom(0xF, 1);
    let kind2 = EventKind::custom(0xF, 2);
    let coord = Coordinate::new("entity:filt", "scope:test").expect("valid coord");
    let region = Region::entity("entity:filt");

    let sub = store.subscribe_lossy(&region);

    // Chain two filters and take(2) to prevent blocking forever:
    // first accepts kind1 only, second is always-true (AND semantics)
    let mut ops = sub
        .ops()
        .filter(move |n| n.kind == kind1)
        .filter(|_n| true)
        .take(2);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::Builder::new()
        .name("store-advanced-sub-ops-filter-writer".into())
        .spawn(move || {
            store_w
                .append(&coord_w, kind1, &serde_json::json!({"k": 1}))
                .expect("append");
            store_w
                .append(&coord_w, kind2, &serde_json::json!({"k": 2}))
                .expect("append");
            store_w
                .append(&coord_w, kind1, &serde_json::json!({"k": 3}))
                .expect("append");
        })
        .expect("spawn subscription ops filter writer thread");

    let result = [ops.recv(), ops.recv()];

    writer.join().expect("writer");

    assert_eq!(
        result.iter().flatten().count(),
        2,
        "PROPERTY: chained filter with AND semantics must pass only kind1 events (2 of 3).\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::filter, recv.\n\
         Common causes: filters not chained, last filter replaces previous.\n\
         Run: cargo test --test store_subscription_behaviorsubscription_ops_filter_chains_correctly"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-filter-exhausted-recv".into())
        .spawn(move || {
            let exhausted = ops.recv().is_none();
            let _ = tx.send(exhausted);
        })
        .expect("spawn exhausted subscription ops filter recv thread");
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(100))
            .expect(
                "PROPERTY: exhausted filtered SubscriptionOps::take recv must return immediately while store is open"
            ),
        "PROPERTY: exhausted filtered SubscriptionOps::take recv must return None"
    );

    store.sync().expect("sync");
}

// --- SubscriptionOps::take ---
// Intentional: inner `ops.recv()` exhaustion probes are bounded by the outer
// mpsc `recv_timeout` assertions below.

#[test]
fn subscription_ops_take_limits_count() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:take", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:take");

    let sub = store.subscribe_lossy(&region);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-take-writer".into())
        .spawn(move || {
            for i in 0..5 {
                store_w
                    .append(&coord_w, kind, &serde_json::json!({"i": i}))
                    .expect("append");
            }
            drop(store_w);
        })
        .expect("spawn subscription ops take writer thread")
        .join()
        .expect("writer");

    let mut ops = sub.ops().take(3);
    let result = [ops.recv(), ops.recv(), ops.recv()];

    assert_eq!(
        result.iter().flatten().count(),
        3,
        "PROPERTY: SubscriptionOps::take(3) must return at most 3 notifications from 5 events.\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::take, recv count check.\n\
         Common causes: count not incremented in recv, limit check after return.\n\
         Run: cargo test --test store_subscription_behaviorsubscription_ops_take_limits_count"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-take-exhausted-recv".into())
        .spawn(move || {
            let exhausted = ops.recv().is_none();
            let _ = tx.send(exhausted);
        })
        .expect("spawn exhausted subscription ops take recv thread");
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(100))
            .expect(
                "PROPERTY: exhausted SubscriptionOps::take recv must return immediately while store is open"
            ),
        "PROPERTY: exhausted SubscriptionOps::take recv must return None"
    );

    store.sync().expect("sync");
}
