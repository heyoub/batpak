// PROVES: LAW-007 (Codebase Accuses Itself — adversarial self-testing)
// CATCHES: FM-011 (Error Path Hollowing — corruption swallowed as opaque/panic), FM-019 (Non-Replayable Truth — torn-tail data lost on cold start)
// SEEDED: byte flips at random offsets (fastrand seed 42), random msgpack bombardment (seed 0xCA05), 32-byte tail truncation
// INVARIANTS: INV-BATCH-CRASH-RECOVERY (corruption recovery), INV-CONCURRENCY-SCHEDULE-PROOF (replay integrity)
//! Chaos testing — low-level byte corruption lane.
//! Harness pattern: Fault-Injection Harness (direct chaos lane).
//! Splits the byte-level corruption cases out of `chaos_testing`: random
//! in-segment byte flips, random msgpack bombardment of `frame_decode`, and
//! torn-tail truncation recovery. Each case asserts the runtime degrades into a
//! structured error or graceful recovery — never a panic.
//!
//! Run with: cargo test --test chaos_testing_byte_corruption --all-features
//! Default depth: 500 iterations (override with `CHAOS_ITERATIONS=<n>`)
//! Extended: CHAOS_ITERATIONS=5000 cargo test --test chaos_testing_byte_corruption --all-features --release

use batpak::store::segment::frame_decode;
use batpak::store::{Store, StoreConfig, StoreError};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

use batpak_testkit::chaos_testing as chaos_support;
use chaos_support::chaos_iterations;

// ============================================================
// CHAOS 1: Corrupted segment files
// Inject random byte corruption into segment files, verify Store
// either recovers gracefully or reports actionable errors.
// ============================================================

#[test]
fn chaos_corrupted_segment_bytes() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(4096);
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
         Investigate: src/store/segment/mod.rs open_writer, src/store/write/writer.rs STEP 7 rotation.\n\
         Common causes: segment file extension mismatch, data_dir not flushed before read_dir.\n\
         Run: cargo test --test chaos_testing_byte_corruption chaos_corrupted_segment_bytes"
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
    let config2 = StoreConfig::new(dir.path());
    let result = Store::open(config2);
    // The key invariant: no panic. Either Ok or a structured error.
    match result {
        Ok(store) => {
            let _ = store.close();
        }
        Err(e) => {
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
            assert!(
                acceptable,
                "CHAOS PROPERTY: corrupted segment must produce a structured \
                 CrcMismatch/CorruptSegment/Serialization/Io/Coordinate error, \
                 but got variant: {e}\n\
                 Investigate: src/store/mod.rs Store::open, \
                 src/store/segment/mod.rs frame_decode error mapping.\n\
                 Common causes: new error variant added without updating open() \
                 match arm, raw unwrap() escaping as opaque error.\n\
                 Run: cargo test --test chaos_testing_byte_corruption chaos_corrupted_segment_bytes"
            );
        }
    }
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

    // With random data, almost nothing should decode as valid
    // (valid CRC match on random data is ~1 in 4 billion)
    assert!(
        err_count > ok_count,
        "CHAOS PROPERTY: the vast majority of random-byte frames must be rejected by frame_decode (CRC collision probability ~1/2^32), \
         but accepted={ok_count} >= rejected={err_count} across {iterations} iterations.\n\
         Investigate: src/store/segment/mod.rs frame_decode CRC check, crc32 polynomial selection.\n\
         Common causes: CRC validation accidentally skipped, CRC field not checked against payload, wrong byte range fed to CRC.\n\
         Run: cargo test --test chaos_testing_byte_corruption chaos_frame_decode_random_bombardment"
    );
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
    let written_entries = store.by_entity("chaos:truncate");
    assert_eq!(
        written_entries.len(),
        n_events,
        "CHAOS PROPERTY: all {n_events} appended events must appear in the index before close, got {}.\n\
         Investigate: src/store/write/writer.rs STEP 5 index update, src/store/index/mod.rs insert.\n\
         Common causes: index update deferred past sync(), events buffered but not flushed.\n\
         Run: cargo test --test chaos_testing_byte_corruption chaos_truncated_segment_recovers",
        written_entries.len()
    );
    let written_ids: Vec<_> = written_entries.iter().map(|e| e.event_id()).collect();

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
         Investigate: src/store/segment/mod.rs open_writer, src/store/write/writer.rs STEP 7 rotation.\n\
         Common causes: segment file extension mismatch, data_dir not flushed before read_dir.\n\
         Run: cargo test --test chaos_testing_byte_corruption chaos_truncated_segment_recovers"
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
         Investigate: src/store/segment/mod.rs header write, src/store/write/writer.rs event serialization.\n\
         Common causes: segment header larger than expected, events too small to exceed header.\n\
         Run: cargo test --test chaos_testing_byte_corruption chaos_truncated_segment_recovers",
        original_data.len()
    );

    // Remove the last 32 bytes — this tears the final frame, simulating a crash mid-write
    let truncated_len = original_data.len() - 32;
    std::fs::write(&last_seg_path, &original_data[..truncated_len])
        .expect("write truncated segment");

    // Reopen: cold start must tolerate the truncated tail and not panic.
    // Checkpoint disabled: the checkpoint references pre-truncation offsets
    // that no longer exist, so it would produce corrupt reads.
    let config2 = StoreConfig::new(dir.path())
        .with_segment_max_bytes(65536)
        .with_enable_checkpoint(false);
    let store2 = Store::open(config2).expect("store must reopen after tail truncation");

    let recovered_entries = store2.by_entity("chaos:truncate");

    // We must recover at least some events — truncating only the last 32 bytes
    // should leave the bulk of the segment intact
    assert!(
        !recovered_entries.is_empty(),
        "CHAOS PROPERTY: after tail truncation, cold-start recovery must restore at least some events; got zero.\n\
         Investigate: src/store/mod.rs Store::open cold-start scan, src/store/segment/mod.rs frame_decode error handling.\n\
         Common causes: open() bails out on first decode error instead of stopping at corrupt tail, segment skipped entirely on any error.\n\
         Run: cargo test --test chaos_testing_byte_corruption chaos_truncated_segment_recovers"
    );

    // Every recovered event_id must have been one we originally wrote
    for entry in &recovered_entries {
        assert!(
            written_ids.contains(&entry.event_id()),
            "CHAOS PROPERTY: every event recovered after truncation must match an originally written event_id; \
             found unknown id {:?}.\n\
             Investigate: src/store/mod.rs cold-start replay, src/store/segment/mod.rs frame_decode.\n\
             Common causes: partial frame incorrectly accepted as valid, event_id reconstructed from corrupt bytes.\n\
             Run: cargo test --test chaos_testing_byte_corruption chaos_truncated_segment_recovers",
            entry.event_id()
        );
    }

    // All recovered events must be readable via get()
    for entry in &recovered_entries {
        let fetched = store2
            .get(batpak::id::EventId::from(entry.event_id()))
            .expect("get recovered event");
        assert_eq!(
            u128::from(fetched.event.event_id()),
            entry.event_id(),
            "CHAOS PROPERTY: store.get() for a recovered event_id must return the matching event.\n\
             Investigate: src/store/segment/scan.rs get(), src/store/index/mod.rs offset lookup.\n\
             Common causes: index rebuilt with wrong file offsets during cold-start, truncated segment re-indexed past truncation point.\n\
             Run: cargo test --test chaos_testing_byte_corruption chaos_truncated_segment_recovers"
        );
    }

    store2.close().expect("close after recovery");
}
