use crate::coordinate::Coordinate;
use crate::event::{Event, StoredEvent};
use crate::store::segment::{self, FramePayload, SEGMENT_MAGIC};
use crate::store::{DiskPos, StoreError};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Reader: reads events from segment files. LRU file descriptor cache.
/// Behind parking_lot::Mutex for Send + Sync. [SPEC:src/store/reader.rs]
/// [SPEC:IMPLEMENTATION NOTES item 6 — Store is Send + Sync]
pub(crate) struct Reader {
    data_dir: PathBuf,
    /// LRU FD cache: segment_id -> open File handle. Evicts oldest when full.
    /// [DEP:parking_lot::Mutex] — lock() returns guard directly, no poisoning
    fd_cache: Mutex<FdCache>,
}

struct FdCache {
    fds: HashMap<u64, File>,
    order: Vec<u64>, // LRU order: most recent at end
    budget: usize,
}

/// ScannedEntry: what cold start produces per event in a segment.
pub(crate) struct ScannedEntry {
    pub event: Event<serde_json::Value>,
    pub entity: String,
    pub scope: String,
    pub segment_id: u64,
    pub offset: u64,
    pub length: u32,
}

impl Reader {
    pub(crate) fn new(data_dir: PathBuf, fd_budget: usize) -> Self {
        Self {
            data_dir,
            fd_cache: Mutex::new(FdCache {
                fds: HashMap::new(),
                order: Vec::new(),
                budget: fd_budget,
            }),
        }
    }

    /// Read a single event by disk position. CRC32 verified.
    /// [DEP:crc32fast::hash] verifies frame integrity on every read.
    pub(crate) fn read_entry(
        &self,
        pos: &DiskPos,
    ) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let file = self.get_fd(pos.segment_id)?;
        let mut buf = vec![0u8; pos.length as usize];

        // Use pread (read_at) — doesn't modify file cursor. [SPEC:IMPLEMENTATION NOTES item 7]
        // Loop to handle short reads (read_at may return fewer bytes than requested).
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut total_read = 0;
            while total_read < buf.len() {
                let n = file
                    .read_at(&mut buf[total_read..], pos.offset + total_read as u64)
                    .map_err(StoreError::Io)?;
                if n == 0 {
                    return Err(StoreError::CorruptSegment {
                        segment_id: pos.segment_id,
                        detail: "unexpected EOF during read".into(),
                    });
                }
                total_read += n;
            }
        }
        #[cfg(not(unix))]
        {
            // Fallback: seek + read (holds the mutex so this is safe)
            use std::io::{Seek, SeekFrom};
            let mut file = file; // need mut for seek
            file.seek(SeekFrom::Start(pos.offset))
                .map_err(StoreError::Io)?;
            file.read_exact(&mut buf).map_err(StoreError::Io)?;
        }

        let (msgpack, _) = segment::frame_decode(&buf)?;
        let payload: FramePayload<serde_json::Value> =
            rmp_serde::from_slice(msgpack).map_err(|e| StoreError::Serialization(e.to_string()))?;

        let coord =
            Coordinate::new(&payload.entity, &payload.scope).map_err(StoreError::Coordinate)?;
        Ok(StoredEvent {
            coordinate: coord,
            event: payload.event,
        })
    }

    /// Scan an entire segment for cold start. Returns all events in order.
    pub(crate) fn scan_segment(&self, path: &Path) -> Result<Vec<ScannedEntry>, StoreError> {
        let mut file = File::open(path).map_err(StoreError::Io)?;
        let mut all_bytes = Vec::new();
        file.read_to_end(&mut all_bytes).map_err(StoreError::Io)?;

        // Verify magic
        if all_bytes.len() < 4 || &all_bytes[..4] != SEGMENT_MAGIC {
            return Err(StoreError::CorruptSegment {
                segment_id: 0,
                detail: "bad magic".into(),
            });
        }

        // Extract segment_id from filename: "000042.fbat" → 42
        let segment_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        // Skip magic (4 bytes). Parse segment header from msgpack.
        // [DEP:rmp_serde::from_slice] — deserialize SegmentHeader
        let after_magic = &all_bytes[4..];
        let _header: segment::SegmentHeader = rmp_serde::from_slice(after_magic)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;

        // Find where header ends and frames begin.
        // Re-encode header to measure its serialized size (simplest approach).
        let header_bytes = rmp_serde::to_vec_named(&_header)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let mut cursor = 4 + header_bytes.len();

        // Read frames until EOF. Each frame: [len:u32 BE][crc32:u32 BE][msgpack]
        let mut entries = Vec::new();
        while cursor < all_bytes.len() {
            let remaining = &all_bytes[cursor..];
            if remaining.len() < 8 {
                break;
            } // not enough for a frame header

            let frame_offset = cursor as u64;
            match segment::frame_decode(remaining) {
                Ok((msgpack, frame_size)) => {
                    // Deserialize frame payload
                    match rmp_serde::from_slice::<FramePayload<serde_json::Value>>(msgpack) {
                        Ok(payload) => {
                            entries.push(ScannedEntry {
                                event: payload.event,
                                entity: payload.entity,
                                scope: payload.scope,
                                segment_id,
                                offset: frame_offset,
                                length: frame_size as u32,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(
                                segment_id,
                                offset = frame_offset,
                                "skipping unreadable frame: {e}"
                            );
                        }
                    }
                    cursor += frame_size;
                }
                Err(StoreError::CrcMismatch { .. }) => {
                    tracing::warn!(
                        segment_id,
                        offset = frame_offset,
                        "CRC mismatch, skipping frame"
                    );
                    break; // CRC mismatch = stop scanning this segment
                }
                Err(_) => break, // truncated or corrupt — stop
            }
        }
        Ok(entries)
    }

    fn get_fd(&self, segment_id: u64) -> Result<File, StoreError> {
        let mut cache = self.fd_cache.lock();
        // LRU logic: if in cache, move to end of order vec. If not, open file,
        // evict oldest if over budget, insert.
        if let Some(pos) = cache.order.iter().position(|&id| id == segment_id) {
            cache.order.remove(pos);
            cache.order.push(segment_id);
            return cache.fds[&segment_id].try_clone().map_err(StoreError::Io);
        }

        let path = self.data_dir.join(segment::segment_filename(segment_id));
        let file = File::open(&path).map_err(StoreError::Io)?;

        if cache.fds.len() >= cache.budget {
            if let Some(oldest) = cache.order.first().copied() {
                cache.fds.remove(&oldest);
                cache.order.remove(0);
            }
        }

        cache
            .fds
            .insert(segment_id, file.try_clone().map_err(StoreError::Io)?);
        cache.order.push(segment_id);
        Ok(file)
    }
}
