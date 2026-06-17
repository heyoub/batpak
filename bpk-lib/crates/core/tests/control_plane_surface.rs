// justifies: INV-TEST-PANIC-AS-ASSERTION, ADR-0007; this control-plane smoke harness treats invariant violations as test failures; panic! is the assertion style throughout this file.
#![allow(clippy::panic)]
//! PROVES: the end-to-end control-plane surface -- ticket submit/reaction,
//! try_submit, batch + outbox staging/flush, scan folding, generation-gated
//! projection, visibility-fence commit and cancel, cursor worker shutdown, and
//! read-only reopen -- all interoperate on one live store (INV-MULTI-VIEW-PUBLISH-AFTER-VIEW-SYNC).
//! CATCHES: drift where any single control-plane primitive regresses its
//! published surface or its interaction with the visibility/cancel watermark.
//! SEEDED: a single deterministic store driven through every public submit path.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::{Event, EventKind, EventSourced};
use batpak::store::delivery::cursor::{CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle};
use batpak::store::delivery::subscription::{ScanSubscriptionOps, SubscriptionOps};
use batpak::store::Freshness;
use batpak::store::{
    AppendOptions, AppendReceipt, AppendTicket, BatchAppendItem, BatchAppendTicket, IndexTopology,
    Outbox, ReadOnly, Store, StoreError, VisibilityFence, WriterPressure,
};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[path = "support/control_plane_surface.rs"]
mod cps_support;
use cps_support::{test_config, KIND_COUNTER};

#[path = "support/bounded_blocking.rs"]
mod bounded_blocking;
#[path = "support/bounded_writer_reply.rs"]
mod bounded_writer_reply;
use bounded_blocking::blocking;
use bounded_writer_reply::writer_reply;

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

fn wait_append_ticket(ticket: &AppendTicket, label: &str) -> Result<AppendReceipt, StoreError> {
    writer_reply(ticket.receiver(), label)
}

fn wait_batch_ticket(
    ticket: &BatchAppendTicket,
    label: &str,
) -> Result<Vec<AppendReceipt>, StoreError> {
    writer_reply(ticket.receiver(), label)
}

#[test]
fn control_plane_surface_smoke() {
    let dir = TempDir::new().expect("temp dir");
    let config = test_config(&dir)
        .with_writer_pressure_retry_threshold_pct(60)
        .with_enable_mmap_index(true)
        .with_index_topology(IndexTopology::all());
    let store = Store::open(config).expect("open store");

    let coord = Coordinate::new("entity:control", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let pressure = store.writer_pressure();
    let pressure_headroom = pressure.headroom();
    let pressure_utilization = pressure.utilization();
    let pressure_is_idle = pressure.is_idle();
    assert!(
        pressure.capacity > 0,
        "writer pressure capacity should be populated"
    );
    assert!(pressure_headroom <= pressure.capacity);
    assert!(pressure_utilization >= 0.0);
    assert!(pressure_is_idle);

    let ticket = store
        .submit(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("submit");
    let receipt = wait_append_ticket(&ticket, "control-plane submit").expect("wait");
    assert_eq!(receipt.sequence, 1);

    let reaction_ticket = store
        .submit_reaction(
            &coord,
            kind,
            &serde_json::json!({"n": 2}),
            batpak::id::CorrelationId::from(u128::from(receipt.event_id)),
            batpak::id::CausationId::from(u128::from(receipt.event_id)),
        )
        .expect("submit reaction");
    let reaction = wait_append_ticket(&reaction_ticket, "control-plane submit reaction")
        .expect("wait reaction");
    assert_eq!(reaction.sequence, 2);

    let outcome = store
        .try_submit(&coord, kind, &serde_json::json!({"n": 3}))
        .expect("try_submit");
    let ticket: AppendTicket = outcome.into_result().expect("ok outcome");
    let receipt = wait_append_ticket(&ticket, "control-plane try_submit").expect("wait try_submit");
    assert_eq!(receipt.sequence, 3);

    let try_reaction = store
        .try_submit_reaction(
            &coord,
            kind,
            &serde_json::json!({"n": 3.5}),
            batpak::id::CorrelationId::from(u128::from(receipt.event_id)),
            batpak::id::CausationId::from(u128::from(receipt.event_id)),
        )
        .expect("try submit reaction")
        .into_result()
        .expect("reaction outcome");
    let _ = wait_append_ticket(&try_reaction, "control-plane try submit reaction")
        .expect("wait try reaction");

    let batch_items = vec![
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 4}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xAA)),
            batpak::store::CausationRef::None,
        )
        .expect("batch item"),
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 5}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xBB)),
            batpak::store::CausationRef::None,
        )
        .expect("batch item"),
    ];
    let batch_ticket = store.submit_batch(batch_items).expect("submit batch");
    let receipts =
        wait_batch_ticket(&batch_ticket, "control-plane submit batch").expect("wait batch");
    assert_eq!(receipts.len(), 2);

    let try_batch_items = vec![BatchAppendItem::new(
        coord.clone(),
        kind,
        &serde_json::json!({"n": 6}),
        AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xCC)),
        batpak::store::CausationRef::None,
    )
    .expect("batch item")];
    let try_batch = store
        .try_submit_batch(try_batch_items)
        .expect("try submit batch")
        .into_result()
        .expect("batch outcome");
    let try_batch: BatchAppendTicket = try_batch;
    let _ = wait_batch_ticket(&try_batch, "control-plane try submit batch").expect("batch wait");

    let mut outbox: Outbox<'_> = store.outbox();
    let outbox_empty = outbox.is_empty();
    assert!(outbox_empty);
    outbox
        .stage_with_options(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 7}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xDC)),
        )
        .expect("stage");
    outbox
        .stage_with_options(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 8}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xDD)),
        )
        .expect("stage with options");
    outbox
        .stage_with_options_and_causation(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 8.5}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xDDE)),
            batpak::store::CausationRef::Absolute(u128::from(receipt.event_id)),
        )
        .expect("stage with options and causation");
    outbox.push_item(
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 9}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xEE)),
            batpak::store::CausationRef::None,
        )
        .expect("push item"),
    );
    let outbox_len = outbox.len();
    assert_eq!(outbox_len, 4);
    assert!(
        !outbox.is_empty(),
        "PROPERTY: staged outbox work must make is_empty false before flush.\n\
         Investigate: src/store/write/control/outbox.rs Outbox::is_empty."
    );
    let flush_ticket = outbox.submit_flush().expect("submit flush");
    let _ =
        wait_batch_ticket(&flush_ticket, "control-plane outbox submit flush").expect("wait flush");
    let outbox_empty_after_flush = outbox.is_empty();
    assert!(outbox_empty_after_flush);

    let mut outbox2: Outbox<'_> = store.outbox();
    outbox2
        .stage_with_options(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 10}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xFF)),
        )
        .expect("stage flush");
    let flushed = outbox2.flush().expect("flush");
    assert_eq!(flushed.len(), 1);

    let ops: SubscriptionOps = store
        .subscribe_lossy(&Region::entity("entity:control"))
        .ops();
    let mut folded: ScanSubscriptionOps<u32, _> = ops.scan(0u32, |count, _| {
        *count += 1;
        Some(*count)
    });
    store
        .append(&coord, kind, &serde_json::json!({"n": 11}))
        .expect("append for scan");
    let folded_count =
        blocking("control-plane-scan-recv", move || folded.recv()).expect("folded count");
    assert!(folded_count >= 1);

    let generation_before = store
        .entity_generation("entity:control")
        .expect("entity generation should exist");
    let projected = store
        .project::<CounterProjection>("entity:control", &Freshness::Consistent)
        .expect("project")
        .expect("projection");
    assert!(projected.count >= 11);
    let unchanged = store
        .project_if_changed::<CounterProjection>(
            "entity:control",
            generation_before,
            &Freshness::Consistent,
        )
        .expect("project if unchanged");
    assert!(
        unchanged.is_none(),
        "generation gate should skip unchanged entities"
    );

    store
        .append(&coord, kind, &serde_json::json!({"n": 12}))
        .expect("append after projection");
    let changed = store
        .project_if_changed::<CounterProjection>(
            "entity:control",
            generation_before,
            &Freshness::Consistent,
        )
        .expect("project if changed")
        .expect("changed projection");
    assert!(changed.0 > generation_before);
    assert!(
        changed.1.expect("projection value").count > projected.count,
        "projection should advance after a new event"
    );

    let fence: VisibilityFence<'_> = store
        .begin_visibility_fence()
        .expect("begin visibility fence");
    assert!(
        matches!(
            store.append(&coord, kind, &serde_json::json!({"n": 12.5})),
            Err(StoreError::VisibilityFenceActive)
        ),
        "normal appends should be blocked while a public fence is active"
    );

    let fenced_ticket = fence
        .submit(&coord, kind, &serde_json::json!({"n": 13}))
        .expect("fence submit");
    let fenced_receiver = fenced_ticket.receiver();
    let fenced_ready = fenced_ticket.try_check();
    assert!(
        fenced_receiver.is_empty() && fenced_ready.is_none(),
        "fenced write should not resolve before commit"
    );

    let mut fence_outbox = fence.outbox();
    fence_outbox
        .stage_with_options(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 14}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0x1234)),
        )
        .expect("fence outbox stage");
    let fenced_batch: BatchAppendTicket = fence_outbox.submit_flush().expect("fence submit flush");

    let visible_before_commit = store.by_fact(kind).len();
    fence.commit().expect("commit fence");
    let _ = wait_append_ticket(&fenced_ticket, "control-plane committed fenced receipt")
        .expect("wait fenced receipt");
    let _ = wait_batch_ticket(&fenced_batch, "control-plane committed fenced batch")
        .expect("wait fenced batch");
    assert!(
        store.by_fact(kind).len() >= visible_before_commit + 2,
        "committed fence writes should become visible together"
    );

    let cancel_fence = store.begin_visibility_fence().expect("begin cancel fence");
    let cancelled_ticket = cancel_fence
        .submit(&coord, kind, &serde_json::json!({"n": 15}))
        .expect("cancelled fence submit");
    cancel_fence.cancel().expect("cancel fence");
    assert!(
        matches!(
            wait_append_ticket(&cancelled_ticket, "control-plane cancelled fence ticket"),
            Err(StoreError::VisibilityFenceCancelled)
        ),
        "cancelled fence tickets should surface cancellation"
    );
    let visible_after_cancel = store.by_fact(kind).len();
    let stream_after_cancel = store.by_entity("entity:control").len();
    store
        .append(&coord, kind, &serde_json::json!({"n": 15.5}))
        .expect("append after cancelled fence");
    assert_eq!(
        store.by_fact(kind).len(),
        visible_after_cancel + 1,
        "later watermark advances must not surface cancelled fence writes"
    );
    assert_eq!(
        store.by_entity("entity:control").len(),
        stream_after_cancel + 1,
        "entity stream must also keep cancelled fence writes hidden"
    );

    let store = Arc::new(store);
    let mut cursor_config = CursorWorkerConfig::default();
    cursor_config.batch_size = 1;
    cursor_config.idle_sleep = Duration::from_millis(1);
    let worker: CursorWorkerHandle = store
        .cursor_worker(
            &Region::entity("entity:control"),
            cursor_config,
            |_batch, _store, _witness| CursorWorkerAction::Stop,
        )
        .expect("spawn cursor worker");
    store
        .append(&coord, kind, &serde_json::json!({"n": 13}))
        .expect("append for cursor worker");
    worker.stop_and_join().expect("stop and join cursor worker");

    let _ = WriterPressure {
        queue_len: 0,
        capacity: 10,
    };

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: cursor worker should release the last Arc"),
    };
    let visible_before_close = store.by_fact(kind).len();
    store.close().expect("close");
    let native_cache_dir = dir.path().join("native-cache");
    let native_ro: Store<ReadOnly> =
        Store::open_read_only_with_native_cache(test_config(&dir), &native_cache_dir)
            .expect("open read-only with native cache");
    drop(native_ro);
    let custom_ro: Store<ReadOnly> =
        Store::open_read_only_with_cache(test_config(&dir), Box::new(batpak::store::NoCache))
            .expect("open read-only with custom cache");
    drop(custom_ro);
    let ro: Store<ReadOnly> = Store::open_read_only(test_config(&dir)).expect("open read-only");
    let ro_entries: Vec<batpak::store::index::IndexEntry> = ro.by_fact(kind);
    assert!(
        !ro_entries.is_empty(),
        "read-only handle should support querying existing events"
    );
    assert_eq!(
        ro_entries.len(),
        visible_before_close,
        "reopen must preserve hidden cancelled-fence ranges"
    );
}
