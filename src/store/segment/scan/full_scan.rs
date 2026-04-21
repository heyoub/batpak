use super::{Reader, ScannedEntry, SEGMENT_MAGIC};
use crate::event::EventKind;
use crate::store::segment;
use crate::store::StoreError;
use std::fs::File;
use std::io::{ErrorKind, Read};
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
        let mut file = File::open(path).map_err(StoreError::Io)?;
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

        let mut cursor = (8 + header_len) as u64; // past magic + header_len + header

        // Read frames until EOF. Each frame: [len:u32 BE][crc32:u32 BE][msgpack]
        let mut entries = Vec::new();
        loop {
            let frame_offset = cursor;
            let mut frame_header = [0u8; 8];
            match file.read_exact(&mut frame_header) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(StoreError::Io(error)),
            }

            let payload_len = u32::from_be_bytes([
                frame_header[0],
                frame_header[1],
                frame_header[2],
                frame_header[3],
            ]) as usize;
            if payload_len > segment::MAX_FRAME_PAYLOAD {
                // Corrupt or truncated frame header — stop scanning this segment
                // rather than allocating unbounded memory. Events before this point
                // are still valid and will be returned.
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
                            tracing::warn!(
                                segment_id,
                                offset = frame_offset,
                                "skipping unreadable frame: {error}"
                            );
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
                    stop_scan = true;
                }
                Err(_) => stop_scan = true, // truncated or corrupt — stop
            }
            self.release_buffer(frame_buf);
            if stop_scan {
                break;
            }
        }
        Ok(entries)
    }
}
