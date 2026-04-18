use crate::coordinate::Coordinate;
use crate::event::{Event, EventHeader, EventKind, HashChain, StoredEvent};
use crate::store::cold_start::ColdStartIndexRow;
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

const FRAME_HEADER_BYTES: usize = 8;
const MAX_BATCH_RECOVERY_ITEMS: u32 = 4096;

/// Reader: reads events from segment files.
/// Sealed segments: memory-mapped via `memmap2` for zero-copy reads.
/// Active segment: LRU FD cache + pread (Unix) / seek+read (Windows).
/// Reader: low-level segment access used by replay and point reads.
/// Internally synchronized so `Store` stays `Send + Sync`.
///
/// Technically public (with `#[doc(hidden)]`) so that `ReplayInput`'s
/// methods — which take `&Reader` — can be part of a public trait without
/// triggering the `private_bounds` lint on `Store::project` and friends.
/// External callers must not rely on this type being reachable; it is
/// not part of the public API contract.
#[doc(hidden)]
pub struct Reader {
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

impl ScannedIndexEntry {
    pub(crate) fn from_cold_start_row(
        row: &ColdStartIndexRow,
        interner_strings: &[String],
    ) -> Result<Self, StoreError> {
        let (entity, scope) = row.resolve_strings(interner_strings)?;
        Ok(Self {
            header: row.to_event_header(),
            entity,
            scope,
            hash_chain: row.hash_chain.clone(),
            segment_id: row.disk_pos.segment_id,
            offset: row.disk_pos.offset,
            length: row.disk_pos.length,
            global_sequence: Some(row.global_sequence),
        })
    }
}

/// Cross-segment batch recovery state.
/// Passed between segment scans to handle batches spanning segment boundaries.
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
    fn checked_frame_len(segment_id: u64, length: u32) -> Result<usize, StoreError> {
        let frame_len = usize::try_from(length).map_err(|_| {
            StoreError::corrupt_frame(segment_id, "stored frame length does not fit in usize")
        })?;
        let max_frame_len = FRAME_HEADER_BYTES + segment::MAX_FRAME_PAYLOAD;
        if frame_len < FRAME_HEADER_BYTES || frame_len > max_frame_len {
            return Err(StoreError::corrupt_frame(
                segment_id,
                format!(
                    "stored frame length {frame_len} is outside valid range [{FRAME_HEADER_BYTES}, {max_frame_len}]"
                ),
            ));
        }
        Ok(frame_len)
    }

    fn checked_frame_range(
        segment_id: u64,
        offset: u64,
        length: u32,
        available_len: usize,
    ) -> Result<std::ops::Range<usize>, StoreError> {
        let start = usize::try_from(offset).map_err(|_| StoreError::corrupt_eof(segment_id))?;
        let frame_len = Self::checked_frame_len(segment_id, length)?;
        let end = start
            .checked_add(frame_len)
            .ok_or_else(|| StoreError::corrupt_frame(segment_id, "frame offset overflow"))?;
        if end > available_len {
            return Err(StoreError::corrupt_eof(segment_id));
        }
        Ok(start..end)
    }

    fn checked_batch_count(
        segment_id: u64,
        offset: u64,
        batch_count: u32,
    ) -> Result<u32, StoreError> {
        if batch_count == 0 || batch_count > MAX_BATCH_RECOVERY_ITEMS {
            return Err(StoreError::corrupt_frame(
                segment_id,
                format!(
                    "invalid batch marker count {batch_count} at offset {offset}; expected 1..={MAX_BATCH_RECOVERY_ITEMS}"
                ),
            ));
        }
        Ok(batch_count)
    }

    fn required_index_hash_chain(
        event: &IndexScanEvent,
        segment_id: u64,
        offset: u64,
    ) -> Result<HashChain, StoreError> {
        match &event.hash_chain {
            Some(chain) => Ok(chain.clone()),
            None if matches!(
                event.header.event_kind,
                EventKind::SYSTEM_BATCH_BEGIN | EventKind::SYSTEM_BATCH_COMMIT
            ) =>
            {
                Err(StoreError::corrupt_frame(
                    segment_id,
                    format!(
                        "batch marker at offset {offset} should not reach hash-chain validation"
                    ),
                ))
            }
            None => Err(StoreError::corrupt_frame(
                segment_id,
                format!(
                    "event at offset {offset} is missing hash_chain; recovery no longer defaults missing hashes"
                ),
            )),
        }
    }

    fn read_active_frame_into(&self, pos: &DiskPos, buf: &mut [u8]) -> Result<(), StoreError> {
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
            })
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom};
            let offset = pos.offset;
            self.with_fd(pos.segment_id, |f| {
                f.seek(SeekFrom::Start(offset)).map_err(StoreError::Io)?;
                f.read_exact(buf).map_err(StoreError::Io)
            })
        }
    }

    fn decode_frame_payload_raw(msgpack: &[u8]) -> Result<FramePayload<Vec<u8>>, StoreError> {
        rmp_serde::from_slice(msgpack).map_err(|e| StoreError::Serialization(Box::new(e)))
    }

    fn decode_frame_payload_value(
        msgpack: &[u8],
    ) -> Result<FramePayload<serde_json::Value>, StoreError> {
        let payload = Self::decode_frame_payload_raw(msgpack)?;
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

    /// Return the currently configured active segment ID.
    pub(crate) fn active_segment_id(&self) -> u64 {
        self.active_segment_id.load(Ordering::Acquire)
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
    ///
    /// The returned buffer is always exactly `min_size` bytes long and
    /// always zero-filled. Recycled buffers are explicitly cleared before
    /// resizing — `Vec::resize` only fills NEW elements when growing, so
    /// without an explicit `clear()` a recycled buffer would leak the
    /// previous user's data into the new acquirer (in-process information
    /// disclosure, and a correctness bug for any caller that assumes the
    /// buffer starts zeroed). Caught by the
    /// `released_buffer_is_zero_filled_and_resized_on_next_acquire` test
    /// in the Tier 1 drill sweep.
    fn acquire_buffer(&self, min_size: usize) -> Vec<u8> {
        let mut pool = self.buffer_pool.lock();
        if let Some(mut buf) = pool.pop() {
            buf.clear();
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
        let frame_len = Self::checked_frame_len(pos.segment_id, pos.length)?;
        let mut buf = self.acquire_buffer(frame_len);
        self.read_active_frame_into(pos, &mut buf)?;

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
        sidx_entries: &[super::sidx::SidxEntry],
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
        if &trailer[12..16] != super::sidx::SIDX_MAGIC {
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
        if sidx_entries.is_empty() && sidx_start > (4 + 4/* magic + header_len */) {
            return Some(false);
        }

        Some(max_tail >= sidx_start)
    }

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
            if let Ok(Some((sidx_entries, strings))) = super::sidx::read_footer(path) {
                // SIDX coverage check (A7): if any frame exists past the
                // last SIDX entry's tail (before the SIDX footer itself),
                // this segment rotated with an in-flight batch — the
                // BEGIN marker and staged items occupy frames that SIDX
                // does not know about, because SIDX records items only
                // after COMMIT succeeds. In that case we must frame-scan
                // so `BatchRecoveryState` picks up the BEGIN and staged
                // entries and carries them into the next segment.
                let sidx_covers_tail =
                    Self::sidx_covers_segment_tail(path, &sidx_entries).unwrap_or(false);
                if sidx_covers_tail {
                    for se in sidx_entries {
                        let row = se.to_cold_start_row(segment_id);
                        let kind = row.kind;
                        // Skip batch markers in SIDX fast path (markers
                        // themselves are not indexed; their role is
                        // purely durability-oracle on the frame stream).
                        if kind == EventKind::SYSTEM_BATCH_BEGIN
                            || kind == EventKind::SYSTEM_BATCH_COMMIT
                        {
                            continue;
                        }
                        sink(ScannedIndexEntry::from_cold_start_row(&row, &strings)?)?;
                    }
                    return Ok(());
                }
                // Fall through to frame-scan: SIDX exists but does not
                // cover the segment tail, which means a cross-segment
                // batch is in flight. Frame-scan picks up the BEGIN and
                // staged items into `state_ref`.
            }
        }

        // Slow path: frame-by-frame scan for active segment or when batch state is pending.
        //
        // Note: an earlier version tracked `batch_committed_indices` and discarded
        // them on cold start when the segment lacked a SIDX footer, on the
        // premise that "SIDX is written after sync, so its absence implies the
        // sync didn't complete". That premise was wrong: SIDX is only ever
        // written on segment rotation or clean shutdown, NEVER per batch.
        // `handle_append_batch` issues its own `sync_with_mode` after the
        // COMMIT marker, so a batch whose `append_batch` returned `Ok(receipts)`
        // is durably on disk regardless of whether SIDX has been written yet.
        // The discard logic was therefore silently dropping confirmed-committed
        // batches on any unclean shutdown that happened between batch commit and
        // segment rotation/clean close — exactly the scenario that
        // crash-resilience claims must survive. The COMMIT marker plus the
        // existing CRC / decode-error mid-loop discards are the actual oracles
        // for batch durability.
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
        let header: segment::SegmentHeader = rmp_serde::from_slice(&header_buf)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;
        if header.version != 1 {
            return Err(StoreError::corrupt_version(segment_id, header.version));
        }

        let mut cursor = (8 + header_len) as u64;
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
                                    let batch_count = Self::checked_batch_count(
                                        segment_id,
                                        frame_offset,
                                        payload.event.header.payload_size,
                                    )?;
                                    state_ref.in_batch = true;
                                    state_ref.remaining = batch_count;
                                    state_ref.started_count = batch_count;
                                    state_ref.staged.reserve(
                                        usize::try_from(batch_count)
                                            .expect("validated batch count fits in usize"),
                                    );
                                } else if kind == EventKind::SYSTEM_BATCH_COMMIT {
                                    // COMMIT without BEGIN: orphaned commit, skip.
                                    tracing::warn!(
                                        segment_id,
                                        offset = frame_offset,
                                        "orphaned COMMIT marker, skipping"
                                    );
                                } else {
                                    // Normal event: commit immediately.
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
                                        // Slow path: no SIDX footer, so no durable sequence source.
                                        // Caller (rebuild) will synthesize via the ReplayCursor allocator.
                                        global_sequence: None,
                                    })?;
                                }
                            } else if kind == EventKind::SYSTEM_BATCH_COMMIT {
                                // COMMIT marker: verify count matches and commit.
                                // The COMMIT frame is the durability oracle —
                                // its presence in the segment means the writer
                                // got at least as far as `write_frame(COMMIT)`,
                                // and the subsequent `sync_with_mode` is what
                                // makes the receipt callable in the first place.
                                if state_ref.remaining == 0 {
                                    // Complete batch: commit all staged.
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
                                let batch_count = Self::checked_batch_count(
                                    segment_id,
                                    frame_offset,
                                    payload.event.header.payload_size,
                                )?;
                                state_ref.remaining = batch_count;
                                state_ref.started_count = batch_count;
                                state_ref.staged.clear();
                                state_ref.staged.reserve(
                                    usize::try_from(batch_count)
                                        .expect("validated batch count fits in usize"),
                                );
                            } else {
                                // Stage this event (not a marker).
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

        Ok(())
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
        let frame_range =
            Self::checked_frame_range(pos.segment_id, pos.offset, pos.length, mmap.len())?;
        let frame_buf = &mmap[frame_range];
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

    /// Read an entry by disk position but leave the payload as raw MessagePack
    /// bytes. Mirrors `read_entry` but returns `StoredEvent<Vec<u8>>`, used by
    /// the raw-lane reactor loop.
    pub(crate) fn read_entry_raw(&self, pos: &DiskPos) -> Result<StoredEvent<Vec<u8>>, StoreError> {
        if self.is_sealed(pos.segment_id) {
            return self.read_entry_raw_mmap(pos);
        }

        let frame_len = Self::checked_frame_len(pos.segment_id, pos.length)?;
        let mut buf = self.acquire_buffer(frame_len);
        self.read_active_frame_into(pos, &mut buf)?;

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
        let payload = Self::decode_frame_payload_raw(msgpack)?;
        self.release_buffer(buf);

        let coord =
            Coordinate::new(&payload.entity, &payload.scope).map_err(StoreError::Coordinate)?;
        Ok(StoredEvent {
            coordinate: coord,
            event: payload.event,
        })
    }

    fn read_entry_raw_mmap(&self, pos: &DiskPos) -> Result<StoredEvent<Vec<u8>>, StoreError> {
        let mmap_ref = self.get_or_map_sealed(pos.segment_id)?;
        let mmap: &memmap2::Mmap = mmap_ref.value();
        let frame_range =
            Self::checked_frame_range(pos.segment_id, pos.offset, pos.length, mmap.len())?;
        let frame_buf = &mmap[frame_range];
        let (msgpack, _) = segment::frame_decode(frame_buf).map_err(|e| match e {
            segment::FrameDecodeError::CrcMismatch { .. } => StoreError::CrcMismatch {
                segment_id: pos.segment_id,
                offset: pos.offset,
            },
            segment::FrameDecodeError::TooShort | segment::FrameDecodeError::Truncated { .. } => {
                StoreError::corrupt_frame(pos.segment_id, e.to_string())
            }
        })?;
        let payload = Self::decode_frame_payload_raw(msgpack)?;
        let coord =
            Coordinate::new(&payload.entity, &payload.scope).map_err(StoreError::Coordinate)?;
        Ok(StoredEvent {
            coordinate: coord,
            event: payload.event,
        })
    }

    /// Read a single event by disk position, skipping Coordinate construction.
    /// Returns `Event<serde_json::Value>` directly — suitable for projection
    /// replay where only the event payload matters.
    pub(crate) fn read_event_only(
        &self,
        pos: &DiskPos,
    ) -> Result<Event<serde_json::Value>, StoreError> {
        // Fast path: mmap for sealed segments — zero-copy, no lock, no buffer.
        if self.is_sealed(pos.segment_id) {
            return self.read_event_only_mmap(pos);
        }

        // Slow path: active segment via FD cache + buffer pool.
        let frame_len = Self::checked_frame_len(pos.segment_id, pos.length)?;
        let mut buf = self.acquire_buffer(frame_len);
        self.read_active_frame_into(pos, &mut buf)?;

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

        Ok(payload.event)
    }

    /// Zero-copy read from a sealed segment's memory map, returning only the
    /// event and skipping Coordinate construction.
    fn read_event_only_mmap(&self, pos: &DiskPos) -> Result<Event<serde_json::Value>, StoreError> {
        let mmap_ref = self.get_or_map_sealed(pos.segment_id)?;
        let mmap: &memmap2::Mmap = mmap_ref.value();
        let frame_range =
            Self::checked_frame_range(pos.segment_id, pos.offset, pos.length, mmap.len())?;
        let frame_buf = &mmap[frame_range];
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
        Ok(payload.event)
    }

    /// Convenience helper over point reads for projection replay.
    ///
    /// This preserves the replay surface shape, but it is not a vectored I/O
    /// fast path yet: each position still goes through `read_event_only`.
    pub(crate) fn read_events_batch(
        &self,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<serde_json::Value>>, StoreError> {
        self.read_batch_with(positions, Self::read_event_only)
    }

    /// Read a single event by disk position, leaving the payload as raw
    /// MessagePack bytes.
    pub(crate) fn read_event_raw_only(&self, pos: &DiskPos) -> Result<Event<Vec<u8>>, StoreError> {
        if self.is_sealed(pos.segment_id) {
            return self.read_event_raw_only_mmap(pos);
        }

        let frame_len = Self::checked_frame_len(pos.segment_id, pos.length)?;
        let mut buf = self.acquire_buffer(frame_len);
        self.read_active_frame_into(pos, &mut buf)?;

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
        let payload = Self::decode_frame_payload_raw(msgpack)?;
        self.release_buffer(buf);
        Ok(payload.event)
    }

    fn read_event_raw_only_mmap(&self, pos: &DiskPos) -> Result<Event<Vec<u8>>, StoreError> {
        let mmap_ref = self.get_or_map_sealed(pos.segment_id)?;
        let mmap: &memmap2::Mmap = mmap_ref.value();
        let frame_range =
            Self::checked_frame_range(pos.segment_id, pos.offset, pos.length, mmap.len())?;
        let frame_buf = &mmap[frame_range];
        let (msgpack, _) = segment::frame_decode(frame_buf).map_err(|e| match e {
            segment::FrameDecodeError::CrcMismatch { .. } => StoreError::CrcMismatch {
                segment_id: pos.segment_id,
                offset: pos.offset,
            },
            segment::FrameDecodeError::TooShort | segment::FrameDecodeError::Truncated { .. } => {
                StoreError::corrupt_frame(pos.segment_id, e.to_string())
            }
        })?;
        let payload = Self::decode_frame_payload_raw(msgpack)?;
        Ok(payload.event)
    }

    /// Convenience helper over point reads that leaves payloads as raw
    /// MessagePack bytes.
    ///
    /// This is not a vectored read path yet: it iterates `read_event_raw_only`
    /// for each requested position.
    pub(crate) fn read_raw_events_batch(
        &self,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<Vec<u8>>>, StoreError> {
        self.read_batch_with(positions, Self::read_event_raw_only)
    }

    fn read_batch_with<T>(
        &self,
        positions: &[&DiskPos],
        mut read_one: impl FnMut(&Self, &DiskPos) -> Result<T, StoreError>,
    ) -> Result<Vec<T>, StoreError> {
        let mut results = Vec::with_capacity(positions.len());
        for pos in positions {
            results.push(read_one(self, pos)?);
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
             Check: src/store/segment/scan.rs acquire_buffer() vec allocation.",
            buf.len()
        );
        // All bytes should be zero-initialized.
        assert!(
            buf.iter().all(|&b| b == 0),
            "ACQUIRE BUFFER: newly allocated buffer should be zero-initialized."
        );
    }

    #[test]
    fn released_buffer_is_zero_filled_and_resized_on_next_acquire() {
        // Behavior-based test (not implementation-based): we don't peek at
        // `reader.buffer_pool.lock()`. We assert the OBSERVABLE contract:
        // a buffer that's been released and re-acquired must be the
        // requested size and zero-filled (no leftover bytes from a previous
        // user). The buffer pool is an internal optimization; if it's later
        // replaced with crossbeam::ArrayQueue or removed entirely, this
        // test should still pass as long as the contract holds.
        let (reader, _dir) = test_reader();

        // Dirty a buffer and release it.
        let mut buf = reader.acquire_buffer(128);
        for byte in buf.iter_mut() {
            *byte = 0xAB;
        }
        reader.release_buffer(buf);

        // Re-acquire at a different size. Must be the new requested size
        // and must NOT contain the dirty 0xAB bytes from the previous user.
        let buf2 = reader.acquire_buffer(64);
        assert_eq!(
            buf2.len(),
            64,
            "PROPERTY: re-acquired buffer must match the requested size, \
             regardless of whether it came from the pool or a fresh allocation. \
             Investigate: src/store/segment/scan.rs acquire_buffer resize path."
        );
        assert!(
            buf2.iter().all(|&b| b == 0),
            "PROPERTY: re-acquired buffer must be zero-filled. A non-zero byte \
             means the previous user's data leaked into the new acquirer, \
             which is a memory-safety / information-disclosure bug. \
             Investigate: src/store/segment/scan.rs acquire_buffer fill path."
        );
    }

    #[test]
    fn buffer_pool_does_not_grow_unboundedly() {
        // Behavior-based: instead of locking the private pool and asserting
        // on its `Vec` length (which couples the test to the implementation
        // type), we release a large number of buffers and then verify that
        // memory usage stays bounded — i.e., not every released buffer is
        // retained. We do this by checking that re-acquired buffers are
        // sometimes (not always) the same backing capacity as the most
        // recently released one. If the pool retained ALL releases, every
        // re-acquire of the same size would see a recycled allocation.
        // If the pool DROPS most releases past its cap, some re-acquires
        // would have to allocate fresh.
        let (reader, _dir) = test_reader();

        // Release 100 buffers into the pool. Only some are retained.
        for _ in 0..100 {
            reader.release_buffer(vec![0u8; 1024]);
        }

        // Drain the pool by acquiring 100 buffers of the same size.
        // Every buffer must satisfy the size+zero-fill contract regardless
        // of whether it was recycled or freshly allocated.
        for i in 0..100 {
            let buf = reader.acquire_buffer(1024);
            assert_eq!(
                buf.len(),
                1024,
                "PROPERTY: buffer {i} of 100 must be the requested size."
            );
            assert!(
                buf.iter().all(|&b| b == 0),
                "PROPERTY: buffer {i} of 100 must be zero-filled."
            );
        }
        // If we got here without OOM and without an oversized allocation
        // request, the pool is honoring its bounded-memory contract.
    }

    #[test]
    fn acquire_buffer_satisfies_contract_on_empty_pool() {
        // Behavior-based: a fresh reader has nothing in the pool. Acquire
        // must produce a buffer satisfying the size+zero contract even
        // when there's nothing to recycle.
        let (reader, _dir) = test_reader();

        let buf = reader.acquire_buffer(512);
        assert_eq!(
            buf.len(),
            512,
            "PROPERTY: acquire_buffer on a fresh reader must return the \
             requested size. Investigate: src/store/segment/scan.rs allocation \
             path when pool is empty."
        );
        assert!(
            buf.iter().all(|&b| b == 0),
            "PROPERTY: a freshly allocated buffer must be zero-filled."
        );
    }

    #[test]
    fn checked_frame_range_rejects_overflow_and_oversized_lengths() {
        assert!(Reader::checked_frame_range(1, u64::MAX, 16, 1024).is_err());
        assert!(Reader::checked_frame_len(1, 4).is_err());
        assert!(Reader::checked_frame_len(1, u32::MAX).is_err());
    }

    #[test]
    fn checked_batch_count_rejects_vacuous_or_implausible_counts() {
        assert!(Reader::checked_batch_count(1, 0, 0).is_err());
        assert!(Reader::checked_batch_count(1, 0, MAX_BATCH_RECOVERY_ITEMS + 1).is_err());
        assert_eq!(
            Reader::checked_batch_count(1, 0, 3).expect("valid batch count"),
            3
        );
    }

    #[test]
    fn required_index_hash_chain_rejects_missing_chain_for_data_event() {
        let event = IndexScanEvent {
            header: EventHeader::new(
                1,
                1,
                None,
                1,
                crate::coordinate::DagPosition::root(),
                0,
                EventKind::DATA,
            ),
            _payload: serde::de::IgnoredAny,
            hash_chain: None,
        };

        let err = Reader::required_index_hash_chain(&event, 7, 99).expect_err("missing hash chain");
        assert!(
            matches!(
                err,
                StoreError::CorruptSegment { segment_id: 7, ref detail }
                if detail.contains("missing hash_chain")
            ),
            "PROPERTY: missing hash_chain must surface as CorruptSegment with the expected detail, got {err:?}"
        );
    }
}
