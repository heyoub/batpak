// PROVES: LAW-007 (Codebase Accuses Itself — adversarial self-testing)
// DEFENDS: FM-011 (Error Path Hollowing), FM-019 (Non-Replayable Truth)
// INVARIANTS: INV-CONC (linearizability, CAS, idempotency), INV-TEMP (corruption recovery)
#![allow(
    clippy::panic,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::inconsistent_digit_grouping,
    clippy::disallowed_methods,    // chaos tests use thread::spawn for stress probes
    clippy::needless_borrows_for_generic_args,
    clippy::unused_enumerate_index
)]
//! Chaos testing: fault injection, data corruption, concurrent stress.
//! The library tests itself under adversarial conditions and feeds
//! results through its own Gate system for actionable diagnostics.
//!
//! Run with: cargo test --test chaos_testing --all-features
//! Default depth: 500 iterations (override with `CHAOS_ITERATIONS=<n>`)
//! Extended: CHAOS_ITERATIONS=5000 cargo test --test chaos_testing --all-features --release

use batpak::prelude::*;
use batpak::store::segment::frame_decode;
use batpak::store::{AppendOptions, Store, StoreConfig, StoreError, SyncConfig};
use std::sync::Arc;
use tempfile::TempDir;

const DEFAULT_CHAOS_ITERATIONS: usize = 500;

fn effective_chaos_iterations(env_value: Option<&str>) -> usize {
    env_value
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CHAOS_ITERATIONS)
}

fn chaos_iterations() -> usize {
    effective_chaos_iterations(std::env::var("CHAOS_ITERATIONS").ok().as_deref())
}

#[test]
fn chaos_iterations_default_to_repo_truth() {
    assert_eq!(effective_chaos_iterations(None), DEFAULT_CHAOS_ITERATIONS);
    assert_eq!(effective_chaos_iterations(Some("5000")), 5000);
    assert_eq!(
        effective_chaos_iterations(Some("not-a-number")),
        DEFAULT_CHAOS_ITERATIONS
    );
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
    let mut rng = fastrand::Rng::with_seed(42);
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
        let corrupt_count = rng.usize(1..=5);
        for _ in 0..corrupt_count {
            let pos = rng.usize(40..data.len().max(41));
            if pos < data.len() {
                data[pos] ^= rng.u8(..) | 1; // ensure at least 1 bit flips
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
            // Verify it's an expected error variant — match typed variants, not Display strings.
            // Corruption injected at a random byte offset 40+ can produce:
            //   - CrcMismatch: CRC32 no longer matches frame data
            //   - CorruptSegment: frame structure is unreadable (bad magic, EOF, bad version)
            //   - Serialization: msgpack payload is unparseable after byte flip
            //   - Io: OS-level read error
            //   - Coordinate: entity/scope string is corrupt
            // Expressed as a `matches!` guard rather than `match { wildcard }`
            // so clippy's `match_wildcard_for_single_variants` lint is happy
            // AND so adding a new StoreError variant doesn't accidentally
            // get accepted here without a conscious decision.
            let acceptable = matches!(
                &e,
                StoreError::CrcMismatch { .. }
                    | StoreError::CorruptSegment { .. }
                    | StoreError::Serialization(_)
                    | StoreError::Io(_)
                    | StoreError::Coordinate(_)
            );
            if !acceptable {
                panic!(
                    "CHAOS PROPERTY: corrupted segment must produce a structured \
                     CrcMismatch/CorruptSegment/Serialization/Io/Coordinate error, \
                     but got variant: {e}\n\
                     Investigate: src/store/mod.rs Store::open, \
                     src/store/segment.rs frame_decode error mapping.\n\
                     Common causes: new error variant added without updating open() \
                     match arm, raw unwrap() escaping as opaque error.\n\
                     Run: cargo test --test chaos_testing chaos_corrupted_segment_bytes"
                );
            }
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
        sync: SyncConfig {
            every_n_events: 10,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
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
    };
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
                     Investigate: src/store/writer.rs CAS check under entity lock.\n\
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

    eprintln!("  CHAOS CAS CONTENTION: 1 winner, {losers} losers");
    assert!(
        winning_receipt.is_some(),
        "CHAOS PROPERTY: exactly one thread must win CAS, but none did.\n\
         Run: cargo test --test chaos_testing chaos_cas_contention"
    );
    assert_eq!(
        losers,
        n_threads - 1,
        "CHAOS PROPERTY: all threads except the CAS winner must receive a conflict error (expected {}, got {losers}).\n\
         Investigate: src/store/writer.rs CAS rejection path, StoreError::SequenceMismatch variant.\n\
         Run: cargo test --test chaos_testing chaos_cas_contention",
        n_threads - 1
    );
    // Verify losers got SequenceMismatch, not some other error
    for err_msg in &loser_errors {
        assert!(
            err_msg.contains("CAS failed"),
            "CHAOS PROPERTY: CAS losers must get SequenceMismatch, got: {err_msg}\n\
             Investigate: src/store/writer.rs CAS rejection, StoreError::SequenceMismatch.\n\
             Run: cargo test --test chaos_testing chaos_cas_contention"
        );
    }

    // Verify the winner's event is actually in the stream
    let entries = store.stream("chaos:cas");
    assert_eq!(
        entries.len(),
        2, // seed + 1 winner
        "CHAOS PROPERTY: stream should have exactly 2 events (seed + CAS winner), got {}.\n\
         Investigate: src/store/writer.rs handle_append commit path.",
        entries.len()
    );

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    };
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
            std::thread::Builder::new()
                .name(format!("chaos-idem-{t}"))
                .spawn(move || {
                    let opts = AppendOptions {
                        idempotency_key: Some(idem_key),
                        ..Default::default()
                    };
                    store.append_with_options(&coord, kind, &serde_json::json!({"thread": t}), opts)
                })
                .expect("spawn thread")
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
    for id in event_ids.iter() {
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
    };
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
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
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
// CHAOS 5b: Batch atomicity under concurrent stress
// Multiple threads appending batches, verifying atomicity.
// ============================================================

#[test]
fn chaos_batch_atomicity_concurrent() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
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

                        match store.append_batch(items) {
                            Ok(_) => {
                                batch_counter.fetch_add(1, Ordering::SeqCst);
                            }
                            Err(e) => {
                                eprintln!("Thread {t} batch {b} failed: {e}");
                            }
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
        let entries = store.stream(coord.entity());

        let expected_count = batches_per_thread * items_per_batch;
        assert!(
            entries.len() == expected_count || entries.is_empty(),
            "CHAOS PROPERTY: batch atomicity - entity {} must have {} or 0 events, got {}.\n\
             Partial batches indicate atomicity violation.\n\
             Investigate: src/store/writer.rs handle_append_batch atomic publish, \
             src/store/index.rs insert_batch all-or-nothing.",
            coord.entity(),
            expected_count,
            entries.len()
        );

        // Verify sequence continuity within each entity
        if !entries.is_empty() {
            for (i, entry) in entries.iter().enumerate() {
                assert_eq!(
                    entry.clock as usize, i,
                    "CHAOS PROPERTY: entity clocks must be contiguous after concurrent batches.\n\
                     Entry {} has clock {} (expected {}).",
                    i, entry.clock, i
                );
            }
        }
    }

    let total_committed = total_batches.load(Ordering::SeqCst);
    eprintln!("  CHAOS BATCH: {total_committed} batches committed across {n_threads} threads");

    drop(store);
}

// ============================================================
// CHAOS 5c: Batch with rapid segment rotation
// Large batches that force mid-batch segment rotation.
// ============================================================

#[test]
fn chaos_batch_cross_segment_rotation() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512, // Tiny - will force rotation mid-batch
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
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
    let entries = store.stream(coord.entity());
    assert_eq!(
        entries.len(),
        20,
        "CHAOS PROPERTY: batch spanning segment rotation must commit all items.\n\
         Expected 20 events, got {}.\n\
         Investigate: src/store/reader.rs cross-segment batch recovery, \
         src/store/writer.rs mid-batch rotation handling.",
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

    eprintln!("  CHAOS BATCH ROTATION: 20 items across {segment_count} segments");
    store.close().expect("close");
}

// ============================================================
// CHAOS 6: Random msgpack garbage to frame_decode
// High-volume fuzzing with truly random data.
// ============================================================

#[test]
fn chaos_frame_decode_random_bombardment() {
    let mut rng = fastrand::Rng::with_seed(0xCA05);
    let iterations = chaos_iterations();
    let mut ok_count = 0u64;
    let mut err_count = 0u64;

    for _ in 0..iterations {
        let len = rng.usize(0..2048);
        let data: Vec<u8> = (0..len).map(|_| rng.u8(..)).collect();
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
        // Buffer must be >= iterations so the "drain after join" pattern
        // sees every notification. The lossy-under-backpressure path is
        // tested separately in subscription_ops::slow_subscriber_*. Here we
        // want to verify that every append() broadcasts synchronously.
        broadcast_capacity: 1024,
        ..StoreConfig::new("")
    };
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

    eprintln!("  CHAOS SUBSCRIPTION: received {received}/{iterations} events");
    assert!(
        received == iterations,
        "PROPERTY: every append() broadcasts before returning, so a subscriber joined after all \
         writes must see exactly {iterations} notifications, got {received}.\n\
         Investigate: src/store/writer.rs (broadcast send path), src/store/index.rs (region filter).\n\
         Run: cargo test --test chaos_testing chaos_subscription_write_storm"
    );

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    };
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
    let mut cursor = store.cursor_guaranteed(&region);
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
    };
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
    // Checkpoint disabled: this test truncates segments to simulate crashes,
    // which invalidates any checkpoint written before the truncation.
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(65536)
        .with_enable_checkpoint(false);
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
    let last_seg_path = segments
        .last()
        .expect("segments should not be empty per preceding assertion")
        .path();

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

    // Reopen: cold start must tolerate the truncated tail and not panic.
    // Checkpoint disabled: the checkpoint references pre-truncation offsets
    // that no longer exist, so it would produce corrupt reads.
    let config2 = StoreConfig::new(dir.path())
        .with_segment_max_bytes(65536)
        .with_enable_checkpoint(false);
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
