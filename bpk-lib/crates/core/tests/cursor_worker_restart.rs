use batpak::store::delivery::cursor::{CursorWorkerAction, CursorWorkerConfig};
use batpak::store::{RestartPolicy, Store, StoreConfig};
use batpak_testkit::prelude::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn test_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_restart_policy(RestartPolicy::Bounded {
            max_restarts: 2,
            within_ms: 5_000,
        })
        .with_sync_every_n_events(1)
}

#[test]
fn cursor_worker_restarts_from_last_committed_checkpoint_after_panic() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(test_config(&dir)).expect("open store"));
    let coord = Coordinate::new("entity:cursor-worker", "scope:restart").expect("coord");
    let kind = EventKind::custom(0xF, 7);

    for n in 0..3u32 {
        store
            .append(&coord, kind, &serde_json::json!({"n": n}))
            .expect("append seed event");
    }

    let seen = Arc::new(Mutex::new(BTreeMap::<u64, usize>::new()));
    let panic_once = Arc::new(AtomicBool::new(true));
    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Bounded {
        max_restarts: 2,
        within_ms: 5_000,
    };

    let worker = store
        .cursor_worker(&Region::entity("entity:cursor-worker"), worker_config, {
            let seen = Arc::clone(&seen);
            let panic_once = Arc::clone(&panic_once);
            move |batch, _store, _witness| {
                let seq = batch[0].global_sequence();
                let mut counts = seen.lock().expect("counts mutex");
                *counts.entry(seq).or_insert(0) += 1;
                drop(counts);

                if seq == 2 && panic_once.swap(false, Ordering::SeqCst) {
                    // Deliberate handler panic to drive the restart path;
                    // black_box keeps the assert clippy-clean.
                    assert!(
                        std::hint::black_box(false),
                        "intentional cursor worker panic after first checkpoint"
                    );
                }

                if seq == 3 {
                    CursorWorkerAction::Stop
                } else {
                    CursorWorkerAction::Continue
                }
            }
        })
        .expect("spawn worker");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = seen.lock().expect("counts mutex").clone();
        if snapshot.get(&1) == Some(&1)
            && snapshot.get(&2) == Some(&2)
            && snapshot.get(&3) == Some(&1)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "PROPERTY: cursor worker must restart from the last committed checkpoint after panic.\n\
             Expected sequence counts {{1:1, 2:2, 3:1}}, got {snapshot:?}."
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    worker.stop_and_join().expect("stop and join worker");

    let snapshot = seen.lock().expect("counts mutex").clone();
    assert_eq!(
        snapshot.get(&1),
        Some(&1),
        "first committed batch should not be replayed after restart"
    );
    assert_eq!(
        snapshot.get(&2),
        Some(&2),
        "failed batch must be retried exactly once after restart"
    );
    assert_eq!(
        snapshot.get(&3),
        Some(&1),
        "subsequent batch should run once after restart recovery"
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("PROPERTY: cursor worker should release the last Arc");
    store.close().expect("close store");
}

#[test]
fn cursor_worker_exits_cleanly_when_restart_budget_exhausted() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(
        Store::open(
            StoreConfig::new(dir.path())
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false)
                .with_restart_policy(RestartPolicy::Bounded {
                    max_restarts: 1,
                    within_ms: 5_000,
                })
                .with_sync_every_n_events(1),
        )
        .expect("open store"),
    );
    let coord = Coordinate::new("entity:budget-exhausted", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 7);

    // Seed 3 events so the worker has something to process.
    for n in 0..3u32 {
        store
            .append(&coord, kind, &serde_json::json!({"n": n}))
            .expect("append seed event");
    }

    // Handler always panics — no panic_once guard.
    // With max_restarts=1 the worker should:
    //   attempt 0 → process batch → panic → restart (1 restart used)
    //   attempt 1 → process batch → panic → budget exhausted → exit
    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Bounded {
        max_restarts: 1,
        within_ms: 5_000,
    };
    let worker = store
        .cursor_worker(
            &Region::entity("entity:budget-exhausted"),
            worker_config,
            |_batch, _store, _witness| {
                // Always panics to exhaust the restart budget; black_box keeps
                // the assert clippy-clean.
                assert!(
                    std::hint::black_box(false),
                    "intentional panic to exhaust restart budget"
                );
                CursorWorkerAction::Stop
            },
        )
        .expect("spawn worker");

    // Worker must exit once the restart budget is exhausted.
    // join() must complete (not hang).
    worker
        .stop_and_join()
        .expect("stop and join worker after budget exhaustion");

    // The store must remain usable after the worker exits.
    let receipt = store
        .append(&coord, kind, &serde_json::json!({"after": true}))
        .expect("append after worker exit");
    assert!(
        receipt.sequence >= 3,
        "PROPERTY: store must remain usable after cursor worker exhausts its restart budget. \
         Expected sequence >= 3, got {}.",
        receipt.sequence
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("PROPERTY: cursor worker should release the last Arc after budget exhaustion");
    store.close().expect("close store");
}

#[test]
fn bounded_restart_window_resets_after_elapsed_window() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(test_config(&dir)).expect("open store"));
    let coord = Coordinate::new("entity:restart-window", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 7);
    store
        .append(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("append seed event");

    let attempts = Arc::new(AtomicUsize::new(0));
    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(1);
    worker_config.restart = RestartPolicy::Bounded {
        max_restarts: 1,
        within_ms: 50,
    };

    let worker = store
        .cursor_worker(&Region::entity("entity:restart-window"), worker_config, {
            let attempts = Arc::clone(&attempts);
            move |_batch, _store, _witness| {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 2 {
                    std::thread::sleep(Duration::from_millis(75));
                }
                if attempt <= 2 {
                    // Deliberate handler panic to exercise the bounded restart
                    // window reset; black_box keeps the assert clippy-clean.
                    assert!(
                        std::hint::black_box(false),
                        "intentional panic to exercise bounded restart window reset"
                    );
                }
                CursorWorkerAction::Stop
            }
        })
        .expect("spawn worker");

    worker
        .join()
        .expect("bounded restart window should reset and allow recovery");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "PROPERTY: Bounded restart policy must reset its counter once within_ms elapses"
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "shared")
        .expect("PROPERTY: cursor worker should release the last Arc");
    store.close().expect("close store");
}
