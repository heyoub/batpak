#![cfg(feature = "dangerous-test-hooks")]
//! PROVES: INV-FRONTIER-WAIT-MONOTONIC. `Store::wait_for_durable`,
//! `Store::wait_for_applied`, and `Store::wait_for_visible` return only after
//! observing their corresponding watermark `>= target`. Timeouts are mandatory
//! and surfaced as `StoreError::WaitTimeout`.
//! CATCHES: a wait returning before its watermark reaches target, a spurious
//! wakeup satisfying a wait, a missing-timeout regression, or a genuine writer
//! crash failing to poison durable waiters with `WriterCrashed`.
//! SEEDED: deterministic single-event/projection watermark advances driven from
//! background threads; the R3 writer-crash case seeds a terminal restart policy
//! so the panic is non-transient.

use batpak_testkit::durable_frontier_waits as dfw_support;

use batpak::prelude::Region;
use batpak::store::{HlcPoint, RestartPolicy, Store, StoreConfig, StoreError, WatermarkKind};
use dfw_support::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Wait-family helper: append one event and return its HLC point. Lives inline
/// in this binary because only the wait surfaces exercise it (the append-gate
/// family appends through the gated `append_with_options`/batch paths).
fn append_number(store: &Store, entity: &str, n: u32) -> HlcPoint {
    let _ = store
        .append(&coord(entity), kind(), &serde_json::json!({ "n": n }))
        .expect("append wait event");
    let entries = store.query(&Region::entity(entity));
    point(entries.last().expect("appended event visible in query"))
}

/// Open a store whose writer treats any panic as terminal (zero restart
/// budget). Under the default `RestartPolicy::Once`, a single panic is a
/// *within-budget transient* — the writer restarts and durable waiters are NOT
/// poisoned (that is the R3 fix). To prove that a genuine writer CRASH surfaces
/// to durable waiters as `WriterCrashed`, the panic must be terminal, which a
/// zero-budget `Bounded` policy guarantees on the first panic.
///
/// Kept inline here (not in shared support) because only the writer-crash case
/// in this binary uses the terminal restart policy.
fn open_store_terminal_on_panic(sync_every_n_events: u32) -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_sync_every_n_events(sync_every_n_events)
            .with_restart_policy(RestartPolicy::Bounded {
                max_restarts: 0,
                within_ms: 0,
            }),
    )
    .expect("open store");
    (dir, store)
}

/// Wait-family helper: assert a wait result is the expected `WaitTimeout`.
fn assert_wait_timeout(result: Result<(), StoreError>, watermark: WatermarkKind, target: HlcPoint) {
    let err =
        result.expect_err("PROPERTY: wait must not succeed before target reaches the watermark");
    assert!(
        matches!(
            err,
            StoreError::WaitTimeout {
                watermark: actual_watermark,
                target: actual,
                ..
            } if actual_watermark == watermark && actual == target
        ),
        "PROPERTY: expected {watermark:?} WaitTimeout for {target:?}, got {err:?}"
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
    assert_wait_timeout(result, WatermarkKind::Durable, target);
    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "PROPERTY: timeout wait must not return immediately"
    );
}

#[test]
fn wait_for_durable_surfaces_writer_crash() {
    // Terminal-on-panic policy: the single panic below must poison waiters.
    // Under the default Once policy this panic is a within-budget transient
    // (writer restarts, no poison), so the waiter would time out instead.
    let (_dir, store) = open_store_terminal_on_panic(1000);
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
    assert_wait_timeout(result, WatermarkKind::Durable, target);
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
    assert_wait_timeout(
        store.wait_for_durable(target, Duration::ZERO),
        WatermarkKind::Durable,
        target,
    );

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

#[test]
fn wait_for_applied_returns_immediately_when_already_past() {
    let (_dir, store) = open_store(1000);
    store.dangerous_register_projection("frontier:applied:immediate");
    let target = append_number(&store, "entity:wait:applied-immediate", 1);
    store.sync().expect("sync target");
    store.dangerous_notify_projection_applied("frontier:applied:immediate", target);

    let started = Instant::now();
    store
        .wait_for_applied(target, Duration::from_secs(1))
        .expect("already applied wait");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "PROPERTY: already-past applied wait must return promptly"
    );
}

#[test]
fn wait_for_applied_returns_min_across_projections() {
    let (_dir, store) = open_store(1000);
    store.dangerous_register_projection("frontier:applied:min:a");
    store.dangerous_register_projection("frontier:applied:min:b");
    let lagging_point = store.dangerous_watermark_snapshot().applied_hlc;
    let target = append_number(&store, "entity:wait:applied-min", 1);
    store.dangerous_notify_projection_applied("frontier:applied:min:a", target);

    let result = store.wait_for_applied(target, Duration::from_millis(200));
    assert_wait_timeout(result, WatermarkKind::Applied, target);
    assert_eq!(
        store.dangerous_watermark_snapshot().applied_hlc,
        lagging_point,
        "PROPERTY: applied_hlc is the min across projections, so one lagging projection blocks wait_for_applied"
    );
}

#[test]
fn wait_for_applied_blocks_until_lagging_projection_advances() {
    let (_dir, store) = open_store(1000);
    let store = Arc::new(store);
    store.dangerous_register_projection("frontier:applied:block:a");
    store.dangerous_register_projection("frontier:applied:block:b");
    let target = append_number(&store, "entity:wait:applied-block", 1);
    store.dangerous_notify_projection_applied("frontier:applied:block:a", target);

    let notify_store = Arc::clone(&store);
    let notifier = std::thread::Builder::new()
        .name("wait-for-applied-lagging-projection".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            notify_store.dangerous_notify_projection_applied("frontier:applied:block:b", target);
        })
        .expect("spawn applied notifier");

    let started = Instant::now();
    store
        .wait_for_applied(target, Duration::from_secs(1))
        .expect("applied wait returns after lagging projection advances");
    let elapsed = started.elapsed();
    notifier.join().expect("applied notifier joins");
    assert!(
        elapsed >= Duration::from_millis(20),
        "PROPERTY: wait_for_applied must block while any registered projection lags"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "PROPERTY: wait_for_applied should wake from the applied watermark notification"
    );
}

#[test]
fn wait_for_visible_returns_immediately_when_already_past() {
    let (_dir, store) = open_store(1000);
    let target = append_number(&store, "entity:wait:visible-immediate", 1);

    let started = Instant::now();
    store
        .wait_for_visible(target, Duration::from_secs(1))
        .expect("already visible wait");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "PROPERTY: already-past visible wait must return promptly"
    );
}

#[test]
fn wait_for_visible_advances_under_cadence_gt_1_without_durable() {
    let (_dir, store) = open_store(1000);
    let target = append_number(&store, "entity:wait:visible-before-durable", 1);

    store
        .wait_for_visible(target, Duration::from_millis(200))
        .expect("visible advances after publish even without cadence sync");
    assert_wait_timeout(
        store.wait_for_durable(target, Duration::from_millis(200)),
        WatermarkKind::Durable,
        target,
    );
}

#[test]
fn mixed_wait_for_durable_applied_visible_converge_in_order() {
    let (_dir, store) = open_store(1000);
    let store = Arc::new(store);
    store.dangerous_register_projection("frontier:mixed:applied");
    let target = append_number(&store, "entity:wait:mixed", 1);
    store.sync().expect("sync target");
    store.dangerous_notify_projection_applied("frontier:mixed:applied", target);

    let visible_store = Arc::clone(&store);
    let visible = std::thread::Builder::new()
        .name("wait-visible-mixed".into())
        .spawn(move || visible_store.wait_for_visible(target, Duration::from_secs(2)))
        .expect("spawn visible waiter");
    let durable_store = Arc::clone(&store);
    let durable = std::thread::Builder::new()
        .name("wait-durable-mixed".into())
        .spawn(move || durable_store.wait_for_durable(target, Duration::from_secs(2)))
        .expect("spawn durable waiter");
    let applied_store = Arc::clone(&store);
    let applied = std::thread::Builder::new()
        .name("wait-applied-mixed".into())
        .spawn(move || applied_store.wait_for_applied(target, Duration::from_secs(2)))
        .expect("spawn applied waiter");

    visible
        .join()
        .expect("visible waiter joins")
        .expect("visible waiter succeeds");
    durable
        .join()
        .expect("durable waiter joins")
        .expect("durable waiter succeeds");
    applied
        .join()
        .expect("applied waiter joins")
        .expect("applied waiter succeeds");
}
