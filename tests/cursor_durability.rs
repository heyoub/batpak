// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/cursor_durability.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Durable cursor checkpoints.
//!
//! [INV-CURSOR-DURABLE] A cursor worker constructed with a `checkpoint_id`
//! persists its position atomically after every successful batch. After the
//! store is closed and reopened, a new cursor worker with the same id
//! resumes exactly from the persisted position: it sees only the events
//! that arrived after the checkpoint, never the ones it has already
//! consumed.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::delivery::cursor::{CursorCheckpoint, CursorWorkerAction, CursorWorkerConfig};
use batpak::store::{Cursor, RestartPolicy, Store, StoreConfig};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xA, 1);
const CHECKPOINT_ID: &str = "durable-cursor-test";

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1)
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

#[test]
fn cursor_checkpoint_round_trips_through_save_and_load() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint = CursorCheckpoint {
        position: 42,
        started: true,
        process_boot_ns: None,
        region_identity: Some("entity=entity:roundtrip|scope=*|fact=none|clock=*".to_owned()),
    };

    Cursor::save_checkpoint(dir.path(), CHECKPOINT_ID, &checkpoint).expect("save checkpoint");
    let loaded = Cursor::load_checkpoint(dir.path(), CHECKPOINT_ID)
        .expect("load checkpoint")
        .expect("checkpoint should exist");

    assert_eq!(loaded, checkpoint);
}

#[test]
fn cursor_worker_fails_closed_on_corrupt_checkpoint() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_dir = dir.path().join("cursors");
    std::fs::create_dir_all(&checkpoint_dir).expect("create cursor dir");
    let checkpoint_path = checkpoint_dir.join("corrupt-start.ckpt");
    std::fs::write(&checkpoint_path, b"not-msgpack").expect("write corrupt checkpoint");

    let store = Arc::new(Store::open(config(&dir)).expect("open store"));
    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Once;
    worker_config.checkpoint_id = Some("corrupt-start".into());

    let worker = store
        .cursor_worker(
            &Region::entity("entity:cursor-corrupt"),
            worker_config,
            |_batch, _store| CursorWorkerAction::Stop,
        )
        .expect("spawn cursor worker");

    let err = match worker.join() {
        Ok(()) => panic!("PROPERTY: corrupt durable checkpoint must fail closed on startup"),
        Err(err) => err,
    };
    let batpak::store::StoreError::CursorCheckpointCorrupt { path, .. } = err else {
        panic!("expected CursorCheckpointCorrupt");
    };
    assert_eq!(path, checkpoint_path);

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("cursor worker must release its Arc before close"),
    };
    store.close().expect("close store after corrupt checkpoint");
}

#[test]
fn cursor_resumes_from_checkpoint_across_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let coord = Coordinate::new("entity:cursor-durable", "scope:test").expect("valid coord");

    // Phase 1: seed 100 events, spawn a cursor worker that stops after
    // processing the first 50. Capture the exact set of sequences it saw.
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
            worker_config.checkpoint_id = Some(CHECKPOINT_ID.into());
            store
                .cursor_worker(
                    &Region::entity("entity:cursor-durable"),
                    worker_config,
                    move |batch, _store| {
                        let mut seen = seen.lock().expect("seen mutex");
                        for entry in batch {
                            seen.push(entry.global_sequence);
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

        // The worker issues a durable Stop after it reaches 50 events, so
        // join() (passive) will return once that stop is observed.
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

    // Phase 2: reopen, append 50 more events, spawn a NEW worker with the
    // same checkpoint_id. It must see ONLY the new events.
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
            worker_config.checkpoint_id = Some(CHECKPOINT_ID.into());
            store
                .cursor_worker(
                    &Region::entity("entity:cursor-durable"),
                    worker_config,
                    move |batch, _store| {
                        let mut seen = seen.lock().expect("seen mutex");
                        for entry in batch {
                            seen.push(entry.global_sequence);
                        }
                        processed.fetch_add(batch.len() as u64, Ordering::SeqCst);
                        CursorWorkerAction::Continue
                    },
                )
                .expect("spawn second-pass cursor worker")
        };

        // Wait until the second-pass worker has consumed the remaining
        // new events. The worker keeps polling; we stop it externally once
        // the observable progress counter reaches the expected count.
        // The durable checkpoint recorded position ~50, so the second pass
        // must cover sequences 50..150 — 100 events total.
        let processed_for_wait = Arc::clone(&processed);
        wait_until(
            || processed_for_wait.load(Ordering::SeqCst) >= 100,
            Duration::from_secs(5),
            "second-pass cursor to process the 100 post-checkpoint events",
        );

        worker.stop_and_join().expect("stop second-pass worker");

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
}

#[test]
fn cursor_worker_rejects_checkpoint_id_reused_for_different_region() {
    let dir = TempDir::new().expect("temp dir");
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
    worker_config.checkpoint_id = Some("region-bound".into());
    let first_worker = store
        .cursor_worker(
            &Region::entity("entity:cursor-a"),
            worker_config.clone(),
            |_batch, _store| CursorWorkerAction::Stop,
        )
        .expect("spawn first worker");
    first_worker.join().expect("first worker join");

    let second_worker = store
        .cursor_worker(
            &Region::entity("entity:cursor-b"),
            worker_config,
            |_batch, _store| CursorWorkerAction::Stop,
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
}

#[test]
fn cursor_worker_surfaces_checkpoint_write_failure_through_join() {
    let dir = TempDir::new().expect("temp dir");
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
        worker_config.checkpoint_id = Some("checkpoint-write-fails".into());
        store
            .cursor_worker(
                &Region::entity("entity:cursor-ckpt-fail"),
                worker_config,
                move |batch, _store| {
                    processed.fetch_add(batch.len() as u64, Ordering::SeqCst);
                    std::fs::create_dir_all(
                        checkpoint_blocker_root
                            .join("cursors")
                            .join("checkpoint-write-fails.ckpt"),
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
    assert_eq!(id, "checkpoint-write-fails");
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
}
