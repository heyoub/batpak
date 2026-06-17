//! SIDX frame-region boundary validation tests.
//!
//! Extracted from the inline `mod tests` island in `segment/mod.rs` to stay
//! within the inline-test-island budget; these pin the unauthenticated-offset
//! guards (`validate_sidx_boundary_not_truncating`) and the compaction-copy
//! lower-bound check on `append_frames_from_segment` (audit P1s).

use super::*;
use tempfile::TempDir;

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
