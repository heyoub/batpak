use super::{Reader, ScannedIndexEntry};
use crate::event::{EventHeader, EventKind, HashChain};
use crate::store::segment::{self, SegmentHeader, SEGMENT_MAGIC};
use crate::store::StoreError;
use serde::Deserialize;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::sync::atomic::Ordering;

impl Reader {
    /// Check whether the SIDX entries cover every frame in the segment up to
    /// the SIDX footer.
    ///
    /// Returns `Some(true)` when the max (frame_offset + frame_length)
    /// across SIDX entries equals the SIDX footer start — meaning every
    /// frame in the segment is represented. Returns `Some(false)` when
    /// there are trailing frames that SIDX doesn't know about (the
    /// cross-segment batch case — see `scan_segment_index_into`'s
    /// contract). Returns `None` on I/O trouble; callers interpret as
    /// "can't prove coverage, frame-scan to be safe".
    fn sidx_covers_segment_tail(
        path: &Path,
        sidx_entries: &[crate::store::segment::sidx::SidxEntry],
    ) -> Option<bool> {
        // file_len - TRAILER_SIZE - entries_block - string_table = SIDX start,
        // which is also the `string_table_offset` written in the trailer.
        // We want to compare the tail of the last SIDX entry to that start.
        let file_len = std::fs::metadata(path).ok()?.len();
        // Trailer is 16 bytes: string_table_offset(8) + entry_count(4) + magic(4).
        // Read only the trailer to get string_table_offset without reparsing
        // the entire footer.
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path).ok()?;
        if file_len < 16 {
            return Some(false);
        }
        file.seek(SeekFrom::End(-16)).ok()?;
        let mut trailer = [0u8; 16];
        file.read_exact(&mut trailer).ok()?;
        // If this isn't a SIDX footer the caller shouldn't have reached
        // this path — but guard anyway.
        if &trailer[12..16] != crate::store::segment::sidx::SIDX_MAGIC {
            return None;
        }
        let offset_bytes: [u8; 8] = trailer[0..8].try_into().ok()?;
        let sidx_start = u64::from_le_bytes(offset_bytes);

        // Max tail across entries. Batch markers are written as frames but
        // are NOT recorded into the SIDX collector, so a segment with a
        // BEGIN at its tail will have sidx_max_tail < sidx_start. Items
        // written between BEGIN and rotation are also not in SIDX (they
        // land in the collector only at COMMIT time, and the segment
        // rotated before COMMIT), so they push sidx_max_tail further
        // below sidx_start. Either case fails this check and forces the
        // frame-scan path.
        let max_tail = sidx_entries
            .iter()
            .map(|e| e.frame_offset.saturating_add(u64::from(e.frame_length)))
            .max()
            .unwrap_or(0);

        // Segments with an empty SIDX but frames present are an unusual
        // case; force frame-scan there too by reporting "not covered".
        if sidx_entries.is_empty() && sidx_start > (4 + 4) {
            return Some(false);
        }

        Some(max_tail >= sidx_start)
    }

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
    pub(crate) fn scan_segment_index_into<F>(
        &self,
        path: &Path,
        mut batch_state: Option<&mut BatchRecoveryState>,
        mut sink: F,
    ) -> Result<(), StoreError>
    where
        F: FnMut(ScannedIndexEntry) -> Result<(), StoreError>,
    {
        // Fast path: try SIDX footer for sealed segments only.
        // Sealed segments cannot have incomplete batches, so SIDX is safe.
        // Active segment might have incomplete batches, so use slow path.
        let segment_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let is_active = self.active_segment_id.load(Ordering::Acquire) == segment_id;

        if !is_active && batch_state.as_ref().is_none_or(|s| !s.in_batch) {
            if let Ok(Some((sidx_entries, strings))) =
                crate::store::segment::sidx::read_footer(path)
            {
                let sidx_covers_tail =
                    Self::sidx_covers_segment_tail(path, &sidx_entries).unwrap_or(false);
                if sidx_covers_tail {
                    for se in sidx_entries {
                        let row = se.to_cold_start_row(segment_id);
                        let kind = row.kind;
                        if kind == EventKind::SYSTEM_BATCH_BEGIN
                            || kind == EventKind::SYSTEM_BATCH_COMMIT
                        {
                            continue;
                        }
                        sink(ScannedIndexEntry::from_cold_start_row(&row, &strings)?)?;
                    }
                    return Ok(());
                }
            }
        }

        let mut file = File::open(path).map_err(StoreError::Io)?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(StoreError::Io)?;
        if &magic != SEGMENT_MAGIC {
            return Err(StoreError::corrupt_magic(0));
        }

        let segment_id = match path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
        {
            Some(id) => id,
            None => {
                tracing::warn!(?path, "skipping segment with unparseable filename");
                return Ok(());
            }
        };

        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .map_err(StoreError::Io)?;
        let header_len = u32::from_be_bytes(header_len_buf) as usize;
        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf).map_err(StoreError::Io)?;
        let header: SegmentHeader = rmp_serde::from_slice(&header_buf)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;
        if header.version != 1 {
            return Err(StoreError::corrupt_version(segment_id, header.version));
        }

        let mut cursor = (8 + header_len) as u64;
        let mut local_state = BatchRecoveryState::default();
        let state_ref: &mut BatchRecoveryState = match batch_state {
            Some(ref mut s) => s,
            None => &mut local_state,
        };

        loop {
            let frame_offset = cursor;
            let mut frame_header = [0u8; 8];
            match file.read_exact(&mut frame_header) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                    if state_ref.in_batch {
                        tracing::warn!(
                            segment_id,
                            staged_count = state_ref.staged.len(),
                            "incomplete batch at EOF, will discard or continue in next segment"
                        );
                    }
                    break;
                }
                Err(error) => return Err(StoreError::Io(error)),
            }

            let payload_len = u32::from_be_bytes([
                frame_header[0],
                frame_header[1],
                frame_header[2],
                frame_header[3],
            ]) as usize;
            if payload_len > segment::MAX_FRAME_PAYLOAD {
                tracing::warn!(
                    segment_id,
                    payload_len,
                    "frame payload exceeds MAX_FRAME_PAYLOAD, stopping segment scan"
                );
                break;
            }
            let mut frame_buf = self.acquire_buffer(8 + payload_len);
            frame_buf[..8].copy_from_slice(&frame_header);
            if let Err(error) = file.read_exact(&mut frame_buf[8..]) {
                self.release_buffer(frame_buf);
                if error.kind() == ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(StoreError::Io(error));
            }

            let mut stop_scan = false;
            match segment::frame_decode(&frame_buf) {
                Ok((msgpack, frame_size)) => {
                    match rmp_serde::from_slice::<IndexScanFramePayload>(msgpack) {
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
                                state_ref.staged.push(ScannedIndexEntry {
                                    header: payload.event.header,
                                    entity: payload.entity,
                                    scope: payload.scope,
                                    hash_chain,
                                    segment_id,
                                    offset: frame_offset,
                                    length,
                                    global_sequence: None,
                                });
                                if state_ref.remaining > 0 {
                                    state_ref.remaining -= 1;
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                segment_id,
                                offset = frame_offset,
                                "skipping unreadable frame metadata: {error}"
                            );
                            if state_ref.in_batch {
                                tracing::warn!(
                                    segment_id,
                                    staged_count = state_ref.staged.len(),
                                    "discarding incomplete batch due to corruption"
                                );
                                state_ref.staged.clear();
                                state_ref.in_batch = false;
                            }
                        }
                    }
                    cursor += frame_size as u64;
                }
                Err(segment::FrameDecodeError::CrcMismatch { .. }) => {
                    tracing::warn!(
                        segment_id,
                        offset = frame_offset,
                        "CRC mismatch, skipping frame"
                    );
                    if state_ref.in_batch {
                        tracing::warn!(
                            segment_id,
                            staged_count = state_ref.staged.len(),
                            "discarding incomplete batch due to CRC mismatch"
                        );
                        state_ref.staged.clear();
                        state_ref.in_batch = false;
                    }
                    stop_scan = true;
                }
                Err(_) => {
                    if state_ref.in_batch {
                        tracing::warn!(
                            segment_id,
                            staged_count = state_ref.staged.len(),
                            "discarding incomplete batch due to decode error"
                        );
                        state_ref.staged.clear();
                        state_ref.in_batch = false;
                    }
                    stop_scan = true;
                }
            }
            self.release_buffer(frame_buf);
            if stop_scan {
                break;
            }
        }

        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct BatchRecoveryState {
    pub staged: Vec<ScannedIndexEntry>,
    pub remaining: u32,
    pub started_count: u32,
    pub in_batch: bool,
}

#[derive(Deserialize)]
pub(crate) struct IndexScanFramePayload {
    pub(crate) event: IndexScanEvent,
    pub(crate) entity: String,
    pub(crate) scope: String,
}

#[derive(Deserialize)]
pub(crate) struct IndexScanEvent {
    pub(crate) header: EventHeader,
    #[serde(rename = "payload")]
    pub(crate) _payload: serde::de::IgnoredAny,
    pub(crate) hash_chain: Option<HashChain>,
}
