// PROVES: LAW-007 (Codebase Accuses Itself — adversarial self-testing)
// CATCHES: FM-011 (Error Path Hollowing — partial batch published), FM-019 (Non-Replayable Truth — events lost across rotation)
// SEEDED: tiny segment limits (256/512 bytes) forcing mid-batch rotation, concurrent batch storms (4 threads × 10 batches × 10 items)
// INVARIANTS: INV-BATCH-CRASH-RECOVERY (all-or-nothing publish), INV-CONCURRENCY-SCHEDULE-PROOF (clock continuity)
//! Chaos testing — batch atomicity and rapid segment rotation lane.
//! Harness pattern: Fault-Injection Harness (direct chaos lane).
//! Splits the rotation-stress and batch-atomicity cases out of `chaos_testing`:
//! rapid rotation under a tiny segment budget, concurrent batch atomicity (no
//! partial publish), and batches that span a segment rotation mid-write. Every
//! case asserts all-or-nothing publish and zero event loss across rotation.
//!
//! Run with: cargo test --test chaos_testing_batch_rotation --all-features
//! Default depth: 500 iterations (override with `CHAOS_ITERATIONS=<n>`)
//! Extended: CHAOS_ITERATIONS=5000 cargo test --test chaos_testing_batch_rotation --all-features --release

use batpak::store::{AppendOptions, Store, StoreConfig};
use batpak_testkit::prelude::*;
use std::sync::Arc;
use tempfile::TempDir;

use batpak_testkit::chaos_testing as chaos_support;
use chaos_support::chaos_iterations;

// ============================================================
// CHAOS 5: Rapid segment rotation stress
// Tiny segment size forces constant rotation.
// ============================================================

#[test]
fn chaos_rapid_segment_rotation() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(256) // extremely tiny
        .with_fd_budget(3) // very constrained
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("chaos:rotation", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let iterations = chaos_iterations().min(200);

    for i in 0..iterations {
        let _ = store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");

    // Count segments created
    let segment_count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();

    assert!(
        segment_count > 1,
        "CHAOS PROPERTY: with a 256-byte segment limit, {iterations} events must span more than one segment file (got {segment_count}).\n\
         Investigate: src/store/write/writer.rs STEP 7 rotation trigger, src/store/segment/mod.rs segment_max_bytes enforcement.\n\
         Common causes: byte budget checked after write instead of before, segment size accumulated incorrectly.\n\
         Run: cargo test --test chaos_testing_batch_rotation chaos_rapid_segment_rotation"
    );

    // Verify ALL events are still readable after all the rotation
    let entries = store.by_entity("chaos:rotation");
    assert_eq!(
        entries.len(),
        iterations,
        "CHAOS PROPERTY: no events must be lost across {segment_count} segment rotations (expected {iterations}, got {}).\n\
         Investigate: src/store/write/writer.rs STEP 7 rotation, src/store/segment/scan.rs multi-segment scan, src/store/index/mod.rs insert.\n\
         Common causes: index entry for last event in a segment dropped on rotation, reader skips sealed segments.\n\
         Run: cargo test --test chaos_testing_batch_rotation chaos_rapid_segment_rotation",
        entries.len()
    );

    // Spot-check first and last events
    let first = store.get(entries[0].event_id()).expect("first event");
    let last = store
        .get(entries[entries.len() - 1].event_id())
        .expect("last event");
    assert_eq!(
        first.event.event_id(),
        entries[0].event_id(),
        "CHAOS PROPERTY: store.get() for the first indexed event_id must return the matching event.\n\
         Investigate: src/store/segment/scan.rs get(), src/store/index/mod.rs lookup offset.\n\
         Common causes: index stores wrong file offset after rotation, event_id collision from monotonic-clock reset.\n\
         Run: cargo test --test chaos_testing_batch_rotation chaos_rapid_segment_rotation"
    );
    assert_eq!(
        last.event.event_id(),
        entries[entries.len() - 1].event_id(),
        "CHAOS PROPERTY: store.get() for the last indexed event_id must return the matching event.\n\
         Investigate: src/store/segment/scan.rs get(), src/store/segment/mod.rs seek by offset.\n\
         Common causes: final segment not flushed before get(), write buffer not committed on sync().\n\
         Run: cargo test --test chaos_testing_batch_rotation chaos_rapid_segment_rotation"
    );

    store.close().expect("close");
}

// ============================================================
// CHAOS 5b: Batch atomicity under concurrent stress
// Multiple threads appending batches, verifying atomicity.
// ============================================================

#[test]
fn chaos_batch_atomicity_concurrent() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Arc::new(Store::open(config).expect("open"));
    let kind = EventKind::custom(0xF, 1);
    let n_threads = 4;
    let batches_per_thread = 10;
    let items_per_batch = 10;

    let total_batches = Arc::new(AtomicU64::new(0));

    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            let batch_counter = Arc::clone(&total_batches);
            std::thread::Builder::new()
                .name(format!("chaos-batch-{t}"))
                .spawn(move || {
                    let coord = Coordinate::new(
                        format!("chaos:batch_thread{t}").as_str(),
                        "chaos:batch_scope",
                    )
                    .expect("valid");

                    for b in 0..batches_per_thread {
                        let items: Vec<_> = (0..items_per_batch)
                            .map(|i| {
                                BatchAppendItem::new(
                                    coord.clone(),
                                    kind,
                                    &serde_json::json!({"batch": b, "item": i, "thread": t}),
                                    AppendOptions::default(),
                                    CausationRef::None,
                                )
                                .expect("valid item")
                            })
                            .collect();

                        if store.append_batch(items).is_ok() {
                            batch_counter.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                })
                .expect("spawn")
        })
        .collect();

    for h in handles {
        h.join().expect("thread join");
    }

    // Verify atomicity: each entity should have 0 or N*items_per_batch events
    // never a partial batch
    for t in 0..n_threads {
        let coord = Coordinate::new(
            format!("chaos:batch_thread{t}").as_str(),
            "chaos:batch_scope",
        )
        .expect("valid");
        let entries = store.by_entity(coord.entity());

        let expected_count = batches_per_thread * items_per_batch;
        assert!(
            entries.len() == expected_count || entries.is_empty(),
            "CHAOS PROPERTY: batch atomicity - entity {} must have {} or 0 events, got {}.\n\
             Partial batches indicate atomicity violation.\n\
             Investigate: src/store/write/writer.rs handle_append_batch atomic publish, \
             src/store/index/mod.rs insert_batch all-or-nothing.",
            coord.entity(),
            expected_count,
            entries.len()
        );

        // Verify sequence continuity within each entity
        if !entries.is_empty() {
            for (i, entry) in entries.iter().enumerate() {
                assert_eq!(
                    usize::try_from(entry.clock())
                        .expect("entity clock fits in usize for a bounded test batch"),
                    i,
                    "CHAOS PROPERTY: entity clocks must be contiguous after concurrent batches.\n\
                     Entry {} has clock {} (expected {}).",
                    i,
                    entry.clock(),
                    i
                );
            }
        }
    }

    let total_committed = total_batches.load(Ordering::SeqCst);
    assert!(
        total_committed > 0,
        "CHAOS PROPERTY: concurrent batch atomicity must observe at least one committed batch.\n\
         A zero-commit run is vacuous and does not prove anything about atomic publish.\n\
         Investigate: test harness stress level, writer availability, or batch fault paths."
    );

    drop(store);
}

// ============================================================
// CHAOS 5c: Batch with rapid segment rotation
// Large batches that force mid-batch segment rotation.
// ============================================================

#[test]
fn chaos_batch_cross_segment_rotation() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(512) // Tiny - will force rotation mid-batch
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("chaos:batch_rot", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);

    // Create a batch that will span multiple segments
    let items: Vec<_> = (0..20)
        .map(|i| {
            // Large payload to force rotation
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"i": i, "pad": "x".repeat(100)}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("valid item")
        })
        .collect();

    store.append_batch(items).expect("batch across segments");
    store.sync().expect("sync");

    // Verify all items committed despite segment rotation
    let entries = store.by_entity(coord.entity());
    assert_eq!(
        entries.len(),
        20,
        "CHAOS PROPERTY: batch spanning segment rotation must commit all items.\n\
         Expected 20 events, got {}.\n\
         Investigate: src/store/segment/scan.rs cross-segment batch recovery, \
         src/store/write/writer.rs mid-batch rotation handling.",
        entries.len()
    );

    // Count segments to verify rotation occurred
    let segment_count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();

    assert!(
        segment_count >= 2,
        "CHAOS PROPERTY: with 512-byte segments, 20 large items must span multiple segments (got {}).",
        segment_count
    );

    store.close().expect("close");
}
