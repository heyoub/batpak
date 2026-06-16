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
        let frames_end =
            segment::detect_sidx_boundary(&mut file, file_len, segment_id)?.unwrap_or(file_len);
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

        // Lower-bound check: frames_end (the SIDX string_table_offset) must not
        // fall below the start of the frame region. A corrupt offset < cursor
        // would make the scan loop break immediately and return zero events —
        // silent data loss. Error with CorruptSegment instead. frames_end ==
        // cursor (empty frame region) stays valid.
        if frames_end < cursor {
            return Err(StoreError::corrupt_segment_with_detail(
                segment_id,
                format!(
                    "SIDX string_table_offset {frames_end} is below the frame region start \
                     {cursor} (8 + header_len {header_len})"
                ),
            ));
        }

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
                    match Self::decode_frame_payload_value(msgpack) {
                        Ok(payload) => {
                            if matches!(
                                payload.event.header.event_kind,
                                EventKind::SYSTEM_BATCH_BEGIN | EventKind::SYSTEM_BATCH_COMMIT
                            ) {
                                cursor += frame_size as u64;
                                continue;
                            }
                            entries.push(ScannedEntry {
                                event: payload.event,
                                entity: payload.entity,
                                scope: payload.scope,
                                receipt_extensions: payload.receipt_extensions,
                            });
                        }
                        Err(error) => {
                            return Err(StoreError::CorruptSegment {
                                segment_id,
                                detail: format!(
                                    "frame at offset {frame_offset} has unreadable payload: {error}"
                                ),
                            });
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
        let reader = Reader::new(dir.path().to_path_buf(), 4, &clock);
        let entries = reader
            .scan_segment(&path)
            .expect("EOF after the segment header is the clean frame terminator");

        assert!(
            entries.is_empty(),
            "PROPERTY: a valid segment with no frames scans as empty, not corrupt"
        );
    }
}
