//! PROVES: the governing untrusted-footer principle for FORGED in-bounds offsets
//! — once a forged SDX3 footer fails its CRC authentication, its
//! `string_table_offset` is GARBAGE and FULLY INERT. Cold start discards the hint
//! and recovers EVERY CRC-valid frame whether the offset is too-low onto an
//! interior boundary, mid-frame, or too-high into the footer region — while still
//! FailingClosed on genuine mid-stream corruption and recovering the clean prefix
//! on a torn last frame.
//! CATCHES: a recovery path that trusts an unauthenticated offset to bound, drop,
//! or brick recovery — a denial-of-availability vector — or that silently
//! truncates to a prefix when CRC-valid frames follow interior corruption.
//! SEEDED: deterministic single-segment stores (6/8 user frames) whose SDX3
//! trailer offset is forged or whose interior frame is corrupted before reopen.

#[path = "support/segment_scan_hardening.rs"]
mod ssh_support;

use batpak::store::{Store, StoreError};
use ssh_support::*;
use tempfile::TempDir;

#[test]
fn sidx_footer_offset_forged_too_low_into_an_interior_frame_recovers_all_frames() {
    // ROUND-4 P1: a forged SIDX string_table_offset that lands on an EARLIER real
    // frame boundary (too LOW). Overwriting the offset breaks the SDX3 footer CRC,
    // so the boundary is UNTRUSTED — the offset is GARBAGE and must NEVER bound
    // recovery. Earlier rounds rejected this with CorruptSegment (treating the
    // hint as a truncation proof). The DEFINITIVE behavior: discard the hint and
    // walk the CRC-valid frames bounded only by file_len, so cold start recovers
    // EVERY committed frame instead of either dropping the later ones or failing
    // closed. (Trusting an unauthenticated offset to FAIL is itself a denial-of-
    // availability vector; the CRC-valid frames are the durability oracle.)
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    // Walk frames from the header end to find the offset of the SECOND frame —
    // a real frame boundary that sits strictly inside the frame region.
    let frames_start = frame_scan_header_end(&bytes);
    let true_frames_end = usize::try_from(u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    ))
    .expect("SIDX string table offset fits usize");
    let mut cursor = frames_start;
    // skip the first frame
    let first_len = u32::from_be_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .expect("4-byte frame length prefix"),
    ) as usize;
    cursor += 8 + first_len;
    let forged_offset = cursor;
    assert!(
        forged_offset > frames_start && forged_offset < true_frames_end,
        "forged boundary must land on an interior real frame boundary"
    );

    // Overwrite the SIDX trailer's string_table_offset with the forged boundary.
    let mut forged = bytes.clone();
    let off_pos = forged.len() - 16;
    forged[off_pos..off_pos + 8].copy_from_slice(&(forged_offset as u64).to_le_bytes());
    std::fs::write(&seg, &forged).expect("write forged-offset segment");

    // Cold start must recover ALL committed frames: the untrusted offset is
    // discarded and the CRC-valid frame walk finds every event.
    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a forged too-low untrusted offset must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a forged too-LOW unauthenticated SIDX offset must recover ALL CRC-valid \
         frames (hint discarded), not drop frames or FailClosed; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn sidx_footer_offset_forged_mid_frame_recovers_all_frames() {
    // ROUND-4 P1 (the gap that kept slipping): a forged SIDX string_table_offset
    // landing INSIDE a later CRC-valid frame's header/payload — NOT at a frame
    // boundary. No frame begins at the offset, so the old truncation guard never
    // fired; and the old walker (hint as upper bound) hit `frame_tail > hint` for
    // the frame CONTAINING the offset and returned that frame's START — silently
    // dropping that CRC-valid frame and all later ones. With the hint discarded
    // and file_len the only bound, the walk decodes that frame cleanly and
    // recovers EVERY committed event.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    let frames_start = frame_scan_header_end(&bytes);
    let true_frames_end = usize::try_from(u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    ))
    .expect("SIDX string table offset fits usize");
    // Skip the first two frames to land at the start of the THIRD frame, then
    // point the offset a few bytes INTO it (mid-header / mid-payload).
    let mut cursor = frames_start;
    let first_len = u32::from_be_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .expect("4-byte frame length prefix"),
    ) as usize;
    cursor += 8 + first_len;
    let second_len = u32::from_be_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .expect("4-byte frame length prefix"),
    ) as usize;
    cursor += 8 + second_len;
    let mid_frame_offset = cursor + 3; // strictly inside the third frame
    assert!(
        mid_frame_offset > cursor && mid_frame_offset < true_frames_end,
        "forged offset must land strictly inside a later CRC-valid frame, not at a boundary"
    );

    let mut forged = bytes.clone();
    let off_pos = forged.len() - 16;
    forged[off_pos..off_pos + 8].copy_from_slice(&(mid_frame_offset as u64).to_le_bytes());
    std::fs::write(&seg, &forged).expect("write mid-frame-offset segment");

    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a mid-frame untrusted offset must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a forged MID-FRAME unauthenticated SIDX offset must recover ALL CRC-valid \
         frames (hint discarded), not drop the containing frame; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn corrupt_sdx3_footer_offset_too_high_into_footer_recovers_all_crc_valid_frames() {
    // ROUND-3 P1: a CORRUPT (CRC-failing) SDX3 footer whose `string_table_offset`
    // points too HIGH — past the real frames, INTO the footer region (string
    // table / entries / CRC). The round-2 truncation guard only rejects an offset
    // too LOW (a CRC-valid frame begins at the claimed boundary). A too-HIGH
    // offset lands on footer bytes, where NO CRC-valid frame begins, so the guard
    // passed it through. Before this fix, cold start trusted that unauthenticated
    // offset as `frames_end`, scanned the real frames fine, then parsed FOOTER
    // bytes as frame headers and FailClosed — bricking recovery even though every
    // real frame is CRC-valid. With provenance-aware recovery, the untrusted
    // offset is clamped down to the true end of CRC-valid frames, so all events
    // are recovered.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    let n = bytes.len();
    let off_pos = n - 16;
    let true_frames_end = u64::from_le_bytes(
        bytes[off_pos..off_pos + 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    );
    // The footer region spans [true_frames_end .. n). Point the offset INTO it,
    // strictly past the real frames but still <= file_len - 16 (the trailer can't
    // begin inside itself). Landing it a few bytes into the string table is the
    // adversarial too-high case. This also breaks the footer CRC (read_layout
    // recomputes the covered region from the offset and the geometry no longer
    // matches the stored CRC / errors), so the footer is UNTRUSTED and recovery
    // must rebuild from the CRC-valid frames it actually finds.
    let max_offset = (n as u64) - 16;
    let forged_high = (true_frames_end + 4).min(max_offset);
    assert!(
        forged_high > true_frames_end,
        "forged offset must point strictly into the footer region (too high)"
    );
    bytes[off_pos..off_pos + 8].copy_from_slice(&forged_high.to_le_bytes());
    std::fs::write(&seg, &bytes).expect("write too-high-offset segment");

    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a too-high corrupt-footer offset must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a too-HIGH unauthenticated SIDX offset (into the footer) must recover ALL \
         CRC-valid frames, not FailClosed; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn untrusted_footer_mid_stream_corruption_fails_closed() {
    // ROUND-5 P1 (the gap the round-4 fix exposed): an UNTRUSTED footer (CRC-failed
    // SDX3, via a forged offset) over a frame region that has MID-STREAM corruption
    // — a corrupt (CRC-failing) frame with CRC-VALID frames still after it. The
    // round-4 `crc_valid_frames_end` walked from frames_start, hit the first
    // non-decodable frame at P, and returned P as a "clean EOF" — silently dropping
    // the corrupt frame AND every later valid event. A trusted/no-footer scan would
    // FailClosed on the same interior corruption, so the untrusted path silently
    // diverging is a data-integrity hole.
    //
    // The DEFINITIVE behavior: a non-decodable position P is the true end of frames
    // ONLY if nothing CRC-valid follows it. Here CRC-valid frames DO follow P, so P
    // is interior corruption and recovery must FailClosed with CorruptSegment — NOT
    // recover the prefix. (This test FAILS on round-4 code, which returns the prefix
    // and lets the store reopen with fewer events.)
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 8);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    let frames_start = frame_scan_header_end(&bytes);
    let true_frames_end = usize::try_from(u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    ))
    .expect("SIDX string table offset fits usize");

    // Walk to the THIRD frame and flip a byte inside its payload. This breaks ONLY
    // that frame's CRC (interior corruption) while leaving every earlier and later
    // frame byte-for-byte intact and CRC-valid. Frames after it still decode.
    let mut cursor = frames_start;
    let first_len = u32::from_be_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .expect("4-byte frame length prefix"),
    ) as usize;
    cursor += 8 + first_len;
    let second_len = u32::from_be_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .expect("4-byte frame length prefix"),
    ) as usize;
    cursor += 8 + second_len;
    let third_frame_offset = cursor;
    let third_len = u32::from_be_bytes(
        bytes[third_frame_offset..third_frame_offset + 4]
            .try_into()
            .expect("4-byte frame length prefix"),
    ) as usize;
    let third_tail = third_frame_offset + 8 + third_len;
    assert!(
        third_tail < true_frames_end,
        "test needs CRC-valid frames AFTER the corrupted third frame (third_tail {third_tail} \
         < frames_end {true_frames_end})"
    );
    // Flip one payload byte of the third frame -> CRC mismatch on that frame only.
    let payload_byte = third_frame_offset + 8;
    bytes[payload_byte] ^= 0x01;

    // Make the footer UNTRUSTED: forge the string_table_offset (too high, into the
    // footer region) so the SDX3 footer CRC no longer matches. This forces the
    // untrusted-footer recovery path — the path under test — instead of the trusted
    // or no-footer path.
    let n = bytes.len();
    let off_pos = n - 16;
    let max_offset = (n as u64) - 16;
    let forged_high = ((true_frames_end as u64) + 4).min(max_offset);
    assert!(
        forged_high > true_frames_end as u64,
        "forged offset must point into the footer region so the footer is untrusted"
    );
    bytes[off_pos..off_pos + 8].copy_from_slice(&forged_high.to_le_bytes());
    std::fs::write(&seg, &bytes).expect("write corrupt+untrusted segment");

    // Reopen MUST fail closed: interior corruption with valid frames after it must
    // never be silently truncated to the prefix.
    let err = Store::open(config(&dir)).map(|_| ()).expect_err(
        "PROPERTY: untrusted-footer recovery must FailClosed on mid-stream corruption \
         (CRC-valid frames follow the corrupt frame), not silently recover the prefix",
    );
    assert!(
        matches!(err, StoreError::CorruptSegment { .. }),
        "PROPERTY: mid-stream corruption under an untrusted footer must surface as CorruptSegment; \
         got {err:?}"
    );
}

#[test]
fn untrusted_footer_torn_last_frame_recovers_prefix() {
    // ROUND-5 composition check (case 4): an UNTRUSTED footer where the LAST frame
    // is torn (truncated mid-payload) and NOTHING CRC-valid follows it. This is
    // genuine torn-tail, NOT interior corruption — the resync look-ahead finds no
    // CRC-valid frame after the failure point, so recovery returns the clean prefix
    // and the earlier committed events survive (availability preserved, no false
    // FailClosed). This proves the mid-stream-corruption fix composes with
    // torn-tail handling instead of regressing it into a hard error.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    let frames_start = frame_scan_header_end(&bytes);
    let true_frames_end = usize::try_from(u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    ))
    .expect("SIDX string table offset fits usize");

    // Find the start of the LAST real frame.
    let mut cursor = frames_start;
    let mut last_frame_offset = frames_start;
    while cursor < true_frames_end {
        let len = u32::from_be_bytes(
            bytes[cursor..cursor + 4]
                .try_into()
                .expect("4-byte frame length prefix"),
        ) as usize;
        last_frame_offset = cursor;
        cursor += 8 + len;
    }
    assert!(
        last_frame_offset > frames_start,
        "need at least two frames so a torn last frame still leaves a recoverable prefix"
    );
    let last_len = u32::from_be_bytes(
        bytes[last_frame_offset..last_frame_offset + 4]
            .try_into()
            .expect("4-byte frame length prefix"),
    ) as usize;
    assert!(last_len > 4, "last frame must have a payload to tear");

    // Tear the last frame in place: keep its 8-byte header (large claimed_len) but
    // truncate the file partway through its payload. The header now claims more
    // bytes than remain, so the frame is non-decodable (torn tail). Then append a
    // forged UNTRUSTED SDX3 trailer (valid magic, offset = the new torn boundary so
    // the upper-bound check passes, but its content does not match a real footer CRC
    // -> untrusted), so the untrusted-footer recovery path is the one under test.
    let torn_payload_keep = last_len / 2; // partial payload -> torn
    let torn_region_end = last_frame_offset + 8 + torn_payload_keep;
    let mut rebuilt = bytes[..torn_region_end].to_vec();
    let new_boundary = rebuilt.len() as u64;
    // Append a 16-byte trailer: [offset:u64 LE][count:u32 LE][SDX3]. The offset
    // points at the trailer start (new_boundary), which is a valid upper bound, but
    // no real CRC-authenticated footer precedes it, so detect_sidx_boundary flags it
    // UNTRUSTED.
    rebuilt.extend_from_slice(&new_boundary.to_le_bytes());
    rebuilt.extend_from_slice(&0u32.to_le_bytes());
    rebuilt.extend_from_slice(b"SDX3");
    std::fs::write(&seg, &rebuilt).expect("write torn-tail + untrusted-footer segment");

    // Reopen must SUCCEED and recover the prefix (every frame before the torn last
    // one): torn tail with nothing valid after is not interior corruption.
    // The decisive property: reopen does NOT FailClosed (the .expect below is the
    // assertion). A torn last frame with nothing CRC-valid after it must recover
    // the prefix instead of erroring as mid-stream corruption. The torn frame here
    // is the segment's trailing lifecycle frame, so every user event written before
    // it survives the prefix recovery.
    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a torn last frame with no valid frame after is torn-tail");
    let entries = user_entries(&store);
    // N=6: seed_store appends 6 user events; the torn LAST frame is the trailing
    // SYSTEM_CLOSE_COMPLETED lifecycle frame, so every committed user event written
    // before it must survive prefix recovery (user_entries filters out OPEN/CLOSE).
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a torn LAST frame under an untrusted footer must recover the clean prefix \
         (all committed user events before the tear), not FailClosed; got {} entries",
        entries.len()
    );
    store.close().expect("close");
}
