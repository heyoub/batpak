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

fn test_reader() -> (Reader, TempDir) {
    let dir = TempDir::new().expect("create temp dir for reader test");
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        std::sync::Arc::new(crate::store::SystemClock::new()),
    );
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
    assert!(Reader::checked_frame_range(1, u64::MAX, 16, 1024).is_err());
    assert!(Reader::checked_frame_len(1, 4).is_err());
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
    assert!(Reader::checked_frame_len(
        1,
        u32::try_from(FRAME_HEADER_BYTES + segment::MAX_FRAME_PAYLOAD + 1)
            .expect("one-past-max frame length fits u32")
    )
    .is_err());
    assert!(Reader::checked_frame_len(1, u32::MAX).is_err());
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
    assert!(Reader::checked_batch_count(1, 0, 0).is_err());
    assert!(Reader::checked_batch_count(1, 0, MAX_BATCH_RECOVERY_ITEMS + 1).is_err());
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
