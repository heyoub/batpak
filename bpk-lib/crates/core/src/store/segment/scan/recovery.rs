mod batch;
mod sidx_fast_path;
mod tail;

pub(crate) use batch::BatchRecoveryState;
use tail::PayloadReadFailure;

use super::{read_frame_header_or_clean_eof, FrameScanTailPolicy, Reader, ScannedIndexEntry};
use crate::event::{EventHeader, EventKind, HashChain};
use crate::store::segment::{self, SegmentHeader, SEGMENT_MAGIC};
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

impl Reader {
    /// Scan only the metadata required to rebuild the in-memory index.
    /// Tries the SIDX footer first (O(1) seek + bulk read); falls back to
    /// frame-by-frame msgpack deserialization if no SIDX footer is present.
    /// Accepts optional `batch_state` for cross-segment batch recovery.
    ///
    /// **SIDX fast-path contract.** The SIDX fast-path may be used only
    /// when the caller has no pending batch (`batch_state.in_batch ==
    /// false`) AND this segment does not itself carry a cross-segment
    /// batch — i.e., its SIDX entries cover every frame in the segment.
    /// If a BEGIN marker in this segment rotates before its COMMIT, the
    /// SIDX written at rotation is empty of that batch's items (items
    /// are recorded to the collector only after COMMIT succeeds), and
    /// the next segment's frame-scan needs the batch-in-progress state
    /// to match its COMMIT against. In that case we must frame-scan this
    /// segment so the BEGIN and staged items propagate via
    /// `BatchRecoveryState`. Otherwise a cross-segment batch is silently
    /// dropped on recovery.
    ///
    /// This lets cold-start rebuild stream scanned entries straight into the
    /// replay cursor instead of allocating a per-segment `Vec` only to fold it
    /// again immediately afterward.
    pub(crate) fn scan_segment_index_into_with_tail_policy<F>(
        &self,
        path: &Path,
        mut batch_state: Option<&mut BatchRecoveryState>,
        tail_policy: FrameScanTailPolicy,
        mut sink: F,
    ) -> Result<(), StoreError>
    where
        F: FnMut(ScannedIndexEntry) -> Result<(), StoreError>,
    {
        // Fast path: try SIDX footer for sealed segments only.
        // Sealed segments cannot have incomplete batches, so SIDX is safe.
        // Active segment might have incomplete batches, so use slow path.
        // A malformed filename is soft-skipped: we cannot safely attribute
        // its frames to any segment id, so the safe move is to log and
        // return rather than guess.
        let segment_id = match segment::SegmentId::from_filename(path) {
            Ok(parsed) => parsed.as_u64(),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping malformed segment filename"
                );
                return Ok(());
            }
        };
        let batch_in_progress = batch_state.as_ref().is_some_and(|state| state.in_batch);
        if self.try_sidx_fast_path(path, segment_id, batch_in_progress, &mut sink)? {
            return Ok(());
        }

        let mut file = crate::store::platform::fs::open_file(path).map_err(StoreError::Io)?;
        let file_len = file.seek(SeekFrom::End(0)).map_err(StoreError::Io)?;
        let boundary = segment::detect_sidx_boundary(&mut file, file_len, segment_id)?;
        let frames_end = boundary.map_or(file_len, |b| b.frames_end);
        // An untrusted footer boundary (CRC-failed SDX3, legacy SDX2, or forged
        // trailer) yields an unauthenticated `frames_end` hint that may over-read
        // into the corrupt footer. Only then do we recover-what-was-found. With NO
        // footer, frames legitimately run to EOF and mid-stream corruption must
        // still FailClosed — so this is gated on the footer actually being present.
        let untrusted_boundary = boundary.is_some_and(|b| !b.trusted);
        file.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;

        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(StoreError::Io)?;
        if &magic != SEGMENT_MAGIC {
            return Err(StoreError::corrupt_magic(segment_id));
        }

        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .map_err(StoreError::Io)?;
        let header_len =
            Self::checked_header_len(segment_id, u32::from_be_bytes(header_len_buf) as usize)?;
        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf).map_err(StoreError::Io)?;
        let header: SegmentHeader = crate::encoding::from_bytes(&header_buf)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;
        if header.version != 1 {
            return Err(StoreError::corrupt_version(segment_id, header.version));
        }

        let mut cursor = (8 + header_len) as u64;

        // Lower-bound check (TRUSTED boundaries only): an authenticated SDX3
        // `string_table_offset` must not fall below the start of the frame region.
        // A corrupt-but-authenticated offset < cursor would make the scan loop
        // break on the first iteration and return an empty Ok(()) with zero events
        // — silent data loss. Erroring with CorruptSegment is the correct DO-178B
        // behavior. frames_end == cursor (empty frame region) stays valid. For an
        // UNTRUSTED boundary the offset is garbage and discarded below (recovery
        // walks from `cursor` bounded by `file_len`), so a too-low untrusted hint
        // must NOT error — it recovers all CRC-valid frames instead.
        if !untrusted_boundary && frames_end < cursor {
            return Err(StoreError::corrupt_segment_with_detail(
                segment_id,
                format!(
                    "SIDX string_table_offset {frames_end} is below the frame region start \
                     {cursor} (8 + header_len {header_len})"
                ),
            ));
        }

        // Resolve the frame-region end based on the offset's provenance.
        //
        // TRUSTED (CRC-valid SDX3 footer): the offset is authoritative and
        // byte-for-byte authenticated by the footer CRC, so it cannot be
        // truncating. A frame-decode failure BEFORE this boundary is genuine
        // mid-stream corruption and the scan loop below FailCloses on it.
        //
        // UNTRUSTED (CRC-failed SDX3, legacy SDX2, or forged trailer): the offset
        // is GARBAGE — it may point too LOW (truncating real frames), MID-FRAME
        // (inside a later CRC-valid frame), or too HIGH (into the corrupt footer).
        // Trusting it as a bound either silently drops CRC-valid frames or makes
        // the scan parse footer bytes as frame headers and FailClose. So discard
        // the hint entirely and recover via the SIDX-manifest path
        // (`resolve_untrusted_frames_end`): it walks the CRC-valid frames bounded
        // only by `file_len` (truncation-proof, still FailClosed on mid-stream
        // corruption), AND consults the CRC-independent SIDX entry table as a
        // self-authenticating manifest. If a CORROBORATED entry (matching offset +
        // length + content event_hash of a recovered frame) attests to a committed
        // frame at/after the recovered prefix end that the stream is missing — the
        // torn-last-frame-under-corrupt-footer case (round-7) — it FailCloses
        // regardless of tail policy. With no corroborated manifest it degrades to
        // the existing recover-the-prefix behavior, honoring `tail_policy` for that
        // fall-back.
        let frames_end = if untrusted_boundary {
            segment::resolve_untrusted_frames_end(
                &mut file,
                cursor,
                file_len,
                segment_id,
                !tail_policy.can_recover_torn_tail(),
            )?
        } else {
            frames_end
        };
        file.seek(SeekFrom::Start(cursor)).map_err(StoreError::Io)?;

        let mut local_state = BatchRecoveryState::default();
        let state_ref: &mut BatchRecoveryState = match batch_state {
            Some(ref mut s) => s,
            None => &mut local_state,
        };

        loop {
            if cursor >= frames_end {
                break;
            }
            let frame_offset = cursor;
            let Some(frame_header) =
                read_frame_header_or_clean_eof(&mut file).map_err(StoreError::Io)?
            else {
                if state_ref.in_batch {
                    tracing::warn!(
                        segment_id,
                        staged_count = state_ref.staged.len(),
                        "incomplete batch at EOF, will discard or continue in next segment"
                    );
                }
                break;
            };

            let payload_len = u32::from_be_bytes([
                frame_header[0],
                frame_header[1],
                frame_header[2],
                frame_header[3],
            ]) as usize;
            if Self::payload_len_exceeds_max(payload_len) {
                if tail_policy.can_recover_torn_tail() {
                    tracing::warn!(
                        segment_id,
                        payload_len,
                        "frame payload exceeds MAX_FRAME_PAYLOAD, stopping segment scan as torn tail"
                    );
                    break;
                }
                return Err(StoreError::CorruptFrame {
                    segment_id,
                    offset: frame_offset,
                    reason: format!("frame payload length {payload_len} exceeds MAX_FRAME_PAYLOAD"),
                });
            }
            let frame_tail = frame_offset
                .checked_add(8)
                .and_then(|base| base.checked_add(u64::try_from(payload_len).ok()?))
                .ok_or_else(|| {
                    StoreError::corrupt_segment_with_detail(segment_id, "frame tail overflow")
                })?;
            if frame_tail > frames_end {
                if tail_policy.can_recover_torn_tail() && frames_end == file_len {
                    break;
                }
                return Err(StoreError::CorruptFrame {
                    segment_id,
                    offset: frame_offset,
                    reason: "frame payload extends past the frame region".into(),
                });
            }
            let mut frame_buf = self.acquire_buffer(8 + payload_len);
            frame_buf[..8].copy_from_slice(&frame_header);
            if let Err(error) = file.read_exact(&mut frame_buf[8..]) {
                self.release_buffer(frame_buf);
                match tail::classify_payload_read_error(segment_id, error, tail_policy)? {
                    PayloadReadFailure::RecoverTornTail => {
                        break;
                    }
                }
            }

            match segment::frame_decode(&frame_buf) {
                Ok((msgpack, frame_size)) => {
                    match crate::encoding::from_bytes::<IndexScanFramePayload>(msgpack) {
                        Ok(payload) => {
                            let kind = payload.event.header.event_kind;

                            if !state_ref.in_batch {
                                if kind == EventKind::SYSTEM_BATCH_BEGIN {
                                    let batch_count = Self::checked_batch_count(
                                        segment_id,
                                        frame_offset,
                                        payload.event.header.payload_size,
                                    )?;
                                    state_ref.in_batch = true;
                                    state_ref.remaining = batch_count;
                                    state_ref.started_count = batch_count;
                                    let batch_capacity = usize::try_from(batch_count).map_err(
                                        |_| StoreError::CorruptFrame {
                                            segment_id,
                                            offset: frame_offset,
                                            reason: format!(
                                                "validated batch count {batch_count} does not fit usize"
                                            ),
                                        },
                                    )?;
                                    state_ref.staged.reserve(batch_capacity);
                                } else if kind == EventKind::SYSTEM_BATCH_COMMIT {
                                    tracing::warn!(
                                        segment_id,
                                        offset = frame_offset,
                                        "orphaned COMMIT marker, skipping"
                                    );
                                } else {
                                    let hash_chain = Self::required_index_hash_chain(
                                        &payload.event,
                                        segment_id,
                                        frame_offset,
                                    )?;
                                    let length = u32::try_from(frame_size).map_err(|_| {
                                        StoreError::CorruptFrame {
                                            segment_id,
                                            offset: frame_offset,
                                            reason: format!(
                                                "frame size {frame_size} overflows u32"
                                            ),
                                        }
                                    })?;
                                    sink(ScannedIndexEntry {
                                        header: payload.event.header,
                                        entity: payload.entity,
                                        scope: payload.scope,
                                        hash_chain,
                                        segment_id,
                                        offset: frame_offset,
                                        length,
                                        receipt_extensions: payload.receipt_extensions,
                                        global_sequence: None,
                                    })?;
                                }
                            } else if kind == EventKind::SYSTEM_BATCH_COMMIT {
                                if state_ref.remaining == 0 {
                                    let completed_batch = std::mem::take(&mut state_ref.staged);
                                    for entry in completed_batch {
                                        sink(entry)?;
                                    }
                                    state_ref.in_batch = false;
                                    tracing::debug!(
                                        segment_id,
                                        batch_count = state_ref.started_count,
                                        "batch committed via COMMIT marker"
                                    );
                                } else {
                                    tracing::warn!(
                                        segment_id,
                                        expected = state_ref.started_count,
                                        remaining = state_ref.remaining,
                                        staged_count = state_ref.staged.len(),
                                        "batch COMMIT mismatch, discarding"
                                    );
                                }
                                state_ref.in_batch = false;
                                state_ref.staged.clear();
                            } else if kind == EventKind::SYSTEM_BATCH_BEGIN {
                                tracing::warn!(
                                    segment_id,
                                    staged_count = state_ref.staged.len(),
                                    "nested BEGIN without COMMIT, discarding incomplete batch"
                                );
                                let batch_count = Self::checked_batch_count(
                                    segment_id,
                                    frame_offset,
                                    payload.event.header.payload_size,
                                )?;
                                state_ref.remaining = batch_count;
                                state_ref.started_count = batch_count;
                                state_ref.staged.clear();
                                let batch_capacity =
                                    usize::try_from(batch_count).map_err(|_| {
                                        StoreError::CorruptFrame {
                                            segment_id,
                                            offset: frame_offset,
                                            reason: format!(
                                            "validated batch count {batch_count} does not fit usize"
                                        ),
                                        }
                                    })?;
                                state_ref.staged.reserve(batch_capacity);
                            } else {
                                let hash_chain = Self::required_index_hash_chain(
                                    &payload.event,
                                    segment_id,
                                    frame_offset,
                                )?;
                                let length = u32::try_from(frame_size).map_err(|_| {
                                    StoreError::CorruptFrame {
                                        segment_id,
                                        offset: frame_offset,
                                        reason: format!("frame size {frame_size} overflows u32"),
                                    }
                                })?;
                                let entry = ScannedIndexEntry {
                                    header: payload.event.header,
                                    entity: payload.entity,
                                    scope: payload.scope,
                                    hash_chain,
                                    segment_id,
                                    offset: frame_offset,
                                    length,
                                    receipt_extensions: payload.receipt_extensions,
                                    global_sequence: None,
                                };
                                if !state_ref.stage_entry(entry) {
                                    tracing::warn!(
                                        segment_id,
                                        offset = frame_offset,
                                        expected = state_ref.started_count,
                                        "batch contains more items than declared, discarding"
                                    );
                                    state_ref.discard_incomplete();
                                }
                            }
                        }
                        Err(error) => {
                            if state_ref.in_batch {
                                tracing::warn!(
                                    segment_id,
                                    staged_count = state_ref.staged.len(),
                                    "discarding incomplete batch due to unreadable frame metadata"
                                );
                                state_ref.staged.clear();
                                state_ref.in_batch = false;
                            }
                            return Err(StoreError::CorruptSegment {
                                segment_id,
                                detail: format!(
                                    "frame at offset {frame_offset} has unreadable index metadata: {error}"
                                ),
                            });
                        }
                    }
                    cursor += frame_size as u64;
                }
                Err(error) => {
                    let is_crc_mismatch =
                        matches!(&error, segment::FrameDecodeError::CrcMismatch { .. });
                    if state_ref.in_batch {
                        if is_crc_mismatch {
                            tracing::warn!(
                                segment_id,
                                staged_count = state_ref.staged.len(),
                                "discarding incomplete batch due to CRC mismatch"
                            );
                        } else {
                            tracing::warn!(
                                segment_id,
                                staged_count = state_ref.staged.len(),
                                "discarding incomplete batch due to decode error"
                            );
                        }
                        state_ref.staged.clear();
                        state_ref.in_batch = false;
                    }
                    self.release_buffer(frame_buf);
                    return Err(Self::frame_decode_error(segment_id, frame_offset, error));
                }
            }
            self.release_buffer(frame_buf);
        }

        Ok(())
    }
}

#[derive(Deserialize)]
pub(crate) struct IndexScanFramePayload {
    pub(crate) event: IndexScanEvent,
    pub(crate) entity: String,
    pub(crate) scope: String,
    #[serde(default)]
    pub(crate) receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

#[derive(Deserialize)]
pub(crate) struct IndexScanEvent {
    pub(crate) header: EventHeader,
    #[serde(rename = "payload")]
    pub(crate) _payload: serde::de::IgnoredAny,
    pub(crate) hash_chain: Option<HashChain>,
}

#[cfg(test)]
mod tests;
