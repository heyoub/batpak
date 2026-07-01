use super::*;
use crate::coordinate::DagPosition;
use crate::store::index::DiskPos;
use std::io::ErrorKind;
use tempfile::TempDir;

struct FailingRead {
    kind: ErrorKind,
}

impl std::io::Read for FailingRead {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(self.kind))
    }
}

fn test_clock() -> std::sync::Arc<dyn crate::store::Clock> {
    std::sync::Arc::new(crate::store::SystemClock::new())
}

fn test_fs() -> std::sync::Arc<dyn crate::store::platform::fs::StoreFs> {
    std::sync::Arc::new(crate::store::platform::fs::RealFs)
}

fn test_reader() -> (Reader, TempDir) {
    let dir = TempDir::new().expect("create temp dir for reader test");
    let reader = Reader::new(dir.path().to_path_buf(), 4, &test_clock(), test_fs());
    (reader, dir)
}

fn write_segment_bytes(dir: &TempDir, segment_id: u64, bytes: &[u8]) {
    let path = dir.path().join(segment::segment_filename(segment_id));
    std::fs::write(&path, bytes).expect("write segment bytes");
}

#[test]
fn read_frame_header_policy_treats_unexpected_eof_as_clean_end() {
    let mut reader = FailingRead {
        kind: ErrorKind::UnexpectedEof,
    };

    let result = read_frame_header_or_clean_eof(&mut reader).expect("EOF should be non-fatal");

    assert!(
        result.is_none(),
        "PROPERTY: EOF while reading the next frame header is the clean segment terminator"
    );
}

#[test]
fn read_frame_header_policy_surfaces_non_eof_io_errors() {
    let mut reader = FailingRead {
        kind: ErrorKind::PermissionDenied,
    };

    let result = read_frame_header_or_clean_eof(&mut reader);

    assert!(
        matches!(result, Err(error) if error.kind() == ErrorKind::PermissionDenied),
        "PROPERTY: non-EOF frame-header read errors must surface as I/O failures"
    );
}

#[test]
fn frame_decode_error_mapping_preserves_segment_and_offset_context() {
    fn assert_error_trait<E: std::error::Error>() {}

    assert_error_trait::<segment::FrameDecodeError>();

    let crc_error = Reader::frame_decode_error(
        7,
        42,
        segment::FrameDecodeError::CrcMismatch {
            expected: 0xAAAA_AAAA,
            actual: 0xBBBB_BBBB,
        },
    );
    assert!(
        matches!(
            crc_error,
            StoreError::CrcMismatch {
                segment_id: 7,
                offset: 42
            }
        ),
        "PROPERTY: frame CRC failures must retain exact disk position context"
    );

    let truncated_error = Reader::frame_decode_error(
        7,
        42,
        segment::FrameDecodeError::Truncated {
            expected_len: 16,
            available: 12,
        },
    );
    assert!(
        matches!(
            truncated_error,
            StoreError::CorruptSegment { segment_id: 7, ref detail }
            if detail.contains("frame at offset 42")
                && detail.contains("frame truncated: expected 16 bytes, got 12")
        ),
        "PROPERTY: structural frame decode failures must retain segment, offset, and decode reason; got {truncated_error:?}"
    );
}

#[test]
fn acquire_buffer_returns_requested_size() {
    let (reader, _dir) = test_reader();
    let buf = reader.acquire_buffer(256);
    assert_eq!(
        buf.len(),
        256,
        "ACQUIRE BUFFER: expected buffer of size 256, got {}.\n\
         Check: src/store/segment/scan.rs acquire_buffer() vec allocation.",
        buf.len()
    );
    assert!(
        buf.iter().all(|&b| b == 0),
        "ACQUIRE BUFFER: newly allocated buffer should be zero-initialized."
    );
}

#[test]
fn released_buffer_is_zero_filled_and_resized_on_next_acquire() {
    let (reader, _dir) = test_reader();

    let mut buf = reader.acquire_buffer(128);
    for byte in buf.iter_mut() {
        *byte = 0xAB;
    }
    reader.release_buffer(buf);

    let buf2 = reader.acquire_buffer(64);
    assert_eq!(
        buf2.len(),
        64,
        "PROPERTY: re-acquired buffer must match the requested size, \
         regardless of whether it came from the pool or a fresh allocation. \
         Investigate: src/store/segment/scan.rs acquire_buffer resize path."
    );
    assert!(
        buf2.iter().all(|&b| b == 0),
        "PROPERTY: re-acquired buffer must be zero-filled. A non-zero byte \
         means the previous user's data leaked into the new acquirer, \
         which is a memory-safety / information-disclosure bug. \
         Investigate: src/store/segment/scan.rs acquire_buffer fill path."
    );
}

#[test]
fn buffer_pool_does_not_grow_unboundedly() {
    let (reader, _dir) = test_reader();

    for _ in 0..100 {
        reader.release_buffer(vec![0u8; 1024]);
    }

    for i in 0..100 {
        let buf = reader.acquire_buffer(1024);
        assert_eq!(
            buf.len(),
            1024,
            "PROPERTY: buffer {i} of 100 must be the requested size."
        );
        assert!(
            buf.iter().all(|&b| b == 0),
            "PROPERTY: buffer {i} of 100 must be zero-filled."
        );
    }
}

#[test]
fn acquire_buffer_satisfies_contract_on_empty_pool() {
    let (reader, _dir) = test_reader();

    let buf = reader.acquire_buffer(512);
    assert_eq!(
        buf.len(),
        512,
        "PROPERTY: acquire_buffer on a fresh reader must return the \
         requested size. Investigate: src/store/segment/scan.rs allocation \
         path when pool is empty."
    );
    assert!(
        buf.iter().all(|&b| b == 0),
        "PROPERTY: a freshly allocated buffer must be zero-filled."
    );
}

#[test]
fn buffer_pool_retains_at_most_sixteen_released_buffers() {
    let (reader, _dir) = test_reader();

    for _ in 0..17 {
        reader.release_buffer(vec![0u8; 32]);
    }

    let retained = reader.buffer_pool.lock().len();
    assert_eq!(
        retained, 16,
        "PROPERTY: release_buffer must cap the internal pool at exactly 16 buffers; \
         retaining a seventeenth buffer weakens the bounded-memory contract"
    );
}

#[test]
fn batch_marker_payload_decode_ignores_marker_payload_bytes() {
    let header = EventHeader::new(
        1,
        1,
        None,
        1,
        DagPosition::root(),
        0,
        EventKind::SYSTEM_BATCH_BEGIN,
    );
    let event = Event {
        header,
        payload: vec![0xC1],
        hash_chain: Some(HashChain::default()),
    };
    let frame = FramePayload {
        event,
        entity: "entity:batch-marker".to_owned(),
        scope: "scope:test".to_owned(),
        receipt_extensions: BTreeMap::new(),
    };
    let encoded = crate::encoding::to_bytes(&frame).expect("encode batch marker frame");

    let decoded = Reader::decode_frame_payload_value(&encoded)
        .expect("batch marker payload bytes are ignored by value decode");

    assert_eq!(
        decoded.event.payload,
        serde_json::Value::Null,
        "PROPERTY: SYSTEM_BATCH_BEGIN/COMMIT markers carry count semantics in the header; \
         value decoding must not deserialize their raw marker payload bytes"
    );
}

#[test]
fn set_active_segment_advances_the_sealed_cutoff() {
    let (reader, _dir) = test_reader();

    reader.set_active_segment(7);

    assert_eq!(reader.active_segment_id(), 7);
    assert!(
        reader.is_sealed(6),
        "PROPERTY: segments older than the configured active segment must be treated as sealed"
    );
    assert!(
        !reader.is_sealed(7),
        "PROPERTY: the configured active segment itself must stay writable/non-sealed"
    );
    assert!(
        !reader.is_sealed(8),
        "PROPERTY: future segment ids must not be treated as sealed before rotation reaches them"
    );
}

#[test]
fn read_active_frame_into_reads_the_full_requested_slice() {
    let (reader, dir) = test_reader();
    write_segment_bytes(&dir, 0, b"0123456789abcdef");

    let pos = DiskPos::new(0, 3, 5);
    let mut buf = [0u8; 5];
    reader
        .read_active_frame_into(&pos, &mut buf)
        .expect("read active bytes");

    assert_eq!(
        &buf, b"34567",
        "PROPERTY: active-segment reads must advance until the caller's buffer is fully populated"
    );
}

#[test]
fn checked_frame_range_rejects_overflow_and_oversized_lengths() {
    assert!(
        Reader::checked_frame_range(1, u64::MAX, 16, 1024).is_err(),
        "PROPERTY: a frame range that overflows u64 must be rejected"
    );
    assert!(
        Reader::checked_frame_len(1, 4).is_err(),
        "PROPERTY: a frame shorter than the fixed header must be rejected"
    );
    assert!(
        Reader::checked_frame_len(
            1,
            u32::try_from(FRAME_HEADER_BYTES).expect("frame header size fits u32")
        )
        .is_ok(),
        "PROPERTY: a frame length exactly equal to the frame header size is the minimum valid empty-payload frame"
    );
    assert!(Reader::checked_frame_len(
        1,
        u32::try_from(FRAME_HEADER_BYTES + segment::MAX_FRAME_PAYLOAD)
            .expect("max frame length fits u32")
    )
    .is_ok());
    assert!(
        Reader::checked_frame_len(
            1,
            u32::try_from(FRAME_HEADER_BYTES + segment::MAX_FRAME_PAYLOAD + 1)
                .expect("one-past-max frame length fits u32")
        )
        .is_err(),
        "PROPERTY: a frame one byte above MAX_FRAME_PAYLOAD must be rejected"
    );
    assert!(
        Reader::checked_frame_len(1, u32::MAX).is_err(),
        "PROPERTY: a frame length at u32::MAX must be rejected before allocation"
    );
}

#[test]
fn payload_len_exceeds_max_respects_the_exact_boundary() {
    assert!(
        !Reader::payload_len_exceeds_max(segment::MAX_FRAME_PAYLOAD),
        "PROPERTY: a frame exactly at MAX_FRAME_PAYLOAD remains valid"
    );
    assert!(
        Reader::payload_len_exceeds_max(segment::MAX_FRAME_PAYLOAD + 1),
        "PROPERTY: a frame one byte past MAX_FRAME_PAYLOAD must stop scan/recovery before allocation"
    );
}

#[test]
fn checked_header_len_respects_the_exact_boundary() {
    assert_eq!(
        Reader::checked_header_len(7, segment::MAX_SEGMENT_HEADER)
            .expect("a header exactly at MAX_SEGMENT_HEADER remains valid"),
        segment::MAX_SEGMENT_HEADER,
        "PROPERTY: a header exactly at MAX_SEGMENT_HEADER is accepted unchanged"
    );

    let err = Reader::checked_header_len(7, segment::MAX_SEGMENT_HEADER + 1)
        .expect_err("a header one byte past MAX_SEGMENT_HEADER must stop scan before allocation");
    assert!(
        matches!(
            err,
            StoreError::CorruptSegment { segment_id: 7, ref detail }
            if detail.contains("exceeds MAX_SEGMENT_HEADER")
        ),
        "PROPERTY: an oversize header_len must be rejected as CorruptSegment before any vec![0u8; header_len] allocation, got {err:?}"
    );
}

#[test]
fn checked_batch_count_rejects_vacuous_or_implausible_counts() {
    assert!(
        Reader::checked_batch_count(1, 0, 0).is_err(),
        "PROPERTY: a batch count of zero is malformed and must be rejected"
    );
    assert!(
        Reader::checked_batch_count(1, 0, MAX_BATCH_RECOVERY_ITEMS + 1).is_err(),
        "PROPERTY: a batch count above MAX_BATCH_RECOVERY_ITEMS is refused before allocation"
    );
    assert_eq!(
        Reader::checked_batch_count(1, 0, MAX_BATCH_RECOVERY_ITEMS)
            .expect("max batch count remains valid"),
        MAX_BATCH_RECOVERY_ITEMS,
        "PROPERTY: the exact MAX_BATCH_RECOVERY_ITEMS boundary is allowed"
    );
    assert_eq!(
        Reader::checked_batch_count(1, 0, 3).expect("valid batch count"),
        3
    );
}

#[test]
fn scan_oom_posture_is_input_bounded_fail_closed() {
    use crate::store::segment;

    assert_eq!(
        segment::MAX_FRAME_PAYLOAD,
        256 * 1024 * 1024,
        "PROPERTY: frame payload cap is fixed before vec allocation"
    );
    assert_eq!(
        segment::MAX_SEGMENT_HEADER,
        64 * 1024,
        "PROPERTY: segment header cap is fixed before vec allocation"
    );
    assert!(
        Reader::payload_len_exceeds_max(segment::MAX_FRAME_PAYLOAD + 1),
        "PROPERTY: oversize frame claims are refused before allocation (no try_reserve path)"
    );
    assert!(
        Reader::checked_batch_count(1, 0, MAX_BATCH_RECOVERY_ITEMS + 1).is_err(),
        "PROPERTY: oversize recovery batch counts are refused before allocation"
    );
}

/// Build a valid single-frame sealed segment on disk via the real `Segment`
/// writer (which routes all file contact through `crate::store::platform`),
/// returning the `DiskPos` of the written frame and its expected event payload.
/// The segment is sealed by closing its file handle; callers then point the
/// reader's active-segment cutoff past `segment_id` to treat it as sealed.
fn write_valid_sealed_segment(
    dir: &TempDir,
    segment_id: u64,
    entity: &str,
    scope: &str,
    payload: &serde_json::Value,
) -> DiskPos {
    // Mirror the writer: the frame stores the event payload as pre-encoded
    // MessagePack bytes (Event<Vec<u8>>), not an inline serde_json::Value.
    let payload_bytes = crate::encoding::to_bytes(payload).expect("encode payload bytes");
    let event = Event {
        header: EventHeader::new(1, 1, None, 1, DagPosition::root(), 0, EventKind::DATA),
        payload: payload_bytes,
        hash_chain: Some(HashChain::default()),
    };
    let frame = segment::FramePayloadRef {
        event: &event,
        entity,
        scope,
        receipt_extensions: &BTreeMap::new(),
    };
    let frame_bytes = segment::frame_encode(&frame).expect("encode frame");

    let fs: std::sync::Arc<dyn crate::store::platform::fs::StoreFs> =
        std::sync::Arc::new(crate::store::platform::fs::RealFs);
    let mut active = segment::Segment::<segment::Active>::create_with_created_ns_on(
        dir.path(),
        segment_id,
        0,
        &fs,
    )
    .expect("create segment");
    let offset = active.write_frame(&frame_bytes).expect("write frame");
    active
        .sync_with_mode(&crate::store::SyncMode::SyncAll)
        .expect("sync segment");
    let _sealed = active.seal();

    DiskPos::new(
        segment_id,
        offset,
        u32::try_from(frame_bytes.len()).expect("frame length fits u32"),
    )
}

#[test]
fn sealed_read_falls_back_to_fd_when_mmap_admission_is_absent() {
    let (mut reader, dir) = test_reader();
    // Force the no-mmap-admission state (as would happen on a host where the
    // one-time probe could not run, e.g. a read-only data dir).
    reader.disable_sealed_mmap_for_test();
    assert!(
        !reader.sealed_mmap_admitted_for_test(),
        "PRECONDITION: the test must exercise the FD fallback, so mmap admission must be absent"
    );

    let payload = serde_json::json!({"v": "sealed-fd-fallback", "n": 7});
    let pos = write_valid_sealed_segment(&dir, 0, "entity:fd", "scope:fallback", &payload);
    // Mark segment 0 as sealed by advancing the active cutoff past it.
    reader.set_active_segment(1);
    assert!(
        reader.is_sealed(pos.segment_id),
        "PRECONDITION: segment 0 must be sealed once the active cutoff is 1"
    );

    let stored = reader
        .read_entry(&pos)
        .expect("FD fallback must read a valid sealed frame when mmap is not admitted");
    assert_eq!(
        stored.event.payload, payload,
        "PROPERTY: the FD/pread fallback must decode the sealed frame byte-identically to the mmap path"
    );

    let event_only = reader
        .read_event_only(&pos)
        .expect("read_event_only must also fall back to FD on a sealed segment");
    assert_eq!(
        event_only.payload, payload,
        "PROPERTY: read_event_only's FD fallback must return the same event payload"
    );

    // No mmap mapping should have been created on the no-admission path.
    assert!(
        reader.sealed_maps.get(&pos.segment_id).is_none(),
        "PROPERTY: with mmap admission absent, sealed reads must not create any memory mapping"
    );
}

#[test]
fn sealed_mmap_and_fd_paths_return_identical_bytes() {
    // Reader A keeps mmap admission (default); reader B is forced to FD fallback.
    let (reader_mmap, dir) = test_reader();
    let payload = serde_json::json!({"v": "parity", "items": [1, 2, 3], "nested": {"k": "x"}});
    let pos = write_valid_sealed_segment(&dir, 0, "entity:parity", "scope:p", &payload);
    reader_mmap.set_active_segment(1);

    // Build a second reader over the SAME data dir and disable mmap on it.
    let mut reader_fd = Reader::new(dir.path().to_path_buf(), 4, &test_clock(), test_fs());
    reader_fd.disable_sealed_mmap_for_test();
    reader_fd.set_active_segment(1);

    let via_mmap = reader_mmap.read_event_raw_only(&pos).expect("mmap read");
    let via_fd = reader_fd.read_event_raw_only(&pos).expect("fd read");
    assert_eq!(
        via_mmap.payload, via_fd.payload,
        "PROPERTY: mmap and FD reads of the same valid sealed frame must yield identical raw bytes \
         (no silent data divergence between the two read paths)"
    );

    let coord_mmap = reader_mmap.read_entry(&pos).expect("mmap entry");
    let coord_fd = reader_fd.read_entry(&pos).expect("fd entry");
    assert_eq!(
        coord_mmap.event.payload, coord_fd.event.payload,
        "PROPERTY: decoded event payloads must match across mmap and FD paths"
    );
}

#[test]
fn corrupt_sealed_frame_surfaces_same_error_class_on_both_paths() {
    let dir = TempDir::new().expect("tmpdir");
    // Build a valid frame, then flip a payload byte so the CRC fails on decode.
    let payload = serde_json::json!({"v": "corruptible"});
    let pos = write_valid_sealed_segment(&dir, 0, "entity:corrupt", "scope:c", &payload);

    // Corrupt one msgpack byte in the frame. The frame layout is
    // [len:u32 BE][crc:u32 BE][msgpack...]; flipping a payload byte leaves the
    // stored CRC stale, so frame_decode must report CrcMismatch on BOTH paths.
    // All file contact routes through the platform layer (read + atomic write).
    let frame_path = dir.path().join(segment::segment_filename(0));
    let mut segment_bytes =
        crate::store::platform::fs::read(&frame_path).expect("read full segment");
    let payload_byte =
        usize::try_from(pos.offset).expect("offset fits usize") + 8 /* frame header */;
    segment_bytes[payload_byte] ^= 0xFF;
    crate::store::platform::fs::write_derivative_file_atomically(
        dir.path(),
        &frame_path,
        "corrupt-sealed-frame-test",
        &segment_bytes,
    )
    .expect("rewrite corrupted segment");

    // mmap path
    let reader_mmap = Reader::new(dir.path().to_path_buf(), 4, &test_clock(), test_fs());
    reader_mmap.set_active_segment(1);
    let err_mmap = reader_mmap
        .read_entry(&pos)
        .expect_err("a corrupt sealed frame must fail to decode on the mmap path");

    // FD path
    let mut reader_fd = Reader::new(dir.path().to_path_buf(), 4, &test_clock(), test_fs());
    reader_fd.disable_sealed_mmap_for_test();
    reader_fd.set_active_segment(1);
    let err_fd = reader_fd
        .read_entry(&pos)
        .expect_err("a corrupt sealed frame must fail to decode on the FD fallback path");

    assert!(
        matches!(err_mmap, StoreError::CrcMismatch { .. }),
        "PROPERTY: the mmap path must surface CrcMismatch on a corrupt frame, got {err_mmap:?}"
    );
    assert!(
        matches!(err_fd, StoreError::CrcMismatch { .. }),
        "PROPERTY: the FD fallback must surface the SAME error class (CrcMismatch), \
         not swallow or remap corruption differently, got {err_fd:?}"
    );
    assert_eq!(
        std::mem::discriminant(&err_mmap),
        std::mem::discriminant(&err_fd),
        "PROPERTY: corrupt-frame error class must be identical across mmap and FD read paths"
    );
}

#[test]
fn sealed_mmap_probe_runs_at_construction_not_per_read() {
    // A reader constructed on a normal (writable) dir admits mmap exactly once
    // at construction; subsequent sealed reads must not re-probe or re-temp-file.
    let (reader, dir) = test_reader();
    assert!(
        reader.sealed_mmap_admitted_for_test(),
        "PROPERTY: on a writable data dir the one-time construction probe must admit mmap"
    );

    // Perform several sealed reads of distinct segment ids; none of these may
    // re-run the probe (which would write a temp file into the data dir).
    let active_cutoff = 4u64;
    for sid in 0..active_cutoff {
        let pos = write_valid_sealed_segment(
            &dir,
            sid,
            "entity:probe",
            "scope:once",
            &serde_json::json!({"sid": sid}),
        );
        reader.set_active_segment(active_cutoff);
        let stored = reader.read_entry(&pos).expect("read sealed frame via mmap");
        assert_eq!(stored.event.payload, serde_json::json!({"sid": sid}));
    }

    // The cached admission token is the sole probe artifact; it stays stable.
    assert!(
        reader.sealed_mmap_admitted_for_test(),
        "PROPERTY: the cached admission token persists for the reader's lifetime"
    );
}

#[test]
fn required_index_hash_chain_rejects_missing_chain_for_data_event() {
    let event = IndexScanEvent {
        header: EventHeader::new(
            1,
            1,
            None,
            1,
            crate::coordinate::DagPosition::root(),
            0,
            EventKind::DATA,
        ),
        _payload: serde::de::IgnoredAny,
        hash_chain: None,
    };

    let err = Reader::required_index_hash_chain(&event, 7, 99).expect_err("missing hash chain");
    assert!(
        matches!(
            err,
            StoreError::CorruptSegment { segment_id: 7, ref detail }
            if detail.contains("missing hash_chain")
        ),
        "PROPERTY: missing hash_chain must surface as CorruptSegment with the expected detail, got {err:?}"
    );
}
