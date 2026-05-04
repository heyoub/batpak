// justifies: INV-TEST-PANIC-AS-ASSERTION, ADR-0007; test bodies in tests/control_plane_surface.rs treat invariant violations as test failures; panic! is the assertion style throughout this file.
#![allow(clippy::panic)]

use batpak::coordinate::{Coordinate, Region};
use batpak::event::{Event, EventKind, EventSourced};
use batpak::store::delivery::cursor::{CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle};
use batpak::store::delivery::subscription::{ScanSubscriptionOps, SubscriptionOps};
use batpak::store::Freshness;
use batpak::store::{
    AppendOptions, AppendReceipt, AppendTicket, BatchAppendItem, BatchAppendTicket, IndexTopology,
    Outbox, ReadOnly, Store, StoreConfig, StoreError, SyncConfig, VisibilityFence, WriterPressure,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const KIND_COUNTER: EventKind = EventKind::custom(0xF, 1);

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

fn test_config(dir: &TempDir) -> StoreConfig {
    StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new(dir.path())
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
            receipt.event_id,
            receipt.event_id,
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
            receipt.event_id,
            receipt.event_id,
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
            AppendOptions::new().with_idempotency(0xAA),
            batpak::store::CausationRef::None,
        )
        .expect("batch item"),
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 5}),
            AppendOptions::new().with_idempotency(0xBB),
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
        AppendOptions::new().with_idempotency(0xCC),
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
            AppendOptions::new().with_idempotency(0xDC),
        )
        .expect("stage");
    outbox
        .stage_with_options(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 8}),
            AppendOptions::new().with_idempotency(0xDD),
        )
        .expect("stage with options");
    outbox
        .stage_with_options_and_causation(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 8.5}),
            AppendOptions::new().with_idempotency(0xDDE),
            batpak::store::CausationRef::Absolute(receipt.event_id),
        )
        .expect("stage with options and causation");
    outbox.push_item(
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 9}),
            AppendOptions::new().with_idempotency(0xEE),
            batpak::store::CausationRef::None,
        )
        .expect("push item"),
    );
    let outbox_len = outbox.len();
    assert_eq!(outbox_len, 4);
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
            AppendOptions::new().with_idempotency(0xFF),
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
            AppendOptions::new().with_idempotency(0x1234),
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
    let stream_after_cancel = store.stream("entity:control").len();
    store
        .append(&coord, kind, &serde_json::json!({"n": 15.5}))
        .expect("append after cancelled fence");
    assert_eq!(
        store.by_fact(kind).len(),
        visible_after_cancel + 1,
        "later watermark advances must not surface cancelled fence writes"
    );
    assert_eq!(
        store.stream("entity:control").len(),
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
    let append_receipt = match append_ticket.try_check() {
        Some(Ok(receipt)) => receipt,
        Some(Err(err)) => panic!(
            "PROPERTY: ready append ticket must surface its receipt through try_check, got error {err:?}"
        ),
        None => panic!(
            "PROPERTY: once the append ticket receiver is non-empty, try_check must return Some(Ok(_))"
        ),
    };
    assert_eq!(append_receipt.sequence, 1);
    assert_ne!(
        append_receipt.event_id, 0,
        "PROPERTY: ready append ticket must surface the committed event identity, not a default receipt."
    );

    let batch_ticket = store
        .submit_batch(vec![
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"n": "batch-a"}),
                AppendOptions::new().with_idempotency(0xFACE),
                batpak::store::CausationRef::None,
            )
            .expect("batch item a"),
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"n": "batch-b"}),
                AppendOptions::new().with_idempotency(0xB00C),
                batpak::store::CausationRef::None,
            )
            .expect("batch item b"),
        ])
        .expect("submit batch ticket");
    wait_until_ticket_receiver_has_value(batch_ticket.receiver(), "batch ticket receiver");
    let batch_receipts = match batch_ticket.try_check() {
        Some(Ok(receipts)) => receipts,
        Some(Err(err)) => panic!(
            "PROPERTY: ready batch ticket must surface its receipts through try_check, got error {err:?}"
        ),
        None => panic!(
            "PROPERTY: once the batch ticket receiver is non-empty, try_check must return Some(Ok(_))"
        ),
    };
    assert_eq!(batch_receipts.len(), 2);
    assert!(
        batch_receipts.iter().all(|receipt| receipt.event_id != 0),
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
fn try_submit_returns_retry_under_pressure() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new(dir.path())
    }
    .with_writer_channel_capacity(8)
    .with_writer_pressure_retry_threshold_pct(50);

    let store = Arc::new(Store::open(config).expect("open store"));
    let coord = Coordinate::new("entity:pressure", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    // With channel_capacity=8 and threshold=50%, the pressure gate fires
    // when 4 or more commands are queued. We flood the channel from background
    // threads while the writer is busy syncing (sync_every_n_events=1 forces
    // an fsync per event, slowing the writer drain). On the main thread we
    // poll try_submit until we observe Outcome::Retry.
    let saw_retry = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));

    let handles: Vec<_> = (0..4u32)
        .map(|i| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("pressure-producer-{i}"))
                .spawn(move || {
                    let mut n = 0u32;
                    while !stop.load(Ordering::Relaxed) {
                        let _ = store.submit(&coord, kind, &serde_json::json!({"t": i, "n": n}));
                        n += 1;
                    }
                })
                .expect("spawn pressure producer")
        })
        .collect();

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match store.try_submit(&coord, kind, &serde_json::json!({"probe": true})) {
            Ok(outcome) if outcome.is_retry() => {
                saw_retry.store(true, Ordering::SeqCst);
                break;
            }
            _ => {}
        }
    }

    stop.store(true, Ordering::SeqCst);
    for h in handles {
        let _ = h.join();
    }

    assert!(
        saw_retry.load(Ordering::SeqCst),
        "PROPERTY: try_submit must return Outcome::Retry when the writer channel \
         exceeds the pressure threshold (50% of capacity 8 = 4 queued commands)."
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: producer threads should release the last Arc"),
    };
    store.close().expect("close store");
}

#[test]
fn try_submit_batch_returns_retry_under_pressure() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new(dir.path())
    }
    .with_writer_channel_capacity(8)
    .with_writer_pressure_retry_threshold_pct(50);

    let store = Arc::new(Store::open(config).expect("open store"));
    let coord = Coordinate::new("entity:pressure-batch", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let saw_retry = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));

    let handles: Vec<_> = (0..4u32)
        .map(|i| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("pressure-batch-producer-{i}"))
                .spawn(move || {
                    let mut n = 0u32;
                    while !stop.load(Ordering::Relaxed) {
                        let items = vec![BatchAppendItem::new(
                            coord.clone(),
                            kind,
                            &serde_json::json!({"t": i, "n": n}),
                            AppendOptions::new().with_idempotency(
                                (((i as u64) << 32) | u64::from(n) | 0xB000_0000).into(),
                            ),
                            batpak::store::CausationRef::None,
                        )
                        .expect("batch item")];
                        let _ = store.submit_batch(items);
                        n += 1;
                    }
                })
                .expect("spawn pressure producer")
        })
        .collect();

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let items = vec![BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"probe": true}),
            AppendOptions::new().with_idempotency(0xCAFE_BA5E),
            batpak::store::CausationRef::None,
        )
        .expect("batch probe item")];
        match store.try_submit_batch(items) {
            Ok(outcome) if outcome.is_retry() => {
                saw_retry.store(true, Ordering::SeqCst);
                break;
            }
            _ => {}
        }
    }

    stop.store(true, Ordering::SeqCst);
    for h in handles {
        let _ = h.join();
    }

    assert!(
        saw_retry.load(Ordering::SeqCst),
        "PROPERTY: try_submit_batch must return Outcome::Retry when the writer channel \
         exceeds the pressure threshold (50% of capacity 8 = 4 queued commands)."
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: producer threads should release the last Arc"),
    };
    store.close().expect("close store");
}

#[test]
fn fence_drop_without_commit_auto_cancels() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-drop", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let fenced_ticket = {
        let fence = store.begin_visibility_fence().expect("begin fence");
        // Drop the fence without calling commit() or cancel().
        // The Drop impl sends CancelVisibilityFence to the writer.
        fence
            .submit(&coord, kind, &serde_json::json!({"fenced": true}))
            .expect("fence submit")
    };

    // The ticket should resolve with VisibilityFenceCancelled because the
    // fence was implicitly cancelled on drop.
    let fenced_result = fenced_ticket
        .receiver()
        .recv_timeout(Duration::from_secs(2))
        .expect("PROPERTY: dropped VisibilityFence must auto-cancel outstanding tickets promptly");
    assert!(
        matches!(fenced_result, Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: dropping a VisibilityFence without commit or cancel must auto-cancel, \
         and any outstanding tickets must surface VisibilityFenceCancelled."
    );

    // The fenced event must NOT be visible.
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: events submitted through a dropped (auto-cancelled) fence must not be visible."
    );

    // The store must remain usable after a fence auto-cancel.
    let receipt = store
        .append(&coord, kind, &serde_json::json!({"after_drop": true}))
        .expect("append after fence drop");
    assert!(
        receipt.sequence >= 1,
        "PROPERTY: store must be usable after an auto-cancelled fence drop. \
         Got sequence {}, expected >= 1.",
        receipt.sequence
    );

    store.close().expect("close store");
}

#[test]
fn fenced_root_submit_stays_hidden_until_commit_and_cancel_discards_it() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-root", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit(&coord, kind, &serde_json::json!({"root": true}))
        .expect("submit fenced root");

    assert!(
        ticket.receiver().is_empty(),
        "PROPERTY: a root submission under a live fence must not resolve before commit."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: a root submission under a live fence must remain invisible before commit."
    );
    assert_eq!(
        store.stream("entity:fence-root").len(),
        0,
        "PROPERTY: the entity stream must also keep fenced root submissions hidden before commit."
    );

    fence.cancel().expect("cancel fence");
    assert!(
        matches!(wait_append_ticket(&ticket, "cancelled fence ticket"), Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: cancelling a fence after a root submission must surface VisibilityFenceCancelled."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: cancelling a fence must discard the pending root submission."
    );

    store.close().expect("close store");
    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.by_fact(kind).len(),
        0,
        "PROPERTY: a cancelled root submission under a fence must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn fenced_batch_submit_stays_hidden_until_commit_and_cancel_discards_it() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-batch", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let fence = store.begin_visibility_fence().expect("begin fence");
    let mut outbox = fence.outbox();
    outbox
        .stage_with_options(
            coord.clone(),
            kind,
            &serde_json::json!({"batch": "a"}),
            AppendOptions::new().with_idempotency(0xAAA1),
        )
        .expect("stage item a");
    outbox
        .stage_with_options(
            coord,
            kind,
            &serde_json::json!({"batch": "b"}),
            AppendOptions::new().with_idempotency(0xAAA2),
        )
        .expect("stage item b");
    let ticket = outbox.submit_flush().expect("submit fenced batch");

    assert!(
        ticket.receiver().is_empty(),
        "PROPERTY: a batch submission under a live fence must not resolve before commit."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: a batch submission under a live fence must remain invisible before commit."
    );
    assert_eq!(
        store.stream("entity:fence-batch").len(),
        0,
        "PROPERTY: the entity stream must also keep fenced batch submissions hidden before commit."
    );

    fence.cancel().expect("cancel fence");
    assert!(
        matches!(
            wait_batch_ticket(&ticket, "cancelled fence batch ticket"),
            Err(StoreError::VisibilityFenceCancelled)
        ),
        "PROPERTY: cancelling a fence after batch submit_flush must surface VisibilityFenceCancelled."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: cancelling a fence must discard the pending batch submission."
    );

    store.close().expect("close store");
    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.by_fact(kind).len(),
        0,
        "PROPERTY: a cancelled batch submission under a fence must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn fenced_reaction_submit_stays_hidden_until_commit_and_cancel_discards_it() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let root_coord = Coordinate::new("entity:fence-reaction-root", "scope:test").expect("coord");
    let reaction_coord =
        Coordinate::new("entity:fence-reaction-child", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let root = store
        .append(&root_coord, kind, &serde_json::json!({"root": true}))
        .expect("append root");

    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit_reaction(
            &reaction_coord,
            kind,
            &serde_json::json!({"reaction": true}),
            root.event_id,
            root.event_id,
        )
        .expect("submit fenced reaction");

    assert!(
        ticket.receiver().is_empty(),
        "PROPERTY: a reaction submission under a live fence must not resolve before commit."
    );
    assert_eq!(
        store.stream("entity:fence-reaction-child").len(),
        0,
        "PROPERTY: a reaction submission under a live fence must remain invisible before commit."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        1,
        "PROPERTY: the unfenced root event must remain visible while the fenced reaction stays hidden."
    );

    fence.cancel().expect("cancel fence");
    assert!(
        matches!(wait_append_ticket(&ticket, "cancelled fence ticket"), Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: cancelling a fence after a reaction submission must surface VisibilityFenceCancelled."
    );
    assert_eq!(
        store.stream("entity:fence-reaction-child").len(),
        0,
        "PROPERTY: cancelling a fence must discard the pending reaction submission."
    );

    store.close().expect("close store");
    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.stream("entity:fence-reaction-child").len(),
        0,
        "PROPERTY: a cancelled reaction submission under a fence must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn fenced_reaction_commit_preserves_reaction_metadata() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let root_coord =
        Coordinate::new("entity:fence-reaction-commit-root", "scope:test").expect("coord");
    let reaction_coord =
        Coordinate::new("entity:fence-reaction-commit-child", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let root = store
        .append(&root_coord, kind, &serde_json::json!({"root": true}))
        .expect("append root");

    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit_reaction(
            &reaction_coord,
            kind,
            &serde_json::json!({"reaction": "commit"}),
            root.event_id,
            root.event_id,
        )
        .expect("submit fenced reaction");
    assert_eq!(
        store.stream("entity:fence-reaction-commit-child").len(),
        0,
        "PROPERTY: a fenced reaction must stay hidden until the fence commits."
    );

    fence.commit().expect("commit fence");
    let reaction =
        wait_append_ticket(&ticket, "committed fenced reaction").expect("wait committed reaction");
    let entries = store.stream("entity:fence-reaction-commit-child");
    assert_eq!(
        entries.len(),
        1,
        "PROPERTY: committing a fenced reaction must publish exactly one reaction entry."
    );
    let reaction_entry = &entries[0];
    assert_eq!(
        reaction_entry.event_id, reaction.event_id,
        "PROPERTY: the committed reaction receipt must identify the stored reaction event."
    );
    assert_eq!(
        reaction_entry.correlation_id, root.event_id,
        "PROPERTY: a committed fenced reaction must preserve the triggering correlation id."
    );
    assert_eq!(
        reaction_entry.causation_id,
        Some(root.event_id),
        "PROPERTY: a committed fenced reaction must preserve the triggering causation id."
    );

    store.close().expect("close store");
}

#[test]
fn shutdown_with_live_fence_cancels_pending_fence_work() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-shutdown", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let ticket = {
        let fence = store.begin_visibility_fence().expect("begin fence");
        let ticket = fence
            .submit(&coord, kind, &serde_json::json!({"fenced": "shutdown"}))
            .expect("submit fenced work");
        let _fence = std::mem::ManuallyDrop::new(fence);
        ticket
    };

    store.close().expect("close store");

    assert!(
        matches!(
            wait_append_ticket(&ticket, "cancelled fence ticket"),
            Err(StoreError::VisibilityFenceCancelled)
        ),
        "PROPERTY: shutting down with a still-live visibility fence must cancel its pending work \
         rather than silently committing or hanging."
    );

    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.by_fact(kind).len(),
        0,
        "PROPERTY: shutdown-cancelled fence writes must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
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
