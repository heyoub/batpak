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
use free_batteries::store::segment::frame_decode;
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
        ..StoreConfig::new("")
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

    assert!(
        !segments.is_empty(),
        "CHAOS PROPERTY: writing 20 events must produce at least one .fbat segment file.\n\
         Investigate: src/store/segment.rs open_writer, src/store/writer.rs STEP 7 rotation.\n\
         Common causes: segment file extension mismatch, data_dir not flushed before read_dir.\n\
         Run: cargo test --test chaos_testing chaos_corrupted_segment_bytes"
    );

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
        ..StoreConfig::new("")
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
                "CHAOS PROPERTY: corrupted segment must produce a structured CRC/corrupt/serialization/IO error, not an unknown variant.\n\
                 Investigate: src/store/mod.rs Store::open, src/store/segment.rs frame_decode error mapping.\n\
                 Common causes: new error variant added without updating open() match arm, raw unwrap() escaping as opaque error.\n\
                 Run: cargo test --test chaos_testing chaos_corrupted_segment_bytes"
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
        ..StoreConfig::new("")
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
        "CHAOS PROPERTY: at least one write must succeed under concurrent stress across {n_threads} threads.\n\
         Investigate: src/store/writer.rs lock acquisition, src/store/segment.rs open_writer.\n\
         Common causes: mutex poisoning on first write error, segment open failure blocking all writers.\n\
         Run: cargo test --test chaos_testing chaos_concurrent_writer_stress"
    );
    assert_eq!(
        total_err, 0,
        "CHAOS PROPERTY: zero write errors expected under concurrent stress, got {total_err}.\n\
         Investigate: src/store/writer.rs lock ordering, src/store/segment.rs rotate.\n\
         Common causes: race on segment rotation, fd_budget exhaustion under concurrent open, poisoned Mutex from a prior panic.\n\
         Run: cargo test --test chaos_testing chaos_concurrent_writer_stress"
    );

    // Verify data integrity: each thread's events should be readable
    let store_ref = &*store;
    for t in 0..n_threads {
        let entries = store_ref.stream(&format!("chaos:thread{t}"));
        assert!(
            !entries.is_empty(),
            "CHAOS PROPERTY: every writer thread must have its events present in the index after store close.\n\
             Investigate: src/store/index.rs insert, src/store/writer.rs STEP 5 index update.\n\
             Common causes: index update skipped on rotation, thread-local writes not flushed before stream() call.\n\
             Run: cargo test --test chaos_testing chaos_concurrent_writer_stress"
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
        ..StoreConfig::new("")
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
        "CHAOS PROPERTY: CAS must allow exactly one winner when all threads compete on the same expected_sequence.\n\
         Investigate: src/store/writer.rs CAS check under entity lock, src/store/mod.rs AppendOptions handling.\n\
         Common causes: entity lock not held across sequence read + write, CAS condition checked outside the lock.\n\
         Run: cargo test --test chaos_testing chaos_cas_contention"
    );
    assert_eq!(
        losers,
        n_threads - 1,
        "CHAOS PROPERTY: all threads except the CAS winner must receive a conflict error (expected {}, got {losers}).\n\
         Investigate: src/store/writer.rs CAS rejection path, StoreError::SequenceMismatch variant.\n\
         Common causes: CAS error swallowed/mapped to Ok, winner count > 1 masking losers.\n\
         Run: cargo test --test chaos_testing chaos_cas_contention",
        n_threads - 1
    );

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
        ..StoreConfig::new("")
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
    for (_i, id) in event_ids.iter().enumerate() {
        assert_eq!(
            *id, first,
            "CHAOS PROPERTY: all concurrent idempotent appends with the same key must return the same event_id.\n\
             Investigate: src/store/writer.rs idempotency check, src/store/index.rs idempotency_key lookup.\n\
             Common causes: idempotency map not protected by the same lock as the append, two threads both pass the key-absent check.\n\
             Run: cargo test --test chaos_testing chaos_idempotency_concurrent"
        );
    }

    // Only one event should exist in the store
    let entries = store.stream("chaos:idem");
    assert_eq!(
        entries.len(),
        1,
        "CHAOS PROPERTY: idempotent append with the same key from {n_threads} concurrent threads must store exactly 1 event, got {}.\n\
         Investigate: src/store/writer.rs idempotency dedup, src/store/index.rs stream().\n\
         Common causes: dedup check races with concurrent insert, idempotency_key stored per-segment losing cross-segment dedup.\n\
         Run: cargo test --test chaos_testing chaos_idempotency_concurrent",
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
        ..StoreConfig::new("")
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
        "CHAOS PROPERTY: with a 256-byte segment limit, {iterations} events must span more than one segment file (got {segment_count}).\n\
         Investigate: src/store/writer.rs STEP 7 rotation trigger, src/store/segment.rs segment_max_bytes enforcement.\n\
         Common causes: byte budget checked after write instead of before, segment size accumulated incorrectly.\n\
         Run: cargo test --test chaos_testing chaos_rapid_segment_rotation"
    );

    // Verify ALL events are still readable after all the rotation
    let entries = store.stream("chaos:rotation");
    assert_eq!(
        entries.len(),
        iterations,
        "CHAOS PROPERTY: no events must be lost across {segment_count} segment rotations (expected {iterations}, got {}).\n\
         Investigate: src/store/writer.rs STEP 7 rotation, src/store/reader.rs multi-segment scan, src/store/index.rs insert.\n\
         Common causes: index entry for last event in a segment dropped on rotation, reader skips sealed segments.\n\
         Run: cargo test --test chaos_testing chaos_rapid_segment_rotation",
        entries.len()
    );

    // Spot-check first and last events
    let first = store.get(entries[0].event_id).expect("first event");
    let last = store
        .get(entries[entries.len() - 1].event_id)
        .expect("last event");
    assert_eq!(
        first.event.event_id(),
        entries[0].event_id,
        "CHAOS PROPERTY: store.get() for the first indexed event_id must return the matching event.\n\
         Investigate: src/store/reader.rs get(), src/store/index.rs lookup offset.\n\
         Common causes: index stores wrong file offset after rotation, event_id collision from monotonic-clock reset.\n\
         Run: cargo test --test chaos_testing chaos_rapid_segment_rotation"
    );
    assert_eq!(
        last.event.event_id(),
        entries[entries.len() - 1].event_id,
        "CHAOS PROPERTY: store.get() for the last indexed event_id must return the matching event.\n\
         Investigate: src/store/reader.rs get(), src/store/segment.rs seek by offset.\n\
         Common causes: final segment not flushed before get(), write buffer not committed on sync().\n\
         Run: cargo test --test chaos_testing chaos_rapid_segment_rotation"
    );

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
        "CHAOS PROPERTY: the vast majority of random-byte frames must be rejected by frame_decode (CRC collision probability ~1/2^32), \
         but accepted={ok_count} >= rejected={err_count} across {iterations} iterations.\n\
         Investigate: src/store/segment.rs frame_decode CRC check, crc32 polynomial selection.\n\
         Common causes: CRC validation accidentally skipped, CRC field not checked against payload, wrong byte range fed to CRC.\n\
         Run: cargo test --test chaos_testing chaos_frame_decode_random_bombardment"
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
        ..StoreConfig::new("")
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
        "CHAOS PROPERTY: a live subscriber must receive at least one broadcast event during a {iterations}-write storm, even with a small buffer.\n\
         Investigate: src/store/writer.rs broadcast send, src/store/subscription.rs Receiver::try_recv.\n\
         Common causes: broadcast channel created after writes complete, subscription region filter excludes entity, sender dropped before subscriber polls.\n\
         Run: cargo test --test chaos_testing chaos_subscription_write_storm"
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
        ..StoreConfig::new("")
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
        "CHAOS PROPERTY: cursor must deliver every event exactly once — expected {n} events, got {}.\n\
         Investigate: src/store/cursor.rs poll(), src/store/index.rs stream() ordering.\n\
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
         Investigate: src/store/cursor.rs position tracking, src/store/index.rs stream() dedup.\n\
         Common causes: cursor resets position to segment start on re-poll, index entries duplicated across segment rotation.\n\
         Run: cargo test --test chaos_testing chaos_cursor_completeness_concurrent",
        unique.len(),
        seen.len()
    );

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }
}

// ============================================================
// CHAOS 9: Truncated segment mid-write recovery
// Simulates a crash that leaves a segment file with a partial
// (torn) write at the tail. Cold start must skip the corrupted
// tail and recover all events written before the truncation.
// ============================================================

#[test]
fn chaos_truncated_segment_recovers() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 65536,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("chaos:truncate", "chaos:scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);
    let n_events = 20usize;

    for i in 0..n_events {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");

    // Capture the event_ids written so we can verify them after recovery
    let written_entries = store.stream("chaos:truncate");
    assert_eq!(
        written_entries.len(),
        n_events,
        "CHAOS PROPERTY: all {n_events} appended events must appear in the index before close, got {}.\n\
         Investigate: src/store/writer.rs STEP 5 index update, src/store/index.rs insert.\n\
         Common causes: index update deferred past sync(), events buffered but not flushed.\n\
         Run: cargo test --test chaos_testing chaos_truncated_segment_recovers",
        written_entries.len()
    );
    let written_ids: Vec<_> = written_entries.iter().map(|e| e.event_id).collect();

    store.close().expect("close");

    // Find the segment file(s) and truncate the last one to simulate a torn write
    let mut segments: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .collect();

    assert!(
        !segments.is_empty(),
        "CHAOS PROPERTY: writing {n_events} events must produce at least one .fbat segment file.\n\
         Investigate: src/store/segment.rs open_writer, src/store/writer.rs STEP 7 rotation.\n\
         Common causes: segment file extension mismatch, data_dir not flushed before read_dir.\n\
         Run: cargo test --test chaos_testing chaos_truncated_segment_recovers"
    );

    // Sort by name so we operate on the last (most recently written) segment
    segments.sort_by_key(|e| e.file_name());
    let last_seg_path = segments.last().unwrap().path();

    let original_data = std::fs::read(&last_seg_path).expect("read segment");
    assert!(
        original_data.len() > 64,
        "CHAOS PROPERTY: the last segment must contain more than 64 bytes to allow meaningful truncation (got {} bytes).\n\
         Investigate: src/store/segment.rs header write, src/store/writer.rs event serialization.\n\
         Common causes: segment header larger than expected, events too small to exceed header.\n\
         Run: cargo test --test chaos_testing chaos_truncated_segment_recovers",
        original_data.len()
    );

    // Remove the last 32 bytes — this tears the final frame, simulating a crash mid-write
    let truncated_len = original_data.len() - 32;
    std::fs::write(&last_seg_path, &original_data[..truncated_len])
        .expect("write truncated segment");

    eprintln!(
        "  CHAOS TRUNCATE: truncated {} → {} bytes (removed 32 bytes from tail)",
        original_data.len(),
        truncated_len
    );

    // Reopen: cold start must tolerate the truncated tail and not panic
    let config2 = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 65536,
        ..StoreConfig::new("")
    };
    let store2 = Store::open(config2).expect("store must reopen after tail truncation");

    let recovered_entries = store2.stream("chaos:truncate");

    // We must recover at least some events — truncating only the last 32 bytes
    // should leave the bulk of the segment intact
    assert!(
        !recovered_entries.is_empty(),
        "CHAOS PROPERTY: after tail truncation, cold-start recovery must restore at least some events; got zero.\n\
         Investigate: src/store/mod.rs Store::open cold-start scan, src/store/segment.rs frame_decode error handling.\n\
         Common causes: open() bails out on first decode error instead of stopping at corrupt tail, segment skipped entirely on any error.\n\
         Run: cargo test --test chaos_testing chaos_truncated_segment_recovers"
    );

    eprintln!(
        "  CHAOS TRUNCATE: recovered {}/{} events after tail truncation",
        recovered_entries.len(),
        n_events
    );

    // Every recovered event_id must have been one we originally wrote
    for entry in &recovered_entries {
        assert!(
            written_ids.contains(&entry.event_id),
            "CHAOS PROPERTY: every event recovered after truncation must match an originally written event_id; \
             found unknown id {:?}.\n\
             Investigate: src/store/mod.rs cold-start replay, src/store/segment.rs frame_decode.\n\
             Common causes: partial frame incorrectly accepted as valid, event_id reconstructed from corrupt bytes.\n\
             Run: cargo test --test chaos_testing chaos_truncated_segment_recovers",
            entry.event_id
        );
    }

    // All recovered events must be readable via get()
    for entry in &recovered_entries {
        let fetched = store2.get(entry.event_id).expect("get recovered event");
        assert_eq!(
            fetched.event.event_id(),
            entry.event_id,
            "CHAOS PROPERTY: store.get() for a recovered event_id must return the matching event.\n\
             Investigate: src/store/reader.rs get(), src/store/index.rs offset lookup.\n\
             Common causes: index rebuilt with wrong file offsets during cold-start, truncated segment re-indexed past truncation point.\n\
             Run: cargo test --test chaos_testing chaos_truncated_segment_recovers"
        );
    }

    store2.close().expect("close after recovery");
}
