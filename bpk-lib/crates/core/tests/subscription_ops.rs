//! Integration tests for SubscriptionOps: filter, take, and combined chains.
//!
//! PROVES: LAW-004 (Composition Over Construction — ops chain correctly)
//! DEFENDS: FM-009 (Polite Downgrade — map must not silently drop events)
//! INVARIANTS: INV-SUBSCRIPTION-STATE-MACHINE (subscription: open to recv to closed)
//! Intentional: direct `SubscriptionOps::recv()` calls exercise the blocking API
//! after deterministic producer appends; exhaustion probes are bounded by an
//! outer `recv_timeout` channel.

use batpak::store::Notification;
use batpak_testkit::prelude::*;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const PROMPT_EXHAUSTION_TIMEOUT: Duration = Duration::from_secs(2);

use batpak_testkit::small_store as small_store_support;

fn test_store() -> (tempfile::TempDir, Store) {
    small_store_support::small_segment_store().expect("small segment store")
}

#[test]
fn ops_recv_without_filters() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    let mut ops = sub.ops();

    // Spawn producer thread — recv() is blocking so append must happen concurrently.
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-recv-no-filters-producer".into())
        .spawn(move || {
            let payload = serde_json::json!({"hello": "world"});
            store_w.append(&coord_w, kind, &payload).expect("append");
        })
        .expect("spawn producer thread");

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS RECV WITHOUT FILTERS: expected a notification, got None.\n\
         Check: src/store/delivery/subscription.rs SubscriptionOps::recv(), writer broadcast."
    );
    let notif = notif.expect("notification should be Some per preceding assert");
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
fn subscription_notifications_preserve_committed_position() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:position", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity:position");
    let sub = store.subscribe_lossy(&region);
    let mut ops = sub.ops().take(1);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("subscription-position-producer".into())
        .spawn(move || {
            store_w
                .append_with_options(
                    &coord_w,
                    kind,
                    &serde_json::json!({"position": true}),
                    AppendOptions::new().with_position_hint(AppendPositionHint::new(6, 2)),
                )
                .expect("append with position hint");
        })
        .expect("spawn producer thread");

    let notif = ops.recv().expect("position notification");
    assert_eq!(notif.position.lane(), 6);
    assert_eq!(notif.position.depth(), 2);
    assert!(notif.position.wall_ms() > 0);
    assert_eq!(notif.position.sequence(), 0);

    producer.join().expect("producer thread");
}

#[test]
fn ops_filter_passes_matching() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let target_kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    let mut ops = sub
        .ops()
        .filter(move |n: &Notification| n.kind == target_kind);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-filter-matching-producer".into())
        .spawn(move || {
            let payload = serde_json::json!({"x": 1});
            // Append an event with the matching kind.
            store_w
                .append(&coord_w, target_kind, &payload)
                .expect("append matching");
        })
        .expect("spawn producer thread");

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS FILTER PASSES MATCHING: filter should pass events with matching kind.\n\
         Check: src/store/delivery/subscription.rs SubscriptionOps::filter()."
    );
    let notif = notif.expect("notification should be Some per preceding assert");
    assert_eq!(
        notif.kind, target_kind,
        "OPS FILTER PASSES MATCHING: received event has wrong kind."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_filter_rejects_non_matching() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let wanted_kind = EventKind::custom(0xF, 1);
    let unwanted_kind = EventKind::custom(0xF, 2);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    // Filter only passes wanted_kind. Take 1 so the test terminates.
    let mut ops = sub
        .ops()
        .filter(move |n: &Notification| n.kind == wanted_kind)
        .take(1);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-filter-rejects-producer".into())
        .spawn(move || {
            let payload = serde_json::json!({"x": 1});
            // First: append non-matching event — should be rejected by filter.
            store_w
                .append(&coord_w, unwanted_kind, &payload)
                .expect("append unwanted");
            // Second: append matching event — should pass through.
            store_w
                .append(&coord_w, wanted_kind, &payload)
                .expect("append wanted");
        })
        .expect("spawn producer thread");

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS FILTER REJECTS: should have received the matching event after the non-matching one."
    );
    let notif = notif.expect("notification should be Some per preceding assert");
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
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    let mut ops = sub.ops().take(2);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-take-limits-producer".into())
        .spawn(move || {
            let payload = serde_json::json!({"x": 1});
            for _ in 0..5 {
                store_w.append(&coord_w, kind, &payload).expect("append");
            }
        })
        .expect("spawn producer thread");

    // Should receive exactly 2.
    let first = ops.recv();
    assert!(first.is_some(), "OPS TAKE: first recv should return Some.");
    let second = ops.recv();
    assert!(
        second.is_some(),
        "OPS TAKE: second recv should return Some."
    );
    let (tx, rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ops-take-exhausted-recv".into())
        .spawn(move || {
            let exhausted = ops.recv().is_none();
            let _ = tx.send(exhausted);
        })
        .expect("spawn exhausted recv thread");
    assert!(
        rx.recv_timeout(PROMPT_EXHAUSTION_TIMEOUT)
            .expect("OPS TAKE: exhausted recv should return promptly while store is open"),
        "OPS TAKE: third recv should return None after take(2), but got Some.\n\
         Check: src/store/delivery/subscription.rs SubscriptionOps::take() limit enforcement."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_take_limit_returns_none_immediately_while_store_is_open() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:take-open", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 9);

    let sub = store.subscribe_lossy(&Region::entity("entity:take-open"));
    let mut ops = sub.ops().take(1);
    store
        .append(&coord, kind, &serde_json::json!({"x": 1}))
        .expect("append");
    assert!(
        ops.recv().is_some(),
        "OPS TAKE OPEN: first recv should consume the single allowed notification"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ops-take-open-exhausted-recv".into())
        .spawn(move || {
            let exhausted = ops.recv().is_none();
            let _ = tx.send(exhausted);
        })
        .expect("spawn exhausted recv thread");
    assert!(
        rx.recv_timeout(PROMPT_EXHAUSTION_TIMEOUT).expect(
            "OPS TAKE OPEN: exhausted recv must return immediately while store is still open"
        ),
        "OPS TAKE OPEN: exhausted recv must return None"
    );
}

#[test]
fn ops_filter_and_take_combined() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let wanted_kind = EventKind::custom(0xF, 1);
    let other_kind = EventKind::custom(0xF, 2);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    // Filter for wanted_kind only, take 2.
    let mut ops = sub
        .ops()
        .filter(move |n: &Notification| n.kind == wanted_kind)
        .take(2);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-filter-take-combined-producer".into())
        .spawn(move || {
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
        })
        .expect("spawn producer thread");

    // Should receive exactly 2 matching events, skipping the non-matching ones.
    let first = ops.recv();
    assert!(
        first.is_some(),
        "OPS COMBINED: first recv should return Some."
    );
    let first = first.expect("first notification should be Some");
    assert_eq!(
        first.kind, wanted_kind,
        "OPS COMBINED: first event should be wanted_kind."
    );

    let second = ops.recv();
    assert!(
        second.is_some(),
        "OPS COMBINED: second recv should return Some."
    );
    assert_eq!(
        second.expect("second notification should be Some").kind,
        wanted_kind,
        "OPS COMBINED: second event should be wanted_kind."
    );

    let (tx, rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("ops-filter-take-combined-exhausted-recv".into())
        .spawn(move || {
            let exhausted = ops.recv().is_none();
            let _ = tx.send(exhausted);
        })
        .expect("spawn exhausted recv thread");
    assert!(
        rx.recv_timeout(PROMPT_EXHAUSTION_TIMEOUT)
            .expect("OPS COMBINED: exhausted recv should return promptly while store is open"),
        "OPS COMBINED: third recv should return None (take(2) exhausted), but got Some.\n\
         Check: src/store/delivery/subscription.rs filter + take interaction."
    );

    producer.join().expect("producer thread");
}

// ===== Wave 3D: Subscription composition depth tests =====
// PROVES: LAW-004 (Composition Over Construction — ops chain correctly)
// DEFENDS: FM-009 (Polite Downgrade — map must not silently drop events)
// INVARIANTS: INV-SUBSCRIPTION-STATE-MACHINE (subscription state machine: open to recv to closed)

#[test]
fn ops_map_transforms_notification() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let mapped_kind = EventKind::custom(0xF, 5);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    // Map: change the kind field of every notification
    let mut ops = sub
        .ops()
        .map(move |n: &Notification| {
            Some(Notification {
                event_id: n.event_id,
                correlation_id: n.correlation_id,
                causation_id: n.causation_id,
                coord: n.coord.clone(),
                kind: mapped_kind,
                sequence: n.sequence,
                position: n.position,
            })
        })
        .take(1);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-map-transforms-producer".into())
        .spawn(move || {
            store_w
                .append(&coord_w, kind, &serde_json::json!({"x": 1}))
                .expect("append");
        })
        .expect("spawn producer thread");

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS MAP: should receive a mapped notification."
    );
    assert_eq!(
        notif
            .expect("notification should be Some per preceding assert")
            .kind,
        mapped_kind,
        "OPS MAP: map should transform the notification kind.\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::map().\n\
         Common causes: map_fn not applied, original notification returned instead."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_map_returning_none_skips_event() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let skip_kind = EventKind::custom(0xF, 1);
    let pass_kind = EventKind::custom(0xF, 2);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    // Map: return None for skip_kind (acts as filter), Some for pass_kind
    let mut ops = sub
        .ops()
        .map(move |n: &Notification| {
            if n.kind == skip_kind {
                None
            } else {
                Some(n.clone())
            }
        })
        .take(1);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-map-none-skips-producer".into())
        .spawn(move || {
            // First event: skip_kind — map returns None, should be skipped
            store_w
                .append(&coord_w, skip_kind, &serde_json::json!({"skip": true}))
                .expect("append skip");
            // Second event: pass_kind — map returns Some, should pass through
            store_w
                .append(&coord_w, pass_kind, &serde_json::json!({"pass": true}))
                .expect("append pass");
        })
        .expect("spawn producer thread");

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS MAP SKIP: should receive the pass_kind event."
    );
    assert_eq!(
        notif
            .expect("notification should be Some per preceding assert")
            .kind,
        pass_kind,
        "OPS MAP SKIP: map returning None should skip that event.\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::recv() map branch.\n\
         Common causes: None from map not triggering continue in recv loop."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_multiple_filters_all_must_pass() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:multi", "scope:test").expect("valid coord");
    let kind_a = EventKind::custom(0xF, 1);
    let kind_b = EventKind::custom(0xF, 2);
    let kind_c = EventKind::custom(0xF, 3);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    // Two independent filters: must be kind_a OR kind_b, AND must have sequence > 0
    // Only events passing BOTH filters are received.
    let mut ops = sub
        .ops()
        .filter(move |n: &Notification| n.kind == kind_a || n.kind == kind_b)
        .filter(move |n: &Notification| n.sequence > 0)
        .take(1);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let producer = thread::Builder::new()
        .name("ops-multi-filter-producer".into())
        .spawn(move || {
            // Event 1: kind_c — fails first filter
            store_w
                .append(&coord_w, kind_c, &serde_json::json!({"x": 1}))
                .expect("append kind_c");
            // Event 2: kind_a, sequence=1 — passes both filters
            store_w
                .append(&coord_w, kind_a, &serde_json::json!({"x": 2}))
                .expect("append kind_a");
        })
        .expect("spawn producer thread");

    let notif = ops.recv();
    assert!(
        notif.is_some(),
        "OPS MULTI FILTER: should receive kind_a event."
    );
    let notif = notif.expect("notification should be Some per preceding assert");
    assert_eq!(
        notif.kind, kind_a,
        "OPS MULTI FILTER: only kind_a/kind_b with sequence>0 should pass both filters.\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::recv() filter chain.\n\
         Common causes: filters short-circuiting, only first filter applied."
    );

    producer.join().expect("producer thread");
}

#[test]
fn ops_channel_closed_returns_none() {
    let (_dir, store) = test_store();
    let store = Arc::new(store);

    let region = Region::entity("entity");
    let sub = store.subscribe_lossy(&region);
    let mut ops = sub.ops();

    // Close the store — this shuts down the writer, which closes broadcast channels
    let store_clone = Arc::into_inner(store).expect("Arc should have single owner at test end");
    store_clone.close().expect("close");

    // After channel closes, recv should return None
    let notif = ops.recv();
    assert!(
        notif.is_none(),
        "OPS CHANNEL CLOSED: recv should return None after store is closed.\n\
         Investigate: src/store/delivery/subscription.rs Subscription::recv() channel close path.\n\
         Common causes: recv blocking forever instead of returning None on closed channel."
    );
}
