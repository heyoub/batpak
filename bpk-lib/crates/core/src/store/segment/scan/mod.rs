mod full_scan;
mod point_read;
mod recovery;
mod validate;

use crate::event::{Event, EventHeader, EventKind, HashChain};
use crate::store::cold_start::ColdStartIndexRow;
use crate::store::index::DiskPos;
use crate::store::segment::{self, FramePayload};
use crate::store::{Clock, EncodedBytes, ExtensionKey, StoreError};
use dashmap::DashMap;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Error, ErrorKind, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const FRAME_HEADER_BYTES: usize = 8;
const MAX_BATCH_RECOVERY_ITEMS: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameScanTailPolicy {
    FailClosed,
    RecoverTornTail,
}

impl FrameScanTailPolicy {
    fn can_recover_torn_tail(self) -> bool {
        matches!(self, Self::RecoverTornTail)
    }
}

fn read_frame_header_or_clean_eof(
    reader: &mut impl Read,
) -> Result<Option<[u8; FRAME_HEADER_BYTES]>, Error> {
    let mut frame_header = [0u8; FRAME_HEADER_BYTES];
    match reader.read_exact(&mut frame_header) {
        Ok(()) => Ok(Some(frame_header)),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

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
    clock: Arc<dyn Clock>,
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
    pub receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

pub(crate) struct ScannedIndexEntry {
    pub header: EventHeader,
    pub entity: String,
    pub scope: String,
    pub hash_chain: HashChain,
    pub segment_id: u64,
    pub offset: u64,
    pub length: u32,
    pub receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
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
            receipt_extensions: BTreeMap::new(),
            global_sequence: Some(row.global_sequence),
        })
    }
}

pub(crate) use recovery::{BatchRecoveryState, IndexScanEvent};

impl Reader {
    fn decode_frame_payload_raw(msgpack: &[u8]) -> Result<FramePayload<Vec<u8>>, StoreError> {
        crate::encoding::from_bytes(msgpack).map_err(|e| StoreError::Serialization(Box::new(e)))
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
            _ => crate::encoding::from_bytes(&event.payload)
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
            receipt_extensions: payload.receipt_extensions,
        })
    }

    fn frame_decode_error(
        segment_id: u64,
        offset: u64,
        error: segment::FrameDecodeError,
    ) -> StoreError {
        match error {
            segment::FrameDecodeError::CrcMismatch { .. } => {
                StoreError::CrcMismatch { segment_id, offset }
            }
            error @ (segment::FrameDecodeError::TooShort
            | segment::FrameDecodeError::Truncated { .. }) => StoreError::CorruptSegment {
                segment_id,
                detail: format!(
                    "frame at offset {offset} is corrupt after full payload read: {error}"
                ),
            },
        }
    }

    fn frame_decode_error_for_pos(pos: &DiskPos, error: segment::FrameDecodeError) -> StoreError {
        Self::frame_decode_error(pos.segment_id, pos.offset, error)
    }

    pub(crate) fn new(data_dir: PathBuf, fd_budget: usize, clock: Arc<dyn Clock>) -> Self {
        Self {
            data_dir,
            clock,
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
        let file = crate::store::platform::fs::open_file(&path).map_err(StoreError::Io)?;
        // SAFETY: memmap2::Mmap::map is unsafe because the file could be modified externally.
        // Sealed segments are immutable by design — only compaction deletes them, and
        // evict_segment drops the mapping before deletion.
        let evidence =
            crate::store::platform::evidence::collect_for_store_path(&self.data_dir, &*self.clock);
        let admission = crate::store::platform::mmap::admit_sealed_segment_mmap(
            evidence.store_path.sealed_segment_mmap,
        )?;
        let mmap =
            unsafe { crate::store::platform::mmap::map_sealed_segment_file(&file, admission) }
                .map_err(StoreError::Io)?;
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
mod tests;
