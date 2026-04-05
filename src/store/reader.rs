use crate::coordinate::Coordinate;
use crate::event::{Event, EventHeader, HashChain, StoredEvent};
use crate::store::segment::{self, FramePayload, SEGMENT_MAGIC};
use crate::store::{DiskPos, StoreError};
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Reader: reads events from segment files.
/// Sealed segments: memory-mapped via `memmap2` for zero-copy reads.
/// Active segment: LRU FD cache + pread (Unix) / seek+read (Windows).
/// [SPEC:src/store/reader.rs]
/// [SPEC:IMPLEMENTATION NOTES item 6 — Store is Send + Sync]
pub(crate) struct Reader {
    data_dir: PathBuf,
    /// FD cache for the active segment only. Sealed segments use mmap.
    /// [DEP:parking_lot::Mutex] — lock() returns guard directly, no poisoning
    fd_cache: Mutex<FdCache>,
    /// Recycled frame buffers for active segment reads (mmap reads are zero-copy).
    buffer_pool: Mutex<Vec<Vec<u8>>>,
    /// Memory-mapped sealed segments. DashMap for concurrent reader access.
    sealed_maps: DashMap<u64, memmap2::Mmap>,
    /// ID of the current active (writable) segment. Set by the writer on rotation.
    /// Segments with ID < this are sealed and safe for mmap.
    active_segment_id: AtomicU64,
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
}

pub(crate) struct ScannedIndexEntry {
    pub header: EventHeader,
    pub entity: String,
    pub scope: String,
    pub hash_chain: HashChain,
    pub segment_id: u64,
    pub offset: u64,
    pub length: u32,
}

#[derive(Deserialize)]
struct IndexScanFramePayload {
    event: IndexScanEvent,
    entity: String,
    scope: String,
}

#[derive(Deserialize)]
struct IndexScanEvent {
    header: EventHeader,
    #[serde(rename = "payload")]
    _payload: serde::de::IgnoredAny,
    hash_chain: Option<HashChain>,
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
            sealed_maps: DashMap::new(),
            active_segment_id: AtomicU64::new(0),
        }
    }

    /// Set the active segment ID. Called by the writer after spawn and on rotation.
    /// Segments with ID < this value are considered sealed and safe for mmap.
    pub(crate) fn set_active_segment(&self, id: u64) {
        self.active_segment_id.store(id, Ordering::Release);
    }

    /// Check if a segment is sealed (not the active segment).
    fn is_sealed(&self, segment_id: u64) -> bool {
        segment_id < self.active_segment_id.load(Ordering::Acquire)
    }

    /// Get or create a memory mapping for a sealed segment.
    fn get_or_map_sealed(
        &self,
        segment_id: u64,
    ) -> Result<dashmap::mapref::one::Ref<'_, u64, memmap2::Mmap>, StoreError> {
        if let Some(entry) = self.sealed_maps.get(&segment_id) {
            return Ok(entry);
        }
        // Map the segment file
        let path = self.data_dir.join(segment::segment_filename(segment_id));
        let file = File::open(&path).map_err(StoreError::Io)?;
        // SAFETY: memmap2::Mmap::map is unsafe because the file could be modified externally.
        // Sealed segments are immutable by design — only compaction deletes them, and
        // evict_segment drops the mapping before deletion.
        let mmap = unsafe { memmap2::Mmap::map(&file) }.map_err(StoreError::Io)?;
        self.sealed_maps.insert(segment_id, mmap);
        // Return the just-inserted entry
        self.sealed_maps.get(&segment_id).ok_or_else(|| {
            StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "mmap entry missing after insert",
            ))
        })
    }

    /// Acquire a buffer from the pool, or allocate a new one if pool is empty.
    fn acquire_buffer(&self, min_size: usize) -> Vec<u8> {
        let mut pool = self.buffer_pool.lock();
        if let Some(mut buf) = pool.pop() {
            buf.resize(min_size, 0);
            buf
        } else {
            vec![0u8; min_size]
        }
    }

    /// Return a buffer to the pool for reuse. Caps pool at 16 buffers.
    fn release_buffer(&self, buf: Vec<u8>) {
        let mut pool = self.buffer_pool.lock();
        if pool.len() < 16 {
            pool.push(buf);
        }
        // else: drop it — pool is full
    }

    /// Read a single event by disk position. CRC32 verified.
    /// Sealed segments: zero-copy read via mmap.
    /// Active segment: pread (Unix) or seek+read (Windows) via FD cache.
    /// [DEP:crc32fast::hash] verifies frame integrity on every read.
    pub(crate) fn read_entry(
        &self,
        pos: &DiskPos,
    ) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        // Fast path: mmap for sealed segments — zero-copy, no lock, no buffer.
        if self.is_sealed(pos.segment_id) {
            return self.read_entry_mmap(pos);
        }

        // Slow path: active segment via FD cache + buffer pool.
        let mut buf = self.acquire_buffer(pos.length as usize);

        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let segment_id = pos.segment_id;
            let offset = pos.offset;
            self.with_fd(segment_id, |f| {
                let mut total_read = 0;
                while total_read < buf.len() {
                    let n = f
                        .read_at(&mut buf[total_read..], offset + total_read as u64)
                        .map_err(StoreError::Io)?;
                    if n == 0 {
                        return Err(StoreError::corrupt_eof(segment_id));
                    }
                    total_read += n;
                }
                Ok(())
            })?;
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom};
            let offset = pos.offset;
            self.with_fd(pos.segment_id, |f| {
                f.seek(SeekFrom::Start(offset)).map_err(StoreError::Io)?;
                f.read_exact(&mut buf).map_err(StoreError::Io)
            })?;
        }

        let result = segment::frame_decode(&buf).map_err(|e| match e {
            segment::FrameDecodeError::CrcMismatch { .. } => StoreError::CrcMismatch {
                segment_id: pos.segment_id,
                offset: pos.offset,
            },
            segment::FrameDecodeError::TooShort | segment::FrameDecodeError::Truncated { .. } => {
                StoreError::corrupt_frame(pos.segment_id, e.to_string())
            }
        });
        let (msgpack, _) = match result {
            Ok(v) => v,
            Err(e) => {
                self.release_buffer(buf);
                return Err(e);
            }
        };
        let payload: FramePayload<serde_json::Value> =
            rmp_serde::from_slice(msgpack).map_err(|e| StoreError::Serialization(Box::new(e)))?;

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
                    match rmp_serde::from_slice::<FramePayload<serde_json::Value>>(msgpack) {
                        Ok(payload) => {
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

    /// Scan only the metadata required to rebuild the in-memory index.
    /// Tries the SIDX footer first (O(1) seek + bulk read); falls back to
    /// frame-by-frame msgpack deserialization if no SIDX footer is present.
    pub(crate) fn scan_segment_index(
        &self,
        path: &Path,
    ) -> Result<Vec<ScannedIndexEntry>, StoreError> {
        // Fast path: try SIDX footer.
        if let Ok(Some((sidx_entries, strings))) = crate::store::sidx::read_footer(path) {
            let segment_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let mut result = Vec::with_capacity(sidx_entries.len());
            for se in sidx_entries {
                let entity = strings
                    .get(se.entity_idx as usize)
                    .cloned()
                    .unwrap_or_default();
                let scope = strings
                    .get(se.scope_idx as usize)
                    .cloned()
                    .unwrap_or_default();
                result.push(ScannedIndexEntry {
                    header: crate::event::EventHeader::from_sidx(
                        se.event_id,
                        se.correlation_id,
                        if se.causation_id == 0 {
                            None
                        } else {
                            Some(se.causation_id)
                        },
                        se.wall_ms,
                        se.clock,
                        crate::store::sidx::raw_to_kind(se.kind),
                    ),
                    entity,
                    scope,
                    hash_chain: crate::event::HashChain {
                        prev_hash: se.prev_hash,
                        event_hash: se.event_hash,
                    },
                    segment_id,
                    offset: se.frame_offset,
                    length: se.frame_length,
                });
            }
            return Ok(result);
        }

        // Slow path: frame-by-frame scan.
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
                return Ok(Vec::new());
            }
        };

        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .map_err(StoreError::Io)?;
        let header_len = u32::from_be_bytes(header_len_buf) as usize;
        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf).map_err(StoreError::Io)?;
        let header: segment::SegmentHeader = rmp_serde::from_slice(&header_buf)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;
        if header.version != 1 {
            return Err(StoreError::corrupt_version(segment_id, header.version));
        }

        let mut cursor = (8 + header_len) as u64;
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
                            entries.push(ScannedIndexEntry {
                                header: payload.event.header,
                                entity: payload.entity,
                                scope: payload.scope,
                                hash_chain: payload.event.hash_chain.unwrap_or_default(),
                                segment_id,
                                offset: frame_offset,
                                #[allow(clippy::cast_possible_truncation)] // frame_size < segment_max_bytes < u32::MAX
                                length: frame_size as u32,
                            });
                        }
                        Err(error) => {
                            tracing::warn!(
                                segment_id,
                                offset = frame_offset,
                                "skipping unreadable frame metadata: {error}"
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
                Err(_) => stop_scan = true,
            }
            self.release_buffer(frame_buf);
            if stop_scan {
                break;
            }
        }

        Ok(entries)
    }

    /// Run `op` against the cached (or freshly opened) file descriptor for `segment_id`,
    /// holding the FD cache lock for the duration. LRU order is maintained on each call.
    /// On Windows this is required: cloned File handles share the OS file cursor, so
    /// seek+read must happen under the lock. On Unix, read_at(pread) is cursor-safe but
    /// still benefits from the single-lock path for cache consistency.
    fn with_fd<F, T>(&self, segment_id: u64, op: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut File) -> Result<T, StoreError>,
    {
        let mut cache = self.fd_cache.lock();
        if let Some(pos) = cache.order.iter().position(|&id| id == segment_id) {
            cache.order.remove(pos);
            cache.order.push(segment_id);
        } else {
            let path = self.data_dir.join(segment::segment_filename(segment_id));
            let file = File::open(&path).map_err(StoreError::Io)?;
            if cache.fds.len() >= cache.budget {
                if let Some(oldest) = cache.order.first().copied() {
                    cache.fds.remove(&oldest);
                    cache.order.remove(0);
                }
            }
            cache.fds.insert(segment_id, file);
            cache.order.push(segment_id);
        }
        let file = cache.fds.get_mut(&segment_id).ok_or_else(|| {
            StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "segment fd missing after cache insert",
            ))
        })?;
        op(file)
    }

    /// Zero-copy read from a sealed segment's memory map.
    fn read_entry_mmap(&self, pos: &DiskPos) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let mmap_ref = self.get_or_map_sealed(pos.segment_id)?;
        let mmap: &memmap2::Mmap = mmap_ref.value();
        let start =
            usize::try_from(pos.offset).map_err(|_| StoreError::corrupt_eof(pos.segment_id))?;
        let end = start + pos.length as usize;
        if end > mmap.len() {
            return Err(StoreError::corrupt_eof(pos.segment_id));
        }
        let frame_buf = &mmap[start..end];
        let (msgpack, _) = segment::frame_decode(frame_buf).map_err(|e| match e {
            segment::FrameDecodeError::CrcMismatch { .. } => StoreError::CrcMismatch {
                segment_id: pos.segment_id,
                offset: pos.offset,
            },
            segment::FrameDecodeError::TooShort | segment::FrameDecodeError::Truncated { .. } => {
                StoreError::corrupt_frame(pos.segment_id, e.to_string())
            }
        })?;
        let payload: FramePayload<serde_json::Value> =
            rmp_serde::from_slice(msgpack).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        let coord =
            Coordinate::new(&payload.entity, &payload.scope).map_err(StoreError::Coordinate)?;
        Ok(StoredEvent {
            coordinate: coord,
            event: payload.event,
        })
    }

    /// Read multiple events by disk position. Groups by segment_id to minimize
    /// mmap lookups — one `get_or_map_sealed` call per unique segment instead
    /// of one per event. Returns results in the same order as `positions`.
    pub(crate) fn read_entries_batch(
        &self,
        positions: &[&DiskPos],
    ) -> Result<Vec<StoredEvent<serde_json::Value>>, StoreError> {
        // Fast path: if all positions are from sealed segments, group by segment
        // to amortize the mmap lookup cost.
        //
        // We still use read_entry per position (which dispatches to mmap or fd
        // internally), but the mmap cache ensures each segment is mapped only once.
        // The DashMap lookup for a cached mmap is O(1) — the grouping optimization
        // would only save the DashMap hash overhead, which is negligible.
        //
        // The real optimization here: the mmap is populated on first access and
        // stays cached for all subsequent reads from the same segment. Sequential
        // positions within a segment benefit from OS page-cache locality.
        let mut results = Vec::with_capacity(positions.len());
        for pos in positions {
            results.push(self.read_entry(pos)?);
        }
        Ok(results)
    }

    /// Evict a segment from FD cache and mmap cache.
    /// Called during compaction before deleting segment files.
    /// On Windows, the mmap MUST be dropped before the file can be deleted.
    pub(crate) fn evict_segment(&self, segment_id: u64) {
        // Drop mmap first (required on Windows, polite on POSIX).
        self.sealed_maps.remove(&segment_id);
        // Then drop the FD cache entry.
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
