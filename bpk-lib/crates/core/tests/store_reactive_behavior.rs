//! Advanced Store pipeline and reactive-flow integration tests.

use batpak::event::Reactive;
use batpak::id::EntityIdType;
use batpak::store::{Store, StoreConfig, StoreError};
use batpak_testkit::prelude::*;
use std::sync::Arc;
use tempfile::TempDir;

use batpak_testkit::small_store as small_store_support;

fn test_store() -> (TempDir, Store) {
    small_store_support::small_segment_store().expect("small segment store")
}

// --- Pipeline::commit_bypass ---

#[test]
fn pipeline_commit_bypass_persists() {
    use batpak::pipeline::bypass::BypassReason;

    struct TestBypass;
    impl BypassReason for TestBypass {
        fn name(&self) -> &'static str {
            "test-bypass"
        }
        fn justification(&self) -> &'static str {
            "testing commit_bypass"
        }
    }

    let (_dir, store) = test_store();
    let coord = Coordinate::new("entity:bypass", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let proposal = Proposal::new(serde_json::json!({"bypassed": true}));
    let bypass_receipt = Pipeline::<()>::bypass(proposal, &TestBypass);

    let committed: Committed<serde_json::Value> =
        Pipeline::<()>::commit_bypass(bypass_receipt, |p| -> Result<_, StoreError> {
            let r = store.append(&coord, kind, &p)?;
            CommitMetadata::from_append_receipt(&r)
        })
        .expect("commit_bypass");
    let committed_event_id = committed.event_id();
    let committed_audit = committed
        .bypass_audit()
        .expect("commit_bypass should retain bypass audit");

    // Verify persisted
    let stored = store.get(committed_event_id).expect("get");
    assert_eq!(
        stored.event.event_kind(),
        kind,
        "PROPERTY: commit_bypass must persist the event through the store.\n\
         Investigate: src/pipeline/mod.rs commit_bypass.\n\
         Common causes: commit_fn not called, payload not forwarded.\n\
         Run: cargo test --test store_reactive_behavior pipeline_commit_bypass_persists"
    );
    assert_eq!(
        committed_audit.reason,
        "test-bypass",
        "PROPERTY: commit_bypass must retain the bypass audit reason alongside the persisted event."
    );

    store.close().expect("close");
}

// --- Store::react_loop ---

#[test]
fn react_loop_spawns_and_processes() {
    use batpak::event::sourcing::Reactive;

    struct TestReactor;
    impl Reactive<serde_json::Value> for TestReactor {
        fn react(
            &self,
            event: &batpak::prelude::Event<serde_json::Value>,
        ) -> Vec<(Coordinate, EventKind, serde_json::Value)> {
            if event.event_kind() == EventKind::custom(0xA, 1) {
                vec![(
                    Coordinate::new("entity:reactions", "scope:test").expect("valid"),
                    EventKind::custom(0xA, 2),
                    serde_json::json!({"reacted_to": event.event_id().to_string()}),
                )]
            } else {
                vec![]
            }
        }
    }

    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Arc::new(Store::open(config).expect("open store"));

    let region = Region::entity("entity:trigger");
    // The opaque handle type is part of the sealed public surface; bind it by
    // name so this surface is witnessed.
    let _handle: batpak::store::ReactLoopHandle = store
        .react_loop(&region, TestReactor)
        .expect("spawn reactor");

    // Append a trigger event
    let coord = Coordinate::new("entity:trigger", "scope:test").expect("valid coord");
    let _ = store
        .append(
            &coord,
            EventKind::custom(0xA, 1),
            &serde_json::json!({"trigger": true}),
        )
        .expect("append");

    // Poll for the reactor to produce a reaction instead of sleeping a fixed duration.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let reactions = loop {
        let r = store.query(&Region::entity("entity:reactions"));
        if !r.is_empty() {
            break r;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PROPERTY: react_loop must produce reaction events when the reactor emits them. \
             Got nothing after 5s deadline. \
             Investigate: src/store/mod.rs react_loop, src/event/sourcing.rs Reactive."
        );
        std::thread::yield_now();
    };
    assert_eq!(
        reactions[0].event_kind(),
        EventKind::custom(0xA, 2),
        "PROPERTY: reaction event must have the kind returned by the reactor.\n\
         Investigate: src/store/mod.rs react_loop.\n\
         Run: cargo test --test store_reactive_behavior react_loop_spawns_and_processes"
    );

    store.sync().expect("sync");
}

#[test]
fn react_loop_handle_is_opaque_and_detaches_on_drop() {
    use batpak::event::sourcing::Reactive;
    use batpak::store::ReactLoopHandle;

    struct NoopReactor;
    impl Reactive<serde_json::Value> for NoopReactor {
        fn react(
            &self,
            _event: &batpak::prelude::Event<serde_json::Value>,
        ) -> Vec<(Coordinate, EventKind, serde_json::Value)> {
            vec![]
        }
    }

    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1);
    let store = Arc::new(Store::open(config).expect("open store"));

    let region = Region::entity("entity:trigger");
    let handle: ReactLoopHandle = store
        .react_loop(&region, NoopReactor)
        .expect("spawn reactor");

    // The handle is opaque: its only observable surface is Debug. It does NOT
    // expose the concrete background-thread type, and it carries no join() —
    // the legacy reactor is fire-and-detach (the worker holds its own store
    // Arc, so it cannot be joined to completion). Dropping the handle detaches
    // the loop without panicking; the store teardown at scope end reaps it.
    let rendered = format!("{handle:?}");
    assert!(
        rendered.starts_with("ReactLoopHandle"),
        "PROPERTY: ReactLoopHandle Debug must not leak the inner thread handle type; got {rendered}"
    );
    drop(handle); // detach — must not panic or block
}
// ================================================================
// Reactive pattern
// ================================================================

struct OrderReactor;
impl batpak::event::Reactive<serde_json::Value> for OrderReactor {
    fn react(
        &self,
        event: &Event<serde_json::Value>,
    ) -> Vec<(Coordinate, EventKind, serde_json::Value)> {
        // When we see a "create_order" event, emit an "order_created" reaction
        if event.event_kind() == EventKind::custom(0xA, 1) {
            vec![(
                Coordinate::new("order:reactions", "scope:test").expect("valid"),
                EventKind::custom(0xA, 2),
                serde_json::json!({"reacted_to": event.event_id().to_string()}),
            )]
        } else {
            vec![]
        }
    }
}

#[test]
fn reactive_subscribe_react_append_pattern() {
    // This test proves the minimal reactive wiring pattern works:
    // subscribe → receive → react() → append_reaction()

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1);
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("order:1", "scope:test").expect("valid");
    let kind = EventKind::custom(0xA, 1); // "create_order"

    // Subscribe before writing
    let region = Region::all();
    let sub = store.subscribe_lossy(&region);

    // Write the root event from another thread
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::Builder::new()
        .name("store-advanced-reactive-writer".into())
        .spawn(move || {
            store_w
                .append(&coord_w, kind, &serde_json::json!({"item": "widget"}))
                .expect("append root")
        })
        .expect("spawn reactive writer thread");
    let root_receipt = writer.join().expect("writer thread");

    // Receive the notification
    let rx = sub.receiver();
    let notif = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("should receive notification");

    // React: the OrderReactor decides what to emit
    let reactor = OrderReactor;
    // Build a minimal event for the reactor (it only needs kind + event_id)
    let header = EventHeader::new(
        notif.event_id.as_u128(),
        notif.correlation_id,
        notif.causation_id,
        0,
        DagPosition::root(),
        0,
        notif.kind,
    );
    let event = Event::<serde_json::Value>::new(header, serde_json::Value::Null);
    let reactions = reactor.react(&event);

    assert_eq!(
        reactions.len(),
        1,
        "PROPERTY: OrderReactor must produce exactly 1 reaction for a create_order event.\n\
         Investigate: src/event/sourcing.rs Reactive trait react() method.\n\
         Common causes: react() returning an empty vec because event_kind comparison \
         fails, or EventKind::custom encoding mismatch between writer and reactor.\n\
         Run: cargo test --test store_reactive_behavior reactive_subscribe_react_append_pattern"
    );

    // Append reactions via append_reaction (the causal link)
    for (react_coord, react_kind, react_payload) in reactions {
        let _ = store
            .append_reaction(
                &react_coord,
                react_kind,
                &react_payload,
                batpak::id::CorrelationId::from(u128::from(root_receipt.event_id)),
                batpak::id::CausationId::from(u128::from(root_receipt.event_id)),
            )
            .expect("append reaction");
    }

    // Verify: 2 events total (root + reaction)
    let stats = store.stats();
    assert_eq!(
        stats.event_count, 3,
        "PROPERTY: After root event + 1 reaction, store must contain the lifecycle event plus those 2 user-visible events.\n\
         Investigate: src/store/mod.rs Store::append_reaction() src/event/sourcing.rs.\n\
         Common causes: append_reaction() not writing to the store, or stats.event_count \
         not counting reaction events that go to a different coordinate.\n\
         Run: cargo test --test store_reactive_behavior reactive_subscribe_react_append_pattern"
    );

    store.sync().expect("sync");
}
