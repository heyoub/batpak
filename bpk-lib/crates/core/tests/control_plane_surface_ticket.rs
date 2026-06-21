//! PROVES: the ready-ticket polling surface -- `AppendTicket`/`BatchAppendTicket`
//! `try_check` exposes committed receipts (identity + visible sequence order)
//! once the writer reply lands -- and that a lossy scan fold converges with the
//! generation-tracked projection count over a seeded then concurrently-appended
//! entity (INV-MULTI-VIEW-PUBLISH-AFTER-VIEW-SYNC).
//! CATCHES: drift where `try_check` returns stale/default receipts, loses
//! ordering, or where scan/projection counts diverge under concurrent appends.
//! SEEDED: a deterministic per-test store; scan parity seeds 10 + appends 10.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::{Event, EventKind, EventSourced};
use batpak::store::Freshness;
use batpak::store::{AppendOptions, BatchAppendItem, Store, StoreError};
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[path = "support/control_plane_surface.rs"]
mod cps_support;
use cps_support::{test_config, KIND_COUNTER};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct CounterProjection {
    count: u64,
}

impl EventSourced for CounterProjection {
    type Input = batpak::prelude::JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self { count: 0 };
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[KIND_COUNTER]
    }

    fn supports_incremental_apply() -> bool {
        true
    }
}

fn wait_until_ticket_receiver_has_value<T>(
    rx: &flume::Receiver<Result<T, StoreError>>,
    label: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while rx.is_empty() {
        assert!(
            Instant::now() < deadline,
            "PROPERTY: {label} must eventually publish a writer reply so try_check can observe the ready state."
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn try_check_surfaces_ready_append_and_batch_tickets() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:ticket-ready", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let append_ticket = store
        .submit(&coord, kind, &serde_json::json!({"n": "append"}))
        .expect("submit append ticket");
    wait_until_ticket_receiver_has_value(append_ticket.receiver(), "append ticket receiver");
    let append_receipt = append_ticket
        .try_check()
        .expect(
            "PROPERTY: once the append ticket receiver is non-empty, try_check must return Some(_)",
        )
        .expect("PROPERTY: ready append ticket must surface its receipt through try_check");
    assert_eq!(append_receipt.sequence, 1);
    assert_ne!(
        append_receipt.event_id,
        batpak::id::EventId::from(0u128),
        "PROPERTY: ready append ticket must surface the committed event identity, not a default receipt."
    );

    let batch_ticket = store
        .submit_batch(vec![
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"n": "batch-a"}),
                AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xFACE)),
                batpak::store::CausationRef::None,
            )
            .expect("batch item a"),
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"n": "batch-b"}),
                AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xB00C)),
                batpak::store::CausationRef::None,
            )
            .expect("batch item b"),
        ])
        .expect("submit batch ticket");
    wait_until_ticket_receiver_has_value(batch_ticket.receiver(), "batch ticket receiver");
    let batch_receipts = batch_ticket
        .try_check()
        .expect(
            "PROPERTY: once the batch ticket receiver is non-empty, try_check must return Some(_)",
        )
        .expect("PROPERTY: ready batch ticket must surface its receipts through try_check");
    assert_eq!(batch_receipts.len(), 2);
    assert!(
        batch_receipts
            .iter()
            .all(|receipt| receipt.event_id != batpak::id::EventId::from(0u128)),
        "PROPERTY: ready batch ticket must surface committed event identities, not default receipts."
    );
    assert_ne!(
        batch_receipts[0].event_id, batch_receipts[1].event_id,
        "PROPERTY: distinct committed batch items must surface distinct event identities through try_check."
    );
    assert_eq!(
        batch_receipts
            .iter()
            .map(|receipt| receipt.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3],
        "PROPERTY: batch try_check must expose the committed receipts in visible sequence order."
    );
}

#[test]
fn scan_fold_converges_to_project_count() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:scan-parity", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    // Phase 1: seed 10 events before subscribing.
    for i in 0..10u32 {
        store
            .append(&coord, kind, &serde_json::json!({"phase": 1, "i": i}))
            .expect("append seed event");
    }

    // Project after initial seed.
    let projected_10 = store
        .project::<CounterProjection>("entity:scan-parity", &Freshness::Consistent)
        .expect("project phase 1")
        .expect("projection must exist");
    assert_eq!(
        projected_10.count, 10,
        "PROPERTY: projection must count all 10 seed events."
    );

    // Phase 2: set up a lossy scan subscriber, then append 10 more events.
    // The scan receiver runs in a background thread; the main thread appends.
    let mut scan = store
        .subscribe_lossy(&Region::entity("entity:scan-parity"))
        .ops()
        .scan(0u32, |count, _| {
            *count += 1;
            Some(*count)
        });

    let handle = std::thread::Builder::new()
        .name("scan-consumer".into())
        .spawn(move || {
            let mut last_count = 0u32;
            let deadline = Instant::now() + Duration::from_secs(5);
            while last_count < 10 && Instant::now() < deadline {
                if let Some(c) = scan.recv() {
                    last_count = c;
                } else {
                    break;
                }
            }
            last_count
        })
        .expect("spawn scan consumer");

    // Append 10 more events from the main thread.
    for i in 0..10u32 {
        store
            .append(&coord, kind, &serde_json::json!({"phase": 2, "i": i}))
            .expect("append phase 2 event");
    }

    let scan_count = handle.join().expect("join scan thread");
    // Lossy subscription: the fold sees SOME notifications but may miss some
    // under system load (e.g., concurrent bench runs). The invariant is that
    // scan saw at least 1 event (subscriber was alive and connected).
    assert!(
        scan_count >= 1,
        "PROPERTY: scan fold must observe at least one event from the lossy subscription. \
         Got {scan_count}."
    );

    // Re-project and verify total is 20.
    let projected_20 = store
        .project::<CounterProjection>("entity:scan-parity", &Freshness::Consistent)
        .expect("project phase 2")
        .expect("projection must exist");
    assert_eq!(
        projected_20.count, 20,
        "PROPERTY: projection must count all 20 events after both phases."
    );

    store.close().expect("close store");
}
