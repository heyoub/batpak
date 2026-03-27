//! Tests for critical coverage gaps identified by deterministic audit.
//! Targets: frame_decode edge cases, subscription lifecycle, concurrent ops,
//! config edge cases, Store drop behavior.

use batpak::prelude::*;
use std::io::Write;
use tempfile::TempDir;

fn test_store(dir: &TempDir) -> Store {
    let mut config = StoreConfig::new(dir.path());
    config.segment_max_bytes = 64 * 1024;
    Store::open(config).expect("open store")
}

fn test_coord() -> Coordinate {
    Coordinate::new("entity:test", "scope:test").expect("coord")
}

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

// ===== Subscription lifecycle =====

#[test]
fn subscription_recv_returns_none_on_store_drop() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);
    let region = Region::entity("entity:test");
    let sub = store.subscribe(&region);

    // Spawn a thread that will block on recv
    let handle = std::thread::spawn(move || sub.recv());

    // Small delay then drop the store to close channels
    std::thread::sleep(std::time::Duration::from_millis(50));
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
    let sub = store.subscribe(&region);

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
            let s = store.clone();
            let c = coord.clone();
            std::thread::spawn(move || {
                for i in 0..n_per_thread {
                    s.append(&c, kind, &format!("t{t}_e{i}")).expect("append");
                }
            })
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

    // Find segment files and append garbage to one
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
    assert!(!segments.is_empty(), "should have segment files");

    // Append garbage to first segment
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
