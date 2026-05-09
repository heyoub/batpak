use super::{Reader, ScannedEntry};
use crate::event::EventKind;
use crate::store::segment::{self, SEGMENT_MAGIC};
use crate::store::StoreError;
use std::fs::File;
use std::io::{Error, ErrorKind, Read};
use std::path::Path;

fn frame_header_error_ends_scan(error: &Error) -> bool {
    error.kind() == ErrorKind::UnexpectedEof
}

impl Reader {
    /// Scan an entire segment for cold start. Returns all events in order.
    ///
    /// **SIDX fast-path contract.** This function does not use the SIDX
    /// fast-path at all — it always frame-scans. The mirror contract in
    /// `scan_segment_index_into` is the one that must be careful about
    /// cross-segment batches; here we return every frame so callers that
    /// need the full event stream always get it.
    pub(crate) fn scan_segment(&self, path: &Path) -> Result<Vec<ScannedEntry>, StoreError> {
        let mut file = File::open(path).map_err(StoreError::Io)?;
        let file_len = file.metadata().map_err(StoreError::Io)?.len();
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(StoreError::Io)?;
        if &magic != SEGMENT_MAGIC {
            return Err(StoreError::corrupt_magic(0));
        }

        // Extract segment_id from filename: "000042.fbat" → 42
        let segment_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .map_err(StoreError::Io)?;
        let header_len = u32::from_be_bytes(header_len_buf) as usize;
        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf).map_err(StoreError::Io)?;
        let header: segment::SegmentHeader = rmp_serde::from_slice(&header_buf)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;

        // Version check — reject unknown segment versions
        if header.version != 1 {
            return Err(StoreError::corrupt_version(segment_id, header.version));
        }

        let mut cursor = u64::try_from(8usize.checked_add(header_len).ok_or_else(|| {
            StoreError::corrupt_frame(segment_id, "segment header offset overflow")
        })?)
        .map_err(|_| StoreError::corrupt_frame(segment_id, "segment header offset overflow"))?; // past magic + header_len + header

        // Read frames until EOF. Each frame: [len:u32 BE][crc32:u32 BE][msgpack]
        let mut entries = Vec::new();
        loop {
            let frame_offset = cursor;
            let mut frame_header = [0u8; 8];
            match file.read_exact(&mut frame_header) {
                Ok(()) => {}
                Err(error) if frame_header_error_ends_scan(&error) => break,
                Err(error) => return Err(StoreError::Io(error)),
            }

            let payload_len = u32::from_be_bytes([
                frame_header[0],
                frame_header[1],
                frame_header[2],
                frame_header[3],
            ]) as usize;
            if Self::payload_len_exceeds_max(payload_len) {
                tracing::warn!(
                    segment_id,
                    payload_len,
                    "frame payload exceeds MAX_FRAME_PAYLOAD, stopping segment scan as torn tail"
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
                Err(segment::FrameDecodeError::CrcMismatch { .. }) => {
                    return Err(StoreError::CrcMismatch {
                        segment_id,
                        offset: frame_offset,
                    });
                }
                Err(error) => {
                    if frame_offset + u64::try_from(frame_buf.len()).unwrap_or(u64::MAX) >= file_len
                    {
                        stop_scan = true;
                    } else {
                        return Err(StoreError::CorruptSegment {
                            segment_id,
                            detail: format!("frame at offset {frame_offset} is corrupt: {error}"),
                        });
                    }
                }
            }
            self.release_buffer(frame_buf);
            if stop_scan {
                break;
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_error_policy_only_treats_eof_as_clean_end() {
        assert!(
            frame_header_error_ends_scan(&Error::from(ErrorKind::UnexpectedEof)),
            "PROPERTY: EOF while reading the next frame header is the clean segment terminator"
        );
        assert!(
            !frame_header_error_ends_scan(&Error::from(ErrorKind::InvalidData)),
            "PROPERTY: non-EOF frame-header read errors must surface as StoreError::Io, not end the scan"
        );
    }
}
