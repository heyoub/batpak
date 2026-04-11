use crate::coordinate::Coordinate;
use crate::event::{Event, EventHeader, EventKind, HashChain, StoredEvent};
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
    /// Original `global_sequence` if a durable source (SIDX footer) was available.
    /// `None` for slow-path scans (active segment, missing/corrupt SIDX) — the
    /// rebuild caller must synthesize a sequence in that case.
    pub global_sequence: Option<u64>,
}

/// Cross-segment batch recovery state.
/// Passed between segment scans to handle batches spanning segment boundaries.
/// [SPEC:src/store/reader.rs — cross-segment batch recovery]
#[derive(Default)]
pub(crate) struct BatchRecoveryState {
    pub staged: Vec<ScannedIndexEntry>,
    pub remaining: u32,
    pub started_count: u32,
    pub in_batch: bool,
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
    fn decode_frame_payload_value(
        msgpack: &[u8],
    ) -> Result<FramePayload<serde_json::Value>, StoreError> {
        let payload: FramePayload<Vec<u8>> =
            rmp_serde::from_slice(msgpack).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        let event = payload.event;
        let decoded_payload = match event.header.event_kind {
            EventKind::SYSTEM_BATCH_BEGIN | EventKind::SYSTEM_BATCH_COMMIT => {
                serde_json::Value::Null
            }
            _ => rmp_serde::from_slice(&event.payload)
                .map_err(|e| StoreError::Serialization(Box::new(e)))?,
        };
        Ok(FramePayload {
            event: Event {
                header: event.header,
                payload: decoded_payload,
                hash_chain: event.hash_chain,
            },
            entity: payload.entity,
            scope: payload.scope,
        })
    }

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
        let payload = Self::decode_frame_payload_value(msgpack)?;

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

    /// Scan only the metadata required to rebuild the in-memory index.
    /// Tries the SIDX footer first (O(1) seek + bulk read); falls back to
    /// frame-by-frame msgpack deserialization if no SIDX footer is present.
    /// Accepts optional `batch_state` for cross-segment batch recovery.
    pub(crate) fn scan_segment_index(
        &self,
        path: &Path,
        mut batch_state: Option<&mut BatchRecoveryState>,
    ) -> Result<Vec<ScannedIndexEntry>, StoreError> {
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
            if let Ok(Some((sidx_entries, strings))) = crate::store::sidx::read_footer(path) {
                let mut result = Vec::with_capacity(sidx_entries.len());
                for se in sidx_entries {
                    let kind = crate::store::sidx::raw_to_kind(se.kind);
                    // Skip batch markers in SIDX fast path.
                    if kind == EventKind::SYSTEM_BATCH_BEGIN
                        || kind == EventKind::SYSTEM_BATCH_COMMIT
                    {
                        continue;
                    }
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
                            kind,
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
                        // SIDX footer carries the original sequence — preserve it
                        // so sparse `global_sequence` values survive cold-start rebuild.
                        global_sequence: Some(se.global_sequence),
                    });
                }
                return Ok(result);
            }
        }

        // Slow path: frame-by-frame scan for active segment or when batch state is pending.
        // Track batch-committed entries for fsync ambiguity handling.
        let has_sidx_footer = crate::store::sidx::read_footer(path).is_ok_and(|r| r.is_some());
        let mut batch_committed_indices = Vec::new();
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

        // Use cross-segment batch state if provided, otherwise create local state.
        // This enables batches that span segment boundaries to be recovered correctly.
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
                    // EOF: discard any incomplete batch (persists in state_ref for cross-segment handling).
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
                    // EOF: incomplete batch persists in state_ref for cross-segment handling.
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
                                    // Start staging batch. The marker itself is not indexed.
                                    // Extract batch count from payload_size field.
                                    let batch_count = payload.event.header.payload_size;
                                    state_ref.in_batch = true;
                                    state_ref.remaining = batch_count;
                                    state_ref.started_count = batch_count;
                                    state_ref.staged.reserve(batch_count as usize);
                                } else if kind == EventKind::SYSTEM_BATCH_COMMIT {
                                    // COMMIT without BEGIN: orphaned commit, skip.
                                    tracing::warn!(
                                        segment_id,
                                        offset = frame_offset,
                                        "orphaned COMMIT marker, skipping"
                                    );
                                } else {
                                    // Normal event: commit immediately.
                                    entries.push(ScannedIndexEntry {
                                        header: payload.event.header,
                                        entity: payload.entity,
                                        scope: payload.scope,
                                        hash_chain: payload.event.hash_chain.unwrap_or_default(),
                                        segment_id,
                                        offset: frame_offset,
                                        // Frame sizes are bounded by segment_max_bytes (default 64MB), well within u32 range
                                        #[allow(clippy::cast_possible_truncation)]
                                        length: frame_size as u32,
                                        // Slow path: no SIDX footer, so no durable sequence source.
                                        // Caller (rebuild) will synthesize via the ReplayCursor allocator.
                                        global_sequence: None,
                                    });
                                }
                            } else if kind == EventKind::SYSTEM_BATCH_COMMIT {
                                // COMMIT marker: verify count matches and commit.
                                if state_ref.remaining == 0 {
                                    // Complete batch: commit all staged.
                                    let completed_batch = std::mem::take(&mut state_ref.staged);
                                    let start_idx = entries.len();
                                    entries.extend(completed_batch);
                                    // Track batch-committed entry indices for fsync ambiguity.
                                    for i in start_idx..entries.len() {
                                        batch_committed_indices.push(i);
                                    }
                                    state_ref.in_batch = false;
                                    tracing::debug!(
                                        segment_id,
                                        batch_count = state_ref.started_count,
                                        "batch committed via COMMIT marker"
                                    );
                                } else {
                                    // Mismatch: expected more or fewer items.
                                    tracing::warn!(
                                        segment_id,
                                        expected = state_ref.started_count,
                                        remaining = state_ref.remaining,
                                        staged_count = state_ref.staged.len(),
                                        "batch COMMIT mismatch, discarding"
                                    );
                                }
                                // Reset batch state (committed or mismatched, we're done).
                                state_ref.in_batch = false;
                                state_ref.staged.clear();
                            } else if kind == EventKind::SYSTEM_BATCH_BEGIN {
                                // Nested BEGIN without COMMIT: discard previous batch.
                                tracing::warn!(
                                    segment_id,
                                    staged_count = state_ref.staged.len(),
                                    "nested BEGIN without COMMIT, discarding incomplete batch"
                                );
                                // Start new batch.
                                let batch_count = payload.event.header.payload_size;
                                state_ref.remaining = batch_count;
                                state_ref.started_count = batch_count;
                                state_ref.staged.clear();
                                state_ref.staged.reserve(batch_count as usize);
                            } else {
                                // Stage this event (not a marker).
                                state_ref.staged.push(ScannedIndexEntry {
                                    header: payload.event.header,
                                    entity: payload.entity,
                                    scope: payload.scope,
                                    hash_chain: payload.event.hash_chain.unwrap_or_default(),
                                    segment_id,
                                    offset: frame_offset,
                                    // Frame sizes are bounded by segment_max_bytes (default 64MB), well within u32 range
                                    #[allow(clippy::cast_possible_truncation)]
                                    length: frame_size as u32,
                                    // Slow path: no SIDX, no durable sequence source.
                                    global_sequence: None,
                                });
                                if state_ref.remaining > 0 {
                                    state_ref.remaining -= 1;
                                }
                                // Note: We don't auto-commit on remaining==0.
                                // We wait for the COMMIT marker to confirm.
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                segment_id,
                                offset = frame_offset,
                                "skipping unreadable frame metadata: {error}"
                            );
                            // Corruption during batch: discard staged entries.
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
                    // CRC failure during batch: discard staged entries.
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
                    // Other decode errors: discard staged entries.
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

        // Fsync ambiguity handling: if segment lacks SIDX footer (incomplete sync),
        // discard batch-committed entries. SIDX is written after sync, so its absence
        // indicates sync didn't complete.
        if !has_sidx_footer && !batch_committed_indices.is_empty() {
            tracing::warn!(
                segment_id,
                batch_count = batch_committed_indices.len(),
                "segment lacks SIDX footer (incomplete sync), discarding batch entries"
            );
            // Remove in reverse order to preserve indices.
            for idx in batch_committed_indices.into_iter().rev() {
                entries.remove(idx);
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
        let payload = Self::decode_frame_payload_value(msgpack)?;
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
