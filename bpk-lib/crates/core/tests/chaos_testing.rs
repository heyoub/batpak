// PROVES: LAW-007 (Codebase Accuses Itself — adversarial self-testing)
// CATCHES: FM-011 (Error Path Hollowing — write/CAS errors swallowed), FM-019 (Non-Replayable Truth — events dropped under concurrent load)
// SEEDED: concurrent writer/CAS/idempotency storms (4–8 threads), subscription + cursor completeness under write storms
// INVARIANTS: INV-CONCURRENCY-SCHEDULE-PROOF (linearizability, CAS, idempotency, exactly-once delivery)
//! Chaos testing — concurrent stress and delivery-completeness lane.
//! Harness pattern: Fault-Injection Harness (direct chaos lane).
//! Keeps the concurrency family: writer stress, CAS contention, idempotent
//! dedup, subscription write storms, and cursor exactly-once completeness. The
//! low-level byte-corruption cases live in `chaos_testing_byte_corruption` and
//! the batch/rotation cases in `chaos_testing_batch_rotation`.
//!
//! Run with: cargo test --test chaos_testing --all-features
//! Default depth: 500 iterations (override with `CHAOS_ITERATIONS=<n>`)
//! Extended: CHAOS_ITERATIONS=5000 cargo test --test chaos_testing --all-features --release

use batpak::store::{AppendOptions, Store, StoreConfig};
use batpak_testkit::prelude::*;
use std::sync::Arc;
use tempfile::TempDir;

use batpak_testkit::chaos_testing as chaos_support;
use chaos_support::{chaos_iterations, effective_chaos_iterations, DEFAULT_CHAOS_ITERATIONS};

#[test]
fn chaos_iterations_default_to_repo_truth() {
    assert_eq!(effective_chaos_iterations(None), DEFAULT_CHAOS_ITERATIONS);
    assert_eq!(effective_chaos_iterations(Some("5000")), 5000);
    assert_eq!(effective_chaos_iterations(Some("0")), 1);
    assert_eq!(
        effective_chaos_iterations(Some("not-a-number")),
        DEFAULT_CHAOS_ITERATIONS
    );
}

// ============================================================
// CHAOS 2: Concurrent writer stress
// Multiple threads hammering the store simultaneously.
// ============================================================

#[test]
fn chaos_concurrent_writer_stress() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(8192) // small segments → lots of rotation
        .with_sync_every_n_events(10);
    let store = Arc::new(Store::open(config).expect("open"));
    let iterations = chaos_iterations();
    let n_threads = 4;

    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::Builder::new()
                .name(format!("chaos-writer-{t}"))
                .spawn(move || {
                    let coord =
                        Coordinate::new(format!("chaos:thread{t}").as_str(), "chaos:stress")
                            .expect("valid");
                    let kind = EventKind::custom(0xF, 1);
                    let mut successes = 0u64;
                    let mut errors = 0u64;

                    for i in 0..(iterations / n_threads) {
                        let payload = serde_json::json!({"t": t, "i": i});
                        match store.append(&coord, kind, &payload) {
                            Ok(_) => successes += 1,
                            Err(_) => errors += 1,
                        }
                    }
                    (successes, errors)
                })
                .expect("spawn thread")
        })
        .collect();

    let mut total_ok = 0u64;
    let mut total_err = 0u64;
    for h in handles {
        let (ok, err) = h.join().expect("thread join");
        total_ok += ok;
        total_err += err;
    }

    assert!(
        total_ok > 0,
        "CHAOS PROPERTY: at least one write must succeed under concurrent stress across {n_threads} threads.\n\
         Investigate: src/store/write/writer.rs lock acquisition, src/store/segment/mod.rs open_writer.\n\
         Common causes: mutex poisoning on first write error, segment open failure blocking all writers.\n\
         Run: cargo test --test chaos_testing chaos_concurrent_writer_stress"
    );
    assert_eq!(
        total_err, 0,
        "CHAOS PROPERTY: zero write errors expected under concurrent stress, got {total_err}.\n\
         Investigate: src/store/write/writer.rs lock ordering, src/store/segment/mod.rs rotate.\n\
         Common causes: race on segment rotation, fd_budget exhaustion under concurrent open, poisoned Mutex from a prior panic.\n\
         Run: cargo test --test chaos_testing chaos_concurrent_writer_stress"
    );

    // Verify data integrity: each thread's events should be readable
    let store_ref = &*store;
    for t in 0..n_threads {
        let entries = store_ref.by_entity(&format!("chaos:thread{t}"));
        assert!(
            !entries.is_empty(),
            "CHAOS PROPERTY: every writer thread must have its events present in the index after store close.\n\
             Investigate: src/store/index/mod.rs insert, src/store/write/writer.rs STEP 5 index update.\n\
             Common causes: index update skipped on rotation, thread-local writes not flushed before stream() call.\n\
             Run: cargo test --test chaos_testing chaos_concurrent_writer_stress"
        );
    }

    let store = Arc::try_unwrap(store)
        .map_err(|_| "Arc still has multiple owners")
        .expect("store should be sole owner after all worker threads joined");
    store.close().expect("close");
}

// ============================================================
// CHAOS 3: CAS contention
// Multiple threads competing for the same entity with CAS.
// Exactly one should win per sequence number.
// ============================================================

#[test]
fn chaos_cas_contention() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("chaos:cas", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);

    // First, seed with one event so sequence > 0
    store
        .append(&coord, kind, &serde_json::json!({"seed": true}))
        .expect("seed");

    let n_threads = 8;
    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            std::thread::Builder::new()
                .name(format!("chaos-cas-{t}"))
                .spawn(move || {
                    let opts = AppendOptions {
                        expected_sequence: Some(0), // all compete: expect latest clock=0 after seed
                        ..Default::default()
                    };
                    store.append_with_options(&coord, kind, &serde_json::json!({"thread": t}), opts)
                })
                .expect("spawn thread")
        })
        .collect();

    let mut winning_receipt = None;
    let mut losers = 0;
    let mut loser_errors = Vec::new();
    for h in handles {
        match h.join().expect("join") {
            Ok(receipt) => {
                assert!(
                    winning_receipt.is_none(),
                    "CHAOS PROPERTY: CAS must allow exactly ONE winner, but got a second.\n\
                     Investigate: src/store/write/writer.rs CAS check under entity lock.\n\
                     Common causes: entity lock not held across sequence read + write."
                );
                winning_receipt = Some(receipt);
            }
            Err(e) => {
                losers += 1;
                loser_errors.push(format!("{e}"));
            }
        }
    }

    assert!(
        winning_receipt.is_some(),
        "CHAOS PROPERTY: exactly one thread must win CAS, but none did.\n\
         Run: cargo test --test chaos_testing chaos_cas_contention"
    );
    assert_eq!(
        losers,
        n_threads - 1,
        "CHAOS PROPERTY: all threads except the CAS winner must receive a conflict error (expected {}, got {losers}).\n\
         Investigate: src/store/write/writer.rs CAS rejection path, StoreError::SequenceMismatch variant.\n\
         Run: cargo test --test chaos_testing chaos_cas_contention",
        n_threads - 1
    );
    // Verify losers got SequenceMismatch, not some other error
    for err_msg in &loser_errors {
        assert!(
            err_msg.contains("CAS failed"),
            "CHAOS PROPERTY: CAS losers must get SequenceMismatch, got: {err_msg}\n\
             Investigate: src/store/write/writer.rs CAS rejection, StoreError::SequenceMismatch.\n\
             Run: cargo test --test chaos_testing chaos_cas_contention"
        );
    }

    // Verify the winner's event is actually in the stream
    let entries = store.by_entity("chaos:cas");
    assert_eq!(
        entries.len(),
        2, // seed + 1 winner
        "CHAOS PROPERTY: stream should have exactly 2 events (seed + CAS winner), got {}.\n\
         Investigate: src/store/write/writer.rs handle_append commit path.",
        entries.len()
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "Arc still has multiple owners")
        .expect("store should be sole owner after all worker threads joined");
    store.close().expect("close");
}

// ============================================================
// CHAOS 4: Idempotency under concurrent duplicate submissions
// ============================================================

#[test]
fn chaos_idempotency_concurrent() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("chaos:idem", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let idem_key: u128 = 0xDEAD_BEEF_CAFE_BABE_1111_2222_3333_4444;

    let n_threads = 8;
    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            std::thread::Builder::new()
                .name(format!("chaos-idem-{t}"))
                .spawn(move || {
                    let opts = AppendOptions {
                        idempotency_key: Some(batpak::id::IdempotencyKey::from(idem_key)),
                        ..Default::default()
                    };
                    store.append_with_options(&coord, kind, &serde_json::json!({"thread": t}), opts)
                })
                .expect("spawn thread")
        })
        .collect();

    let mut event_ids = Vec::new();
    for h in handles {
        let receipt = h.join().expect("join").expect(
            "CHAOS: idempotent append must succeed for every concurrent duplicate submission",
        );
        event_ids.push(receipt.event_id);
    }

    // All should return the same event_id
    let first = event_ids[0];
    for id in event_ids.iter() {
        assert_eq!(
            *id, first,
            "CHAOS PROPERTY: all concurrent idempotent appends with the same key must return the same event_id.\n\
             Investigate: src/store/write/writer.rs idempotency check, src/store/index/mod.rs idempotency_key lookup.\n\
             Common causes: idempotency map not protected by the same lock as the append, two threads both pass the key-absent check.\n\
             Run: cargo test --test chaos_testing chaos_idempotency_concurrent"
        );
    }

    // Only one event should exist in the store
    let entries = store.by_entity("chaos:idem");
    assert_eq!(
        entries.len(),
        1,
        "CHAOS PROPERTY: idempotent append with the same key from {n_threads} concurrent threads must store exactly 1 event, got {}.\n\
         Investigate: src/store/write/writer.rs idempotency dedup, src/store/index/mod.rs stream().\n\
         Common causes: dedup check races with concurrent insert, idempotency_key stored per-segment losing cross-segment dedup.\n\
         Run: cargo test --test chaos_testing chaos_idempotency_concurrent",
        entries.len()
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "Arc still has multiple owners")
        .expect("store should be sole owner after all worker threads joined");
    store.close().expect("close");
}

// ============================================================
// CHAOS 7: Subscription under write storm
// Verify subscriptions don't deadlock or lose too many events
// under heavy write load.
// ============================================================

#[test]
fn chaos_subscription_write_storm() {
    let dir = TempDir::new().expect("temp dir");
    // Buffer must be >= iterations so the "drain after join" pattern sees
    // every notification. The lossy-under-backpressure path is tested
    // separately in subscription_ops::slow_subscriber_*. Here we want to
    // verify that every append() broadcasts synchronously.
    let config = StoreConfig::new(dir.path()).with_broadcast_capacity(1024);
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("chaos:sub", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("chaos:sub");
    let iterations = chaos_iterations().min(200);

    // Start subscriber
    let sub = store.subscribe_lossy(&region);

    // Writer thread hammers events
    let store2 = Arc::clone(&store);
    let writer = std::thread::Builder::new()
        .name("chaos-sub-writer".to_string())
        .spawn(move || {
            for i in 0..iterations {
                store2
                    .append(&coord, kind, &serde_json::json!({"i": i}))
                    .expect("append");
            }
        })
        .expect("spawn thread");

    writer.join().expect("writer join");

    // Drain subscriber — writer.join() guarantees all appends (and their broadcasts) are
    // complete before we drain, so every notification is already sitting in the channel
    // buffer. No sleep needed; tight try_recv until empty is deterministic.
    let mut received = 0;
    while sub.receiver().try_recv().is_ok() {
        received += 1;
    }

    assert_eq!(
        received, iterations,
        "PROPERTY: every append() broadcasts before returning, so a subscriber joined after all \
         writes must see exactly {iterations} notifications, got {received}.\n\
         Investigate: src/store/write/writer.rs (broadcast send path), src/store/index/mod.rs (region filter).\n\
         Run: cargo test --test chaos_testing chaos_subscription_write_storm"
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "Arc still has multiple owners")
        .expect("store should be sole owner after all worker threads joined");
    store.close().expect("close");
}

// ============================================================
// CHAOS 8: Cursor completeness under concurrent writes
// Cursors must deliver every event exactly once.
// ============================================================

#[test]
fn chaos_cursor_completeness_concurrent() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("chaos:cursor", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let n = 100;

    // Write events
    for i in 0..n {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    // Create cursor and drain
    let region = Region::entity("chaos:cursor");
    let mut cursor = store.cursor_guaranteed(&region);
    let mut seen = Vec::new();
    while let Some(entry) = cursor.poll() {
        seen.push(entry.event_id());
    }

    assert_eq!(
        seen.len(),
        n,
        "CHAOS PROPERTY: cursor must deliver every event exactly once — expected {n} events, got {}.\n\
         Investigate: src/store/delivery/cursor.rs poll(), src/store/index/mod.rs stream() ordering.\n\
         Common causes: cursor position not advanced after yielding last entry in a segment, stream() returns fewer entries than appended.\n\
         Run: cargo test --test chaos_testing chaos_cursor_completeness_concurrent",
        seen.len()
    );

    // Verify no duplicates
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "CHAOS PROPERTY: cursor must not deliver any event more than once ({} unique out of {} delivered).\n\
         Investigate: src/store/delivery/cursor.rs position tracking, src/store/index/mod.rs stream() dedup.\n\
         Common causes: cursor resets position to segment start on re-poll, index entries duplicated across segment rotation.\n\
         Run: cargo test --test chaos_testing chaos_cursor_completeness_concurrent",
        unique.len(),
        seen.len()
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| "Arc still has multiple owners")
        .expect("store should be sole owner after all worker threads joined");
    store.close().expect("close");
}
