// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/cursor_durability.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Durable cursor checkpoints.
//!
//! [INV-DELIVERY-AT-LEAST-ONCE-WITNESS] A cursor worker constructed with a `checkpoint_id`
//! persists its position atomically after every successful batch. After the
//! store is closed and reopened, a new cursor worker with the same id
//! resumes exactly from the persisted position: it sees only the events
//! that arrived after the checkpoint, never the ones it has already
//! consumed.
//! Harness pattern: State-Machine Harness.
//!
//! PROVES: durable cursor checkpoints only commit honest progress and restart
//! from the last committed checkpoint.
//! CATCHES: checkpoint write/startup corruption, region mismatch, rollback
//! leaks, and panic restarts that resume from an uncommitted batch.
//! SEEDED: deterministic / no randomness.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::delivery::cursor::{CursorCheckpoint, CursorWorkerAction, CursorWorkerConfig};
use batpak::store::{CheckpointId, Cursor, RestartPolicy, Store, StoreConfig};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xA, 1);
const CHECKPOINT_ID: &str = "batpak-test-durable-cursor";
const CORRUPT_START_CHECKPOINT_ID: &str = "batpak-test-corrupt-start";
const REGION_BOUND_CHECKPOINT_ID: &str = "batpak-test-region-bound";
const CHECKPOINT_WRITE_FAILS_ID: &str = "batpak-test-checkpoint-write-fails";
const STATE_MACHINE_CHECKPOINT_ID: &str = "batpak-test-cursor-state-machine";

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1)
}

fn valid_checkpoint_id(id: &str) -> CheckpointId {
    CheckpointId::new(id).expect("valid checkpoint id")
}

fn wait_until(cond: impl Fn() -> bool, timeout: Duration, description: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("timed out waiting for: {description}");
}

struct StrayCheckpointGuard {
    path: PathBuf,
}

impl StrayCheckpointGuard {
    fn new(id: &str) -> Self {
        assert!(
            id.starts_with("batpak-test-"),
            "test-owned checkpoint ids must stay namespaced, got `{id}`"
        );
        let path = std::env::current_dir()
            .expect("current dir")
            .join(format!("{id}.ckpt"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    fn assert_absent(&self) {
        assert!(
            !self.path.exists(),
            "PROPERTY: durable checkpoint writes must stay under the store data dir, not leak to {}",
            self.path.display()
        );
    }
}

impl Drop for StrayCheckpointGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
    }
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
fn cursor_checkpoint_round_trips_through_save_and_load() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_guard = StrayCheckpointGuard::new(CHECKPOINT_ID);
    let checkpoint = CursorCheckpoint {
        position: 42,
        started: true,
        process_boot_ns: None,
        region_identity: Some("entity=entity:roundtrip|scope=*|fact=none|clock=*".to_owned()),
    };

    let checkpoint_id = valid_checkpoint_id(CHECKPOINT_ID);
    Cursor::save_checkpoint(dir.path(), &checkpoint_id, &checkpoint).expect("save checkpoint");
    let loaded = Cursor::load_checkpoint(dir.path(), &checkpoint_id)
        .expect("load checkpoint")
        .expect("checkpoint should exist");

    assert_eq!(loaded, checkpoint);
    checkpoint_guard.assert_absent();
}

#[test]
fn cursor_worker_fails_closed_on_corrupt_checkpoint() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_guard = StrayCheckpointGuard::new(CORRUPT_START_CHECKPOINT_ID);
    let checkpoint_dir = dir.path().join("cursors");
    std::fs::create_dir_all(&checkpoint_dir).expect("create cursor dir");
    let checkpoint_path = checkpoint_dir.join(format!("{CORRUPT_START_CHECKPOINT_ID}.ckpt"));
    std::fs::write(&checkpoint_path, b"not-msgpack").expect("write corrupt checkpoint");

    let store = Arc::new(Store::open(config(&dir)).expect("open store"));
    let coord = Coordinate::new("entity:cursor-corrupt", "scope:test").expect("valid coord");
    // Seed a matching event so silent checkpoint-load skips cannot idle forever.
    store
        .append(&coord, KIND, &serde_json::json!({"i": 0}))
        .expect("append seed event");
    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Once;
    worker_config.checkpoint_id = Some(valid_checkpoint_id(CORRUPT_START_CHECKPOINT_ID));

    let worker = store
        .cursor_worker(
            &Region::entity("entity:cursor-corrupt"),
            worker_config,
            |_batch, _store, _witness| CursorWorkerAction::Stop,
        )
        .expect("spawn cursor worker");

    let err = match worker.join() {
        Ok(()) => panic!("PROPERTY: corrupt durable checkpoint must fail closed on startup"),
        Err(err) => err,
    };
    let batpak::store::StoreError::CursorCheckpointCorrupt { path, .. } = err else {
        panic!("expected CursorCheckpointCorrupt");
    };
    let expected_checkpoint_path =
        std::fs::canonicalize(&checkpoint_path).expect("canonical checkpoint path");
    assert_eq!(path, expected_checkpoint_path);

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("cursor worker must release its Arc before close"),
    };
    store.close().expect("close store after corrupt checkpoint");
    checkpoint_guard.assert_absent();
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
            store
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
                        let total = processed.fetch_add(batch.len() as u64, Ordering::SeqCst)
                            + batch.len() as u64;
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

        let store = match Arc::try_unwrap(store) {
            Ok(store) => store,
            Err(_) => panic!("cursor worker must release its Arc before close"),
        };
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
            store
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
                        let total = processed.fetch_add(batch.len() as u64, Ordering::SeqCst)
                            + batch.len() as u64;
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

        let store = match Arc::try_unwrap(store) {
            Ok(store) => store,
            Err(_) => panic!("cursor worker must release its Arc before close"),
        };
        store.close().expect("close store after second pass");
    }

    checkpoint_guard.assert_absent();
}

#[test]
fn cursor_worker_rejects_checkpoint_id_reused_for_different_region() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_guard = StrayCheckpointGuard::new(REGION_BOUND_CHECKPOINT_ID);
    let coord_a = Coordinate::new("entity:cursor-a", "scope:test").expect("coord a");
    let coord_b = Coordinate::new("entity:cursor-b", "scope:test").expect("coord b");
    let store = Arc::new(Store::open(config(&dir)).expect("open store"));

    store
        .append(&coord_a, KIND, &serde_json::json!({"i": 0}))
        .expect("append a");
    store
        .append(&coord_b, KIND, &serde_json::json!({"i": 1}))
        .expect("append b");

    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Once;
    worker_config.checkpoint_id = Some(valid_checkpoint_id(REGION_BOUND_CHECKPOINT_ID));
    let first_worker = store
        .cursor_worker(
            &Region::entity("entity:cursor-a"),
            worker_config.clone(),
            |_batch, _store, _witness| CursorWorkerAction::Stop,
        )
        .expect("spawn first worker");
    first_worker.join().expect("first worker join");

    let second_worker = store
        .cursor_worker(
            &Region::entity("entity:cursor-b"),
            worker_config,
            |_batch, _store, _witness| CursorWorkerAction::Stop,
        )
        .expect("spawn second worker");
    let err = match second_worker.join() {
        Ok(()) => panic!("PROPERTY: checkpoint_id reused for a different region must fail closed"),
        Err(err) => err,
    };
    let batpak::store::StoreError::CursorCheckpointRegionMismatch { expected, .. } = err else {
        panic!("expected CursorCheckpointRegionMismatch");
    };
    assert!(
        expected.contains("entity:cursor-b"),
        "expected region identity should mention the second worker's entity filter"
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("cursor workers must release their Arc before close"),
    };
    store.close().expect("close store");
    checkpoint_guard.assert_absent();
}

#[test]
fn cursor_worker_surfaces_checkpoint_write_failure_through_join() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_guard = StrayCheckpointGuard::new(CHECKPOINT_WRITE_FAILS_ID);
    let coord = Coordinate::new("entity:cursor-ckpt-fail", "scope:test").expect("valid coord");
    let store = Arc::new(Store::open(config(&dir)).expect("open store"));

    // Allow startup to bind the durable cursor. The handler will create a
    // blocking directory at the final checkpoint path immediately before the
    // worker tries to persist, so startup stays clean and the write path
    // fails deterministically.
    let cursor_dir = dir.path().join("cursors");
    std::fs::create_dir_all(&cursor_dir).expect("create cursor dir");

    let processed = Arc::new(AtomicU64::new(0));
    let worker = {
        let processed = Arc::clone(&processed);
        let checkpoint_blocker_root = dir.path().to_path_buf();
        let mut worker_config = CursorWorkerConfig::default();
        worker_config.batch_size = 1;
        worker_config.idle_sleep = Duration::from_millis(1);
        worker_config.restart = RestartPolicy::Once;
        worker_config.checkpoint_id = Some(valid_checkpoint_id(CHECKPOINT_WRITE_FAILS_ID));
        store
            .cursor_worker(
                &Region::entity("entity:cursor-ckpt-fail"),
                worker_config,
                move |batch, _store, _witness| {
                    processed.fetch_add(batch.len() as u64, Ordering::SeqCst);
                    std::fs::create_dir_all(
                        checkpoint_blocker_root
                            .join("cursors")
                            .join(format!("{CHECKPOINT_WRITE_FAILS_ID}.ckpt")),
                    )
                    .expect("create blocking checkpoint path");
                    CursorWorkerAction::Stop
                },
            )
            .expect("spawn cursor worker")
    };

    store
        .append(&coord, KIND, &serde_json::json!({"i": 0}))
        .expect("append");

    let err = match worker.join() {
        Ok(()) => panic!("PROPERTY: durable cursor worker must surface checkpoint write failure"),
        Err(err) => err,
    };
    let batpak::store::StoreError::CheckpointWriteFailed { id, .. } = err else {
        panic!("expected CheckpointWriteFailed");
    };
    assert_eq!(id, CHECKPOINT_WRITE_FAILS_ID);
    assert_eq!(
        processed.load(Ordering::SeqCst),
        1,
        "worker should process exactly one batch before surfacing the checkpoint failure"
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("cursor worker must release its Arc before close"),
    };
    store.close().expect("close store after checkpoint failure");
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
        store
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
                        _ => panic!("PROPERTY: phase one should only reach sequences 0 and 1"),
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
                            panic!("intentional durable cursor panic after checkpointed progress");
                        }
                        4 => CursorWorkerAction::Continue,
                        5 => CursorWorkerAction::Stop,
                        _ => panic!(
                            "PROPERTY: phase two should only reach rolled-back tail sequences 1..=4"
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

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("cursor worker must release its Arc before close"),
    };
    store
        .close()
        .expect("close store after state-machine harness");
    checkpoint_guard.assert_absent();
}
