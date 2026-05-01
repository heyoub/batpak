// justifies: INV-TEST-PANIC-AS-ASSERTION; wait API tests use panic! through assert macros and explicit error extraction to pin blocking invariants.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]

//! PROVES:
//!   - `Store::wait_for_durable` returns only after observing
//!     `durable_hlc >= target`.
//!   - Timeouts are mandatory and surfaced as `StoreError::WaitTimeout`.
//!   - Spurious wakeups do not satisfy the wait, and writer panics poison
//!     waiters with `StoreError::WriterCrashed`.

use batpak::prelude::{Coordinate, EventKind, Region};
use batpak::store::{HlcPoint, Store, StoreConfig, StoreError, WatermarkKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const WAIT_SCOPE: &str = "scope:frontier-waits";

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, WAIT_SCOPE).expect("valid wait coordinate")
}

fn kind() -> EventKind {
    EventKind::custom(0xF, 0x94)
}

fn point(entry: &batpak::store::IndexEntry) -> HlcPoint {
    HlcPoint {
        wall_ms: entry.wall_ms,
        global_sequence: entry.global_sequence,
    }
}

fn open_store(sync_every_n_events: u32) -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store =
        Store::open(StoreConfig::new(dir.path()).with_sync_every_n_events(sync_every_n_events))
            .expect("open store");
    (dir, store)
}

fn append_number(store: &Store, entity: &str, n: u32) -> HlcPoint {
    store
        .append(&coord(entity), kind(), &serde_json::json!({ "n": n }))
        .expect("append wait event");
    let entries = store.query(&Region::entity(entity));
    point(entries.last().expect("appended event visible in query"))
}

fn assert_wait_timeout(result: Result<(), StoreError>, target: HlcPoint) {
    let err = match result {
        Ok(()) => panic!("PROPERTY: wait must not succeed before target is durable"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            StoreError::WaitTimeout {
                watermark: WatermarkKind::Durable,
                target: actual,
                ..
            } if actual == target
        ),
        "PROPERTY: expected durable WaitTimeout for {target:?}, got {err:?}"
    );
}

#[test]
fn wait_for_durable_returns_immediately_when_already_past() {
    let (_dir, store) = open_store(1000);
    let target = append_number(&store, "entity:wait:immediate", 1);
    store.sync().expect("sync target");

    let started = Instant::now();
    store
        .wait_for_durable(target, Duration::from_secs(1))
        .expect("already durable wait");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "PROPERTY: already-past durable wait must return promptly"
    );
}

#[test]
fn wait_for_durable_blocks_then_returns_after_advance() {
    let (_dir, store) = open_store(1000);
    let store = Arc::new(store);
    let target = append_number(&store, "entity:wait:block", 1);

    let sync_store = Arc::clone(&store);
    let sync_thread = std::thread::Builder::new()
        .name("wait-for-durable-sync".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            sync_store.sync().expect("sync from background thread");
        })
        .expect("spawn sync thread");

    let started = Instant::now();
    store
        .wait_for_durable(target, Duration::from_secs(1))
        .expect("wait returns after sync");
    let elapsed = started.elapsed();
    sync_thread.join().expect("sync thread joins");
    assert!(
        elapsed >= Duration::from_millis(20),
        "PROPERTY: wait should block until the background sync advances durable_hlc"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "PROPERTY: wait should be woken by the watermark notification, not by timeout"
    );
}

#[test]
fn wait_for_durable_returns_timeout_when_target_unreachable() {
    let (_dir, store) = open_store(1000);
    let target = append_number(&store, "entity:wait:timeout", 1);

    let started = Instant::now();
    let result = store.wait_for_durable(target, Duration::from_millis(100));
    assert_wait_timeout(result, target);
    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "PROPERTY: timeout wait must not return immediately"
    );
}

#[test]
fn wait_for_durable_surfaces_writer_crash() {
    let (_dir, store) = open_store(1000);
    let store = Arc::new(store);
    let target = HlcPoint {
        wall_ms: u64::MAX,
        global_sequence: u64::MAX,
    };

    let waiter_store = Arc::clone(&store);
    let waiter = std::thread::Builder::new()
        .name("wait-for-durable-poison".into())
        .spawn(move || waiter_store.wait_for_durable(target, Duration::from_secs(5)))
        .expect("spawn waiter thread");

    std::thread::sleep(Duration::from_millis(50));
    store.panic_writer_for_test().expect("trigger writer panic");
    let result = waiter.join().expect("waiter joins");
    assert!(
        matches!(result, Err(StoreError::WriterCrashed)),
        "PROPERTY: writer panic must poison durable waiters, got {result:?}"
    );
}

#[test]
fn wait_for_durable_spurious_wakeup_safe() {
    let (_dir, store) = open_store(1000);
    let store = Arc::new(store);
    let target = append_number(&store, "entity:wait:spurious", 1);
    let stop = Arc::new(AtomicBool::new(false));

    let notifier_store = Arc::clone(&store);
    let notifier_stop = Arc::clone(&stop);
    let notifier = std::thread::Builder::new()
        .name("wait-for-durable-spurious".into())
        .spawn(move || {
            while !notifier_stop.load(Ordering::Acquire) {
                notifier_store.dangerous_notify_watermark_waiters();
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .expect("spawn notifier thread");

    let result = store.wait_for_durable(target, Duration::from_millis(100));
    stop.store(true, Ordering::Release);
    notifier.join().expect("notifier joins");
    assert_wait_timeout(result, target);
}

#[test]
fn wait_for_durable_mandatory_timeout_compiles_only_with_duration() {
    fn accepts_duration_signature(_: fn(&Store, HlcPoint, Duration) -> Result<(), StoreError>) {}

    accepts_duration_signature(Store::wait_for_durable);
}

#[test]
fn wait_for_durable_zero_timeout_observes_current_state() {
    let (_dir, store) = open_store(1000);
    let target = append_number(&store, "entity:wait:zero-timeout", 1);
    assert_wait_timeout(store.wait_for_durable(target, Duration::ZERO), target);

    store.sync().expect("sync target");
    store
        .wait_for_durable(target, Duration::ZERO)
        .expect("zero-timeout wait succeeds when already durable");
}

#[test]
fn wait_for_durable_origin_returns_immediately() {
    let (_dir, store) = open_store(1000);
    store
        .wait_for_durable(HlcPoint::ORIGIN, Duration::ZERO)
        .expect("origin is always durable");
}
