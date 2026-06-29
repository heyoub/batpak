//! Durable cursor checkpoints: delivery-progress family.
//!
//! [INV-DELIVERY-AT-LEAST-ONCE-WITNESS] A cursor worker constructed with a
//! `checkpoint_id` persists its position atomically after every successful
//! batch. This binary covers the delivery-progress seam: a reopened worker
//! resumes exactly from the persisted position and a worker only ever re-delivers
//! rolled-back or panicked batches, never the last committed checkpoint.
//! Harness pattern: State-Machine Harness.
//!
//! PROVES: durable cursor checkpoints only commit honest progress and restart
//! from the last committed checkpoint across reopen, rollback, and panic.
//! CATCHES: rollback leaks and panic restarts that resume from an uncommitted
//! batch, and re-delivery of events the previous run already acked.
//! SEEDED: deterministic / no randomness.

use batpak_testkit::cursor_durability as cd_support;

use batpak::coordinate::{Coordinate, Region};
use batpak::store::delivery::cursor::{CursorWorkerAction, CursorWorkerConfig};
use batpak::store::{Cursor, RestartPolicy, Store};
use cd_support::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const CHECKPOINT_ID: &str = "batpak-test-durable-cursor";
const STATE_MACHINE_CHECKPOINT_ID: &str = "batpak-test-cursor-state-machine";

fn wait_until(cond: impl Fn() -> bool, timeout: Duration, description: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::yield_now();
    }
    assert!(cond(), "timed out waiting for: {description}");
}

fn assert_checkpoint_position(
    dir: &TempDir,
    checkpoint_id: &str,
    expected_position: u64,
    description: &str,
) {
    let checkpoint_id = valid_checkpoint_id(checkpoint_id);
    let checkpoint = Cursor::load_checkpoint(dir.path(), &checkpoint_id)
        .expect("load checkpoint")
        .expect("checkpoint should exist");
    assert_eq!(
        checkpoint.position, expected_position,
        "PROPERTY: {description} must persist position {expected_position}, got {}",
        checkpoint.position
    );
    assert!(
        checkpoint.started,
        "PROPERTY: {description} must persist started=true once at least one event was delivered"
    );
}

#[test]
fn cursor_resumes_from_checkpoint_across_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_guard = StrayCheckpointGuard::new(CHECKPOINT_ID);
    let coord = Coordinate::new("entity:cursor-durable", "scope:test").expect("valid coord");

    // Phase 1: stop after the first 50 events and capture exact sequences.
    let first_pass_seen: Arc<Mutex<Vec<u64>>> = {
        let first_pass_seen = Arc::new(Mutex::new(Vec::<u64>::new()));
        let store = Arc::new(Store::open(config(&dir)).expect("open store"));
        for i in 0..100u32 {
            let _ = store
                .append(&coord, KIND, &serde_json::json!({"i": i}))
                .expect("seed append");
        }

        let processed = Arc::new(AtomicU64::new(0));
        let worker = {
            let seen = Arc::clone(&first_pass_seen);
            let processed = Arc::clone(&processed);
            let mut worker_config = CursorWorkerConfig::default();
            worker_config.batch_size = 1;
            worker_config.idle_sleep = Duration::from_millis(1);
            worker_config.restart = RestartPolicy::Once;
            worker_config.checkpoint_id = Some(valid_checkpoint_id(CHECKPOINT_ID));
            store
                .cursor_worker(
                    &Region::entity("entity:cursor-durable"),
                    worker_config,
                    move |batch, _store, _witness| {
                        let mut seen = seen.lock().expect("seen mutex");
                        for entry in batch {
                            seen.push(entry.global_sequence());
                        }
                        let batch_len =
                            u64::try_from(batch.len()).expect("batch length fits in u64");
                        let total = processed.fetch_add(batch_len, Ordering::SeqCst) + batch_len;
                        // Stop at exactly 50 events so the checkpoint records
                        // position = 49 (inclusive) / next-poll = 50.
                        if total >= 50 {
                            CursorWorkerAction::Stop
                        } else {
                            CursorWorkerAction::Continue
                        }
                    },
                )
                .expect("spawn cursor worker")
        };

        // join() returns once the durable Stop is observed.
        worker.join().expect("worker joined cleanly");

        let final_processed = processed.load(Ordering::SeqCst);
        assert!(
            final_processed >= 50,
            "PROPERTY: first-pass worker must process at least 50 events, got {final_processed}"
        );

        let store = Arc::try_unwrap(store)
            .map_err(|_| "shared")
            .expect("cursor worker must release its Arc before close");
        store.close().expect("close store after first pass");
        first_pass_seen
    };

    let first_pass_set: std::collections::HashSet<u64> = first_pass_seen
        .lock()
        .expect("first_pass_seen mutex")
        .iter()
        .copied()
        .collect();

    // Phase 2: reuse the checkpoint id. It must see only the tail.
    {
        let store = Arc::new(Store::open(config(&dir)).expect("reopen store"));
        for i in 100..150u32 {
            let _ = store
                .append(&coord, KIND, &serde_json::json!({"i": i}))
                .expect("post-reopen append");
        }

        let second_pass_seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let processed = Arc::new(AtomicU64::new(0));

        let worker = {
            let seen = Arc::clone(&second_pass_seen);
            let processed = Arc::clone(&processed);
            let mut worker_config = CursorWorkerConfig::default();
            worker_config.batch_size = 8;
            worker_config.idle_sleep = Duration::from_millis(1);
            worker_config.restart = RestartPolicy::Once;
            worker_config.checkpoint_id = Some(valid_checkpoint_id(CHECKPOINT_ID));
            store
                .cursor_worker(
                    &Region::entity("entity:cursor-durable"),
                    worker_config,
                    move |batch, _store, _witness| {
                        let mut seen = seen.lock().expect("seen mutex");
                        for entry in batch {
                            seen.push(entry.global_sequence());
                        }
                        let batch_len =
                            u64::try_from(batch.len()).expect("batch length fits in u64");
                        let total = processed.fetch_add(batch_len, Ordering::SeqCst) + batch_len;
                        if total < 100 {
                            return CursorWorkerAction::Continue;
                        }
                        CursorWorkerAction::Stop
                    },
                )
                .expect("spawn second-pass cursor worker")
        };

        // The worker stops after the 100 post-checkpoint tail events.
        let processed_for_wait = Arc::clone(&processed);
        wait_until(
            || processed_for_wait.load(Ordering::SeqCst) >= 100,
            Duration::from_secs(30),
            "second-pass cursor to process the 100 post-checkpoint events",
        );

        worker.join().expect("second-pass worker joined cleanly");
        let second_pass = second_pass_seen.lock().expect("second_pass mutex").clone();

        // Every sequence the second pass observed must be strictly greater
        // than any sequence the first pass observed. If the two sets
        // overlap, the durable checkpoint was not honoured.
        let overlap: Vec<u64> = second_pass
            .iter()
            .filter(|seq| first_pass_set.contains(*seq))
            .copied()
            .collect();
        assert!(
            overlap.is_empty(),
            "PROPERTY: a cursor resumed from its durable checkpoint must never re-deliver \
             events the previous run already acked. Overlap: {overlap:?}"
        );

        // The second pass must cover exactly the post-checkpoint tail
        // (the remaining 50 originals + 50 new = 100 events). The first
        // pass covered up to 50; the tail is 100 events.
        assert_eq!(
            second_pass.len(),
            100,
            "PROPERTY: second-pass worker must cover the 100 post-checkpoint events; got {}",
            second_pass.len()
        );

        let store = Arc::try_unwrap(store)
            .map_err(|_| "shared")
            .expect("cursor worker must release its Arc before close");
        store.close().expect("close store after second pass");
    }

    checkpoint_guard.assert_absent();
}

#[test]
fn durable_cursor_worker_state_machine_preserves_last_committed_checkpoint() {
    // PROVES: a durable cursor worker commits `Continue`, rewinds
    // `StopWithRollback`, and restarts from the last committed checkpoint
    // after a panic instead of from the most recently polled batch.
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_id = STATE_MACHINE_CHECKPOINT_ID;
    let checkpoint_guard = StrayCheckpointGuard::new(checkpoint_id);
    let coord = Coordinate::new("entity:cursor-state-machine", "scope:test").expect("coord");
    let store = Arc::new(Store::open(config(&dir)).expect("open store"));

    for i in 0..5u32 {
        let _ = store
            .append(&coord, KIND, &serde_json::json!({"i": i}))
            .expect("seed append");
    }

    let seen = Arc::new(Mutex::new(BTreeMap::<u64, usize>::new()));
    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Once;
    worker_config.checkpoint_id = Some(valid_checkpoint_id(checkpoint_id));

    let phase_one = store
        .cursor_worker(
            &Region::entity("entity:cursor-state-machine"),
            worker_config.clone(),
            {
                let seen = Arc::clone(&seen);
                move |batch, _store, _witness| {
                    let seq = batch[0].global_sequence();
                    let mut counts = seen.lock().expect("counts mutex");
                    *counts.entry(seq).or_insert(0) += 1;
                    drop(counts);

                    match seq {
                        1 => CursorWorkerAction::Continue,
                        2 => CursorWorkerAction::StopWithRollback,
                        other => {
                            unreachable!(
                                "PROPERTY: phase one should only reach sequences 1 and 2, saw {other}"
                            )
                        }
                    }
                }
            },
        )
        .expect("spawn phase one worker");
    phase_one.join().expect("phase one join");

    assert_checkpoint_position(
        &dir,
        checkpoint_id,
        1,
        "phase one durable worker after StopWithRollback",
    );

    let phase_two = store
        .cursor_worker(
            &Region::entity("entity:cursor-state-machine"),
            worker_config,
            {
                let seen = Arc::clone(&seen);
                let panic_once = Arc::new(std::sync::atomic::AtomicBool::new(true));
                move |batch, _store, _witness| {
                    let seq = batch[0].global_sequence();
                    let mut counts = seen.lock().expect("counts mutex");
                    *counts.entry(seq).or_insert(0) += 1;
                    drop(counts);

                    match seq {
                        2 | 3 => CursorWorkerAction::Continue,
                        4 if panic_once.swap(false, std::sync::atomic::Ordering::SeqCst) => {
                            // Deliberate handler panic to exercise the durable
                            // panic-restart path. black_box keeps the condition
                            // non-constant so this stays a clippy-clean assert.
                            assert!(
                                std::hint::black_box(false),
                                "intentional durable cursor panic after checkpointed progress"
                            );
                            CursorWorkerAction::Stop
                        }
                        4 => CursorWorkerAction::Continue,
                        5 => CursorWorkerAction::Stop,
                        other => unreachable!(
                            "PROPERTY: phase two should only reach rolled-back tail sequences 2..=5, saw {other}"
                        ),
                    }
                }
            },
        )
        .expect("spawn phase two worker");
    phase_two.join().expect("phase two join");

    assert_checkpoint_position(
        &dir,
        checkpoint_id,
        5,
        "phase two durable worker after panic restart and clean stop",
    );

    let observed = seen.lock().expect("counts mutex").clone();
    let expected = BTreeMap::from([
        (1, 1usize),
        (2, 2usize),
        (3, 1usize),
        (4, 2usize),
        (5, 1usize),
    ]);
    assert_eq!(
        observed, expected,
        "PROPERTY: durable cursor state machine must re-deliver only the rolled-back or panicked \
         batches, never the last committed one.\n\
         Expected counts {:?}, got {:?}.",
        expected, observed
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("cursor worker must release its Arc before close");
    store
        .close()
        .expect("close store after state-machine harness");
    checkpoint_guard.assert_absent();
}
