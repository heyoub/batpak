use super::{read_frame_header_or_clean_eof, FrameScanTailPolicy, Reader, ScannedIndexEntry};
use crate::event::{EventHeader, EventKind, HashChain};
use crate::store::segment::{self, SegmentHeader, SEGMENT_MAGIC};
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
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
    /// contract), or when the SIDX claims to cover bytes that overlap
    /// the footer itself (structural corruption). Returns `None` on I/O
    /// trouble; callers interpret as "can't prove coverage, frame-scan
    /// to be safe".
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

        Some(max_tail == sidx_start)
    }

    fn classify_payload_read_error(
        segment_id: u64,
        error: Error,
        tail_policy: FrameScanTailPolicy,
    ) -> Result<PayloadReadFailure, StoreError> {
        if error.kind() == ErrorKind::UnexpectedEof {
            if tail_policy.can_recover_torn_tail() {
                Ok(PayloadReadFailure::RecoverTornTail)
            } else {
                Err(StoreError::corrupt_segment_with_detail(
                    segment_id,
                    "frame payload ended before requested length",
                ))
            }
        } else {
            Err(StoreError::Io(error))
        }
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
        let file_len = file.seek(SeekFrom::End(0)).map_err(StoreError::Io)?;
        let frames_end = segment::detect_sidx_boundary(&mut file, file_len)?.unwrap_or(file_len);
        file.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;

        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(StoreError::Io)?;
        if &magic != SEGMENT_MAGIC {
            return Err(StoreError::corrupt_magic(segment_id));
        }

        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .map_err(StoreError::Io)?;
        let header_len = u32::from_be_bytes(header_len_buf) as usize;
        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf).map_err(StoreError::Io)?;
        let header: SegmentHeader = crate::encoding::from_bytes(&header_buf)
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
                match Self::classify_payload_read_error(segment_id, error, tail_policy)? {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PayloadReadFailure {
    RecoverTornTail,
}

#[derive(Default)]
pub(crate) struct BatchRecoveryState {
    pub staged: Vec<ScannedIndexEntry>,
    pub remaining: u32,
    pub started_count: u32,
    pub in_batch: bool,
}

impl BatchRecoveryState {
    fn stage_entry(&mut self, entry: ScannedIndexEntry) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.staged.push(entry);
        self.remaining -= 1;
        true
    }

    fn discard_incomplete(&mut self) {
        self.in_batch = false;
        self.remaining = 0;
        self.started_count = 0;
        self.staged.clear();
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
mod tests {
    use super::*;
    use crate::coordinate::DagPosition;
    use crate::event::EventKind;
    use crate::store::segment::sidx::{kind_to_raw, read_footer, SidxEntry, SidxEntryCollector};
    use std::io::{Cursor, Seek, SeekFrom, Write};
    use tempfile::{NamedTempFile, TempDir};

    fn sample_entry(frame_offset: u64, frame_length: u32) -> SidxEntry {
        SidxEntry {
            event_id: 1,
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
            global_sequence: 1,
            correlation_id: 1,
            causation_id: 0,
        }
    }

    fn footer_bytes(prefix_len: usize, entries: &[SidxEntry]) -> Vec<u8> {
        let mut bytes = vec![0xA5; prefix_len];
        let mut cursor = Cursor::new(&mut bytes);
        cursor.seek(SeekFrom::End(0)).expect("seek to end");

        let mut collector = SidxEntryCollector::new();
        for (idx, entry) in entries.iter().cloned().enumerate() {
            let entity = format!("entity:{idx}");
            collector.record(entry, &entity, "scope:test");
        }
        collector
            .write_footer(&mut cursor, 7)
            .expect("write footer");

        bytes
    }

    fn footer_file(prefix_len: usize, entries: &[SidxEntry]) -> NamedTempFile {
        let bytes = footer_bytes(prefix_len, entries);
        let mut tmp = NamedTempFile::new().expect("create temp file");
        tmp.write_all(&bytes).expect("write temp bytes");
        tmp.flush().expect("flush temp file");
        tmp
    }

    fn footer_segment_path(
        dir: &TempDir,
        segment_id: u64,
        prefix_len: usize,
        entries: &[SidxEntry],
    ) -> std::path::PathBuf {
        let path = dir
            .path()
            .join(crate::store::segment::segment_filename(segment_id));
        std::fs::write(&path, footer_bytes(prefix_len, entries))
            .expect("write segment footer file");
        path
    }

    fn scanned_entry(event_id: u128) -> ScannedIndexEntry {
        ScannedIndexEntry {
            header: EventHeader::new(
                event_id,
                event_id,
                None,
                0,
                DagPosition::root(),
                0,
                EventKind::custom(0x1, 1),
            ),
            entity: "entity:test".to_owned(),
            scope: "scope:test".to_owned(),
            hash_chain: HashChain::default(),
            segment_id: 7,
            offset: u64::try_from(event_id).expect("test ids fit u64"),
            length: 16,
            receipt_extensions: std::collections::BTreeMap::new(),
            global_sequence: None,
        }
    }

    #[test]
    fn batch_recovery_state_refuses_items_past_declared_count() {
        let mut state = BatchRecoveryState {
            remaining: 1,
            started_count: 1,
            in_batch: true,
            staged: Vec::new(),
        };

        assert!(
            state.stage_entry(scanned_entry(1)),
            "PROPERTY: the first item in a one-item recovered batch is accepted"
        );
        assert_eq!(
            state.remaining, 0,
            "PROPERTY: staging a recovered batch item decrements the remaining item budget"
        );
        assert!(
            !state.stage_entry(scanned_entry(2)),
            "PROPERTY: recovery must reject items beyond the BEGIN marker's declared count"
        );
        assert_eq!(
            state.staged.len(),
            1,
            "PROPERTY: rejected over-count items must not be silently staged"
        );

        state.discard_incomplete();
        assert!(
            !state.in_batch && state.staged.is_empty() && state.remaining == 0,
            "PROPERTY: corrupt over-count batches can be discarded without leaving pending state"
        );
    }

    #[test]
    fn sidx_covers_segment_tail_requires_last_entry_to_reach_footer_start() {
        let tmp = footer_file(64, &[sample_entry(0, 64)]);
        let (entries, _) = read_footer(tmp.path())
            .expect("read footer")
            .expect("footer should be present");

        assert_eq!(
            Reader::sidx_covers_segment_tail(tmp.path(), &entries),
            Some(true),
            "PROPERTY: SIDX coverage is complete only when the last indexed frame tail reaches the footer start"
        );
    }

    #[test]
    fn sidx_covers_segment_tail_rejects_trailing_unindexed_bytes() {
        let tmp = footer_file(80, &[sample_entry(0, 64)]);
        let (entries, _) = read_footer(tmp.path())
            .expect("read footer")
            .expect("footer should be present");

        assert_eq!(
            Reader::sidx_covers_segment_tail(tmp.path(), &entries),
            Some(false),
            "PROPERTY: trailing bytes between the last indexed frame and the footer force frame-scan fallback"
        );
    }

    #[test]
    fn sidx_covers_segment_tail_treats_truly_empty_segment_as_covered() {
        let tmp = footer_file(0, &[]);
        let (entries, _) = read_footer(tmp.path())
            .expect("read footer")
            .expect("footer should be present");

        assert!(
            entries.is_empty(),
            "SANITY: empty-footer fixture should not produce SIDX entries"
        );
        assert_eq!(
            Reader::sidx_covers_segment_tail(tmp.path(), &entries),
            Some(true),
            "PROPERTY: an empty segment with an empty SIDX footer is fully covered and must not be forced onto the frame-scan fallback"
        );
    }

    #[test]
    fn sidx_covers_segment_tail_rejects_empty_footer_after_frames() {
        let tmp = footer_file(64, &[]);
        let (entries, _) = read_footer(tmp.path())
            .expect("read footer")
            .expect("footer should be present");

        assert!(
            entries.is_empty(),
            "SANITY: fixture should have frames before an empty SIDX footer"
        );
        assert_eq!(
            Reader::sidx_covers_segment_tail(tmp.path(), &entries),
            Some(false),
            "PROPERTY: an empty SIDX footer does not cover preceding frame bytes and must force frame-scan fallback"
        );
    }

    #[test]
    fn payload_read_unexpected_eof_respects_tail_policy() {
        assert_eq!(
            Reader::classify_payload_read_error(
                7,
                std::io::Error::from(ErrorKind::UnexpectedEof),
                FrameScanTailPolicy::RecoverTornTail,
            )
            .expect("latest tail EOF should be recoverable"),
            PayloadReadFailure::RecoverTornTail,
            "PROPERTY: only the latest tail policy may turn payload EOF into torn-tail recovery"
        );

        let err = Reader::classify_payload_read_error(
            7,
            std::io::Error::from(ErrorKind::UnexpectedEof),
            FrameScanTailPolicy::FailClosed,
        )
        .expect_err("non-tail payload EOF must fail closed");
        assert!(
            matches!(
                err,
                StoreError::CorruptSegment { ref detail, .. }
                if detail.contains("frame payload ended before requested length")
            ),
            "PROPERTY: non-tail payload EOF must surface as committed-frame corruption, got {err:?}"
        );
    }

    #[test]
    fn payload_read_non_eof_error_is_never_torn_tail_recovery() {
        let err = Reader::classify_payload_read_error(
            7,
            std::io::Error::from(ErrorKind::PermissionDenied),
            FrameScanTailPolicy::RecoverTornTail,
        )
        .expect_err("non-EOF read errors must remain I/O errors");
        assert!(
            matches!(err, StoreError::Io(_)),
            "PROPERTY: torn-tail recovery applies only to UnexpectedEof, got {err:?}"
        );
    }

    #[test]
    fn scan_segment_index_into_uses_sidx_fast_path_for_sealed_segments() {
        let dir = TempDir::new().expect("create temp dir");
        let reader = Reader::new(
            dir.path().to_path_buf(),
            4,
            std::sync::Arc::new(crate::store::SystemClock::new()),
        );
        let segment_id = 7;
        let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
        reader.set_active_segment(segment_id + 1);

        let mut rows = Vec::new();
        reader
            .scan_segment_index_into_with_tail_policy(
                &path,
                None,
                FrameScanTailPolicy::FailClosed,
                |row| {
                    rows.push(row);
                    Ok(())
                },
            )
            .expect("sealed scan should succeed");

        assert_eq!(
            rows.len(),
            1,
            "PROPERTY: sealed segments with full SIDX tail coverage should use the footer fast path and emit indexed rows"
        );
    }

    #[test]
    fn scan_segment_index_into_uses_sidx_fast_path_when_batch_state_is_idle() {
        let dir = TempDir::new().expect("create temp dir");
        let reader = Reader::new(
            dir.path().to_path_buf(),
            4,
            std::sync::Arc::new(crate::store::SystemClock::new()),
        );
        let segment_id = 7;
        let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
        reader.set_active_segment(segment_id + 1);
        let mut batch_state = BatchRecoveryState::default();

        let mut rows = Vec::new();
        reader
            .scan_segment_index_into_with_tail_policy(
                &path,
                Some(&mut batch_state),
                FrameScanTailPolicy::FailClosed,
                |row| {
                    rows.push(row);
                    Ok(())
                },
            )
            .expect("idle batch state should not disable the SIDX fast path");

        assert_eq!(
            rows.len(),
            1,
            "PROPERTY: an idle cross-segment batch state is equivalent to no batch state for SIDX fast-path admission"
        );
    }

    #[test]
    fn scan_segment_index_into_rejects_sidx_fast_path_when_batch_is_pending() {
        let dir = TempDir::new().expect("create temp dir");
        let reader = Reader::new(
            dir.path().to_path_buf(),
            4,
            std::sync::Arc::new(crate::store::SystemClock::new()),
        );
        let segment_id = 7;
        let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
        reader.set_active_segment(segment_id + 1);
        let mut batch_state = BatchRecoveryState {
            in_batch: true,
            remaining: 1,
            started_count: 1,
            staged: Vec::new(),
        };

        let mut rows = Vec::new();
        let err = reader
            .scan_segment_index_into_with_tail_policy(
                &path,
                Some(&mut batch_state),
                FrameScanTailPolicy::FailClosed,
                |row| {
                    rows.push(row);
                    Ok(())
                },
            )
            .expect_err("pending batch state must force frame scan over synthetic footer bytes");

        assert!(
            matches!(err, StoreError::CorruptSegment { .. }),
            "PROPERTY: a pending cross-segment batch must not trust a SIDX footer until the batch is resolved; got {err:?}"
        );
        assert!(
            rows.is_empty(),
            "PROPERTY: rejecting the SIDX fast path while a batch is pending must not emit footer rows"
        );
    }

    #[test]
    fn scan_segment_index_into_filters_batch_markers_from_sidx_fast_path() {
        let dir = TempDir::new().expect("create temp dir");
        let reader = Reader::new(
            dir.path().to_path_buf(),
            4,
            std::sync::Arc::new(crate::store::SystemClock::new()),
        );
        let segment_id = 7;
        let mut begin = sample_entry(0, 64);
        begin.event_id = 10;
        begin.kind = kind_to_raw(EventKind::SYSTEM_BATCH_BEGIN);
        begin.global_sequence = 10;
        let mut item = sample_entry(64, 64);
        item.event_id = 11;
        item.kind = kind_to_raw(EventKind::custom(0x1, 2));
        item.global_sequence = 11;
        let mut commit = sample_entry(128, 64);
        commit.event_id = 12;
        commit.kind = kind_to_raw(EventKind::SYSTEM_BATCH_COMMIT);
        commit.global_sequence = 12;
        let path = footer_segment_path(&dir, segment_id, 192, &[begin, item, commit]);
        reader.set_active_segment(segment_id + 1);

        let mut rows = Vec::new();
        reader
            .scan_segment_index_into_with_tail_policy(
                &path,
                None,
                FrameScanTailPolicy::FailClosed,
                |row| {
                    rows.push(row);
                    Ok(())
                },
            )
            .expect("sealed scan should succeed");

        assert_eq!(
            rows.len(),
            1,
            "PROPERTY: SIDX fast-path recovery must filter BEGIN/COMMIT markers and emit only logical user rows"
        );
        assert_eq!(
            rows[0].header.event_id,
            crate::id::EventId::from(11u128),
            "PROPERTY: filtering batch markers must preserve the real batch item"
        );
        assert_eq!(
            rows[0].header.event_kind,
            EventKind::custom(0x1, 2),
            "PROPERTY: filtering batch markers must not rewrite the surviving item kind"
        );
    }

    #[test]
    fn scan_segment_index_into_ignores_sidx_footer_for_active_segments() {
        let dir = TempDir::new().expect("create temp dir");
        let reader = Reader::new(
            dir.path().to_path_buf(),
            4,
            std::sync::Arc::new(crate::store::SystemClock::new()),
        );
        let segment_id = 7;
        let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
        reader.set_active_segment(segment_id);

        let mut rows = Vec::new();
        let err = reader
            .scan_segment_index_into_with_tail_policy(
                &path,
                None,
                FrameScanTailPolicy::FailClosed,
                |row| {
                    rows.push(row);
                    Ok(())
                },
            )
            .expect_err("active segment must not trust the synthetic SIDX footer fixture");

        assert!(
            matches!(err, StoreError::CorruptSegment { .. }),
            "PROPERTY: the active segment must refuse the SIDX fast path and fall back to frame scan even if a footer is present"
        );
        assert!(
            rows.is_empty(),
            "PROPERTY: falling back to frame scan must not synthesize rows from the active segment's footer bytes"
        );
    }
}
