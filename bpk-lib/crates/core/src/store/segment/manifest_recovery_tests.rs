//! Round-7 "cake-and-eat-it" SIDX-manifest untrusted-footer recovery tests.
//!
//! Extracted from `boundary_tests.rs` to keep each inline test file within the
//! structural file-size budget. These pin the untrusted-footer
//! recover-vs-fail-closed decision via the SIDX entry table as a
//! self-authenticating manifest: the trust anchor is the content-addressed
//! `event_hash` (blake3 of the event payload), which the writer records into each
//! SIDX entry AND embeds in each frame's `hash_chain` — so a recovered (CRC-valid)
//! frame corroborates its matching entry, and a corrupt footer cannot forge a
//! corroboration.
//!
//! Decision under test (untrusted boundary only):
//! - (a) a corroborated manifest attesting to a committed frame missing from the
//!   recovered stream FAILS CLOSED regardless of tail policy (round-7 gap);
//! - (b) a corroborated manifest over intact frames RECOVERS them all;
//! - (c) an unparseable / uncorroborated manifest falls back to prefix recovery
//!   (no false fail-closed). Mid-stream corruption still fails closed first.

use super::*;
use crate::event::{EventHeader, EventKind, HashChain};
use crate::store::segment::sidx::{kind_to_raw, read_entries_unauthenticated, SidxEntryCollector};
use std::io::Cursor;

/// Build an in-memory buffer of `[real CRC-valid frames][CRC-valid SDX3 footer]`.
/// Mirror of the helper in `boundary_tests.rs`, used by the mid-stream-corruption
/// regression below. Returns `(bytes, frames_end)`.
fn frames_then_sdx3_footer(payloads: &[&str]) -> (Vec<u8>, u64) {
    use crate::store::segment::sidx::SidxEntry;

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
            kind: kind_to_raw(EventKind::custom(0x1, 1)),
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

/// Total on-disk size of the frame beginning at `offset` in `bytes`.
fn frame_total_len_at(bytes: &[u8], offset: u64) -> u64 {
    let start = usize::try_from(offset).expect("offset fits usize");
    let header: [u8; 4] = bytes[start..start + 4]
        .try_into()
        .expect("4-byte frame length prefix");
    let payload_len = u64::from(u32::from_be_bytes(header));
    8 + payload_len
}

/// Build one real CRC-valid frame whose `FramePayload` carries a `hash_chain`
/// with `event_hash = blake3(payload_bytes)` (the exact writer invariant), plus
/// the matching [`crate::store::segment::sidx::SidxEntry`] (same offset, length,
/// and content event_hash). Returns `(frame_bytes, entry)`.
fn corroboratable_frame(
    frame_offset: u64,
    seq: u64,
    payload: &serde_json::Value,
) -> (Vec<u8>, crate::store::segment::sidx::SidxEntry) {
    let payload_bytes = crate::encoding::to_bytes(payload).expect("encode payload");
    let event_hash = crate::event::hash::compute_hash(&payload_bytes);
    let header = EventHeader::new(
        seq as u128,
        seq as u128,
        None,
        0,
        crate::coordinate::DagPosition::root(),
        u32::try_from(payload_bytes.len()).expect("payload size fits u32"),
        EventKind::custom(0x1, 1),
    );
    let event = crate::event::Event {
        header,
        payload: payload_bytes,
        hash_chain: Some(HashChain {
            prev_hash: [0; 32],
            event_hash,
        }),
    };
    let frame_payload = FramePayload {
        event,
        entity: "entity:test".to_owned(),
        scope: "scope:test".to_owned(),
        receipt_extensions: std::collections::BTreeMap::new(),
    };
    let frame = frame_encode(&frame_payload).expect("encode frame");
    let frame_length = u32::try_from(frame.len()).expect("frame length fits u32");
    let entry = crate::store::segment::sidx::SidxEntry {
        event_id: seq as u128,
        entity_idx: 0,
        scope_idx: 0,
        kind: kind_to_raw(EventKind::custom(0x1, 1)),
        wall_ms: 1,
        clock: 1,
        dag_lane: 0,
        dag_depth: 0,
        prev_hash: [0; 32],
        event_hash,
        frame_offset,
        frame_length,
        global_sequence: seq,
        correlation_id: seq as u128,
        causation_id: 0,
    };
    (frame, entry)
}

/// Append an SDX3 footer recording `entries` to `bytes`, then (optionally) flip a
/// byte inside the footer's covered region so the footer CRC fails → UNTRUSTED.
/// The 16-byte trailer geometry (offset/count/magic) is left intact, so the entry
/// table stays parseable by `read_entries_unauthenticated` while the footer CRC
/// no longer authenticates the boundary. Returns the footer-body byte offset that
/// was corrupted (= the footer start, the entries' recorded frames_end).
fn append_untrusted_footer(
    bytes: &mut Vec<u8>,
    entries: &[crate::store::segment::sidx::SidxEntry],
) -> u64 {
    let footer_start = bytes.len() as u64;
    let mut collector = SidxEntryCollector::new();
    for entry in entries.iter().cloned() {
        collector.record(entry, "entity:test", "scope:test");
    }
    let mut cursor = Cursor::new(&mut *bytes);
    cursor.seek(SeekFrom::End(0)).expect("seek to footer start");
    collector
        .write_footer(&mut cursor, 7)
        .expect("write footer");
    // Corrupt one byte of the footer string-table/entries region (the byte right
    // at footer_start) to break the footer CRC without touching the trailer
    // geometry. This is exactly how `detect_sidx_boundary_distrusts_a_crc_failed_sdx3_footer`
    // produces an untrusted boundary.
    let corrupt_at = usize::try_from(footer_start).expect("footer_start fits usize");
    bytes[corrupt_at] ^= 0xFF;
    footer_start
}

#[test]
fn untrusted_entries_parse_is_crc_independent() {
    // The untrusted entry-table read decodes entries even when the footer CRC has
    // been broken — proving SidxEntry::decode_from is CRC-independent and the
    // manifest is available for corroboration on a corrupt footer.
    let (mut bytes, e0) = {
        let (f0, e0) = corroboratable_frame(0, 1, &serde_json::json!({"v": "a"}));
        (f0, e0)
    };
    let (f1, mut e1) = corroboratable_frame(bytes.len() as u64, 2, &serde_json::json!({"v": "b"}));
    e1.frame_offset = bytes.len() as u64;
    bytes.extend_from_slice(&f1);
    append_untrusted_footer(&mut bytes, &[e0.clone(), e1.clone()]);

    let mut cursor = Cursor::new(bytes);
    let parsed = read_entries_unauthenticated(&mut cursor, 7).expect("must not error");
    assert_eq!(
        parsed.len(),
        2,
        "PROPERTY: entries parse from a CRC-failed footer (decode_from is CRC-independent)"
    );
    assert_eq!(
        parsed[0].event_hash, e0.event_hash,
        "PROPERTY: parsed untrusted entry preserves the content event_hash for corroboration"
    );
    assert_eq!(parsed[1].frame_offset, e1.frame_offset);
}

#[test]
fn untrusted_garbage_entry_table_parses_to_zero_entries() {
    // A trailer with absurd entry_count must yield ZERO entries (geometry guard),
    // forcing the fall-back path — never a partial/forged manifest.
    let mut bytes = vec![0xA5u8; 200];
    let trailer_start = bytes.len() - 16;
    bytes[trailer_start..trailer_start + 8].copy_from_slice(&0u64.to_le_bytes());
    // entry_count = u32::MAX → entries_block_len underflows entries_start → zero.
    bytes[trailer_start + 8..trailer_start + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[trailer_start + 12..trailer_start + 16]
        .copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC);

    let mut cursor = Cursor::new(bytes);
    let parsed = read_entries_unauthenticated(&mut cursor, 7).expect("must not error");
    assert!(
        parsed.is_empty(),
        "PROPERTY: an absurd entry_count yields zero entries (fall back), never a forged manifest"
    );
}

#[test]
fn resolve_untrusted_fails_closed_on_torn_last_committed_frame() {
    // ROUND-7 CASE (a): an UNTRUSTED footer whose SIDX manifest records THREE
    // committed frames, but the LAST committed frame is torn (only its header + 1
    // byte survive). Frames 0 and 1 are CRC-valid and corroborate their entries
    // (matching offset + length + content event_hash), anchoring the manifest to
    // THIS segment. The manifest then attests to a committed frame at offset == P
    // (the recovered prefix end) that is missing from the recovered stream → real
    // data loss → FAIL CLOSED, regardless of tail policy.
    let (f0, mut e0) = corroboratable_frame(0, 1, &serde_json::json!({"v": "first"}));
    e0.frame_offset = 0;
    let mut bytes = f0;

    let f1_off = bytes.len() as u64;
    let (f1, mut e1) = corroboratable_frame(f1_off, 2, &serde_json::json!({"v": "second"}));
    e1.frame_offset = f1_off;
    bytes.extend_from_slice(&f1);

    // The (committed) third frame: build it fully to compute its real length +
    // event_hash for the entry, but write only its header + a single payload byte
    // so it can NEVER decode (torn). Its recorded offset is the end of frame 1,
    // which is exactly the recovery stop P.
    let f2_off = bytes.len() as u64;
    let (f2_full, mut e2) = corroboratable_frame(f2_off, 3, &serde_json::json!({"v": "third"}));
    e2.frame_offset = f2_off;
    bytes.extend_from_slice(&f2_full[..9]); // 8-byte header + 1 payload byte = torn

    append_untrusted_footer(&mut bytes, &[e0, e1, e2]);
    let file_len = bytes.len() as u64;

    // STASH-VERIFY (the round-7 gap, made explicit): the OLD primitive
    // `crc_valid_frames_end` — the manifest-blind recover-the-prefix walk the three
    // sites used before this fix — returns Ok(P) for these exact bytes, SILENTLY
    // DROPPING the torn committed frame 2 (P == start of frame 2). This is the
    // round-7 data-loss bug. The NEW manifest path FAILS CLOSED on the same bytes.
    {
        let mut old_cursor = Cursor::new(bytes.clone());
        let old = crc_valid_frames_end(&mut old_cursor, 0, file_len, 7).expect(
            "OLD primitive recovers the prefix (the bug): torn tail has nothing valid after",
        );
        assert_eq!(
            old, f2_off,
            "STASH-VERIFY: the manifest-blind walk recovers the prefix and silently drops the \
             torn committed frame (recovery stop == start of the torn frame 2)"
        );
    }

    let mut cursor = Cursor::new(bytes);

    // FailClosed policy (the non-tail sealed-segment posture cold_start passes).
    let result = resolve_untrusted_frames_end(&mut cursor, 0, file_len, 7, true);
    assert!(
        matches!(
            result,
            Err(StoreError::CorruptSegment { segment_id: 7, .. })
        ),
        "PROPERTY: a corroborated SIDX manifest attesting to a torn/missing last committed frame \
         must FAIL CLOSED, not silently recover the prefix; got {result:?}"
    );

    // Honored REGARDLESS of tail policy: even the recover-torn-tail posture must
    // fail closed when a corroborated manifest proves a committed frame is missing.
    let mut cursor2 = {
        let (f0, mut e0) = corroboratable_frame(0, 1, &serde_json::json!({"v": "first"}));
        e0.frame_offset = 0;
        let mut bytes = f0;
        let f1_off = bytes.len() as u64;
        let (f1, mut e1) = corroboratable_frame(f1_off, 2, &serde_json::json!({"v": "second"}));
        e1.frame_offset = f1_off;
        bytes.extend_from_slice(&f1);
        let f2_off = bytes.len() as u64;
        let (f2_full, mut e2) = corroboratable_frame(f2_off, 3, &serde_json::json!({"v": "third"}));
        e2.frame_offset = f2_off;
        bytes.extend_from_slice(&f2_full[..9]);
        append_untrusted_footer(&mut bytes, &[e0, e1, e2]);
        Cursor::new(bytes)
    };
    let len2 = cursor2.get_ref().len() as u64;
    let result_recover = resolve_untrusted_frames_end(&mut cursor2, 0, len2, 7, false);
    assert!(
        matches!(result_recover, Err(StoreError::CorruptSegment { .. })),
        "PROPERTY: a corroborated missing committed frame fails closed even under \
         RecoverTornTail policy (data loss is policy-independent); got {result_recover:?}"
    );
}

#[test]
fn resolve_untrusted_recovers_all_when_frames_intact_and_footer_corrupt() {
    // ROUND-7 CASE (b): an UNTRUSTED (corrupt) footer but ALL committed frames are
    // intact and CRC-valid. Every SIDX entry corroborates a recovered frame and
    // none reference a frame at/past P → the manifest agrees the segment is
    // complete → RECOVER all frames (return the true frames_end).
    let (f0, mut e0) = corroboratable_frame(0, 1, &serde_json::json!({"v": "a"}));
    e0.frame_offset = 0;
    let mut bytes = f0;
    let f1_off = bytes.len() as u64;
    let (f1, mut e1) = corroboratable_frame(f1_off, 2, &serde_json::json!({"v": "b"}));
    e1.frame_offset = f1_off;
    bytes.extend_from_slice(&f1);
    let f2_off = bytes.len() as u64;
    let (f2, mut e2) = corroboratable_frame(f2_off, 3, &serde_json::json!({"v": "c"}));
    e2.frame_offset = f2_off;
    bytes.extend_from_slice(&f2);
    let frames_end = bytes.len() as u64;

    append_untrusted_footer(&mut bytes, &[e0, e1, e2]);
    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let recovered = resolve_untrusted_frames_end(&mut cursor, 0, file_len, 7, true)
        .expect("intact frames under a corrupt footer must recover, not fail closed");
    assert_eq!(
        recovered, frames_end,
        "PROPERTY: a corrupt footer over intact, corroborated frames recovers ALL committed frames"
    );
}

#[test]
fn resolve_untrusted_falls_back_to_prefix_when_no_entry_corroborates() {
    // ROUND-7 CASE (c): an UNTRUSTED footer whose entry table parses but NONE of
    // its entries corroborate a recovered frame (here: zero entries — the
    // unparseable/no-signal posture). With no anchored manifest there is no
    // trustworthy signal that a committed frame is missing, so recovery degrades
    // to the existing recover-the-CRC-valid-prefix behavior for BOTH policies —
    // no false fail-closed. We assert this for the strict FailClosed fall-back.
    let (f0, _e0) = corroboratable_frame(0, 1, &serde_json::json!({"v": "a"}));
    let mut bytes = f0;
    let f1_off = bytes.len() as u64;
    let (f1, _e1) = corroboratable_frame(f1_off, 2, &serde_json::json!({"v": "b"}));
    bytes.extend_from_slice(&f1);
    let frames_end = bytes.len() as u64;

    // Footer records ZERO entries (empty manifest) but is corrupted to UNTRUSTED.
    append_untrusted_footer(&mut bytes, &[]);
    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let recovered = resolve_untrusted_frames_end(&mut cursor, 0, file_len, 7, true)
        .expect("an empty/unparseable manifest must fall back to prefix recovery, not fail closed");
    assert_eq!(
        recovered, frames_end,
        "PROPERTY: with no corroborating entry, recovery falls back to the CRC-valid prefix \
         (no false fail-closed) even under the strict FailClosed policy"
    );
}

#[test]
fn resolve_untrusted_recovers_for_garbage_entry_table() {
    // ROUND-7 CASE (c), garbage variant: an UNTRUSTED footer whose entry table is
    // unparseable (absurd entry_count). read_entries_unauthenticated returns zero
    // entries → no corroboration → fall back to prefix recovery. This is the
    // "untrusted offset is inert" posture extended to a garbage manifest: never a
    // false fail-closed.
    let (f0, _e0) = corroboratable_frame(0, 1, &serde_json::json!({"v": "x"}));
    let mut bytes = f0;
    let f1_off = bytes.len() as u64;
    let (f1, _e1) = corroboratable_frame(f1_off, 2, &serde_json::json!({"v": "y"}));
    bytes.extend_from_slice(&f1);
    let frames_end = bytes.len() as u64;

    // Append a 16-byte trailer with valid magic but an absurd entry_count so the
    // geometry guard yields zero entries. (No CRC region behind it → also breaks
    // any authentication; the boundary is untrusted and the manifest is empty.)
    let mut trailer = [0u8; 16];
    trailer[0..8].copy_from_slice(&frames_end.to_le_bytes());
    trailer[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    trailer[12..16].copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC);
    bytes.extend_from_slice(&trailer);

    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let recovered = resolve_untrusted_frames_end(&mut cursor, 0, file_len, 7, true)
        .expect("a garbage entry table must fall back to prefix recovery, not fail closed");
    assert_eq!(
        recovered, frames_end,
        "PROPERTY: a garbage/unparseable entry table degrades to prefix recovery (no false \
         fail-closed)"
    );
}

#[test]
fn resolve_untrusted_legacy_sdx2_intact_frames_recovers() {
    // ROUND-7: a legacy SDX2 footer (never CRC-authenticated → always UNTRUSTED)
    // over intact frames. The SDX2 entry table is parseable by
    // read_entries_unauthenticated (it accepts both magics) and its entries
    // corroborate the recovered frames → RECOVER all.
    let (f0, mut e0) = corroboratable_frame(0, 1, &serde_json::json!({"v": "a"}));
    e0.frame_offset = 0;
    let mut bytes = f0;
    let f1_off = bytes.len() as u64;
    let (f1, mut e1) = corroboratable_frame(f1_off, 2, &serde_json::json!({"v": "b"}));
    e1.frame_offset = f1_off;
    bytes.extend_from_slice(&f1);
    let frames_end = bytes.len() as u64;

    // Write a real SDX3 footer, then rewrite the trailing magic to legacy SDX2 so
    // the boundary reads as un-CRC'd/untrusted while the entry geometry is intact.
    append_untrusted_footer(&mut bytes, &[e0, e1]);
    let n = bytes.len();
    bytes[n - 4..n].copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC_LEGACY_SDX2);

    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let recovered = resolve_untrusted_frames_end(&mut cursor, 0, file_len, 7, true)
        .expect("legacy SDX2 over intact frames must recover");
    assert_eq!(
        recovered, frames_end,
        "PROPERTY: a legacy SDX2 manifest corroborates intact frames and recovers them all"
    );
}

#[test]
fn resolve_untrusted_still_fails_closed_on_mid_stream_corruption() {
    // ROUND-5 invariant preserved THROUGH the manifest path: mid-stream corruption
    // (a CRC-valid frame after the first bad frame) must still FAIL CLOSED before
    // the manifest is even consulted. The walk's look-ahead resync fires inside
    // crc_valid_frames_end_with_map exactly as in crc_valid_frames_end.
    let (bytes, frames_end) = frames_then_sdx3_footer(&["a", "b", "c", "d", "e"]);
    let mut bytes = bytes;
    let f2_start = frame_total_len_at(&bytes, 0);
    let f3_start = f2_start + frame_total_len_at(&bytes, f2_start);
    let third_payload_byte = usize::try_from(f3_start + 8).expect("offset fits usize");
    assert!((third_payload_byte as u64) < frames_end);
    bytes[third_payload_byte] ^= 0x01; // break the third frame's CRC (interior)
    let file_len = bytes.len() as u64;
    let mut cursor = Cursor::new(bytes);
    let result = resolve_untrusted_frames_end(&mut cursor, 0, file_len, 7, true);
    assert!(
        matches!(
            result,
            Err(StoreError::CorruptSegment { segment_id: 7, .. })
        ),
        "PROPERTY: mid-stream corruption still fails closed via the manifest path; got {result:?}"
    );
}

#[test]
fn corroborate_property_missing_trailing_frame_fails_intact_recovers() {
    // PROPERTY (adversarial, unit-level): for a manifest with >= 1 corroborated
    // entry, ANY entry naming a committed frame at/after P that is absent from R
    // forces FailClosed; an intact set (every entry present in R) recovers. We
    // drive corroborate_untrusted_entries directly over synthetic R + entries.
    let recovered: RecoveredFrameMap = [
        (
            0u64,
            RecoveredFrame {
                frame_length: 64,
                event_hash: Some([7u8; 32]),
            },
        ),
        (
            64u64,
            RecoveredFrame {
                frame_length: 64,
                event_hash: Some([8u8; 32]),
            },
        ),
    ]
    .into_iter()
    .collect();
    let p = 128u64; // recovered prefix end (two 64-byte frames)

    let entry = |frame_offset: u64, frame_length: u32, event_hash: [u8; 32]| {
        crate::store::segment::sidx::SidxEntry {
            event_id: 1,
            entity_idx: 0,
            scope_idx: 0,
            kind: kind_to_raw(EventKind::custom(0x1, 1)),
            wall_ms: 1,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            prev_hash: [0; 32],
            event_hash,
            frame_offset,
            frame_length,
            global_sequence: 1,
            correlation_id: 1,
            causation_id: 0,
        }
    };

    // Intact: both entries corroborate, none past P → recover prefix.
    let intact = vec![entry(0, 64, [7; 32]), entry(64, 64, [8; 32])];
    assert_eq!(
        corroborate_untrusted_entries(&intact, &recovered, p, true),
        UntrustedRecovery::RecoverPrefix(p),
        "PROPERTY: a fully-corroborated manifest with nothing past P recovers the prefix"
    );

    // Missing trailing committed frame: one corroborated anchor + an entry naming a
    // committed frame at offset == P that is NOT in R → fail closed.
    let missing = vec![entry(0, 64, [7; 32]), entry(128, 64, [9; 32])];
    assert_eq!(
        corroborate_untrusted_entries(&missing, &recovered, p, true),
        UntrustedRecovery::FailClosed,
        "PROPERTY: an anchored manifest attesting to a committed frame at/after P missing from R \
         always fails closed"
    );

    // No corroboration (forged hashes): zero anchored entries → fall back to prefix.
    let forged = vec![entry(0, 64, [0xAA; 32]), entry(128, 64, [0xBB; 32])];
    assert_eq!(
        corroborate_untrusted_entries(&forged, &recovered, p, true),
        UntrustedRecovery::RecoverPrefix(p),
        "PROPERTY: an un-anchored (forged) manifest is inert — fall back to prefix, no false \
         fail-closed"
    );
}
