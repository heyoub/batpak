// justifies: INV-TEST-PANIC-AS-ASSERTION, ADR-0007; this writer-pressure harness treats invariant violations as test failures; panic! is the assertion style throughout this file.
#![allow(clippy::panic)]
//! PROVES: the writer-pressure backpressure gate -- `try_submit` and
//! `try_submit_batch` return `Outcome::Retry` once the writer channel exceeds the
//! configured pressure threshold (50% of capacity 8 = 4 queued commands) under a
//! concurrent producer flood, rather than blocking or silently dropping work.
//! CATCHES: drift where the pressure threshold stops firing, where try_submit
//! variants block instead of yielding Retry, or where the gate never recovers.
//! SEEDED: a capacity-8 / 50%-threshold store flooded by 4 background producers.

use batpak::coordinate::Coordinate;
use batpak::store::{AppendOptions, BatchAppendItem, Store};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[path = "support/control_plane_surface.rs"]
mod cps_support;
use cps_support::{test_config, KIND_COUNTER};

#[test]
fn try_submit_returns_retry_under_pressure() {
    let dir = TempDir::new().expect("temp dir");
    let config = test_config(&dir)
        .with_writer_channel_capacity(8)
        .with_writer_pressure_retry_threshold_pct(50);

    let store = Arc::new(Store::open(config).expect("open store"));
    let coord = Coordinate::new("entity:pressure", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    // With channel_capacity=8 and threshold=50%, the pressure gate fires
    // when 4 or more commands are queued. We flood the channel from background
    // threads while the writer is busy syncing (sync_every_n_events=1 forces
    // an fsync per event, slowing the writer drain). On the main thread we
    // poll try_submit until we observe Outcome::Retry.
    let saw_retry = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));

    let handles: Vec<_> = (0..4u32)
        .map(|i| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("pressure-producer-{i}"))
                .spawn(move || {
                    let mut n = 0u32;
                    while !stop.load(Ordering::Relaxed) {
                        let _ = store.submit(&coord, kind, &serde_json::json!({"t": i, "n": n}));
                        n += 1;
                    }
                })
                .expect("spawn pressure producer")
        })
        .collect();

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match store.try_submit(&coord, kind, &serde_json::json!({"probe": true})) {
            Ok(outcome) if outcome.is_retry() => {
                saw_retry.store(true, Ordering::SeqCst);
                break;
            }
            _ => {}
        }
    }

    stop.store(true, Ordering::SeqCst);
    for h in handles {
        let _ = h.join();
    }

    assert!(
        saw_retry.load(Ordering::SeqCst),
        "PROPERTY: try_submit must return Outcome::Retry when the writer channel \
         exceeds the pressure threshold (50% of capacity 8 = 4 queued commands)."
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: producer threads should release the last Arc"),
    };
    store.close().expect("close store");
}

#[test]
fn try_submit_batch_returns_retry_under_pressure() {
    let dir = TempDir::new().expect("temp dir");
    let config = test_config(&dir)
        .with_writer_channel_capacity(8)
        .with_writer_pressure_retry_threshold_pct(50);

    let store = Arc::new(Store::open(config).expect("open store"));
    let coord = Coordinate::new("entity:pressure-batch", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let saw_retry = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));

    let handles: Vec<_> = (0..4u32)
        .map(|i| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("pressure-batch-producer-{i}"))
                .spawn(move || {
                    let mut n = 0u32;
                    while !stop.load(Ordering::Relaxed) {
                        let items = vec![BatchAppendItem::new(
                            coord.clone(),
                            kind,
                            &serde_json::json!({"t": i, "n": n}),
                            AppendOptions::new().with_idempotency(
                                batpak::id::IdempotencyKey::from(u128::from(
                                    ((i as u64) << 32) | u64::from(n) | 0xB000_0000,
                                )),
                            ),
                            batpak::store::CausationRef::None,
                        )
                        .expect("batch item")];
                        let _ = store.submit_batch(items);
                        n += 1;
                    }
                })
                .expect("spawn pressure producer")
        })
        .collect();

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let items = vec![BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"probe": true}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xCAFE_BA5E)),
            batpak::store::CausationRef::None,
        )
        .expect("batch probe item")];
        match store.try_submit_batch(items) {
            Ok(outcome) if outcome.is_retry() => {
                saw_retry.store(true, Ordering::SeqCst);
                break;
            }
            _ => {}
        }
    }

    stop.store(true, Ordering::SeqCst);
    for h in handles {
        let _ = h.join();
    }

    assert!(
        saw_retry.load(Ordering::SeqCst),
        "PROPERTY: try_submit_batch must return Outcome::Retry when the writer channel \
         exceeds the pressure threshold (50% of capacity 8 = 4 queued commands)."
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: producer threads should release the last Arc"),
    };
    store.close().expect("close store");
}
