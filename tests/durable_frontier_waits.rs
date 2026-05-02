// justifies: INV-TEST-PANIC-AS-ASSERTION; wait API tests use panic! through assert macros and explicit error extraction to pin blocking invariants.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]

//! PROVES:
//!   - `Store::wait_for_durable`, `Store::wait_for_applied`, and
//!     `Store::wait_for_visible` return only after observing their
//!     corresponding watermark `>= target`.
//!   - Timeouts are mandatory and surfaced as `StoreError::WaitTimeout`.
//!   - Spurious wakeups do not satisfy the wait, and writer panics poison
//!     waiters with `StoreError::WriterCrashed`.

use batpak::prelude::{Coordinate, EventKind, Region};
use batpak::store::{
    AppendOptions, BatchAppendItem, CausationRef, DurabilityGate, HlcPoint, Store, StoreConfig,
    StoreError, WatermarkKind,
};
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

fn batch_item(entity: &str, n: u32, options: AppendOptions) -> BatchAppendItem {
    BatchAppendItem::new(
        coord(entity),
        kind(),
        &serde_json::json!({ "n": n }),
        options,
        CausationRef::None,
    )
    .expect("batch item")
}

fn durable_gate(timeout: Duration) -> DurabilityGate {
    DurabilityGate {
        kind: WatermarkKind::Durable,
        timeout,
    }
}

fn applied_gate(timeout: Duration) -> DurabilityGate {
    DurabilityGate {
        kind: WatermarkKind::Applied,
        timeout,
    }
}

fn visible_gate(timeout: Duration) -> DurabilityGate {
    DurabilityGate {
        kind: WatermarkKind::Visible,
        timeout,
    }
}

fn assert_wait_timeout(result: Result<(), StoreError>, watermark: WatermarkKind, target: HlcPoint) {
    let err = match result {
        Ok(()) => panic!("PROPERTY: wait must not succeed before target reaches {watermark:?}"),
        Err(err) => err,
    };
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

#[test]
fn append_without_gate_returns_immediately() {
    let (_dir, store) = open_store(1000);

    let started = Instant::now();
    store
        .append_with_options(
            &coord("entity:gate:none"),
            kind(),
            &serde_json::json!({ "n": 1 }),
            AppendOptions::default(),
        )
        .expect("append without gate");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "PROPERTY: append without DurabilityGate must not wait for cadence sync"
    );
}

#[test]
fn append_with_durable_gate_blocks_until_synced() {
    let (_dir, store) = open_store(3);
    let store = Arc::new(store);

    let second_store = Arc::clone(&store);
    let second = std::thread::Builder::new()
        .name("durable-gate-second-append".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            second_store
                .append(
                    &coord("entity:gate:durable-second"),
                    kind(),
                    &serde_json::json!({ "n": 2 }),
                )
                .expect("second append triggers cadence sync");
        })
        .expect("spawn second append");

    let started = Instant::now();
    store
        .append_with_options(
            &coord("entity:gate:durable-first"),
            kind(),
            &serde_json::json!({ "n": 1 }),
            AppendOptions::default().with_gate(durable_gate(Duration::from_secs(2))),
        )
        .expect("durable gate satisfied after cadence sync");
    let elapsed = started.elapsed();
    second.join().expect("second append joins");
    assert!(
        elapsed >= Duration::from_millis(20),
        "PROPERTY: durable gate should block until a later sync advances durable_hlc"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "PROPERTY: durable gate should wake promptly once cadence sync fires"
    );
}

#[test]
fn append_with_applied_gate_blocks_until_min_projection_advances() {
    let (_dir, store) = open_store(1000);
    let store = Arc::new(store);
    store.dangerous_register_projection("frontier:gate:applied:a");
    store.dangerous_register_projection("frontier:gate:applied:b");

    let notifier_store = Arc::clone(&store);
    let notifier = std::thread::Builder::new()
        .name("applied-gate-projection-advance".into())
        .spawn(move || {
            let target = loop {
                let entries = notifier_store.query(&Region::entity("entity:gate:applied"));
                if let Some(entry) = entries.last() {
                    break point(entry);
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            notifier_store.dangerous_notify_projection_applied("frontier:gate:applied:a", target);
            std::thread::sleep(Duration::from_millis(50));
            notifier_store.dangerous_notify_projection_applied("frontier:gate:applied:b", target);
        })
        .expect("spawn projection notifier");

    let started = Instant::now();
    store
        .append_with_options(
            &coord("entity:gate:applied"),
            kind(),
            &serde_json::json!({ "n": 1 }),
            AppendOptions::default().with_gate(applied_gate(Duration::from_secs(2))),
        )
        .expect("applied gate satisfied after lagging projection advances");
    let elapsed = started.elapsed();
    notifier.join().expect("projection notifier joins");
    assert!(
        elapsed >= Duration::from_millis(20),
        "PROPERTY: applied gate must honor the min across registered projections"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "PROPERTY: applied gate should wake promptly after the lagging projection advances"
    );
}

#[test]
fn append_with_visible_gate_returns_after_publish() {
    let (_dir, store) = open_store(1000);

    let started = Instant::now();
    store
        .append_with_options(
            &coord("entity:gate:visible"),
            kind(),
            &serde_json::json!({ "n": 1 }),
            AppendOptions::default().with_gate(visible_gate(Duration::from_secs(1))),
        )
        .expect("visible gate satisfied by publish");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "PROPERTY: visible gate should return after publish even when durable cadence is not reached"
    );
}

#[test]
fn append_with_gate_surfaces_wait_timeout_when_unreachable() {
    let (_dir, store) = open_store(1000);
    let entity = "entity:gate:timeout";

    let result = store.append_with_options(
        &coord(entity),
        kind(),
        &serde_json::json!({ "n": 1 }),
        AppendOptions::default().with_gate(durable_gate(Duration::from_millis(100))),
    );
    let err = match result {
        Ok(_) => panic!("PROPERTY: unreachable durable gate must not return a receipt"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            StoreError::WaitTimeout {
                watermark: WatermarkKind::Durable,
                ..
            }
        ),
        "PROPERTY: durable gate timeout must surface WaitTimeout, got {err:?}"
    );
    assert_eq!(
        store.query(&Region::entity(entity)).len(),
        1,
        "PROPERTY: gate timeout reflects the guarantee, not the commit; the event remains queryable"
    );
}

#[test]
fn batch_append_with_durable_gate_covers_entire_batch() {
    let (_dir, store) = open_store(10000);
    let entity = "entity:gate:batch-durable";
    let items: Vec<_> = (0..5)
        .map(|n| batch_item(entity, n, AppendOptions::default()))
        .collect();

    let receipts = store
        .append_batch_with_options(
            items,
            AppendOptions::default().with_gate(durable_gate(Duration::from_secs(2))),
        )
        .expect("batch durable gate");
    assert_eq!(receipts.len(), 5);
    let durable_hlc = store.dangerous_watermark_snapshot().durable_hlc;
    for entry in store.query(&Region::entity(entity)) {
        assert!(
            durable_hlc >= point(&entry),
            "PROPERTY: durable gate on the last batch item must cover every prior batch item"
        );
    }
}

#[test]
fn batch_per_item_gate_ignored() {
    let (_dir, store) = open_store(1000);
    let item = batch_item(
        "entity:gate:batch-item-ignored",
        1,
        AppendOptions::default().with_gate(durable_gate(Duration::from_millis(100))),
    );

    #[cfg(debug_assertions)]
    {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.append_batch(vec![item])
        }));
        assert!(
            result.is_err(),
            "PROPERTY: debug builds should catch ignored per-item batch gates with debug_assert"
        );
    }

    #[cfg(not(debug_assertions))]
    {
        let started = Instant::now();
        store
            .append_batch(vec![item])
            .expect("per-item gate ignored");
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "PROPERTY: release builds silently ignore per-item batch gates"
        );
    }
}
