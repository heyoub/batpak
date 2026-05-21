use super::Reader;
use crate::coordinate::Coordinate;
use crate::event::{Event, StoredEvent};
use crate::store::index::DiskPos;
use crate::store::segment;
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use std::collections::BTreeMap;
use std::fs::File;

impl Reader {
    pub(super) fn read_active_frame_into(
        &self,
        pos: &DiskPos,
        buf: &mut [u8],
    ) -> Result<(), StoreError> {
        let segment_id = pos.segment_id;
        let offset = pos.offset;
        self.with_fd(segment_id, |f| {
            crate::store::platform::fs::read_exact_at(f, offset, buf).map_err(|error| match error {
                crate::store::platform::fs::PositionedReadError::Io(error) => StoreError::Io(error),
                crate::store::platform::fs::PositionedReadError::ShortRead { bytes_read } => {
                    if bytes_read == 0 {
                        StoreError::corrupt_eof(segment_id)
                    } else {
                        StoreError::corrupt_segment_with_detail(
                            segment_id,
                            "active frame read ended before requested length",
                        )
                    }
                }
            })
        })
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
            let file = crate::store::platform::fs::open_file(&path).map_err(StoreError::Io)?;
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
}
