// justifies: edge-case tests spawn threads for concurrent stress probes, rely on panic as the assertion style, and intentionally build config via field-by-field mutation; these allows are the file-wide idioms.
#![allow(
    clippy::disallowed_methods,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::field_reassign_with_default
)]
//! Store edge case tests: frame_decode error paths, subscription lifecycle,
//! concurrent append correctness, config edge cases, Store drop behavior.
//!
//! PROVES: LAW-006 (Bidirectional Traceability — audit findings drove these tests)
//! DEFENDS: FM-011 (Error Path Hollowing), FM-013 (Coverage Mirage)
//! INVARIANTS: INV-TYPE (frame decode totality), INV-CONC (concurrent appends)

use batpak::prelude::*;
use std::io::Write;
use tempfile::TempDir;

mod common;
use common::medium_segment_store as test_store;
use common::test_coord;

// ===== frame_decode edge cases =====

#[test]
fn frame_decode_too_short() {
    use batpak::store::segment::{frame_decode, FrameDecodeError};
    let buf = [0u8; 7]; // less than 8 bytes
    match frame_decode(&buf) {
        Err(FrameDecodeError::TooShort) => {}
        other => panic!("expected TooShort, got {other:?}"),
    }
}

#[test]
fn frame_decode_truncated() {
    use batpak::store::segment::{frame_decode, FrameDecodeError};
    // Header says 100 bytes of payload, but only 8+4=12 bytes provided
    let mut buf = vec![0u8; 12];
    buf[0..4].copy_from_slice(&100u32.to_be_bytes()); // len = 100
    buf[4..8].copy_from_slice(&0u32.to_be_bytes()); // crc = 0
    match frame_decode(&buf) {
        Err(FrameDecodeError::Truncated {
            expected_len,
            available,
        }) => {
            assert_eq!(expected_len, 108);
            assert_eq!(available, 12);
        }
        other => panic!("expected Truncated, got {other:?}"),
    }
}

#[test]
fn frame_decode_crc_mismatch() {
    use batpak::store::segment::{frame_decode, FrameDecodeError};
    let payload = b"hello";
    // justifies: b"hello" has len 5, far below u32::MAX, so usize-to-u32 narrowing cannot truncate in this fixed-size test payload.
    #[allow(clippy::cast_possible_truncation)]
    let len = payload.len() as u32;
    let bad_crc = 0xDEADBEEFu32;
    let mut buf = Vec::new();
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&bad_crc.to_be_bytes());
    buf.extend_from_slice(payload);
    match frame_decode(&buf) {
        Err(FrameDecodeError::CrcMismatch { .. }) => {}
        other => panic!("expected CrcMismatch, got {other:?}"),
    }
}

#[test]
fn frame_decode_valid_round_trip() {
    use batpak::store::segment::{frame_decode, frame_encode};
    let data = "test_data";
    let frame = frame_encode(&data).expect("encode");
    let (msgpack, consumed) = frame_decode(&frame).expect("decode");
    assert_eq!(consumed, frame.len());
    let decoded: String = rmp_serde::from_slice(msgpack).expect("deserialize");
    assert_eq!(decoded, "test_data");
}

#[test]
fn append_frames_from_segment_copies_frame_bytes_exactly() {
    use batpak::store::segment::{
        frame_encode, segment_filename, Active, Sealed, Segment, SEGMENT_MAGIC,
    };

    fn frame_bytes(path: &std::path::Path) -> Vec<u8> {
        let bytes = std::fs::read(path).expect("read segment");
        assert_eq!(
            &bytes[..4],
            SEGMENT_MAGIC,
            "segment should start with FBAT magic"
        );
        let header_len = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        bytes[(8 + header_len)..].to_vec()
    }

    let dir = TempDir::new().expect("tmpdir");
    let source_path;
    {
        let mut source: Segment<Active> =
            Segment::create(dir.path(), 1).expect("create source segment");
        let frame_a = frame_encode(&serde_json::json!({"a": 1})).expect("encode frame a");
        let frame_b = frame_encode(&serde_json::json!({"b": 2})).expect("encode frame b");
        source.write_frame(&frame_a).expect("write frame a");
        source.write_frame(&frame_b).expect("write frame b");
        source
            .sync_with_mode(&SyncMode::SyncData)
            .expect("sync source");
        source_path = source.path.clone();
        let _sealed: Segment<Sealed> = source.seal();
    }

    let destination_path;
    {
        let mut destination: Segment<Active> =
            Segment::create(dir.path(), 2).expect("create destination segment");
        destination
            .append_frames_from_segment(&source_path)
            .expect("append frames");
        destination
            .sync_with_mode(&SyncMode::SyncData)
            .expect("sync destination");
        destination_path = destination.path.clone();
        let _sealed: Segment<Sealed> = destination.seal();
    }

    let expected_source_name = segment_filename(1);
    let expected_destination_name = segment_filename(2);
    assert!(
        source_path.ends_with(&expected_source_name),
        "source segment path should end with the canonical segment filename"
    );
    assert!(
        destination_path.ends_with(&expected_destination_name),
        "destination segment path should end with the canonical segment filename"
    );

    assert_eq!(
        frame_bytes(&destination_path),
        frame_bytes(&source_path),
        "APPEND FRAMES: destination segment should contain exactly the source frame bytes after both headers are stripped."
    );
}

#[test]
fn segment_needs_rotation_tracks_written_bytes_threshold() {
    use batpak::store::segment::{frame_encode, Active, Segment};

    let dir = TempDir::new().expect("tmpdir");
    let mut segment: Segment<Active> = Segment::create(dir.path(), 1).expect("create segment");
    let frame =
        frame_encode(&serde_json::json!({"payload": "rotation-threshold"})).expect("encode frame");
    let initially_needs_rotation = segment.needs_rotation(1024);

    assert!(
        !initially_needs_rotation,
        "PROPERTY: a fresh segment must not report rotation before any frames are written"
    );

    segment.write_frame(&frame).expect("write frame");
    let needs_rotation_at_one_byte = segment.needs_rotation(1);
    let still_below_large_threshold = segment.needs_rotation(1024);

    assert!(
        needs_rotation_at_one_byte,
        "PROPERTY: needs_rotation(max_bytes=1) must flip true after any real frame write"
    );
    assert!(
        !still_below_large_threshold,
        "PROPERTY: needs_rotation must stay false when written_bytes remains below the threshold"
    );
}

// ===== Subscription lifecycle =====

#[test]
fn subscription_recv_returns_none_on_store_drop() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);
    let region = Region::entity("entity:test");
    let sub = store.subscribe_lossy(&region);

    // Spawn a thread that will block on recv
    let handle = std::thread::Builder::new()
        .name("store-edge-sub-recv-block".into())
        .spawn(move || sub.recv())
        .expect("spawn subscription recv thread");

    // Drop the store immediately — no sleep needed. subscribe() registers synchronously,
    // so the subscriber thread's recv() is already blocked (or will be before recv()
    // ever returns a value). Dropping the store closes the broadcaster; recv() returns
    // None and the thread exits cleanly.
    drop(store);

    let result = handle.join().expect("thread join");
    assert!(
        result.is_none(),
        "recv should return None when store is dropped"
    );
}

#[test]
fn subscription_filters_by_region_in_recv_loop() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);
    let kind = EventKind::custom(1, 1);
    let coord_a = Coordinate::new("entity:a", "scope:test").expect("coord");
    let coord_b = Coordinate::new("entity:b", "scope:test").expect("coord");

    // Subscribe only to entity:a
    let region = Region::entity("entity:a");
    let sub = store.subscribe_lossy(&region);

    // Append to entity:b first (should be filtered), then entity:a
    store.append(&coord_b, kind, &"ignored").expect("append b");
    store.append(&coord_a, kind, &"wanted").expect("append a");

    // recv should skip entity:b and return entity:a
    let notif = sub.recv().expect("should get notification");
    assert_eq!(notif.coord.entity(), "entity:a");
}

// ===== Store drop without close =====

#[test]
fn store_drop_without_close_persists_data() {
    let dir = TempDir::new().expect("tmpdir");
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);

    // Write and drop without calling close()
    {
        let store = test_store(&dir);
        store.append(&coord, kind, &"event1").expect("append");
        store.sync().expect("sync");
        // No close() — just drop
    }

    // Reopen and verify data survived
    let store = test_store(&dir);
    let events = store.stream("entity:test");
    assert_eq!(events.len(), 1, "event should survive drop-without-close");
}

// ===== Config edge cases =====

#[test]
fn segment_max_bytes_very_small_forces_frequent_rotation() {
    let dir = TempDir::new().expect("tmpdir");
    let mut config = StoreConfig::new(dir.path());
    config.segment_max_bytes = 128; // Tiny — forces rotation after ~1 event
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);

    for i in 0..5 {
        store
            .append(&coord, kind, &format!("event_{i}"))
            .expect("append");
    }
    store.sync().expect("sync");

    // Verify all events survived despite frequent rotation
    let events = store.stream("entity:test");
    assert_eq!(
        events.len(),
        5,
        "all events should survive frequent rotation"
    );
}

#[test]
fn single_append_payload_over_limit_is_rejected_cleanly() {
    let dir = TempDir::new().expect("tmpdir");
    let config = StoreConfig::new(dir.path()).with_single_append_max_bytes(8);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);
    let payload = "this payload is larger than eight bytes";

    let err = match store.append(&coord, kind, &payload) {
        Ok(_) => panic!("PROPERTY: oversized payload should not append successfully"),
        Err(err) => err,
    };

    assert!(
        matches!(err, StoreError::Configuration(ref msg) if msg.contains("single append bytes")),
        "expected Configuration payload-limit error, got {err:?}"
    );
}

/// MUTATION GATE: the boundary check uses `>` (strictly greater), so a
/// payload that serializes to exactly max bytes MUST succeed. The mutant
/// `> → >=` would reject this exact-boundary payload and fail this test.
#[test]
fn single_append_payload_at_exact_limit_succeeds() {
    let dir = TempDir::new().expect("tmpdir");
    // Pick a limit large enough to hold a small msgpack-serialized string
    // but small enough that one extra byte would fail.
    let limit: u32 = 64;
    let config = StoreConfig::new(dir.path()).with_single_append_max_bytes(limit);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);

    // Build a payload whose msgpack serialization is exactly `limit` bytes.
    // Strategy: start with a string, measure, and adjust.
    let mut s = String::new();
    loop {
        let bytes = rmp_serde::to_vec_named(&s).expect("serialize");
        if bytes.len() == limit as usize {
            break;
        }
        if bytes.len() > limit as usize {
            panic!("overshot: could not construct a payload of exactly {limit} msgpack bytes");
        }
        s.push('A');
    }

    // This append MUST succeed: payload is exactly max, check is `> max` not `>= max`.
    store
        .append(&coord, kind, &s)
        .expect("PROPERTY: payload of exactly single_append_max_bytes must be accepted");

    // One more byte MUST fail.
    s.push('B');
    let result = store.append(&coord, kind, &s);
    assert!(
        result.is_err(),
        "PROPERTY: payload exceeding single_append_max_bytes must be rejected"
    );
}

#[test]
fn coordinate_component_length_limit_is_enforced() {
    let long = "x".repeat(batpak::coordinate::MAX_COORDINATE_COMPONENT_LEN + 1);

    let entity_err = match Coordinate::new(&long, "scope:test") {
        Ok(_) => panic!("PROPERTY: overlong entity should be rejected"),
        Err(err) => err,
    };
    assert!(
        matches!(entity_err, CoordinateError::EntityTooLong { .. }),
        "expected EntityTooLong, got {entity_err:?}"
    );

    let scope_err = match Coordinate::new("entity:test", &long) {
        Ok(_) => panic!("PROPERTY: overlong scope should be rejected"),
        Err(err) => err,
    };
    assert!(
        matches!(scope_err, CoordinateError::ScopeTooLong { .. }),
        "expected ScopeTooLong, got {scope_err:?}"
    );
}

#[cfg(unix)]
#[test]
fn close_rejects_checkpoint_symlink_leaf() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().expect("tmpdir");
    // Disable mmap so close() exercises the checkpoint write path.
    // With mmap enabled (default), checkpoint is skipped and the symlink
    // is never touched.
    let config = StoreConfig::new(dir.path())
        .with_enable_checkpoint(true)
        .with_enable_mmap_index(false);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);
    store.append(&coord, kind, &"event").expect("append");

    let target = dir.path().join("attacker-target.ckpt");
    std::fs::write(&target, b"sentinel").expect("write target");
    let checkpoint_path = dir.path().join("index.ckpt");
    symlink(&target, &checkpoint_path).expect("create checkpoint symlink");

    let err = match store.close() {
        Ok(_) => panic!("PROPERTY: close should reject checkpoint symlink leaf"),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::Io(ref io) if io.kind() == std::io::ErrorKind::InvalidInput),
        "expected Io(InvalidInput), got {err:?}"
    );

    assert_eq!(
        std::fs::read(&target).expect("read target"),
        b"sentinel",
        "checkpoint hardening must not clobber the symlink target"
    );
}

#[cfg(unix)]
#[test]
fn open_with_native_cache_rejects_symlink_leaf() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().expect("tmpdir");
    let cache_real = dir.path().join("cache-real");
    std::fs::create_dir_all(&cache_real).expect("create real cache dir");
    let cache_link = dir.path().join("cache-link");
    symlink(&cache_real, &cache_link).expect("create cache symlink");

    let err = match Store::open_with_native_cache(StoreConfig::new(dir.path()), &cache_link) {
        Ok(_) => panic!("PROPERTY: native cache root symlink should be rejected"),
        Err(err) => err,
    };

    assert!(
        matches!(err, StoreError::CacheFailed(_)),
        "expected CacheFailed, got {err:?}"
    );
}

// ===== Concurrent append same entity =====

#[test]
fn concurrent_appends_same_entity_all_persisted() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);
    let store = std::sync::Arc::new(store);
    let n_threads = 4;
    let n_per_thread = 25;

    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let s = std::sync::Arc::<Store>::clone(&store);
            let c = coord.clone();
            std::thread::Builder::new()
                .name(format!("store-edge-concurrent-append-{t}"))
                .spawn(move || {
                    for i in 0..n_per_thread {
                        s.append(&c, kind, &format!("t{t}_e{i}")).expect("append");
                    }
                })
                .expect("spawn concurrent append thread")
        })
        .collect();

    for h in handles {
        h.join().expect("thread join");
    }

    store.sync().expect("sync");
    let events = store.stream("entity:test");
    assert_eq!(
        events.len(),
        n_threads * n_per_thread,
        "all concurrent appends should be persisted"
    );

    // Verify global sequences are unique (contiguous is an implementation detail)
    let mut global_seqs: Vec<u64> = events.iter().map(|e| e.global_sequence).collect();
    global_seqs.sort();
    global_seqs.dedup();
    assert_eq!(
        global_seqs.len(),
        n_threads * n_per_thread,
        "all global sequences should be unique"
    );
}

// ===== Compaction edge cases =====

#[test]
fn compact_skips_when_below_min_segments() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);

    // Append just a few events (won't fill a segment)
    store.append(&coord, kind, &"e1").expect("append");
    store.sync().expect("sync");

    let mut compact_config = CompactionConfig::default();
    compact_config.min_segments = 10; // High threshold — won't trigger
    let result = store.compact(&compact_config).expect("compact");
    assert_eq!(
        result.segments_removed, 0,
        "should skip compaction below min_segments"
    );
}

// ===== Scan segment with corrupt data =====

#[test]
fn scan_recovers_events_before_corruption() {
    let dir = TempDir::new().expect("tmpdir");
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);

    // Write some events, sync, then corrupt the last part of the segment
    {
        let store = test_store(&dir);
        for i in 0..5 {
            store
                .append(&coord, kind, &format!("event_{i}"))
                .expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Find segment files and append garbage to one. Sort by file_name so the
    // chosen segment is deterministic across filesystems — POSIX `readdir`
    // makes no order guarantee, and `remove(0)` on an unsorted Vec used to
    // pick a different file on ext4 vs tmpfs vs APFS.
    let mut segments: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .collect();
    segments.sort_by_key(|e| e.file_name());
    assert!(!segments.is_empty(), "should have segment files");

    // Append garbage to the (deterministically sorted) first segment.
    let seg_path = segments.remove(0).path();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&seg_path)
        .expect("open segment");
    f.write_all(&[0xFF; 64]).expect("write garbage");
    drop(f);

    // Reopen — should recover events before corruption
    let store = test_store(&dir);
    let events = store.stream("entity:test");
    assert!(
        events.len() >= 3,
        "should recover at least some events before corruption, got {}",
        events.len()
    );
}
