// justifies: INV-TEST-PANIC-AS-ASSERTION, ADR-0002; writer command-flow tests in tests/writer_command_flow.rs use panic! to surface unexpected writer states when the WriterCommand handshake breaks.
#![allow(clippy::panic)]

use batpak::prelude::*;
use batpak::store::{AppendOptions, BatchAppendItem, Store, StoreConfig, StoreError};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

#[path = "support/bounded_writer_reply.rs"]
mod bounded_writer_reply;
use bounded_writer_reply::writer_reply;

const KIND: EventKind = EventKind::custom(0xF, 0x55);

fn command_flow_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_group_commit_max_batch(8)
        .with_sync_every_n_events(1024)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
}

fn flow_coord() -> Coordinate {
    Coordinate::new("entity:writer-flow", "scope:test").expect("coord")
}

fn sync_append_with_idempotency(
    store: &Store,
    coord: &Coordinate,
    key: u128,
    payload: &serde_json::Value,
) -> Result<batpak::store::AppendReceipt, StoreError> {
    store.append_with_options(
        coord,
        KIND,
        payload,
        AppendOptions::new().with_idempotency(key),
    )
}

fn flow_batch_item(coord: Coordinate, key: u128, payload: &serde_json::Value) -> BatchAppendItem {
    BatchAppendItem::new(
        coord,
        KIND,
        payload,
        AppendOptions::new().with_idempotency(key),
        batpak::store::CausationRef::None,
    )
    .expect("construct writer flow batch item")
}

fn spawn_named<T>(
    name: impl Into<String>,
    f: impl FnOnce() -> T + Send + 'static,
) -> thread::JoinHandle<T>
where
    T: Send + 'static,
{
    thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("spawn named test thread")
}

#[test]
fn mixed_append_and_batch_commands_complete_under_group_commit_drain() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(command_flow_config(&dir)).expect("open store"));
    let coord = flow_coord();
    let barrier = Arc::new(Barrier::new(3));

    // Intentional: barrier waits coordinate simultaneous writer-command entry;
    // participant count is fixed by this test.
    let append_a = {
        let store = Arc::clone(&store);
        let coord = coord.clone();
        let barrier = Arc::clone(&barrier);
        spawn_named("writer-flow-append-a", move || {
            barrier.wait();
            sync_append_with_idempotency(&store, &coord, 0xA1, &serde_json::json!({"n": 1}))
        })
    };

    let append_b = {
        let store = Arc::clone(&store);
        let coord = coord.clone();
        let barrier = Arc::clone(&barrier);
        spawn_named("writer-flow-append-b", move || {
            barrier.wait();
            sync_append_with_idempotency(&store, &coord, 0xB2, &serde_json::json!({"n": 2}))
        })
    };

    barrier.wait();
    let batch = vec![
        flow_batch_item(coord.clone(), 0xC3, &serde_json::json!({"n": 3})),
        flow_batch_item(coord.clone(), 0xD4, &serde_json::json!({"n": 4})),
    ];
    let batch_receipts = store.append_batch(batch).expect("append batch");
    let receipt_a = append_a.join().expect("append a thread").expect("append a");
    let receipt_b = append_b.join().expect("append b thread").expect("append b");

    store.sync().expect("sync");
    let stream = store.by_entity("entity:writer-flow");
    let sequences: Vec<u64> = stream.iter().map(|entry| entry.global_sequence()).collect();
    assert_eq!(
        stream.len(),
        4,
        "PROPERTY: mixed append and batch commands must all become visible under group commit drain."
    );
    let first_sequence = sequences[0];
    assert_eq!(
        sequences,
        (first_sequence..first_sequence + 4).collect::<Vec<_>>(),
        "PROPERTY: mixed append and batch commands must preserve contiguous visible sequencing."
    );
    assert!(receipt_a.sequence <= first_sequence + 3);
    assert!(receipt_b.sequence <= first_sequence + 3);
    assert_eq!(batch_receipts.len(), 2);

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: mixed command flow test should release all Arc clones"),
    };
    store.close().expect("close");
}

#[test]
fn sync_during_group_commit_drain_preserves_completed_work() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(command_flow_config(&dir)).expect("open store"));
    let coord = flow_coord();
    let barrier = Arc::new(Barrier::new(5));

    // Intentional: barrier waits coordinate a bounded set of append threads.
    let handles: Vec<_> = (0..4u128)
        .map(|idx| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            let barrier = Arc::clone(&barrier);
            spawn_named(format!("writer-flow-sync-{idx}"), move || {
                barrier.wait();
                sync_append_with_idempotency(
                    &store,
                    &coord,
                    0x100 + idx,
                    &serde_json::json!({"idx": idx}),
                )
            })
        })
        .collect();

    barrier.wait();
    store.sync().expect("sync during drain");
    for handle in handles {
        handle
            .join()
            .expect("append thread")
            .expect("append receipt");
    }

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: sync during drain test should release all Arc clones"),
    };
    store.close().expect("close");

    let reopened = Store::open(command_flow_config(&dir)).expect("reopen");
    assert_eq!(
        reopened.by_fact(KIND).len(),
        4,
        "PROPERTY: sync interleaved with group commit drain must preserve all completed writes across reopen."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn begin_visibility_fence_after_unfenced_drain_keeps_pre_fence_work_visible() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(command_flow_config(&dir)).expect("open store"));
    let coord = flow_coord();
    sync_append_with_idempotency(
        &store,
        &coord,
        0x1FF,
        &serde_json::json!({"pre_fence": "seed"}),
    )
    .expect("seed append before fence");
    let barrier = Arc::new(Barrier::new(4));

    // Intentional: barrier waits coordinate a bounded set of append threads.
    let handles: Vec<_> = (0..3u128)
        .map(|idx| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            let barrier = Arc::clone(&barrier);
            spawn_named(format!("writer-flow-fence-{idx}"), move || {
                barrier.wait();
                sync_append_with_idempotency(
                    &store,
                    &coord,
                    0x200 + idx,
                    &serde_json::json!({"pre_fence": idx}),
                )
            })
        })
        .collect();

    barrier.wait();
    let fence = store.begin_visibility_fence().expect("begin fence");
    let mut fenced_outbox = fence.outbox();
    fenced_outbox
        .stage_with_options(
            coord.clone(),
            KIND,
            &serde_json::json!({"fenced": true}),
            AppendOptions::new().with_idempotency(0x2FF),
        )
        .expect("stage fenced work");
    let fenced_ticket = fenced_outbox.submit_flush().expect("submit fenced work");
    fence.cancel().expect("cancel fence");

    let mut successful_unfenced = 1usize;
    for handle in handles {
        match handle.join().expect("append thread") {
            Ok(_) => successful_unfenced += 1,
            Err(StoreError::VisibilityFenceActive) => {}
            Err(err) => panic!(
                "PROPERTY: unfenced drain append must either commit before the fence or be rejected with VisibilityFenceActive, got {err:?}"
            ),
        }
    }

    let err = match writer_reply(fenced_ticket.receiver(), "cancelled fenced batch ticket") {
        Ok(_) => panic!("PROPERTY: cancelled fence work must not resolve as visible success"),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::VisibilityFenceCancelled),
        "cancelled fence work must surface VisibilityFenceCancelled, got {err:?}"
    );
    assert_eq!(
        store.by_fact(KIND).len(),
        successful_unfenced,
        "PROPERTY: beginning and cancelling a fence after unfenced drain work must keep all successfully submitted pre-fence writes visible while keeping fenced work hidden."
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: fence barrier test should release all Arc clones"),
    };
    store.close().expect("close");

    let reopened = Store::open(command_flow_config(&dir)).expect("reopen");
    assert_eq!(
        reopened.by_fact(KIND).len(),
        successful_unfenced,
        "PROPERTY: cancelled fenced work must stay hidden after reopen while the pre-fence drained work remains visible."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn shutdown_auto_cancels_pending_fenced_responses_after_drain_mix() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(command_flow_config(&dir)).expect("open store");
    let coord = flow_coord();

    let visible = sync_append_with_idempotency(&store, &coord, 0x300, &serde_json::json!({"n": 1}))
        .expect("visible append");
    assert_eq!(visible.sequence, 1);

    let ticket = {
        let fence = store.begin_visibility_fence().expect("begin fence");
        let mut outbox = fence.outbox();
        outbox
            .stage_with_options(
                coord.clone(),
                KIND,
                &serde_json::json!({"fenced": "shutdown"}),
                AppendOptions::new().with_idempotency(0x3FF),
            )
            .expect("stage fenced work");
        let ticket = outbox.submit_flush().expect("submit fenced work");
        let _fence = std::mem::ManuallyDrop::new(fence);
        ticket
    };

    store.close().expect("close store");

    assert!(
        matches!(writer_reply(ticket.receiver(), "writer ticket"), Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: shutdown with a still-live fence must cancel its pending response after mixed unfenced/fenced command flow."
    );

    let reopened = Store::open(command_flow_config(&dir)).expect("reopen");
    assert_eq!(
        reopened.by_fact(KIND).len(),
        1,
        "PROPERTY: shutdown auto-cancel must preserve visible unfenced work while keeping fenced work hidden."
    );
    reopened.close().expect("close reopened");
}
