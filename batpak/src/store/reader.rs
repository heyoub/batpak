use crate::coordinate::Coordinate;
use crate::event::{Event, StoredEvent};
use crate::store::segment::{self, FramePayload, SEGMENT_MAGIC};
use crate::store::{DiskPos, StoreError};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Reader: reads events from segment files. LRU file descriptor cache + buffer pool.
/// Behind parking_lot::Mutex for Send + Sync. [SPEC:src/store/reader.rs]
/// [SPEC:IMPLEMENTATION NOTES item 6 — Store is Send + Sync]
pub(crate) struct Reader {
    data_dir: PathBuf,
    /// LRU FD cache: segment_id -> open File handle. Evicts oldest when full.
    /// [DEP:parking_lot::Mutex] — lock() returns guard directly, no poisoning
    fd_cache: Mutex<FdCache>,
    /// Recycled frame buffers to avoid per-read allocations during batch reads.
    /// [CROSS-POLLINATION:czap/compositor-pool.ts — zero-alloc hot path via ring buffer]
    buffer_pool: Mutex<Vec<Vec<u8>>>,
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
            buffer_pool: Mutex::new(Vec::new()),
        }
    }

    /// Acquire a buffer from the pool, or allocate a new one if pool is empty.
    /// [CROSS-POLLINATION:czap/compositor-pool.ts — acquire/release pattern]
    pub(crate) fn acquire_buffer(&self, min_size: usize) -> Vec<u8> {
        let mut pool = self.buffer_pool.lock();
        if let Some(mut buf) = pool.pop() {
            buf.resize(min_size, 0);
            buf
        } else {
            vec![0u8; min_size]
        }
    }

    /// Return a buffer to the pool for reuse. Caps pool at 16 buffers.
    pub(crate) fn release_buffer(&self, buf: Vec<u8>) {
        let mut pool = self.buffer_pool.lock();
        if pool.len() < 16 {
            pool.push(buf);
        }
        // else: drop it — pool is full
    }

    /// Read a single event by disk position. CRC32 verified.
    /// [DEP:crc32fast::hash] verifies frame integrity on every read.
    pub(crate) fn read_entry(
        &self,
        pos: &DiskPos,
    ) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let mut buf = self.acquire_buffer(pos.length as usize);

        // Use pread (read_at) — doesn't modify file cursor. [SPEC:IMPLEMENTATION NOTES item 7]
        // Loop to handle short reads (read_at may return fewer bytes than requested).
        #[cfg(unix)]
        {
            let file = self.get_fd(pos.segment_id)?;
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
            // Non-unix fallback: seek + read under the FD cache lock to prevent
            // concurrent seeks on cloned File handles (which share the file cursor
            // on Windows). The lock ensures sequential access per reader instance.
            // [SPEC:IMPLEMENTATION NOTES item 7 — concurrent read safety]
            use std::io::{Seek, SeekFrom};
            let mut cache = self.fd_cache.lock();
            if let Some(f) = cache.fds.get_mut(&pos.segment_id) {
                f.seek(SeekFrom::Start(pos.offset))
                    .map_err(StoreError::Io)?;
                f.read_exact(&mut buf).map_err(StoreError::Io)?;
            } else {
                // File not in cache — open, seek, read, and cache it
                let path = self
                    .data_dir
                    .join(segment::segment_filename(pos.segment_id));
                let mut f = File::open(&path).map_err(StoreError::Io)?;
                f.seek(SeekFrom::Start(pos.offset))
                    .map_err(StoreError::Io)?;
                f.read_exact(&mut buf).map_err(StoreError::Io)?;
                if cache.fds.len() >= cache.budget {
                    if let Some(oldest) = cache.order.first().copied() {
                        cache.fds.remove(&oldest);
                        cache.order.remove(0);
                    }
                }
                cache.fds.insert(pos.segment_id, f);
                cache.order.push(pos.segment_id);
            }
        }

        let result = segment::frame_decode(&buf).map_err(|e| match e {
            segment::FrameDecodeError::CrcMismatch { .. } => StoreError::CrcMismatch {
                segment_id: pos.segment_id,
                offset: pos.offset,
            },
            segment::FrameDecodeError::TooShort
            | segment::FrameDecodeError::Truncated { .. } => StoreError::CorruptSegment {
                segment_id: pos.segment_id,
                detail: e.to_string(),
            },
        });
        let (msgpack, _) = match result {
            Ok(v) => v,
            Err(e) => {
                self.release_buffer(buf);
                return Err(e);
            }
        };
        let payload: FramePayload<serde_json::Value> =
            rmp_serde::from_slice(msgpack).map_err(|e| StoreError::Serialization(e.to_string()))?;

        // Release buffer back to pool after deserialization
        self.release_buffer(buf);

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
        let segment_id = match path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
        {
            Some(id) => id,
            None => {
                tracing::warn!(?path, "skipping segment with unparseable filename");
                return Ok(Vec::new());
            }
        };

        // Read header_len (u32 BE) after magic, then header bytes
        if all_bytes.len() < 8 {
            return Err(StoreError::CorruptSegment {
                segment_id,
                detail: "segment too short for magic + header_len".into(),
            });
        }
        let header_len =
            u32::from_be_bytes([all_bytes[4], all_bytes[5], all_bytes[6], all_bytes[7]]) as usize;
        if all_bytes.len() < 8 + header_len {
            return Err(StoreError::CorruptSegment {
                segment_id,
                detail: "segment truncated in header".into(),
            });
        }
        let header_slice = &all_bytes[8..8 + header_len];
        let header: segment::SegmentHeader = rmp_serde::from_slice(header_slice)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;

        // Version check — reject unknown segment versions
        if header.version != 1 {
            return Err(StoreError::CorruptSegment {
                segment_id,
                detail: format!("unsupported segment version: {}", header.version),
            });
        }

        let mut cursor = 8 + header_len; // past magic + header_len + header

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
                                #[allow(clippy::cast_possible_truncation)] // frame_size < segment_max_bytes < u32::MAX
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
                Err(segment::FrameDecodeError::CrcMismatch { .. }) => {
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

    /// Evict a segment from the FD cache. Called during compaction before deleting segment files.
    pub(crate) fn evict_segment(&self, segment_id: u64) {
        let mut cache = self.fd_cache.lock();
        cache.fds.remove(&segment_id);
        cache.order.retain(|&id| id != segment_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_reader() -> (Reader, TempDir) {
        let dir = TempDir::new().expect("create temp dir for reader test");
        let reader = Reader::new(dir.path().to_path_buf(), 4);
        (reader, dir)
    }

    #[test]
    fn acquire_buffer_returns_requested_size() {
        let (reader, _dir) = test_reader();
        let buf = reader.acquire_buffer(256);
        assert_eq!(
            buf.len(),
            256,
            "ACQUIRE BUFFER: expected buffer of size 256, got {}.\n\
             Check: src/store/reader.rs acquire_buffer() vec allocation.",
            buf.len()
        );
        // All bytes should be zero-initialized.
        assert!(
            buf.iter().all(|&b| b == 0),
            "ACQUIRE BUFFER: newly allocated buffer should be zero-initialized."
        );
    }

    #[test]
    fn released_buffer_is_recycled() {
        let (reader, _dir) = test_reader();

        // Acquire and release a buffer.
        let buf = reader.acquire_buffer(128);
        assert_eq!(buf.len(), 128);
        reader.release_buffer(buf);

        // Pool should now have 1 buffer. Next acquire should recycle it.
        let buf2 = reader.acquire_buffer(64);
        assert_eq!(
            buf2.len(),
            64,
            "RECYCLED BUFFER: buffer should be resized to requested size.\n\
             Check: src/store/reader.rs acquire_buffer() resize path."
        );

        // Verify pool is now empty (we took the recycled buffer).
        let pool = reader.buffer_pool.lock();
        assert_eq!(
            pool.len(),
            0,
            "RECYCLED BUFFER: pool should be empty after acquiring the recycled buffer."
        );
    }

    #[test]
    fn pool_caps_at_16_buffers() {
        let (reader, _dir) = test_reader();

        // Release 20 buffers into the pool. Only 16 should be retained.
        for _ in 0..20 {
            let buf = vec![0u8; 64];
            reader.release_buffer(buf);
        }

        let pool = reader.buffer_pool.lock();
        assert_eq!(
            pool.len(),
            16,
            "POOL CAP: buffer pool should cap at 16 buffers, got {}.\n\
             Check: src/store/reader.rs release_buffer() cap check.",
            pool.len()
        );
    }

    #[test]
    fn acquire_from_empty_pool_allocates_new() {
        let (reader, _dir) = test_reader();

        // Pool starts empty. Acquire should allocate a fresh buffer.
        {
            let pool = reader.buffer_pool.lock();
            assert_eq!(pool.len(), 0, "Pool should start empty.");
        }

        let buf = reader.acquire_buffer(512);
        assert_eq!(
            buf.len(),
            512,
            "EMPTY POOL ALLOC: should allocate a new buffer of requested size when pool is empty."
        );

        // Pool should still be empty since we allocated, not recycled.
        let pool = reader.buffer_pool.lock();
        assert_eq!(
            pool.len(),
            0,
            "EMPTY POOL ALLOC: pool should remain empty after allocation (buffer not returned yet)."
        );
    }
}
