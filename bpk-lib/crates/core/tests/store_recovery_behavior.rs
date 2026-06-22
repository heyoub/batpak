//! Advanced Store segment, fd-budget, and cold-start recovery integration tests.

use batpak::store::{Store, StoreConfig, StoreError};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

// --- fd_budget LRU eviction ---

#[test]
fn fd_budget_evicts_oldest_segments() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(512) // tiny segments -> many segment files
        .with_sync_every_n_events(1)
        .with_fd_budget(2); // only 2 FDs allowed -> forces eviction
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:fd", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Write enough events to create many segments (>2, exceeding fd_budget)
    for i in 0..100 {
        store
            .append(
                &coord,
                kind,
                &serde_json::json!({"data": format!("payload_{i}")}),
            )
            .expect("append");
    }
    store.sync().expect("sync");

    // Verify: multiple segments created
    let segment_count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "fbat"))
        .count();
    assert!(
        segment_count > 2,
        "PROPERTY: writing 100 events with segment_max_bytes=512 must create more than 2 segment files.\n\
         Investigate: src/store/write/writer.rs segment rotation logic.\n\
         Common causes: rotation threshold not honoured, all events written to one segment.\n\
         Run: cargo test --test store_recovery_behavior fd_budget_evicts_oldest_segments"
    );

    // Read events from different segments — this exercises LRU eviction
    // because fd_budget=2 but we have >2 segments
    let entries = store.by_entity("entity:fd");
    assert_eq!(
        entries.len(),
        100,
        "PROPERTY: stream must return all 100 appended events even when fd_budget forces LRU eviction.\n\
         Investigate: src/store/segment/scan.rs get_fd LRU cache, src/store/mod.rs stream.\n\
         Common causes: evicted segment FD not re-opened on next access, stream skips closed segments.\n\
         Run: cargo test --test store_recovery_behavior fd_budget_evicts_oldest_segments"
    );

    // Read first event (oldest segment), last event (newest), then first again
    // This forces eviction: open seg1, open seg_last (evicts seg1 if budget=2),
    // then re-open seg1 (evicts seg_last)
    let first = store.get(entries[0].event_id()).expect("get first");
    let last = store.get(entries[99].event_id()).expect("get last");
    let first_again = store
        .get(entries[0].event_id())
        .expect("get first again after eviction");

    assert_eq!(
        first.event.event_id(),
        first_again.event.event_id(),
        "PROPERTY: re-reading the same event after LRU fd eviction must return the identical event_id.\n\
         Investigate: src/store/segment/scan.rs get_fd LRU cache.\n\
         Common causes: evicted segment FD reopened to wrong offset, cache key collision after eviction.\n\
         Run: cargo test --test store_recovery_behavior fd_budget_evicts_oldest_segments"
    );

    // Verify event identity integrity through eviction cycles
    assert_eq!(
        first.event.event_kind(),
        last.event.event_kind(),
        "PROPERTY: EventKind must be identical for events written with the same kind, \
         even when read from different segments after LRU eviction.\n\
         Investigate: src/store/segment/scan.rs get_fd LRU cache, src/store/segment/mod.rs read_frame.\n\
         Common causes: frame data corrupted during eviction cycle, wrong frame decoded after re-open.\n\
         Run: cargo test --test store_recovery_behavior fd_budget_evicts_oldest_segments"
    );

    store.close().expect("close");
}

// --- corrupt segment recovery ---

#[test]
fn cold_start_skips_corrupt_segment_gracefully() {
    let dir = TempDir::new().expect("temp dir");
    let kind = EventKind::custom(0xF, 1);

    // Phase 1: populate with good data
    {
        let config = StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1);
        let store = Store::open(config).expect("open");
        let coord = Coordinate::new("entity:corrupt", "scope:test").expect("valid coord");
        for i in 0..20 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: create a corrupt segment file (bad magic)
    let corrupt_path = dir.path().join("999999.fbat");
    std::fs::write(&corrupt_path, b"BAAD_not_a_real_segment").expect("write corrupt");

    // Phase 3: cold start — should skip the corrupt segment
    // The store should either skip it or error on it.
    // scan_segment checks magic bytes and returns CorruptSegment error.
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(512);
    // `Store` doesn't implement Debug (it owns Arc'd internal state), so map the
    // Ok payload to `()` before `expect_err` to satisfy the Debug bound.
    let err = Store::open(config).map(|_| ()).expect_err(
        "PROPERTY: Store::open must return Err when a segment file has an \
         invalid magic header. Investigate: src/store/segment/scan.rs scan_segment \
         magic check. Common causes: magic bytes check skipped or returns \
         Ok(empty), corrupt file silently ignored.",
    );
    assert!(
        matches!(err, StoreError::CorruptSegment { .. }),
        "PROPERTY: invalid magic header must surface as StoreError::CorruptSegment, got {err:?}"
    );
}

#[test]
fn corrupt_frame_in_segment_is_detected() {
    // Write good events, then inject a corrupt frame into the segment file.
    // Verify cold start detects committed-frame corruption instead of silently
    // omitting the bad event and returning a partial index.
    let dir = TempDir::new().expect("temp dir");
    let kind = EventKind::custom(0xF, 1);

    // Phase 1: populate with good data and sync
    {
        let config = StoreConfig::new(dir.path())
            .with_sync_every_n_events(1)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false);
        let store = Store::open(config).expect("open");
        let coord = Coordinate::new("entity:crc", "scope:test").expect("valid");
        for i in 0..3 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: corrupt a segment file by flipping bytes in the middle.
    // Sort by file_name so the chosen segment is deterministic across
    // filesystems (POSIX `readdir` makes no order guarantee).
    let mut segments: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "fbat"))
        .collect();
    segments.sort_by_key(|e| e.file_name());
    assert!(
        !segments.is_empty(),
        "PROPERTY: after appending events and syncing, at least one .fbat segment file must exist.\n\
         Investigate: src/store/write/writer.rs sync, src/store/segment/mod.rs write path.\n\
         Common causes: sync no-op, segment file never flushed to disk, wrong extension used.\n\
         Run: cargo test --test store_recovery_behavior corrupt_frame_in_segment_is_detected"
    );

    let seg_path = segments[0].path();
    let mut data = std::fs::read(&seg_path).expect("read segment");
    let sidx_start = usize::try_from(u64::from_le_bytes(
        data[data.len() - 16..data.len() - 8]
            .try_into()
            .expect("SIDX trailer offset"),
    ))
    .expect("bounded SIDX trailer offset");
    data.truncate(sidx_start);
    let header_len = usize::try_from(u32::from_be_bytes(
        data[4..8].try_into().expect("header len"),
    ))
    .expect("bounded header len");
    let frame_offset = 8 + header_len;
    let payload_len = usize::try_from(u32::from_be_bytes(
        data[frame_offset..frame_offset + 4]
            .try_into()
            .expect("frame payload len"),
    ))
    .expect("bounded frame payload len");
    let frame_end = frame_offset + 8 + payload_len;
    assert!(
        frame_end + 8 < data.len(),
        "PROPERTY: fixture must corrupt a fully committed middle frame, not the torn tail"
    );
    let corrupt_at = frame_offset + 8 + (payload_len / 2);
    data[corrupt_at] ^= 0xFF;
    std::fs::write(&seg_path, &data).expect("write corrupted segment");

    // Phase 3: cold start must fail closed. The old behavior logged the bad
    // frame and returned a partial index, which made committed data loss look
    // like successful recovery.
    let opened = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    );
    // If the store opened, capture how many events it surfaced so the failure
    // diagnostic is honest, then map the Ok payload to that count for `expect_err`.
    let err = opened
        .map(|store| {
            let event_count = store.stats().event_count;
            let _ = store.close();
            event_count
        })
        .expect_err(
            "PROPERTY: committed middle-frame corruption must fail closed instead of \
             opening successfully. Investigate: src/store/segment/scan/full_scan.rs and \
             recovery.rs corrupt-frame handling.",
        );
    assert!(
        matches!(
            err,
            StoreError::CrcMismatch { .. } | StoreError::CorruptSegment { .. }
        ),
        "PROPERTY: committed middle-frame corruption must surface as corruption evidence, got {err:?}"
    );
}
