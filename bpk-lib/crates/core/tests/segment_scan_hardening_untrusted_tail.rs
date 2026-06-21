//! PROVES: the untrusted-footer governing principle at the SEGMENT TAIL — an
//! out-of-bounds forged offset, a torn/truncated SDX3 trailer, and a no-footer
//! segment whose final payload coincidentally ends in the SIDX magic each leave
//! the offset GARBAGE and FULLY INERT, so cold start discards it and recovers
//! every CRC-valid frame; a full adversarial battery (zero, eight, mid-frame,
//! upper-bound, file_len, huge) confirms NO untrusted offset value can ever
//! error or drop a frame over an intact frame region.
//! CATCHES: a recovery path that honors an unauthenticated tail offset to brick
//! reopen (CorruptSegment), drop the containing/later frames, or raise a false
//! footer error on a coincidental magic tail.
//! SEEDED: deterministic single-segment stores (6 user frames, plus one with a
//! magic-tail payload) whose SDX3 trailer is forged, torn, or stripped to a
//! coincidental magic before reopen.

#[path = "support/segment_scan_hardening.rs"]
mod ssh_support;

use batpak::coordinate::Coordinate;
use batpak::store::Store;
use ssh_support::*;
use tempfile::TempDir;

/// Strip a CRC-valid SDX3 footer so the coincidental-magic case starts from a
/// genuinely footer-less segment. Inline (not shared) so the support module
/// stays `dead_code`-free in the sibling untrusted-offset binary, which never
/// strips the footer.
fn strip_sidx(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() >= 16 && &bytes[bytes.len() - 4..] == b"SDX3" {
        let string_table_offset = u64::from_le_bytes(
            bytes[bytes.len() - 16..bytes.len() - 8]
                .try_into()
                .expect("8-byte SIDX trailer offset"),
        );
        bytes.truncate(
            usize::try_from(string_table_offset).expect("SIDX string table offset fits usize"),
        );
    }
    bytes
}

#[test]
fn untrusted_footer_offset_out_of_bounds_recovers_all_frames() {
    // ROUND-6 P1 (the governing-principle fix): an UNTRUSTED SDX3 footer whose
    // `string_table_offset` is OUT OF BOUNDS (> file_len - 16). Forging the offset
    // breaks the SDX3 footer CRC, so the footer is untrusted — the offset is
    // GARBAGE and FULLY INERT. The pre-round-6 `detect_sidx_boundary` ran its
    // upper-bound check BEFORE trust was determined, so an out-of-bounds garbage
    // offset HARD-ERRORED with CorruptSegment and bricked cold start even though
    // every committed frame is CRC-valid and recoverable. The definitive behavior:
    // an out-of-bounds untrusted offset downgrades to the CRC-valid-frame recovery
    // scan and Store::open recovers ALL frames.
    //
    // This test FAILS on pre-fix code (CorruptSegment on reopen).
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    // Forge the offset PAST the upper bound (file_len, which is strictly greater
    // than file_len - 16). Valid magic so it is recognized as a boundary; the
    // bogus offset breaks footer CRC auth -> untrusted -> must be discarded.
    let n = bytes.len() as u64;
    let off_pos = bytes.len() - 16;
    bytes[off_pos..off_pos + 8].copy_from_slice(&n.to_le_bytes());
    std::fs::write(&seg, &bytes).expect("write out-of-bounds-offset segment");

    let store = Store::open(config(&dir)).expect(
        "reopen must succeed: an out-of-bounds UNTRUSTED offset must downgrade to CRC-valid-frame \
         recovery, not CorruptSegment (round-6 P1)",
    );
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: an out-of-bounds unauthenticated SIDX offset must recover ALL CRC-valid frames \
         (hint fully inert), not error or drop frames; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn torn_sdx3_trailer_recovers_all_frames() {
    // A torn / truncated SDX3 footer: the segment was sealed with a real CRC-valid
    // footer, but the footer bytes (string table + entries + CRC) are then
    // truncated away, leaving only the trailing 16-byte trailer whose offset still
    // points where the (now-missing) string table used to begin. The trailer magic
    // is intact so `detect_sidx_boundary` recognizes a boundary, but `read_layout`
    // cannot reconstruct/verify the CRC over the missing region -> untrusted. The
    // offset is discarded and the CRC-valid frame walk recovers every committed
    // frame. (A torn trailer must never hard-error or drop frames.)
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    // The real string table begins at string_table_offset. Truncate the footer
    // body to just a few bytes after that offset (tearing the string table /
    // entries / CRC), then re-append a trailer whose offset still claims the
    // original string_table_offset. The footer body is now too short for
    // read_layout to verify -> untrusted.
    let true_frames_end = usize::try_from(u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    ))
    .expect("SIDX string table offset fits usize");

    // Keep frames + a torn fragment of the footer body, then a 16-byte trailer
    // pointing back at the (now-incomplete) string table.
    let torn_body_end = true_frames_end + 3; // a few torn footer bytes
    let mut rebuilt = bytes[..torn_body_end].to_vec();
    rebuilt.extend_from_slice(&(true_frames_end as u64).to_le_bytes());
    rebuilt.extend_from_slice(&0u32.to_le_bytes());
    rebuilt.extend_from_slice(b"SDX3");
    std::fs::write(&seg, &rebuilt).expect("write torn-trailer segment");

    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a torn SDX3 trailer must recover via CRC-valid-frame scan");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a torn/truncated SDX3 footer must recover ALL CRC-valid frames, not error or \
         drop frames; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn no_footer_segment_ending_in_coincidental_magic_recovers_all_frames() {
    // Codex's "coincidental magic" case: a segment with NO real SIDX footer whose
    // final frame payload COINCIDENTALLY ends with the bytes b"SDX3" (or b"SDX2").
    // detect_sidx_boundary sees the trailing magic and recognizes a "boundary",
    // but there is no real CRC-authenticated footer behind it, so trust fails ->
    // untrusted. The garbage offset (the last 8 bytes of a real frame's payload,
    // reinterpreted as a u64) must be FULLY INERT: cold start discards it and
    // recovers all CRC-valid frames via the frame walk. (If the coincidental offset
    // were honored it could be out of bounds / mid-frame and either error or drop
    // frames.)
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:coincidental", "scope:test").expect("valid coord");
    // Append several frames; the LAST user frame's payload deliberately ends with
    // the SIDX magic so the segment tail coincidentally matches.
    for i in 0..5u32 {
        store
            .append(&coord, KIND, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store
        .append(
            &coord,
            KIND,
            &serde_json::json!({"tag": "ends-with-magic-SDX3"}),
        )
        .expect("append magic-tail frame");
    store.close().expect("close");

    let seg = segment_path(&dir);
    // Strip the real SIDX footer so the segment has NO footer, then force the very
    // last bytes of the file to be the SIDX magic (simulating a frame payload that
    // coincidentally ends in b"SDX3"). The bytes before are intact CRC-valid frames.
    let mut bytes = strip_sidx(std::fs::read(&seg).expect("read segment"));
    let total_user = {
        // Sanity: with no footer, frames run to EOF.
        assert!(bytes.len() > 16, "stripped segment must hold real frames");
        bytes.len()
    };
    // Overwrite the final 4 bytes with the SIDX magic. These bytes are inside the
    // last frame's payload region; the frame's CRC will no longer match, so the
    // last frame becomes a torn/non-CRC-valid tail — but every EARLIER frame stays
    // CRC-valid and must recover. Nothing CRC-valid follows the broken tail, so
    // this is torn-tail recovery (not mid-stream corruption) and reopen succeeds.
    let magic_at = total_user - 4;
    bytes[magic_at..].copy_from_slice(b"SDX3");
    std::fs::write(&seg, &bytes).expect("write coincidental-magic segment");

    let store = Store::open(config(&dir)).expect(
        "reopen must succeed: a no-footer segment ending coincidentally in the SIDX magic must \
         recover via the CRC-valid-frame scan, never a false footer error",
    );
    let entries = user_entries(&store);
    assert!(
        !entries.is_empty(),
        "PROPERTY: a no-footer segment whose tail coincidentally equals the SIDX magic must \
         recover the CRC-valid frames before the (broken) tail, not error; got {} entries",
        entries.len()
    );
    // The 5 leading user frames are intact and CRC-valid; only the magic-tail frame
    // was broken by the overwrite, so it (and only it) may be lost as a torn tail.
    assert!(
        entries.len() >= 5,
        "PROPERTY: every CRC-valid frame before the coincidental-magic tail must recover; \
         got {} (expected >= 5)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn untrusted_footer_adversarial_offsets_always_recover_all_frames() {
    // PROPERTY (round-6): for a battery of adversarial garbage offset values, an
    // UNTRUSTED footer over an intact CRC-valid frame region must ALWAYS recover
    // EVERY frame — never error, never drop a frame. This nails the governing
    // principle ("no untrusted offset value can ever error or drop frames") across
    // the full shape space: too-low (0, 8), interior/mid-frame, file_len-16 (the
    // exact upper bound), file_len (out of bounds), and a huge bounded value.
    //
    // Each iteration re-seeds a fresh store (6 user frames), forges the SIDX
    // trailer offset to the adversarial value (breaking footer CRC -> untrusted),
    // and asserts Store::open recovers all 6 user frames.
    for &offset_kind in &[
        "zero",
        "eight",
        "mid_frame",
        "at_upper_bound",
        "file_len",
        "huge",
    ] {
        let dir = TempDir::new().expect("temp dir");
        seed_store(&dir, 6);

        let seg = segment_path(&dir);
        let mut bytes = std::fs::read(&seg).expect("read segment");
        assert_eq!(
            &bytes[bytes.len() - 4..],
            b"SDX3",
            "seeded segment must carry the SDX3 SIDX magic"
        );

        let n = bytes.len() as u64;
        let frames_start = frame_scan_header_end(&bytes) as u64;
        let true_frames_end = u64::from_le_bytes(
            bytes[bytes.len() - 16..bytes.len() - 8]
                .try_into()
                .expect("8-byte SIDX trailer offset"),
        );
        // An interior offset a few bytes past the first frame boundary (mid-frame).
        let mid_frame = frames_start + 3;
        let forged_offset: u64 = match offset_kind {
            "zero" => 0,
            "eight" => 8,
            "mid_frame" => mid_frame.min(true_frames_end.saturating_sub(1)),
            "at_upper_bound" => n - 16,
            "file_len" => n,
            "huge" => u64::MAX / 2, // bounded but absurd
            _ => unreachable!(),
        };

        let off_pos = bytes.len() - 16;
        bytes[off_pos..off_pos + 8].copy_from_slice(&forged_offset.to_le_bytes());
        std::fs::write(&seg, &bytes).expect("write adversarial-offset segment");

        let store = Store::open(config(&dir)).unwrap_or_else(|err| {
            assert!(
                std::hint::black_box(false),
                "PROPERTY: an UNTRUSTED footer with adversarial offset '{offset_kind}' \
                 ({forged_offset}) must recover, never error; got {err:?}"
            );
            unreachable!("the recovery assertion above always fails on error")
        });
        let entries = user_entries(&store);
        assert_eq!(
            entries.len(),
            6,
            "PROPERTY: adversarial offset '{offset_kind}' ({forged_offset}) over intact frames \
             must recover ALL 6 CRC-valid frames; got {}",
            entries.len()
        );
        store.close().expect("close");
    }
}
