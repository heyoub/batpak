//! Untrusted-footer recovery via the SIDX entry table as a self-authenticating
//! manifest — the round-7 "cake-and-eat-it" resolution.
//!
//! For an UNTRUSTED footer boundary (CRC-failed SDX3, legacy un-CRC'd SDX2, or a
//! forged trailer) the trailer `string_table_offset` is unauthenticated and must
//! never bound recovery. Plain CRC-valid-frame recovery (see
//! [`super::crc_valid_frames_end`]) recovers the prefix and fails closed on
//! mid-stream corruption, but it CANNOT distinguish a torn/corrupt LAST committed
//! frame (followed by the footer) from "intact frames + footer" — so it silently
//! drops a committed event, ignoring a caller's `FailClosed` posture. This module
//! closes that gap by corroborating the CRC-independent SIDX entry table against
//! the independently CRC-verified recovered frames:
//!
//! 1. parse the entry table WITHOUT requiring the footer CRC (every entry is an
//!    UNTRUSTED HYPOTHESIS);
//! 2. recover the CRC-valid prefix and build the recovered-frame map `R`;
//! 3. corroborate each entry against `R` by (offset, length, content event_hash);
//! 4. decide: a corroborated manifest attesting to a committed frame missing from
//!    the recovered stream FAILS CLOSED (real data loss); a corroborated manifest
//!    over intact frames RECOVERS; an uncorroborated/unparseable manifest falls
//!    back to the existing tail-policy prefix recovery (inert, no false
//!    fail-closed).
//!
//! Load-bearing assumptions (validated against the writer):
//! - A corroborated entry anchors the WHOLE table to this segment: a forger
//!   cannot match a real frame's content-addressed blake3 `event_hash`, so once
//!   one entry corroborates, `entry_count` and the append-ordered entries are
//!   trustworthy enough to assert "frame N should exist."
//! - SIDX entries cover ONLY committed frames; batch BEGIN/COMMIT markers are real
//!   frames but NOT SIDX entries, and entries are recorded post-COMMIT. So we
//!   match entries↔frames by (offset, length, event_hash), never assume
//!   contiguity, and only assert a missing-frame for a committed (SIDX-recorded)
//!   frame — leaving the scan loops' BatchRecoveryState discard logic untouched.

use super::{crc_valid_frame_exists_after, frame_decode, sidx, try_decode_frame_at, StoreError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

/// Minimal view of a frame's serialized `FramePayload` used during untrusted
/// recovery to extract the content-addressed `event_hash` for corroboration.
///
/// We deserialize ONLY the `event.hash_chain` field (everything else is
/// `IgnoredAny`) so the corroboration walk stays cheap. The `event_hash` here is
/// the blake3 of the event content (`event.hash_chain.event_hash`) — the SAME
/// value the writer records into each [`sidx::SidxEntry`]. A forger cannot match
/// it for a real frame, which is what makes a corroborated entry trustworthy
/// despite the failed footer CRC.
#[derive(Deserialize)]
struct CorroborationFramePayload {
    event: CorroborationEvent,
}

#[derive(Deserialize)]
struct CorroborationEvent {
    #[serde(rename = "header")]
    _header: serde::de::IgnoredAny,
    #[serde(rename = "payload")]
    _payload: serde::de::IgnoredAny,
    hash_chain: Option<crate::event::HashChain>,
}

/// One recovered, CRC-verified frame keyed by its byte offset: its on-disk
/// `frame_length` and its content-addressed `event_hash` (blake3 of the event
/// content). This is the trusted side of corroboration — these bytes decoded
/// cleanly under their own per-frame CRC, so the `event_hash` here is genuine.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RecoveredFrame {
    pub(crate) frame_length: u32,
    pub(crate) event_hash: Option<[u8; 32]>,
}

/// The map `R` from offset → recovered frame, built during the CRC-valid walk.
pub(crate) type RecoveredFrameMap = BTreeMap<u64, RecoveredFrame>;

/// The outcome of the untrusted-footer recovery decision.
///
/// `RecoverPrefix(end)` means: recover the CRC-valid frame region `[frames_start
/// .. end)`. `FailClosed` means a CORROBORATED manifest attests to a committed
/// frame the recovered stream is missing — real data loss — so the caller must
/// surface [`StoreError::CorruptSegment`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UntrustedRecovery {
    /// Recover the CRC-valid prefix that ends at this offset.
    RecoverPrefix(u64),
    /// A corroborated manifest proves a committed frame is missing → fail closed.
    FailClosed,
}

/// Walk the CRC-valid frames from `frames_start`, building the recovered-frame
/// map `R` AND applying the same mid-stream-corruption fail-closed rule as
/// [`crc_valid_frames_end`]. Returns `(stop_offset, R)` where `stop_offset` (P)
/// is the first non-decodable position (the recovered prefix end) and `R` maps
/// each recovered frame's offset to its `(frame_length, event_hash)`.
///
/// This is the trusted, CRC-verified half of the "cake-and-eat-it" untrusted
/// recovery: every entry in `R` comes from a frame whose own CRC passed, so its
/// `event_hash` is genuine and can corroborate (or refute) an untrusted SIDX
/// entry. Mid-stream corruption (a CRC-valid frame after P) still fails closed
/// here exactly as `crc_valid_frames_end` does — the manifest path is layered ON
/// TOP of that guard, never replacing it.
///
/// # Errors
/// Returns [`StoreError::Io`] on seek/read failure, or
/// [`StoreError::CorruptSegment`] on mid-stream corruption (same contract as
/// [`crc_valid_frames_end`]).
pub(super) fn crc_valid_frames_end_with_map<R: Read + Seek>(
    source: &mut R,
    frames_start: u64,
    file_len: u64,
    segment_id: u64,
) -> Result<(u64, RecoveredFrameMap), StoreError> {
    let mut cursor = frames_start;
    let mut recovered: RecoveredFrameMap = BTreeMap::new();

    loop {
        if cursor >= file_len {
            return Ok((file_len, recovered));
        }
        match try_decode_frame_at(source, cursor, file_len)? {
            Some(frame_size) => {
                // Re-read the frame bytes to extract its content `event_hash`. The
                // frame already CRC-validated in try_decode_frame_at, so this only
                // deserializes the (small) header/hash_chain prefix. A frame whose
                // hash_chain is absent or whose payload cannot be deserialized still
                // counts as a recovered frame (length known) but carries no
                // event_hash, so it can never corroborate an entry — a conservative,
                // safe default.
                let event_hash = read_frame_event_hash(source, cursor, frame_size);
                let frame_length = u32::try_from(frame_size).ok();
                if let Some(frame_length) = frame_length {
                    recovered.insert(
                        cursor,
                        RecoveredFrame {
                            frame_length,
                            event_hash,
                        },
                    );
                }
                cursor = match cursor.checked_add(frame_size) {
                    Some(next) => next,
                    None => return Ok((cursor, recovered)),
                };
            }
            None => {
                let resync_from = match cursor.checked_add(1) {
                    Some(next) => next,
                    None => return Ok((cursor, recovered)),
                };
                if crc_valid_frame_exists_after(source, resync_from, file_len)? {
                    return Err(StoreError::corrupt_segment_with_detail(
                        segment_id,
                        format!(
                            "mid-stream corruption: frame at offset {cursor} is non-decodable but a \
                             CRC-valid frame follows before EOF (file_len {file_len}); refusing to \
                             silently truncate to the prefix during untrusted-footer recovery"
                        ),
                    ));
                }
                return Ok((cursor, recovered));
            }
        }
    }
}

/// Extract the content `event_hash` from the CRC-valid frame of size `frame_size`
/// that begins at `at`. Returns `None` if the frame cannot be re-read or its
/// `FramePayload`/`hash_chain` cannot be deserialized — a frame with no usable
/// hash simply cannot corroborate an entry. Never errors: a corroboration miss is
/// always safe (it degrades to fall-back), so any failure here is `None`.
fn read_frame_event_hash<R: Read + Seek>(
    source: &mut R,
    at: u64,
    frame_size: u64,
) -> Option<[u8; 32]> {
    let total = usize::try_from(frame_size).ok()?;
    if total < 8 {
        return None;
    }
    if source.seek(SeekFrom::Start(at)).is_err() {
        return None;
    }
    let mut frame = vec![0u8; total];
    if source.read_exact(&mut frame).is_err() {
        return None;
    }
    // frame_decode strips the 8-byte [len][crc] header and re-verifies the CRC.
    let (msgpack, _consumed) = frame_decode(&frame).ok()?;
    let payload: CorroborationFramePayload = crate::encoding::from_bytes(msgpack).ok()?;
    payload.event.hash_chain.map(|chain| chain.event_hash)
}

/// Corroborate untrusted SIDX entries against the CRC-verified recovered frames
/// `R`, then decide whether to recover the prefix or fail closed.
///
/// CONTRACT (load-bearing assumptions, validated against the writer):
///
/// 1. A corroborated entry ANCHORS the entire entry table to THIS segment. An
///    entry is corroborated iff there is a recovered frame at `entry.frame_offset`
///    whose `frame_length` AND content `event_hash` (blake3) match the entry. The
///    `event_hash` is content-addressed and unforgeable: a forger cannot fabricate
///    an entry that matches a real frame's blake3. So once >= 1 entry corroborates,
///    the append-ordered entries and `entry_count` are trustworthy enough to assert
///    "a committed frame at offset X should exist."
///
/// 2. SIDX entries cover ONLY committed frames. Batch BEGIN/COMMIT markers are real
///    frames but are NOT SIDX entries, and entries are recorded post-COMMIT. So we
///    match entries↔frames by (offset, length, event_hash), NEVER assume frame
///    contiguity, and only assert a missing-frame for a committed (SIDX-recorded)
///    frame. This does not touch the cross-segment batch discard logic in the scan
///    loops — it only chooses where the CRC-valid frame region ends.
///
/// DECISION (only ever invoked on the UNTRUSTED footer path):
///
/// - (a) FAIL CLOSED iff at least one entry corroborates (the manifest is
///   anchored to this segment) AND some anchored entry references a committed
///   frame at an offset at or after P (the recovery stop) that is NOT in `R`. The
///   manifest attests to a trailing committed frame the stream is missing — the
///   torn-last-frame-under-corrupt-footer case. Honored REGARDLESS of tail
///   policy: a corroborated manifest proving missing committed data is real data
///   loss.
/// - (b) RECOVER the prefix iff at least one entry corroborates and every entry
///   the anchored manifest references either maps to a recovered frame or lies
///   strictly before P (manifest agrees the recovered region is complete).
/// - (c) FALL BACK to existing tail-policy behavior (recover the CRC-valid
///   prefix) iff ZERO entries corroborate (unparseable table or no trustworthy
///   signal) — same posture as "an untrusted offset is inert." The
///   `fallback_fail_closed` flag is threaded for this case; with no corroborated
///   manifest there is no trustworthy signal that a committed frame is missing,
///   so the prefix is recovered for BOTH policies (no false fail-closed). The
///   round-7 trigger is honored by case (a), which is independent of policy.
pub(crate) fn corroborate_untrusted_entries(
    entries: &[sidx::SidxEntry],
    recovered: &RecoveredFrameMap,
    recovery_stop: u64,
    fallback_fail_closed: bool,
) -> UntrustedRecovery {
    // An entry is CORROBORATED when a recovered frame sits at its offset with a
    // matching length AND a matching content event_hash. event_hash match is the
    // unforgeable anchor.
    let is_corroborated = |entry: &sidx::SidxEntry| -> bool {
        match recovered.get(&entry.frame_offset) {
            Some(frame) => {
                frame.frame_length == entry.frame_length
                    && frame.event_hash == Some(entry.event_hash)
            }
            None => false,
        }
    };

    let any_corroborated = entries.iter().any(is_corroborated);
    if !any_corroborated {
        // (c) No trustworthy signal — the manifest is not anchored to this segment
        // (or is unparseable / empty). Degrade to the existing tail-policy behavior:
        // recover the CRC-valid prefix. `fallback_fail_closed` is intentionally not
        // used to override this — without a corroborated manifest there is no proof a
        // committed frame is missing, and a false fail-closed here would brick cold
        // start / compaction on a benign corrupt-footer + intact-frames segment.
        let _ = fallback_fail_closed;
        return UntrustedRecovery::RecoverPrefix(recovery_stop);
    }

    // (a) The manifest is anchored. Any entry that names a COMMITTED frame at or
    // past the recovery stop P which is NOT present in R proves the recovered
    // stream dropped a committed frame (torn last frame under a corrupt footer).
    // Match by (offset, length, event_hash); contiguity is never assumed.
    for entry in entries {
        if entry.frame_offset >= recovery_stop {
            // The manifest claims a committed frame at/after P. It is present in R
            // only if a recovered frame at that offset matches length + content
            // hash. (R never holds offsets >= P — the walk stopped at P — so this is
            // always "missing", but we keep the explicit corroboration check so the
            // intent is self-documenting and robust to future map changes.)
            let present = recovered.get(&entry.frame_offset).is_some_and(|frame| {
                frame.frame_length == entry.frame_length
                    && frame.event_hash == Some(entry.event_hash)
            });
            if !present {
                return UntrustedRecovery::FailClosed;
            }
        }
    }

    // (b) Every committed frame the anchored manifest names is either present in R
    // or strictly before P. The manifest agrees the recovered region is complete.
    UntrustedRecovery::RecoverPrefix(recovery_stop)
}

/// Resolve the frame-region end for an UNTRUSTED footer boundary using the SIDX
/// entry table as a self-authenticating manifest (the "cake-and-eat-it" path).
///
/// This is the single entry point the three scan/compaction sites call instead of
/// bare [`crc_valid_frames_end`]. It:
///   1. walks the CRC-valid frames (recovering the prefix + building `R`, and
///      still failing closed on mid-stream corruption — unchanged round-5/6
///      behavior);
///   2. parses the untrusted entry table (zero entries on any parse failure);
///   3. corroborates entries against `R` and decides (case a/b/c above).
///
/// `fallback_fail_closed` is the caller's [`scan::FrameScanTailPolicy`] reduced to
/// a bool (FailClosed → true), threaded through for case (c). It is passed as a
/// bool to keep this module decoupled from the scan layer.
///
/// # Errors
/// Returns [`StoreError::Io`] on read failure, or [`StoreError::CorruptSegment`]
/// for mid-stream corruption (from the walk) or a corroborated missing committed
/// frame (case a).
pub(crate) fn resolve_untrusted_frames_end<R: Read + Seek>(
    source: &mut R,
    frames_start: u64,
    file_len: u64,
    segment_id: u64,
    fallback_fail_closed: bool,
) -> Result<u64, StoreError> {
    // Step 2/4c: parse the untrusted entry table FIRST (it seeks to EOF). Zero
    // entries on any parse failure → pure fall-back.
    let entries = sidx::read_entries_unauthenticated(source, segment_id)?;

    // Step 2/4b: recover the CRC-valid prefix + build R. This is the mid-stream
    // corruption guard; it errors before we ever consult the manifest.
    let (recovery_stop, recovered) =
        crc_valid_frames_end_with_map(source, frames_start, file_len, segment_id)?;

    // Step 3/4: corroborate + decide.
    match corroborate_untrusted_entries(&entries, &recovered, recovery_stop, fallback_fail_closed) {
        UntrustedRecovery::RecoverPrefix(end) => Ok(end),
        UntrustedRecovery::FailClosed => Err(StoreError::corrupt_segment_with_detail(
            segment_id,
            format!(
                "untrusted-footer recovery: a corroborated SIDX manifest entry attests to a \
                 committed frame at/after the recovered prefix end {recovery_stop} that is missing \
                 from the CRC-valid frame stream (file_len {file_len}); a torn/corrupt last \
                 committed frame under a corrupt footer would silently drop a committed event — \
                 refusing to recover"
            ),
        )),
    }
}
