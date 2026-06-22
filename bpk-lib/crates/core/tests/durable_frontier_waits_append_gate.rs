#![cfg(feature = "dangerous-test-hooks")]
//! PROVES: INV-FRONTIER-APPEND-GATE-HONORED. `append_with_options` returns only
//! after the requested `DurabilityGate` watermark covers the appended event, and
//! a batch durable gate covers every item in the batch. An ungated append never
//! blocks on cadence sync; an unreachable gate surfaces `StoreError::WaitTimeout`
//! while leaving the event queryable.
//! CATCHES: an append returning before its gate watermark is satisfied, a batch
//! gate that fails to cover prior items, a per-item batch gate silently honored,
//! or a gate timeout that masks the appended event.
//! SEEDED: deterministic single/batch appends with explicit durable/applied and
//! visible gates, advanced from background threads.

use batpak_testkit::durable_frontier_waits as dfw_support;

use batpak::prelude::Region;
use batpak::store::{
    AppendOptions, BatchAppendItem, CausationRef, DurabilityGate, StoreError, WatermarkKind,
};
use dfw_support::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Gate-family helper: build a batch item for the given entity/payload/options.
/// Inline here because only the append-gate batch cases construct batch items.
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
    let (_dir, store) = open_store(1000);
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
                .expect("second append before explicit sync");
            second_store.sync().expect("explicit sync advances durable");
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
        .expect("durable gate satisfied after explicit sync");
    let elapsed = started.elapsed();
    second.join().expect("second append joins");
    assert!(
        elapsed >= Duration::from_millis(20),
        "PROPERTY: durable gate should block until an explicit sync advances durable_hlc"
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
    let err = result
        .map(|_| ())
        .expect_err("PROPERTY: unreachable durable gate must not return a receipt");
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
