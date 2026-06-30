//! Durable cursor checkpoints: corruption and write-fault family.
//!
//! [INV-DELIVERY-AT-LEAST-ONCE-WITNESS] A cursor worker constructed with a
//! `checkpoint_id` persists its position atomically. This binary covers the
//! checkpoint-integrity seam: round-trip persistence, fail-closed startup on a
//! corrupt checkpoint, region binding, and surfacing write failures.
//! Harness pattern: State-Machine Harness.
//!
//! PROVES: durable cursor checkpoints round-trip cleanly and fail closed when
//! the persisted checkpoint is corrupt, region-mismatched, or unwritable.
//! CATCHES: checkpoint write/startup corruption, region mismatch, and silent
//! swallowing of checkpoint write failures.
//! SEEDED: deterministic / no randomness.

use batpak_testkit::cursor_durability as cd_support;

use batpak::coordinate::{Coordinate, Region};
use batpak::store::delivery::cursor::{CursorCheckpoint, CursorWorkerAction, CursorWorkerConfig};
use batpak::store::{Cursor, RestartPolicy, Store};
use cd_support::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

const CHECKPOINT_ID: &str = "batpak-test-durable-cursor";
const CORRUPT_START_CHECKPOINT_ID: &str = "batpak-test-corrupt-start";
const REGION_BOUND_CHECKPOINT_ID: &str = "batpak-test-region-bound";
const CHECKPOINT_WRITE_FAILS_ID: &str = "batpak-test-checkpoint-write-fails";

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
    let _ = store
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

    let err = worker
        .join()
        .expect_err("PROPERTY: corrupt durable checkpoint must fail closed on startup");
    let expected_checkpoint_path =
        std::fs::canonicalize(&checkpoint_path).expect("canonical checkpoint path");
    assert!(
        matches!(
            &err,
            batpak::store::StoreError::CursorCheckpointCorrupt { path, .. }
                if *path == expected_checkpoint_path
        ),
        "expected CursorCheckpointCorrupt at {expected_checkpoint_path:?}, got {err:?}"
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("cursor worker must release its Arc before close");
    store.close().expect("close store after corrupt checkpoint");
    checkpoint_guard.assert_absent();
}

#[test]
fn cursor_worker_rejects_checkpoint_id_reused_for_different_region() {
    let dir = TempDir::new().expect("temp dir");
    let checkpoint_guard = StrayCheckpointGuard::new(REGION_BOUND_CHECKPOINT_ID);
    let coord_a = Coordinate::new("entity:cursor-a", "scope:test").expect("coord a");
    let coord_b = Coordinate::new("entity:cursor-b", "scope:test").expect("coord b");
    let store = Arc::new(Store::open(config(&dir)).expect("open store"));

    let _ = store
        .append(&coord_a, KIND, &serde_json::json!({"i": 0}))
        .expect("append a");
    let _ = store
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
    let err = second_worker
        .join()
        .expect_err("PROPERTY: checkpoint_id reused for a different region must fail closed");
    assert!(
        matches!(
            &err,
            batpak::store::StoreError::CursorCheckpointRegionMismatch { expected, .. }
                if expected.contains("entity:cursor-b")
        ),
        "expected CursorCheckpointRegionMismatch mentioning entity:cursor-b, got {err:?}"
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("cursor workers must release their Arc before close");
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
                    let batch_len = u64::try_from(batch.len()).expect("batch length fits in u64");
                    processed.fetch_add(batch_len, Ordering::SeqCst);
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

    let _ = store
        .append(&coord, KIND, &serde_json::json!({"i": 0}))
        .expect("append");

    let err = worker
        .join()
        .expect_err("PROPERTY: durable cursor worker must surface checkpoint write failure");
    assert!(
        matches!(
            &err,
            batpak::store::StoreError::CheckpointWriteFailed { id, .. }
                if id.as_str() == CHECKPOINT_WRITE_FAILS_ID
        ),
        "expected CheckpointWriteFailed for {CHECKPOINT_WRITE_FAILS_ID}, got {err:?}"
    );
    assert_eq!(
        processed.load(Ordering::SeqCst),
        1,
        "worker should process exactly one batch before surfacing the checkpoint failure"
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("cursor worker must release its Arc before close");
    store.close().expect("close store after checkpoint failure");
    checkpoint_guard.assert_absent();
}

#[test]
fn cursor_worker_stop_and_join_surfaces_startup_failure() {
    // Distinct, namespaced id so this never collides with the `.join()` sibling
    // under parallel test execution.
    const STOP_JOIN_CORRUPT_ID: &str = "batpak-test-stop-join-corrupt";

    let dir = TempDir::new().expect("temp dir");
    let checkpoint_guard = StrayCheckpointGuard::new(STOP_JOIN_CORRUPT_ID);

    // Plant a corrupt durable checkpoint so the worker fails closed during its
    // asynchronous startup: it records StoreError::CursorCheckpointCorrupt into
    // its error slot and exits on its own. That slotted error is exactly what
    // `stop_and_join` must join-and-drain — the mutant that returns Ok(()) skips
    // both the join and the drain.
    let checkpoint_dir = dir.path().join("cursors");
    std::fs::create_dir_all(&checkpoint_dir).expect("create cursor dir");
    let checkpoint_path = checkpoint_dir.join(format!("{STOP_JOIN_CORRUPT_ID}.ckpt"));
    std::fs::write(&checkpoint_path, b"not-msgpack").expect("write corrupt checkpoint");

    let store = Arc::new(Store::open(config(&dir)).expect("open store"));
    let coord = Coordinate::new("entity:cursor-stop-join", "scope:test").expect("valid coord");
    // Seed a matching event so a (hypothetical) silent checkpoint-load skip could
    // not idle forever instead of failing closed.
    let _ = store
        .append(&coord, KIND, &serde_json::json!({"i": 0}))
        .expect("append seed event");

    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Once;
    worker_config.checkpoint_id = Some(valid_checkpoint_id(STOP_JOIN_CORRUPT_ID));

    let worker = store
        .cursor_worker(
            &Region::entity("entity:cursor-stop-join"),
            worker_config,
            |_batch, _store, _witness| CursorWorkerAction::Stop,
        )
        .expect("spawn cursor worker");

    // The decisive call: route the startup failure through stop_and_join (NOT
    // join). Real code signals stop, joins the already-exited thread, and
    // surfaces the slotted error. The `Ok(())` mutant skips all of that, so this
    // expect_err fails immediately — by assertion, never by hanging.
    let err = worker.stop_and_join().expect_err(
        "PROPERTY: stop_and_join must signal-stop, join the worker, and surface its startup error",
    );
    let expected_checkpoint_path =
        std::fs::canonicalize(&checkpoint_path).expect("canonical checkpoint path");
    assert!(
        matches!(
            &err,
            batpak::store::StoreError::CursorCheckpointCorrupt { path, .. }
                if *path == expected_checkpoint_path
        ),
        "expected CursorCheckpointCorrupt from stop_and_join at {expected_checkpoint_path:?}, got {err:?}"
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("cursor worker must release its Arc before close");
    store.close().expect("close store after corrupt checkpoint");
    checkpoint_guard.assert_absent();
}
