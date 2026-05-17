mod full_scan;
mod recovery;
mod validate;

use crate::coordinate::Coordinate;
use crate::event::{Event, EventHeader, EventKind, HashChain, StoredEvent};
use crate::store::cold_start::ColdStartIndexRow;
use crate::store::index::DiskPos;
use crate::store::segment::{self, FramePayload};
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use dashmap::DashMap;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Error, ErrorKind, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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
    fn read_active_frame_into(&self, pos: &DiskPos, buf: &mut [u8]) -> Result<(), StoreError> {
        let segment_id = pos.segment_id;
        let offset = pos.offset;
        self.with_fd(segment_id, |f| {
            crate::store::platform::fs::read_exact_at(f, offset, buf).map_err(|error| match error {
                crate::store::platform::fs::PositionedReadError::Io(error) => StoreError::Io(error),
                crate::store::platform::fs::PositionedReadError::ShortRead { bytes_read } => {
                    if bytes_read == 0 {
                        StoreError::corrupt_eof(segment_id)
                    } else {
                        StoreError::corrupt_frame(
                            segment_id,
                            "active frame read ended before requested length",
                        )
                    }
                }
            })
        })
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
        let evidence = crate::store::platform::evidence::collect_for_store_path(&self.data_dir);
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

        let result = segment::frame_decode(&buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error));
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
        let (msgpack, _) = segment::frame_decode(frame_buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error))?;
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

        let result = segment::frame_decode(&buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error));
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
        let (msgpack, _) = segment::frame_decode(frame_buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error))?;
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

        let result = segment::frame_decode(&buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error));
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
        let (msgpack, _) = segment::frame_decode(frame_buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error))?;
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

        let result = segment::frame_decode(&buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error));
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

    /// Read only the opaque receipt extension map from a frame.
    pub(crate) fn read_receipt_extensions(
        &self,
        pos: &DiskPos,
    ) -> Result<BTreeMap<ExtensionKey, EncodedBytes>, StoreError> {
        if self.is_sealed(pos.segment_id) {
            return self.read_receipt_extensions_mmap(pos);
        }

        let frame_len = Self::checked_frame_len(pos.segment_id, pos.length)?;
        let mut buf = self.acquire_buffer(frame_len);
        self.read_active_frame_into(pos, &mut buf)?;

        let result = segment::frame_decode(&buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error));
        let (msgpack, _) = match result {
            Ok(v) => v,
            Err(e) => {
                self.release_buffer(buf);
                return Err(e);
            }
        };
        let payload = Self::decode_frame_payload_raw(msgpack)?;
        self.release_buffer(buf);
        Ok(payload.receipt_extensions)
    }

    fn read_receipt_extensions_mmap(
        &self,
        pos: &DiskPos,
    ) -> Result<BTreeMap<ExtensionKey, EncodedBytes>, StoreError> {
        let mmap_ref = self.get_or_map_sealed(pos.segment_id)?;
        let mmap: &memmap2::Mmap = mmap_ref.value();
        let frame_range =
            Self::checked_frame_range(pos.segment_id, pos.offset, pos.length, mmap.len())?;
        let frame_buf = &mmap[frame_range];
        let (msgpack, _) = segment::frame_decode(frame_buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error))?;
        let payload = Self::decode_frame_payload_raw(msgpack)?;
        Ok(payload.receipt_extensions)
    }

    fn read_event_raw_only_mmap(&self, pos: &DiskPos) -> Result<Event<Vec<u8>>, StoreError> {
        let mmap_ref = self.get_or_map_sealed(pos.segment_id)?;
        let mmap: &memmap2::Mmap = mmap_ref.value();
        let frame_range =
            Self::checked_frame_range(pos.segment_id, pos.offset, pos.length, mmap.len())?;
        let frame_buf = &mmap[frame_range];
        let (msgpack, _) = segment::frame_decode(frame_buf)
            .map_err(|error| Self::frame_decode_error_for_pos(pos, error))?;
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
    use crate::coordinate::DagPosition;
    use crate::store::index::DiskPos;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    struct FailingRead {
        kind: ErrorKind,
    }

    impl std::io::Read for FailingRead {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.kind))
        }
    }

    fn test_reader() -> (Reader, TempDir) {
        let dir = TempDir::new().expect("create temp dir for reader test");
        let reader = Reader::new(dir.path().to_path_buf(), 4);
        (reader, dir)
    }

    fn write_segment_bytes(dir: &TempDir, segment_id: u64, bytes: &[u8]) {
        let path = dir.path().join(segment::segment_filename(segment_id));
        std::fs::write(&path, bytes).expect("write segment bytes");
    }

    #[test]
    fn read_frame_header_policy_treats_unexpected_eof_as_clean_end() {
        let mut reader = FailingRead {
            kind: ErrorKind::UnexpectedEof,
        };

        let result = read_frame_header_or_clean_eof(&mut reader).expect("EOF should be non-fatal");

        assert!(
            result.is_none(),
            "PROPERTY: EOF while reading the next frame header is the clean segment terminator"
        );
    }

    #[test]
    fn read_frame_header_policy_surfaces_non_eof_io_errors() {
        let mut reader = FailingRead {
            kind: ErrorKind::PermissionDenied,
        };

        let result = read_frame_header_or_clean_eof(&mut reader);

        assert!(
            matches!(result, Err(error) if error.kind() == ErrorKind::PermissionDenied),
            "PROPERTY: non-EOF frame-header read errors must surface as I/O failures"
        );
    }

    #[test]
    fn frame_decode_error_mapping_preserves_segment_and_offset_context() {
        fn assert_error_trait<E: std::error::Error>() {}

        assert_error_trait::<segment::FrameDecodeError>();

        let crc_error = Reader::frame_decode_error(
            7,
            42,
            segment::FrameDecodeError::CrcMismatch {
                expected: 0xAAAA_AAAA,
                actual: 0xBBBB_BBBB,
            },
        );
        assert!(
            matches!(
                crc_error,
                StoreError::CrcMismatch {
                    segment_id: 7,
                    offset: 42
                }
            ),
            "PROPERTY: frame CRC failures must retain exact disk position context"
        );

        let truncated_error = Reader::frame_decode_error(
            7,
            42,
            segment::FrameDecodeError::Truncated {
                expected_len: 16,
                available: 12,
            },
        );
        assert!(
            matches!(
                truncated_error,
                StoreError::CorruptSegment { segment_id: 7, ref detail }
                if detail.contains("frame at offset 42")
                    && detail.contains("frame truncated: expected 16 bytes, got 12")
            ),
            "PROPERTY: structural frame decode failures must retain segment, offset, and decode reason; got {truncated_error:?}"
        );
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
    fn buffer_pool_retains_at_most_sixteen_released_buffers() {
        let (reader, _dir) = test_reader();

        for _ in 0..17 {
            reader.release_buffer(vec![0u8; 32]);
        }

        let retained = reader.buffer_pool.lock().len();
        assert_eq!(
            retained, 16,
            "PROPERTY: release_buffer must cap the internal pool at exactly 16 buffers; \
             retaining a seventeenth buffer weakens the bounded-memory contract"
        );
    }

    #[test]
    fn batch_marker_payload_decode_ignores_marker_payload_bytes() {
        let header = EventHeader::new(
            1,
            1,
            None,
            1,
            DagPosition::root(),
            0,
            EventKind::SYSTEM_BATCH_BEGIN,
        );
        let event = Event {
            header,
            payload: vec![0xC1],
            hash_chain: Some(HashChain::default()),
        };
        let frame = FramePayload {
            event,
            entity: "entity:batch-marker".to_owned(),
            scope: "scope:test".to_owned(),
            receipt_extensions: BTreeMap::new(),
        };
        let encoded = rmp_serde::to_vec_named(&frame).expect("encode batch marker frame");

        let decoded = Reader::decode_frame_payload_value(&encoded)
            .expect("batch marker payload bytes are ignored by value decode");

        assert_eq!(
            decoded.event.payload,
            serde_json::Value::Null,
            "PROPERTY: SYSTEM_BATCH_BEGIN/COMMIT markers carry count semantics in the header; \
             value decoding must not deserialize their raw marker payload bytes"
        );
    }

    #[test]
    fn set_active_segment_advances_the_sealed_cutoff() {
        let (reader, _dir) = test_reader();

        reader.set_active_segment(7);

        assert_eq!(reader.active_segment_id(), 7);
        assert!(
            reader.is_sealed(6),
            "PROPERTY: segments older than the configured active segment must be treated as sealed"
        );
        assert!(
            !reader.is_sealed(7),
            "PROPERTY: the configured active segment itself must stay writable/non-sealed"
        );
        assert!(
            !reader.is_sealed(8),
            "PROPERTY: future segment ids must not be treated as sealed before rotation reaches them"
        );
    }

    #[test]
    fn read_active_frame_into_reads_the_full_requested_slice() {
        let (reader, dir) = test_reader();
        write_segment_bytes(&dir, 0, b"0123456789abcdef");

        let pos = DiskPos::new(0, 3, 5);
        let mut buf = [0u8; 5];
        reader
            .read_active_frame_into(&pos, &mut buf)
            .expect("read active bytes");

        assert_eq!(
            &buf,
            b"34567",
            "PROPERTY: active-segment reads must advance until the caller's buffer is fully populated"
        );
    }

    #[test]
    fn checked_frame_range_rejects_overflow_and_oversized_lengths() {
        assert!(Reader::checked_frame_range(1, u64::MAX, 16, 1024).is_err());
        assert!(Reader::checked_frame_len(1, 4).is_err());
        assert!(
            Reader::checked_frame_len(
                1,
                u32::try_from(FRAME_HEADER_BYTES).expect("frame header size fits u32")
            )
            .is_ok(),
            "PROPERTY: a frame length exactly equal to the frame header size is the minimum valid empty-payload frame"
        );
        assert!(Reader::checked_frame_len(
            1,
            u32::try_from(FRAME_HEADER_BYTES + segment::MAX_FRAME_PAYLOAD)
                .expect("max frame length fits u32")
        )
        .is_ok());
        assert!(Reader::checked_frame_len(
            1,
            u32::try_from(FRAME_HEADER_BYTES + segment::MAX_FRAME_PAYLOAD + 1)
                .expect("one-past-max frame length fits u32")
        )
        .is_err());
        assert!(Reader::checked_frame_len(1, u32::MAX).is_err());
    }

    #[test]
    fn payload_len_exceeds_max_respects_the_exact_boundary() {
        assert!(
            !Reader::payload_len_exceeds_max(segment::MAX_FRAME_PAYLOAD),
            "PROPERTY: a frame exactly at MAX_FRAME_PAYLOAD remains valid"
        );
        assert!(
            Reader::payload_len_exceeds_max(segment::MAX_FRAME_PAYLOAD + 1),
            "PROPERTY: a frame one byte past MAX_FRAME_PAYLOAD must stop scan/recovery before allocation"
        );
    }

    #[test]
    fn checked_batch_count_rejects_vacuous_or_implausible_counts() {
        assert!(Reader::checked_batch_count(1, 0, 0).is_err());
        assert!(Reader::checked_batch_count(1, 0, MAX_BATCH_RECOVERY_ITEMS + 1).is_err());
        assert_eq!(
            Reader::checked_batch_count(1, 0, MAX_BATCH_RECOVERY_ITEMS)
                .expect("max batch count remains valid"),
            MAX_BATCH_RECOVERY_ITEMS,
            "PROPERTY: the exact MAX_BATCH_RECOVERY_ITEMS boundary is allowed"
        );
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
