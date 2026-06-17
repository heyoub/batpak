//! SIDX frame-region boundary validation tests.
//!
//! Extracted from the inline `mod tests` island in `segment/mod.rs` to stay
//! within the inline-test-island budget; these pin the unauthenticated-offset
//! guards (`validate_sidx_boundary_not_truncating`) and the compaction-copy
//! lower-bound check on `append_frames_from_segment` (audit P1s).

use super::*;
use std::io::Cursor;
use tempfile::TempDir;

/// Build an in-memory buffer of `[real CRC-valid frames][CRC-valid SDX3 footer]`.
/// Returns `(bytes, frames_end)` where `frames_end` is the SIDX
/// `string_table_offset` the footer records (= the true end of the frames).
fn frames_then_sdx3_footer(payloads: &[&str]) -> (Vec<u8>, u64) {
    use crate::store::segment::sidx::{kind_to_raw, SidxEntry, SidxEntryCollector};

    let mut bytes = Vec::new();
    let mut collector = SidxEntryCollector::new();
    for (idx, p) in payloads.iter().enumerate() {
        let frame_offset = bytes.len() as u64;
        let frame = frame_encode(&serde_json::json!({ "payload": p })).expect("encode frame");
        let frame_length = u32::try_from(frame.len()).expect("frame length fits u32");
        bytes.extend_from_slice(&frame);
        let entry = SidxEntry {
            event_id: idx as u128 + 1,
            entity_idx: 0,
            scope_idx: 0,
            kind: kind_to_raw(crate::event::EventKind::custom(0x1, 1)),
            wall_ms: 1,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            prev_hash: [0; 32],
            event_hash: [1; 32],
            frame_offset,
            frame_length,
            global_sequence: idx as u64 + 1,
            correlation_id: 1,
            causation_id: 0,
        };
        collector.record(entry, "entity:test", "scope:test");
    }
    let frames_end = bytes.len() as u64;
    let mut cursor = Cursor::new(&mut bytes);
    cursor.seek(SeekFrom::End(0)).expect("seek to footer start");
    collector
        .write_footer(&mut cursor, 7)
        .expect("write footer");
    (bytes, frames_end)
}

#[test]
fn detect_sidx_boundary_trusts_a_crc_valid_sdx3_footer() {
    // A real, CRC-valid SDX3 footer authenticates its string_table_offset: the
    // boundary is reported `trusted`, so the scan loops keep the strict
    // (FailClosed-on-bad-frame) policy.
    let (bytes, frames_end) = frames_then_sdx3_footer(&["a", "b", "c"]);
    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let boundary = detect_sidx_boundary(&mut cursor, file_len, 7)
        .expect("must not error")
        .expect("a CRC-valid SDX3 footer is a boundary");
    assert_eq!(
        boundary,
        SidxBoundary {
            frames_end,
            trusted: true,
        },
        "PROPERTY: a CRC-valid SDX3 footer must mark the boundary trusted with the true frames_end"
    );
}

#[test]
fn detect_sidx_boundary_distrusts_a_crc_failed_sdx3_footer() {
    // Flip a byte inside the footer's string-table/entries region so the SDX3
    // CRC fails. The trailer (magic + offset) is untouched, so the boundary is
    // still recognized — but it must be flagged UNTRUSTED, because the offset is
    // no longer authenticated and a too-high garbage offset must trigger
    // recover-what-was-found, not a blind FailClosed.
    let (mut bytes, frames_end) = frames_then_sdx3_footer(&["a", "b", "c"]);
    // Corrupt a byte just inside the string table (right after the real frames),
    // which is covered by the footer CRC but is not the trailer geometry.
    let corrupt_at = usize::try_from(frames_end).expect("frames_end fits usize");
    bytes[corrupt_at] ^= 0xFF;
    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let boundary = detect_sidx_boundary(&mut cursor, file_len, 7)
        .expect("must not error")
        .expect("a CRC-failed footer is still recognized as a boundary");
    assert_eq!(
        boundary,
        SidxBoundary {
            frames_end,
            trusted: false,
        },
        "PROPERTY: a CRC-failed SDX3 footer must be recognized as a boundary but flagged untrusted"
    );
}

#[test]
fn crc_valid_frames_end_clamps_a_too_high_hint_down_to_the_real_frame_end() {
    // The too-HIGH corrupt-offset case: a claimed_end pointing INTO the footer
    // region (past the real frames). crc_valid_frames_end walks the CRC-valid
    // frames and returns where they actually stop, NOT the bogus hint — so the
    // scan recovers every real frame instead of parsing footer bytes as frames.
    let (bytes, real_frames_end) = frames_then_sdx3_footer(&["x", "y", "z"]);
    let file_len = bytes.len() as u64;
    // A hint that points past the real frames but inside the file (into footer).
    let too_high = real_frames_end + 4;
    assert!(
        too_high < file_len,
        "hint must land inside the footer region"
    );
    let mut cursor = Cursor::new(bytes);
    let recovered = crc_valid_frames_end(&mut cursor, 0, too_high).expect("must not error");
    assert_eq!(
        recovered, real_frames_end,
        "PROPERTY: a too-high untrusted hint must clamp down to the true CRC-valid frame end"
    );
}

#[test]
fn crc_valid_frames_end_returns_an_honest_hint_unchanged() {
    // When the hint equals the real frame end (honest footer), the walk reaches
    // the hint with every frame decoding cleanly and returns it unchanged — no
    // frames dropped, no over-read.
    let (bytes, real_frames_end) = frames_then_sdx3_footer(&["p", "q"]);
    let mut cursor = Cursor::new(bytes);
    let recovered = crc_valid_frames_end(&mut cursor, 0, real_frames_end).expect("must not error");
    assert_eq!(
        recovered, real_frames_end,
        "PROPERTY: an honest hint at the true frame end is returned unchanged"
    );
}

#[test]
fn validate_sidx_boundary_rejects_offset_landing_on_a_crc_valid_frame() {
    // A forged SIDX string_table_offset that points at the start of a real,
    // CRC-valid frame must be rejected — trusting it would truncate the scan
    // and silently drop that (and later) frames.
    let frame =
        frame_encode(&serde_json::json!({"payload": "would-be-dropped"})).expect("encode frame");
    // Buffer: [frame][16-byte trailer worth of slack] so file_len > frames_end.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&frame);
    bytes.extend_from_slice(&[0u8; 16]); // stand-in for a footer trailer region
    let file_len = bytes.len() as u64;
    let mut cursor = std::io::Cursor::new(bytes);
    let result = validate_sidx_boundary_not_truncating(&mut cursor, 0, file_len, 7);
    assert!(
        matches!(result, Err(StoreError::CorruptSegment { segment_id: 7, .. })),
        "PROPERTY: a boundary landing on a CRC-valid frame must surface CorruptSegment; got {result:?}"
    );
}

#[test]
fn validate_sidx_boundary_accepts_offset_at_non_frame_bytes() {
    // An honest footer's string-table bytes do not decode as a CRC-valid
    // frame, so the boundary is accepted (graceful path — e.g. a footer whose
    // entry_count is corrupt but whose string_table_offset is correct).
    let frame = frame_encode(&serde_json::json!({"payload": "kept"})).expect("encode frame");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&frame);
    let frames_end = bytes.len() as u64;
    // Non-frame tail: msgpack-ish bytes that will not decode as a frame.
    bytes.extend_from_slice(&[0x82, 0xA1, b'k', 0x01, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11]);
    let file_len = bytes.len() as u64;
    let mut cursor = std::io::Cursor::new(bytes);
    validate_sidx_boundary_not_truncating(&mut cursor, frames_end, file_len, 7)
        .expect("PROPERTY: a boundary at non-frame bytes must be accepted");
}

#[test]
fn validate_sidx_boundary_noops_when_frames_run_to_eof() {
    // frames_end == file_len means no SIDX footer; nothing to validate.
    let mut cursor = std::io::Cursor::new(vec![0u8; 32]);
    validate_sidx_boundary_not_truncating(&mut cursor, 32, 32, 7)
        .expect("PROPERTY: no-footer boundary (frames_end == file_len) is a no-op");
}

#[test]
fn append_frames_from_segment_rejects_sidx_offset_below_frame_region() {
    // Merge-compaction copy path: a sealed source segment whose SIDX trailer
    // has a corrupt string_table_offset pointing BELOW the frame region must
    // surface CorruptSegment, not silently copy zero/prefix bytes. Without
    // the lower-bound guard, `frames_end.saturating_sub(frames_start)` is 0,
    // the copy is empty, and after the merged segment publishes and the old
    // sealed file is cleaned up, those CRC-valid frames are lost.
    use std::io::Write as _;

    let dir = TempDir::new().expect("tmpdir");

    // Source segment with one real frame.
    let mut source: Segment<Active> =
        Segment::create_with_created_ns(dir.path(), 1, 0).expect("create source");
    let frame = frame_encode(&serde_json::json!({"payload": "compaction-lower-bound"}))
        .expect("encode frame");
    source.write_frame(&frame).expect("write frame");
    let source_path = source.path.clone();
    source
        .sync_with_mode(&crate::store::SyncMode::default())
        .expect("sync source");
    drop(source);

    // Append a 16-byte SIDX trailer whose string_table_offset is 0 — well
    // below frames_start (8 + header_len). Valid magic so it is recognized
    // as a boundary; the lower-bound guard is what must reject it.
    let mut trailer = [0u8; 16];
    trailer[0..8].copy_from_slice(&0u64.to_le_bytes());
    trailer[8..12].copy_from_slice(&0u32.to_le_bytes());
    trailer[12..16].copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&source_path)
        .expect("open source for trailer append");
    f.write_all(&trailer).expect("append corrupt trailer");
    drop(f);

    // Destination segment for the compaction copy.
    let mut dest: Segment<Active> =
        Segment::create_with_created_ns(dir.path(), 2, 0).expect("create dest");
    let result = dest.append_frames_from_segment(&source_path);
    assert!(
        matches!(result, Err(StoreError::CorruptSegment { .. })),
        "PROPERTY: a SIDX string_table_offset below the frame region must surface \
         CorruptSegment during compaction copy, not a silent zero-byte copy; got {result:?}"
    );
}
