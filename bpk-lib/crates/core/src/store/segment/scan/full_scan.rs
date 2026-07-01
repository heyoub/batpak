use super::{read_frame_header_or_clean_eof, Reader, ScannedEntry};
use crate::event::EventKind;
use crate::store::segment::{self, SEGMENT_MAGIC};
use crate::store::StoreError;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

impl Reader {
    /// Scan an entire segment for cold start. Returns all events in order.
    ///
    /// **SIDX fast-path contract.** This function does not use the SIDX
    /// fast-path at all — it always frame-scans. The mirror contract in
    /// `scan_segment_index_into` is the one that must be careful about
    /// cross-segment batches; here we return every frame so callers that
    /// need the full event stream always get it.
    pub(crate) fn scan_segment(&self, path: &Path) -> Result<Vec<ScannedEntry>, StoreError> {
        // Extract segment_id from filename: "000042.fbat" → 42.
        // Falls back to 0 if the filename is malformed, but surfaces the parse
        // failure via tracing so a corrupt name on disk is not invisible.
        // Hoisted above detect_sidx_boundary so the boundary check can stamp a
        // CorruptSegment error with this segment_id.
        let segment_id = match segment::SegmentId::from_filename(path) {
            Ok(parsed) => parsed.as_u64(),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping malformed segment filename"
                );
                0
            }
        };

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
        let header: segment::SegmentHeader = crate::encoding::from_bytes(&header_buf)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;

        // Version check — reject unknown segment versions
        if header.version != 1 {
            return Err(StoreError::corrupt_version(segment_id, header.version));
        }

        let mut cursor = u64::try_from(8usize.checked_add(header_len).ok_or_else(|| {
            StoreError::corrupt_segment_with_detail(segment_id, "segment header offset overflow")
        })?)
        .map_err(|_| {
            StoreError::corrupt_segment_with_detail(segment_id, "segment header offset overflow")
        })?; // past magic + header_len + header

        // Lower-bound check (TRUSTED boundaries only): an authenticated SDX3
        // `string_table_offset` must not fall below the start of the frame region.
        // A corrupt-but-authenticated offset < cursor would make the scan loop
        // break immediately and return zero events — silent data loss. Error with
        // CorruptSegment instead. frames_end == cursor (empty frame region) stays
        // valid. For an UNTRUSTED boundary the offset is garbage and discarded
        // below (recovery walks from `cursor` bounded by `file_len`), so a too-low
        // untrusted hint must NOT error — it recovers all CRC-valid frames instead.
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
        // corruption), AND corroborates the CRC-independent SIDX entry table
        // against the recovered frames. A corroborated entry attesting to a missing
        // committed frame at/after the recovered prefix end FailCloses (round-7
        // torn-last-frame-under-corrupt-footer). `scan_segment` is the full-event
        // compaction read of SEALED segments, so its fall-back posture is strict
        // (`fallback_fail_closed = true`); without a corroborated manifest that
        // still degrades to recovering the CRC-valid prefix (no false fail-closed).
        let frames_end = if untrusted_boundary {
            segment::resolve_untrusted_frames_end(&mut file, cursor, file_len, segment_id, true)?
        } else {
            frames_end
        };
        file.seek(SeekFrom::Start(cursor)).map_err(StoreError::Io)?;

        // Read frames until EOF. Each frame: [len:u32 BE][crc32:u32 BE][msgpack]
        let mut entries = Vec::new();
        loop {
            if cursor >= frames_end {
                break;
            }
            let frame_offset = cursor;
            let Some(frame_header) =
                read_frame_header_or_clean_eof(&mut file).map_err(StoreError::Io)?
            else {
                break;
            };

            let payload_len = u32::from_be_bytes([
                frame_header[0],
                frame_header[1],
                frame_header[2],
                frame_header[3],
            ]) as usize;
            if Self::payload_len_exceeds_max(payload_len) {
                return Err(StoreError::corrupt_segment_with_detail(
                    segment_id,
                    format!("frame payload length {payload_len} exceeds MAX_FRAME_PAYLOAD"),
                ));
            }
            let frame_tail = frame_offset
                .checked_add(8)
                .and_then(|base| base.checked_add(u64::try_from(payload_len).ok()?))
                .ok_or_else(|| {
                    StoreError::corrupt_segment_with_detail(segment_id, "frame tail overflow")
                })?;
            if frame_tail > frames_end {
                return Err(StoreError::corrupt_segment_with_detail(
                    segment_id,
                    "frame payload extends past the frame region",
                ));
            }
            let mut frame_buf = self.acquire_buffer(8 + payload_len);
            frame_buf[..8].copy_from_slice(&frame_header);
            if let Err(error) = file.read_exact(&mut frame_buf[8..]) {
                self.release_buffer(frame_buf);
                if error.kind() == ErrorKind::UnexpectedEof {
                    return Err(StoreError::corrupt_segment_with_detail(
                        segment_id,
                        "frame payload ended before requested length",
                    ));
                }
                return Err(StoreError::Io(error));
            }

            match segment::frame_decode(&frame_buf) {
                Ok((msgpack, frame_size)) => {
                    match Self::scanned_entry_from_frame(msgpack, segment_id, frame_offset)? {
                        Some(entry) => entries.push(entry),
                        None => {
                            // In-band batch marker (BEGIN/COMMIT): skip it, leaving
                            // the buffer to drop rather than recycle (mirrors prior).
                            cursor += frame_size as u64;
                            continue;
                        }
                    }
                    cursor += frame_size as u64;
                }
                Err(error) => {
                    self.release_buffer(frame_buf);
                    return Err(Self::frame_decode_error(segment_id, frame_offset, error));
                }
            }
            self.release_buffer(frame_buf);
        }
        Ok(entries)
    }

    /// Decode one already-CRC-validated frame payload into a [`ScannedEntry`],
    /// or `Ok(None)` for an in-band batch marker (`SYSTEM_BATCH_BEGIN` /
    /// `SYSTEM_BATCH_COMMIT`) that the full event scan skips.
    ///
    /// Carries the survivor's ORIGINAL `event.payload` bytes onto the entry so
    /// Retention/Tombstone compaction can re-emit them verbatim (byte-stable
    /// frame + `event_hash`) instead of re-serializing the decoded `Value`.
    ///
    /// Uses the compaction-tolerant decode: an ENCRYPTED payload is not
    /// Value-decoded here (the reader has no key), so it arrives with a `Null`
    /// placeholder in `event.payload` and its raw CIPHERTEXT in `payload_bytes`.
    /// The compaction seam decrypts `payload_bytes` under the keyset for the
    /// predicate's view and re-emits the ciphertext verbatim.
    fn scanned_entry_from_frame(
        msgpack: &[u8],
        segment_id: u64,
        frame_offset: u64,
    ) -> Result<Option<ScannedEntry>, StoreError> {
        let (payload, payload_bytes) = Self::decode_frame_payload_value_for_compaction(msgpack)
            .map_err(|error| StoreError::CorruptSegment {
                segment_id,
                detail: format!("frame at offset {frame_offset} has unreadable payload: {error}"),
            })?;
        if matches!(
            payload.event.header.event_kind,
            EventKind::SYSTEM_BATCH_BEGIN | EventKind::SYSTEM_BATCH_COMMIT
        ) {
            return Ok(None);
        }
        Ok(Some(ScannedEntry {
            event: payload.event,
            entity: payload.entity,
            scope: payload.scope,
            receipt_extensions: payload.receipt_extensions,
            payload_bytes,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn scan_segment_treats_eof_after_header_as_clean_empty_segment() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let segment_id = 7;
        let path = dir.path().join(segment::segment_filename(segment_id));
        let header = segment::SegmentHeader {
            version: 1,
            flags: 0,
            created_ns: 123,
            segment_id,
        };
        let header_bytes = crate::encoding::to_bytes(&header).expect("encode segment header");
        let header_len = u32::try_from(header_bytes.len()).expect("segment header length fits u32");

        let mut file = File::create(&path).expect("create segment");
        file.write_all(SEGMENT_MAGIC).expect("write segment magic");
        file.write_all(&header_len.to_be_bytes())
            .expect("write segment header length");
        file.write_all(&header_bytes).expect("write segment header");
        file.flush().expect("flush segment");

        let clock: std::sync::Arc<dyn crate::store::Clock> =
            std::sync::Arc::new(crate::store::SystemClock::new());
        let reader = Reader::new(
            dir.path().to_path_buf(),
            4,
            &clock,
            std::sync::Arc::new(crate::store::platform::fs::RealFs),
        );
        let entries = reader
            .scan_segment(&path)
            .expect("EOF after the segment header is the clean frame terminator");

        assert!(
            entries.is_empty(),
            "PROPERTY: a valid segment with no frames scans as empty, not corrupt"
        );
    }
}
