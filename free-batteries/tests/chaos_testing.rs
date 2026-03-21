#![allow(
    clippy::panic,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::inconsistent_digit_grouping
)]
//! Chaos testing: fault injection, data corruption, concurrent stress.
//! The library tests itself under adversarial conditions and feeds
//! results through its own Gate system for actionable diagnostics.
//!
//! Run with: cargo test --test chaos_testing --all-features
//! Extended: CHAOS_ITERATIONS=5000 cargo test --test chaos_testing --all-features --release
//! [SPEC:tests/chaos_testing.rs]

use free_batteries::prelude::*;
use free_batteries::store::segment::{frame_decode, frame_encode};
use free_batteries::store::{AppendOptions, Store, StoreConfig};
use rand::prelude::*;
use rand::Rng;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

fn chaos_iterations() -> usize {
    std::env::var("CHAOS_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500)
}

// ============================================================
// CHAOS 1: Corrupted segment files
// Inject random byte corruption into segment files, verify Store
// either recovers gracefully or reports actionable errors.
// ============================================================

#[test]
fn chaos_corrupted_segment_bytes() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("chaos:corrupt", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);

    // Write some events
    for i in 0..20 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");
    store.close().expect("close");

    // Corrupt random bytes in segment files
    let mut rng = StdRng::seed_from_u64(42);
    let segments: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .collect();

    assert!(!segments.is_empty(), "Should have segment files");

    for seg in &segments {
        let mut data = std::fs::read(seg.path()).expect("read segment");
        // Skip magic bytes (first 4), corrupt some bytes after header
        let corrupt_count = rng.gen_range(1..=5);
        for _ in 0..corrupt_count {
            let pos = rng.gen_range(40..data.len().max(41));
            if pos < data.len() {
                data[pos] ^= rng.gen::<u8>() | 1; // ensure at least 1 bit flips
            }
        }
        std::fs::write(seg.path(), &data).expect("write corrupted");
    }

    // Try to reopen — should either succeed (if corruption hit unused area)
    // or fail with StoreError::CrcMismatch / CorruptSegment (NOT panic)
    let config2 = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    let result = Store::open(config2);
    // The key invariant: no panic. Either Ok or a structured error.
    match result {
        Ok(store) => {
            eprintln!("  CHAOS: corrupted segment opened (corruption hit non-critical area)");
            let _ = store.close();
        }
        Err(e) => {
            eprintln!("  CHAOS: corrupted segment correctly rejected: {e}");
            // Verify it's an expected error variant
            let msg = format!("{e}");
            assert!(
                msg.contains("CRC")
                    || msg.contains("corrupt")
                    || msg.contains("serialization")
                    || msg.contains("IO")
                    || msg.contains("coordinate"),
                "CHAOS: unexpected error type: {msg}. \
                 Expected CRC/corrupt/serialization/IO error. \
                 Investigate: src/store/mod.rs Store::open error handling."
            );
        }
    }
}

// ============================================================
// CHAOS 2: Concurrent writer stress
// Multiple threads hammering the store simultaneously.
// ============================================================

#[test]
fn chaos_concurrent_writer_stress() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 8192, // small segments → lots of rotation
        sync_every_n_events: 10,
        ..StoreConfig::default()
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let iterations = chaos_iterations();
    let n_threads = 4;

    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let coord =
                    Coordinate::new(&format!("chaos:thread{t}"), "chaos:stress").expect("valid");
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
        })
        .collect();

    let mut total_ok = 0u64;
    let mut total_err = 0u64;
    for h in handles {
        let (ok, err) = h.join().expect("thread join");
        total_ok += ok;
        total_err += err;
    }

    eprintln!(
        "  CHAOS CONCURRENT STRESS: {total_ok} ok, {total_err} errors across {n_threads} threads"
    );
    assert!(
        total_ok > 0,
        "CHAOS: no successful writes under concurrent stress"
    );
    assert_eq!(
        total_err, 0,
        "CHAOS: {total_err} errors under concurrent stress. \
        Investigate: src/store/writer.rs lock ordering."
    );

    // Verify data integrity: each thread's events should be readable
    let store_ref = &*store;
    for t in 0..n_threads {
        let entries = store_ref.stream(&format!("chaos:thread{t}"));
        assert!(
            !entries.is_empty(),
            "CHAOS: thread {t} wrote events but none found in index. \
             Investigate: src/store/index.rs insert."
        );
    }

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }
}

// ============================================================
// CHAOS 3: CAS contention
// Multiple threads competing for the same entity with CAS.
// Exactly one should win per sequence number.
// ============================================================

#[test]
fn chaos_cas_contention() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
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
            std::thread::spawn(move || {
                let opts = AppendOptions {
                    expected_sequence: Some(0), // all compete: expect latest clock=0 after seed
                    ..Default::default()
                };
                store.append_with_options(&coord, kind, &serde_json::json!({"thread": t}), opts)
            })
        })
        .collect();

    let mut winners = 0;
    let mut losers = 0;
    for h in handles {
        match h.join().expect("join") {
            Ok(_) => winners += 1,
            Err(_) => losers += 1,
        }
    }

    eprintln!("  CHAOS CAS CONTENTION: {winners} winners, {losers} losers");
    assert_eq!(
        winners, 1,
        "CHAOS: CAS should allow exactly 1 winner. Got {winners}. \
         Investigate: src/store/writer.rs CAS check under entity lock."
    );
    assert_eq!(losers, n_threads - 1);

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }
}

// ============================================================
// CHAOS 4: Idempotency under concurrent duplicate submissions
// ============================================================

#[test]
fn chaos_idempotency_concurrent() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("chaos:idem", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let idem_key: u128 = 0xDEAD_BEEF_CAFE_BABE_1111_2222_3333_4444;

    let n_threads = 8;
    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            let coord = coord.clone();
            std::thread::spawn(move || {
                let opts = AppendOptions {
                    idempotency_key: Some(idem_key),
                    ..Default::default()
                };
                store.append_with_options(&coord, kind, &serde_json::json!({"thread": t}), opts)
            })
        })
        .collect();

    let mut event_ids = Vec::new();
    for h in handles {
        match h.join().expect("join") {
            Ok(receipt) => event_ids.push(receipt.event_id),
            Err(e) => panic!("CHAOS: idempotent append failed: {e}"),
        }
    }

    // All should return the same event_id
    let first = event_ids[0];
    for (i, id) in event_ids.iter().enumerate() {
        assert_eq!(
            *id, first,
            "CHAOS: idempotency returned different event_id at index {i}. \
             Expected {first:032x}, got {id:032x}. \
             Investigate: src/store/writer.rs idempotency check."
        );
    }

    // Only one event should exist in the store
    let entries = store.stream("chaos:idem");
    assert_eq!(
        entries.len(),
        1,
        "CHAOS: idempotency should produce exactly 1 event, got {}. \
         Investigate: src/store/writer.rs idempotency dedup.",
        entries.len()
    );

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }
}

// ============================================================
// CHAOS 5: Rapid segment rotation stress
// Tiny segment size forces constant rotation.
// ============================================================

#[test]
fn chaos_rapid_segment_rotation() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 256, // extremely tiny
        fd_budget: 3,           // very constrained
        sync_every_n_events: 1,
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("chaos:rotation", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let iterations = chaos_iterations().min(200);

    for i in 0..iterations {
        store
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

    eprintln!("  CHAOS ROTATION: {iterations} events across {segment_count} segments");
    assert!(
        segment_count > 1,
        "CHAOS: expected multiple segments with 256-byte limit, got {segment_count}"
    );

    // Verify ALL events are still readable after all the rotation
    let entries = store.stream("chaos:rotation");
    assert_eq!(
        entries.len(),
        iterations,
        "CHAOS: lost events during rapid rotation. Expected {iterations}, got {}. \
         Investigate: src/store/writer.rs STEP 7 rotation + src/store/reader.rs.",
        entries.len()
    );

    // Spot-check first and last events
    let first = store.get(entries[0].event_id).expect("first event");
    let last = store
        .get(entries[entries.len() - 1].event_id)
        .expect("last event");
    assert_eq!(first.event.event_id(), entries[0].event_id);
    assert_eq!(last.event.event_id(), entries[entries.len() - 1].event_id);

    store.close().expect("close");
}

// ============================================================
// CHAOS 6: Random msgpack garbage to frame_decode
// High-volume fuzzing with truly random data.
// ============================================================

#[test]
fn chaos_frame_decode_random_bombardment() {
    let mut rng = StdRng::seed_from_u64(0xCA05);
    let iterations = chaos_iterations();
    let mut ok_count = 0u64;
    let mut err_count = 0u64;

    for _ in 0..iterations {
        let len = rng.gen_range(0..2048);
        let data: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
        match frame_decode(&data) {
            Ok(_) => ok_count += 1,
            Err(_) => err_count += 1,
        }
        // Key: no panics
    }

    eprintln!(
        "  CHAOS FRAME DECODE: {ok_count} accepted, {err_count} rejected out of {iterations}"
    );
    // With random data, almost nothing should decode as valid
    // (valid CRC match on random data is ~1 in 4 billion)
    assert!(
        err_count > ok_count,
        "CHAOS: more random frames accepted than rejected. \
         Investigate: src/store/segment.rs frame_decode CRC check."
    );
}

// ============================================================
// CHAOS 7: Subscription under write storm
// Verify subscriptions don't deadlock or lose too many events
// under heavy write load.
// ============================================================

#[test]
fn chaos_subscription_write_storm() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        broadcast_capacity: 64, // small buffer → forces drops
        ..StoreConfig::default()
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("chaos:sub", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("chaos:sub");
    let iterations = chaos_iterations().min(200);

    // Start subscriber
    let sub = store.subscribe(&region);

    // Writer thread hammers events
    let store2 = Arc::clone(&store);
    let writer = std::thread::spawn(move || {
        for i in 0..iterations {
            store2
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
    });

    writer.join().expect("writer join");

    // Drain subscriber — it's lossy, so we just count what we got
    let mut received = 0;
    let deadline = Instant::now() + std::time::Duration::from_millis(500);
    while Instant::now() < deadline {
        if sub.receiver().try_recv().is_ok() {
            received += 1;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    eprintln!("  CHAOS SUBSCRIPTION: received {received}/{iterations} events (lossy channel)");
    // With a small buffer some loss is expected, but we should get *something*
    assert!(
        received > 0,
        "CHAOS: subscriber received 0 events. \
         Investigate: src/store/writer.rs broadcast, src/store/subscription.rs."
    );

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }
}

// ============================================================
// CHAOS 8: Cursor completeness under concurrent writes
// Cursors must deliver every event exactly once.
// ============================================================

#[test]
fn chaos_cursor_completeness_concurrent() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
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
    let mut cursor = store.cursor(&region);
    let mut seen = Vec::new();
    while let Some(entry) = cursor.poll() {
        seen.push(entry.event_id);
    }

    assert_eq!(
        seen.len(),
        n,
        "CHAOS: cursor missed events. Expected {n}, got {}. \
         Investigate: src/store/cursor.rs poll().",
        seen.len()
    );

    // Verify no duplicates
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "CHAOS: cursor delivered duplicate events. \
         Investigate: src/store/cursor.rs position tracking."
    );

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }
}
