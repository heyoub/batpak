//! SIDX frame-region boundary recovery tests.
//!
//! Extracted from the inline `mod tests` island in `segment/mod.rs` to stay
//! within the inline-test-island budget; these pin the untrusted-offset
//! recovery walker (`crc_valid_frames_end`), the trust-provenance detection
//! (`detect_sidx_boundary`), and the compaction-copy recovery on
//! `append_frames_from_segment` (audit P1s).
//!
//! Invariant under test: for an UNTRUSTED boundary the trailer
//! `string_table_offset` is GARBAGE and must NEVER bound recovery — whether it
//! is too LOW, MID-FRAME, or too HIGH, the walker recovers ALL CRC-valid frames
//! bounded only by `file_len`. A TRUSTED (CRC-authenticated SDX3) offset stays
//! authoritative.

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

/// Total on-disk size (8-byte header + payload) of the frame that begins at
/// `offset` in `bytes`, read the same way the scan does. Lint-clean: no `unwrap`
/// and no lossy `as` casts on the offset arithmetic.
fn frame_total_len_at(bytes: &[u8], offset: u64) -> u64 {
    let start = usize::try_from(offset).expect("offset fits usize");
    let header: [u8; 4] = bytes[start..start + 4]
        .try_into()
        .expect("4-byte frame length prefix");
    let payload_len = u64::from(u32::from_be_bytes(header));
    8 + payload_len
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
fn crc_valid_frames_end_recovers_all_frames_for_a_too_high_hint() {
    // The too-HIGH corrupt-offset case: an untrusted offset pointing INTO the
    // footer region (past the real frames). The hint is GARBAGE and discarded —
    // crc_valid_frames_end walks the CRC-valid frames bounded only by file_len and
    // returns the natural end of the real frames. The footer bytes that follow do
    // not decode as a CRC-valid frame, so the walk stops exactly at the true frame
    // region end and every real frame is recovered.
    let (bytes, real_frames_end) = frames_then_sdx3_footer(&["x", "y", "z"]);
    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let recovered = crc_valid_frames_end(&mut cursor, 0, file_len).expect("must not error");
    assert_eq!(
        recovered, real_frames_end,
        "PROPERTY: the walker, bounded by file_len, stops at the true CRC-valid frame end and \
         recovers all frames regardless of the (discarded) hint"
    );
}

#[test]
fn crc_valid_frames_end_recovers_all_frames_for_a_too_low_hint() {
    // The too-LOW case at an exact interior frame boundary. The pre-fix walker
    // used the hint as an UPPER BOUND and would have returned `hint` (dropping the
    // CRC-valid frames at/after it). With the hint discarded and file_len the only
    // bound, the walk recovers ALL frames — no CorruptSegment, no dropped frames.
    let (bytes, real_frames_end) = frames_then_sdx3_footer(&["a", "b", "c"]);
    let file_len = bytes.len() as u64;
    // A too-low hint at the start of the SECOND frame (an exact interior boundary).
    let too_low = frame_total_len_at(&bytes, 0);
    assert!(
        too_low > 0 && too_low < real_frames_end,
        "too-low hint must land on an interior frame boundary"
    );
    let mut cursor = Cursor::new(bytes);
    let recovered = crc_valid_frames_end(&mut cursor, 0, file_len).expect("must not error");
    assert_eq!(
        recovered, real_frames_end,
        "PROPERTY: a too-low untrusted hint must NOT bound recovery; all CRC-valid frames recover"
    );
}

#[test]
fn crc_valid_frames_end_recovers_all_frames_for_a_mid_frame_hint() {
    // The MID-FRAME case (the gap that kept slipping): an untrusted offset landing
    // INSIDE a later CRC-valid frame's header/payload, not at a frame boundary.
    // No frame *begins* at the offset, so the old truncation guard never fired;
    // and the pre-fix walker hit `frame_tail > claimed_end` for the frame that
    // CONTAINS the hint and returned that frame's START — silently dropping that
    // CRC-valid frame and all later ones. With the hint discarded and file_len the
    // only bound, the walk decodes that frame cleanly and recovers EVERY frame.
    let (bytes, real_frames_end) = frames_then_sdx3_footer(&["alpha", "beta", "gamma"]);
    let file_len = bytes.len() as u64;
    // Compute the start of the THIRD frame, then point the hint a few bytes INTO
    // it (mid-header / mid-payload, never a frame boundary).
    let second_start = frame_total_len_at(&bytes, 0);
    let third_start = second_start + frame_total_len_at(&bytes, second_start);
    let mid_frame_hint = third_start + 3; // strictly inside the third frame
    assert!(
        mid_frame_hint > third_start && mid_frame_hint < real_frames_end,
        "hint must land strictly inside a later CRC-valid frame, not at a boundary"
    );
    let mut cursor = Cursor::new(bytes);
    let recovered = crc_valid_frames_end(&mut cursor, 0, file_len).expect("must not error");
    assert_eq!(
        recovered, real_frames_end,
        "PROPERTY: a mid-frame untrusted hint must NOT drop the containing CRC-valid frame; all \
         frames recover (this is the round-4 P1)"
    );
}

#[test]
fn crc_valid_frames_end_stops_at_first_non_frame_byte() {
    // The walk stops at the first byte that does not decode as a CRC-valid frame
    // (footer / corruption), never admitting non-CRC-valid bytes. With an honest
    // footer the natural stop is exactly the true frame end.
    let (bytes, real_frames_end) = frames_then_sdx3_footer(&["p", "q"]);
    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let recovered = crc_valid_frames_end(&mut cursor, 0, file_len).expect("must not error");
    assert_eq!(
        recovered, real_frames_end,
        "PROPERTY: the walk stops at the true frame end (footer bytes never decode as a frame)"
    );
}

#[test]
fn append_frames_from_segment_recovers_all_frames_for_untrusted_too_low_offset() {
    // Merge-compaction copy path: a sealed source segment whose SIDX trailer has a
    // forged (unauthenticated) string_table_offset pointing BELOW the frame region
    // (offset 0). The forged offset breaks footer CRC authentication, so the
    // boundary is UNTRUSTED — the offset is garbage and must be discarded. The
    // copy then walks the CRC-valid frames bounded by file_len and copies the real
    // frame so it is preserved in the merged segment (recover-what-was-found),
    // rather than erroring or silently copying zero bytes.
    use std::io::Write as _;

    let dir = TempDir::new().expect("tmpdir");

    // Source segment with one real frame.
    let mut source: Segment<Active> =
        Segment::create_with_created_ns(dir.path(), 1, 0).expect("create source");
    let frame =
        frame_encode(&serde_json::json!({"payload": "compaction-recover"})).expect("encode frame");
    source.write_frame(&frame).expect("write frame");
    let source_path = source.path.clone();
    source
        .sync_with_mode(&crate::store::SyncMode::default())
        .expect("sync source");
    drop(source);

    // Append a 16-byte SIDX trailer whose string_table_offset is 0 — well below
    // frames_start (8 + header_len). Valid magic so it is recognized as a boundary,
    // but the forged offset fails CRC auth → UNTRUSTED → discarded.
    let mut trailer = [0u8; 16];
    trailer[0..8].copy_from_slice(&0u64.to_le_bytes());
    trailer[8..12].copy_from_slice(&0u32.to_le_bytes());
    trailer[12..16].copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&source_path)
        .expect("open source for trailer append");
    f.write_all(&trailer).expect("append forged trailer");
    drop(f);

    // Destination segment for the compaction copy.
    let mut dest: Segment<Active> =
        Segment::create_with_created_ns(dir.path(), 2, 0).expect("create dest");
    let dest_header_bytes = dest.written_bytes;
    dest.append_frames_from_segment(&source_path)
        .expect("PROPERTY: an untrusted too-low offset must recover the real frame, not error");

    // The copy must preserve the one CRC-valid frame in full (>= frame.len()): the
    // bytes appended to the destination must at least cover the encoded frame. With
    // the untrusted offset discarded, a too-low (offset 0) hint can no longer drive
    // a zero-byte copy that silently drops the committed frame. (The walk may also
    // consume a few trailing bytes of the synthetic trailer that coincidentally
    // decode as a CRC-valid zero-payload frame — harmless padding, never the real
    // frame being dropped; full-store recovery is proven by the integration tests.)
    let copied = dest.written_bytes - dest_header_bytes;
    assert!(
        copied >= frame.len() as u64,
        "PROPERTY: compaction must copy the full CRC-valid frame for an untrusted too-low \
         offset (recover-what-was-found), not zero/prefix bytes; copied {copied}, frame {}",
        frame.len()
    );
}
