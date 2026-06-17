pub(crate) mod id;
pub(crate) mod scan;
pub(crate) mod sidx;

#[cfg(test)]
mod boundary_tests;

pub(crate) use id::SegmentId;

use crate::event::Event;
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
// serde(with) resolves via string path — no explicit wire import needed.

/// Segment file format: magic(4) + header_len(4 BE) + header(msgpack) + frames
/// Frame: \[len:u32 BE\]\[crc32:u32 BE\]\[msgpack\]
/// Files named: {segment_id:06}.fbat. Sequential u64.
pub const SEGMENT_MAGIC: &[u8; 4] = b"FBAT";
/// File extension used for all segment files (without the leading dot).
pub const SEGMENT_EXTENSION: &str = "fbat";

/// Maximum allowed frame payload size in bytes. Frames claiming a payload
/// larger than this are rejected as corrupt before allocation, preventing
/// a malicious or corrupt segment file from causing unbounded memory use.
pub(crate) const MAX_FRAME_PAYLOAD: usize = 256 * 1024 * 1024;

/// Maximum allowed segment header size in bytes. The real header is a fixed
/// four-field msgpack struct (~30 bytes), so a 64 KiB cap rejects only
/// impossible inputs. A segment claiming a larger `header_len` is rejected as
/// corrupt before allocation, preventing a malicious or corrupt segment file
/// from driving an unbounded header buffer allocation. Mirrors MAX_FRAME_PAYLOAD.
pub(crate) const MAX_SEGMENT_HEADER: usize = 64 * 1024;

/// Segment file header, serialized as MessagePack after the magic bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentHeader {
    /// Segment format version number.
    pub version: u16,
    /// Reserved flags field; currently always 0.
    pub flags: u16,
    /// Nanoseconds since Unix epoch when this segment was created.
    pub created_ns: i64,
    /// Numeric identifier of this segment file.
    pub segment_id: u64,
}

/// FramePayload: what gets serialized into each frame.
/// entity and scope are stored as strings (not Coordinate) because segments
/// are the persistence layer — they don't depend on the Coordinate type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FramePayload<P> {
    /// The event data stored in this frame.
    pub event: Event<P>,
    /// Entity name string (e.g. `"entity:42"`).
    pub entity: String,
    /// Scope name string (e.g. `"profile"`).
    pub scope: String,
    /// Opaque receipt extension bytes committed with this frame.
    #[serde(default)]
    pub receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

#[derive(Serialize)]
pub(crate) struct FramePayloadRef<'a, P> {
    pub event: &'a Event<P>,
    pub entity: &'a str,
    pub scope: &'a str,
    pub receipt_extensions: &'a BTreeMap<ExtensionKey, EncodedBytes>,
}

/// Typestate marker for an active (writable) segment.
pub struct Active;
/// Typestate marker for a sealed (read-only) segment.
pub struct Sealed;
/// A segment file handle parameterized by its lifecycle state (`Active` or `Sealed`).
pub struct Segment<State> {
    /// Parsed header of this segment file.
    pub header: SegmentHeader,
    /// Filesystem path to the segment file.
    pub path: std::path::PathBuf,
    file: Option<std::fs::File>,
    written_bytes: u64,
    _state: std::marker::PhantomData<State>,
}

/// Outcome of a compaction run: whether work happened, was skipped because
/// the sealed-segment count was below the configured threshold, or failed
/// inside the swap-point protocol. A `Failed` result guarantees the live
/// index was NOT mutated — the F6 / FREEZE-4 contract routes rebuild
/// failures here so callers can distinguish "did nothing" from "tried and
/// failed" from "did compact" without clobbering the reader-visible state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum CompactionOutcome {
    /// Compaction merged and replaced sealed segments.
    Performed,
    /// Compaction was a no-op: sealed-segment count was below `min_segments`.
    Skipped,
    /// The compact-swap protocol aborted before the swap point; the live
    /// index is unchanged. `reason` describes which step failed (off-side
    /// rebuild error, disk-side scan error, etc.). See
    /// `src/store/lifecycle.rs::compact` and
    /// `StoreIndex::replace_contents_from_fresh` for the swap invariants.
    Failed {
        /// Human-readable description of which swap-point step aborted.
        reason: String,
    },
}

/// Result returned by a compaction run.
#[derive(Debug)]
pub struct CompactionResult {
    /// Whether the compaction actually ran, was skipped, or failed.
    pub outcome: CompactionOutcome,
    /// Number of sealed segment files that were merged and removed. Always
    /// `0` for [`CompactionOutcome::Skipped`] and
    /// [`CompactionOutcome::Failed`].
    pub segments_removed: usize,
    /// Total bytes freed by removing the merged segment files. Always `0`
    /// for [`CompactionOutcome::Skipped`] and
    /// [`CompactionOutcome::Failed`].
    pub bytes_reclaimed: u64,
}

/// frame_encode: serialize data to msgpack, wrap in \[len:u32 BE\]\[crc32:u32 BE\]\[msgpack\]
/// Segment payloads are always encoded with the canonical named-field MessagePack helper.
/// \[DEP:crate::encoding::to_bytes\] -> `Result<Vec<u8>, encode::Error>`
/// \[DEP:crc32fast::hash\] → u32
///
/// # Errors
/// Returns `StoreError::Serialization` if the data cannot be serialized to MessagePack.
pub fn frame_encode<T: serde::Serialize>(data: &T) -> Result<Vec<u8>, StoreError> {
    let msgpack =
        crate::encoding::to_bytes(data).map_err(|e| StoreError::Serialization(Box::new(e)))?;
    let crc = crc32fast::hash(&msgpack);
    let len = u32::try_from(msgpack.len()).map_err(|_| StoreError::ser_msg("frame exceeds 4GB"))?;

    let mut frame = Vec::with_capacity(8 + msgpack.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&crc.to_be_bytes());
    frame.extend_from_slice(&msgpack);
    Ok(frame)
}

/// Error from frame_decode. Does not include segment_id — the caller
/// wraps this with the correct segment context.
#[derive(Debug)]
#[non_exhaustive]
pub enum FrameDecodeError {
    /// The buffer is shorter than the minimum 8-byte frame header.
    TooShort,
    /// The buffer ends before the full frame payload is available.
    Truncated {
        /// Total bytes expected for the complete frame (header + payload).
        expected_len: usize,
        /// Bytes actually available in the buffer.
        available: usize,
    },
    /// The CRC32 checksum in the frame header did not match the payload.
    CrcMismatch {
        /// CRC value stored in the frame header.
        expected: u32,
        /// CRC value computed from the actual payload bytes.
        actual: u32,
    },
}

impl std::fmt::Display for FrameDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "frame too short for header"),
            Self::Truncated {
                expected_len,
                available,
            } => {
                write!(
                    f,
                    "frame truncated: expected {expected_len} bytes, got {available}"
                )
            }
            Self::CrcMismatch { expected, actual } => {
                write!(
                    f,
                    "CRC mismatch: expected {expected:#010x}, got {actual:#010x}"
                )
            }
        }
    }
}

impl std::error::Error for FrameDecodeError {}

/// frame_decode: read \[len\]\[crc\]\[msgpack\], verify CRC, return msgpack bytes.
/// Returns (msgpack_bytes, total_frame_size_consumed).
///
/// # Errors
/// Returns `FrameDecodeError::TooShort` if the buffer is under 8 bytes.
/// Returns `FrameDecodeError::Truncated` if the buffer ends before the full frame payload.
/// Returns `FrameDecodeError::CrcMismatch` if the checksum does not match the payload.
pub fn frame_decode(buf: &[u8]) -> Result<(&[u8], usize), FrameDecodeError> {
    if buf.len() < 8 {
        return Err(FrameDecodeError::TooShort);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let expected_crc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

    // A6: explicit bounds check before slicing. `8 + len` can overflow
    // `usize` on 32-bit targets (where usize is 32-bit and a u32 payload
    // length can approach usize::MAX); the checked_add guards against
    // that case so we never index past the end of the buffer.
    let expected_len = 8usize.checked_add(len).ok_or(FrameDecodeError::Truncated {
        expected_len: usize::MAX,
        available: buf.len(),
    })?;
    if buf.len() < expected_len {
        return Err(FrameDecodeError::Truncated {
            expected_len,
            available: buf.len(),
        });
    }
    let msgpack = &buf[8..expected_len];
    let actual_crc = crc32fast::hash(msgpack);
    if actual_crc != expected_crc {
        return Err(FrameDecodeError::CrcMismatch {
            expected: expected_crc,
            actual: actual_crc,
        });
    }
    Ok((msgpack, expected_len))
}

/// Segment naming helper.
pub fn segment_filename(segment_id: u64) -> String {
    format!("{:06}.{}", segment_id, SEGMENT_EXTENSION)
}

impl Segment<Active> {
    /// Create new active segment.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the segment file cannot be created or the header cannot be written.
    /// Returns `StoreError::Serialization` if the segment header cannot be serialized.
    pub fn create_with_created_ns(
        dir: &std::path::Path,
        segment_id: u64,
        created_ns: i64,
    ) -> Result<Self, StoreError> {
        let path = dir.join(segment_filename(segment_id));
        let mut file = crate::store::platform::fs::create_new_file(&path)?;

        let header = SegmentHeader {
            version: 1,
            flags: 0,
            created_ns,
            segment_id,
        };

        // Write magic + header_len(u32 BE) + header(msgpack)
        file.write_all(SEGMENT_MAGIC).map_err(StoreError::Io)?;
        let header_bytes = crate::encoding::to_bytes(&header)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;
        let header_len = u32::try_from(header_bytes.len())
            .map_err(|_| StoreError::ser_msg("segment header length exceeds u32::MAX"))?
            .to_be_bytes();
        file.write_all(&header_len).map_err(StoreError::Io)?;
        file.write_all(&header_bytes).map_err(StoreError::Io)?;

        // Durability boundary for segment creation/rotation: fsync the file
        // content, THEN the parent directory entry. File-then-dir ordering
        // ensures the header bytes are durable before the directory entry that
        // points at the inode is durable, so a power loss immediately after a
        // rotation cannot lose the freshly-created segment's directory entry.
        // Mirrors the write_file_atomically file-then-dir precedent in
        // platform/fs.rs.
        crate::store::platform::sync::sync_file_all_io(&file).map_err(StoreError::Io)?;
        crate::store::platform::sync::sync_parent_dir(&path)?;

        Ok(Self {
            header,
            path,
            file: Some(file),
            written_bytes: (4 + 4 + header_bytes.len()) as u64, // magic + len + header
            _state: std::marker::PhantomData,
        })
    }

    /// Write a frame. Returns offset where frame starts.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if writing to the segment file fails.
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<u64, StoreError> {
        let offset = self.written_bytes;
        if let Some(ref mut f) = self.file {
            f.write_all(frame).map_err(StoreError::Io)?;
        }
        self.written_bytes += frame.len() as u64;
        Ok(offset)
    }

    /// Append all frame bytes from an existing segment file, skipping that file's header.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the source file cannot be read or frames cannot be written.
    /// Returns `StoreError::Corrupt` if the source file does not begin with the expected magic bytes.
    pub fn append_frames_from_segment(
        &mut self,
        path: &std::path::Path,
    ) -> Result<u64, StoreError> {
        let mut source = crate::store::platform::fs::open_file(path).map_err(StoreError::Io)?;
        let mut magic = [0u8; 4];
        source.read_exact(&mut magic).map_err(StoreError::Io)?;
        if &magic != SEGMENT_MAGIC {
            return Err(StoreError::corrupt_magic(0));
        }

        let mut header_len_buf = [0u8; 4];
        source
            .read_exact(&mut header_len_buf)
            .map_err(StoreError::Io)?;
        let header_len = u32::from_be_bytes(header_len_buf) as u64;
        let frames_start = 8 + header_len;

        // Determine where frames end: if the segment has a SIDX footer,
        // the frames stop at string_table_offset. Otherwise, frames extend
        // to the end of the file.
        let file_len = source.seek(SeekFrom::End(0)).map_err(StoreError::Io)?;
        // segment_id is only used to stamp a CorruptSegment error on a bad SIDX
        // offset; this copy path mirrors corrupt_magic(0) above and has no parsed id.
        let boundary = detect_sidx_boundary(&mut source, file_len, 0)?;
        let frames_end = boundary.map_or(file_len, |b| b.frames_end);
        // Only an actual footer whose offset is unauthenticated triggers the
        // recover-what-was-found copy. With no footer, frames run to EOF and the
        // strict path applies (validate is a no-op when frames_end == file_len).
        let untrusted_boundary = boundary.is_some_and(|b| !b.trusted);

        // Lower-bound check (TRUSTED boundaries only; mirrors scan/recovery.rs +
        // full_scan.rs): an authenticated SDX3 string_table_offset must not fall
        // below the start of the frame region. detect_sidx_boundary only validates
        // the upper bound; the lower bound is the call site's responsibility. A
        // corrupt-but-authenticated offset < frames_start would make
        // `frames_end.saturating_sub(frames_start)` copy zero bytes (or only a
        // prefix), and after compaction publishes the merged segment and cleans up
        // the old sealed files, those CRC-valid frames would be silently lost.
        // Reject with CorruptSegment instead. frames_end == frames_start (empty
        // frame region) stays valid. For an UNTRUSTED boundary the offset is
        // garbage and discarded below (the copy walks from `frames_start` bounded
        // by `file_len`), so a too-low untrusted hint must NOT error — it recovers
        // all CRC-valid frames instead.
        if !untrusted_boundary && frames_end < frames_start {
            return Err(StoreError::corrupt_segment_with_detail(
                0,
                format!(
                    "SIDX string_table_offset {frames_end} is below the frame region start \
                     {frames_start} (8 + header_len {header_len}) during compaction copy"
                ),
            ));
        }

        // Determine the true copy boundary based on the offset's provenance.
        //
        // TRUSTED (CRC-valid SDX3 footer): the offset is authoritative; copy up to
        // it. A truncating offset cannot occur here — the offset is byte-for-byte
        // authenticated by the footer CRC.
        //
        // UNTRUSTED (CRC-failed SDX3, legacy SDX2, or forged trailer): the offset
        // is GARBAGE. It might point too LOW (truncating real frames), MID-FRAME
        // (inside a later CRC-valid frame), or too HIGH (into the corrupt footer);
        // any of those, if trusted as the copy boundary, would either drop
        // CRC-valid frames or splice corrupt footer bytes into the merged segment.
        // So the hint is discarded entirely: walk the CRC-valid frames bounded only
        // by `file_len` and copy exactly the span that decodes cleanly. This
        // recovers ALL CRC-valid frames (the walk is truncation-proof), so no
        // separate truncation guard is needed for the untrusted path.
        let copy_end = if untrusted_boundary {
            crc_valid_frames_end(&mut source, frames_start, file_len)?
        } else {
            frames_end
        };

        source
            .seek(SeekFrom::Start(frames_start))
            .map_err(StoreError::Io)?;

        let offset = self.written_bytes;
        if let Some(ref mut destination) = self.file {
            let bytes_to_copy = copy_end.saturating_sub(frames_start);
            let copied = std::io::copy(&mut source.take(bytes_to_copy), destination)
                .map_err(StoreError::Io)?;
            self.written_bytes += copied;
        }
        Ok(offset)
    }

    /// Returns `true` if the segment has reached or exceeded `max_bytes` and should be rotated.
    pub(crate) fn needs_rotation(&self, max_bytes: u64) -> bool {
        self.written_bytes >= max_bytes
    }

    /// Flush the segment file to durable storage using the specified sync mode.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the OS-level sync call fails.
    pub fn sync_with_mode(&mut self, mode: &crate::store::SyncMode) -> Result<(), StoreError> {
        if let Some(ref f) = self.file {
            crate::store::platform::sync::sync_file_with_mode(f, mode)?;
        }
        Ok(())
    }

    /// Write a SIDX footer to the end of this segment before sealing.
    /// The footer enables fast cold-start index rebuild by storing compact
    /// binary entries instead of requiring full msgpack frame deserialization.
    ///
    /// # Errors
    /// Returns `StoreError::Io`, `StoreError::Serialization`, or
    /// `StoreError::SegmentTooManyEntries` if writing fails.
    pub(crate) fn write_sidx_footer(
        &mut self,
        collector: &crate::store::segment::sidx::SidxEntryCollector,
    ) -> Result<(), StoreError> {
        if let Some(ref mut f) = self.file {
            collector.write_footer(f, self.header.segment_id)?;
        }
        Ok(())
    }

    /// Seal: close file handle, transition to Sealed.
    pub fn seal(mut self) -> Segment<Sealed> {
        drop(self.file.take());
        Segment {
            header: self.header,
            path: self.path,
            file: None,
            written_bytes: self.written_bytes,
            _state: std::marker::PhantomData,
        }
    }
}

/// A SIDX frame-region boundary together with its trust provenance.
///
/// `frames_end` is the trailer's `string_table_offset` — where the frame region
/// is claimed to end. `trusted` records whether that offset came from a
/// CRC-authenticated SDX3 footer:
///
/// - `trusted == true`: the offset is byte-for-byte covered by the SDX3 footer
///   CRC, so it is authoritative. A frame-decode failure *before* this boundary
///   is genuine mid-stream corruption and must FailClosed.
/// - `trusted == false`: the offset is an UNAUTHENTICATED hint — a corrupt SDX3
///   footer (CRC failed), a legacy un-CRC'd SDX2 footer, or a forged trailer.
///   The recovery scan must rebuild from the CRC-valid frames it actually finds
///   and treat over-reading into the (corrupt) footer as the clean end of real
///   frames, NOT a hard error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SidxBoundary {
    pub(crate) frames_end: u64,
    pub(crate) trusted: bool,
}

/// Check whether a segment file ends with a SIDX footer.
///
/// If so, return the [`SidxBoundary`]: the byte offset where the string table
/// starts (= end of frames) plus whether that offset is authenticated by the
/// SDX3 footer CRC. If not, return `None` (frames extend to EOF).
///
/// The trailer geometry (offset/count/magic) is read identically for SDX2 and
/// SDX3, so the *boundary* is detected for both — but only a CRC-valid SDX3
/// footer marks the boundary `trusted`. SDX2 footers and CRC-failed SDX3
/// footers are recognized as boundaries (so the scan knows where the footer
/// region begins) yet flagged untrusted, so the caller does not treat the
/// unauthenticated offset as an authoritative `frames_end`.
pub(crate) fn detect_sidx_boundary<R: Read + Seek>(
    source: &mut R,
    file_len: u64,
    segment_id: u64,
) -> Result<Option<SidxBoundary>, StoreError> {
    // SIDX trailer is the last 16 bytes: [string_table_offset:u64 LE][entry_count:u32 LE][magic:4]
    const TRAILER_LEN: u64 = 16;
    if file_len < TRAILER_LEN {
        return Ok(None);
    }
    source
        .seek(SeekFrom::End(-(TRAILER_LEN as i64)))
        .map_err(StoreError::Io)?;
    let mut trailer = [0u8; 16];
    source.read_exact(&mut trailer).map_err(StoreError::Io)?;

    // Check magic at bytes 12..16. Recognize BOTH the current `SDX3` magic and
    // the legacy pre-0.8.3 `SDX2` magic as a footer boundary marker. This is a
    // boundary-only check: it tells the frame scan where the frame region ends so
    // it stops at `string_table_offset` instead of over-running into the footer's
    // string table + entries + trailer bytes. It does NOT make the footer's
    // content trusted — `sidx::read_footer` / `footer::read_layout` still match
    // only `SIDX_MAGIC` (SDX3) and reject an un-CRC'd SDX2 footer as `Ok(None)`,
    // forcing the CRC-verified frame-scan rebuild. The trailer geometry
    // (offset/count/magic) is byte-identical across SDX2/SDX3, so the
    // `string_table_offset` field is read the same way for both.
    let magic = &trailer[12..16];
    if magic != crate::store::segment::sidx::SIDX_MAGIC
        && magic != crate::store::segment::sidx::SIDX_MAGIC_LEGACY_SDX2
    {
        return Ok(None);
    }
    // string_table_offset at bytes 0..8
    let string_table_offset = u64::from_le_bytes([
        trailer[0], trailer[1], trailer[2], trailer[3], trailer[4], trailer[5], trailer[6],
        trailer[7],
    ]);

    // Upper-bound check: the string table (and therefore the end of the frame
    // region) cannot start inside or past the 16-byte trailer. An offset past
    // file_len - 16 would over-run the scan into the trailer. file_len - 16
    // cannot underflow because file_len >= TRAILER_LEN was checked above.
    // offset == file_len - 16 (empty string table + zero entries) stays valid,
    // matching read_layout's boundary semantics. The lower bound (offset >=
    // 8 + header_len) is validated at each call site, where header_len is known.
    let max_offset = file_len - TRAILER_LEN;
    if string_table_offset > max_offset {
        return Err(StoreError::corrupt_segment_with_detail(
            segment_id,
            format!(
                "SIDX string_table_offset {string_table_offset} is past the frame region \
                 (max {max_offset} = file_len {file_len} - 16-byte trailer)"
            ),
        ));
    }

    // Trust provenance: re-run the SDX3 footer CRC verification over the same
    // reader. Only a CRC-valid SDX3 footer authenticates the offset; a CRC-fail
    // SDX3 footer or a legacy SDX2 footer (no CRC) yields `Ok(None)` here, so the
    // boundary is recognized but flagged untrusted. The CRC check is bounded
    // (chunked hashing in `read_layout`) so a forged footer cannot drive an
    // unbounded allocation here. An authenticated offset must equal the trailer
    // offset we read above — if `read_layout`'s reconstructed offset disagrees
    // with the raw trailer, do not trust it.
    let trusted =
        crate::store::segment::sidx::authenticated_string_table_offset(source, segment_id)?
            == Some(string_table_offset);

    Ok(Some(SidxBoundary {
        frames_end: string_table_offset,
        trusted,
    }))
}

/// Find the true end of CRC-valid frames when the SIDX boundary is UNTRUSTED.
///
/// An untrusted boundary (CRC-failed SDX3 footer, legacy SDX2 footer, or a
/// forged trailer) gives an unauthenticated `string_table_offset`. That offset
/// is GARBAGE and must NEVER bound recovery — whether it is too LOW (truncating
/// real frames), MID-FRAME (landing inside a later CRC-valid frame's
/// header/payload), or too HIGH (pointing into the corrupt footer region).
/// Trusting it as an upper bound silently drops every CRC-valid frame at or
/// after the bogus offset; trusting it as a lower bound makes the scan parse
/// footer bytes as frame headers and FailClosed. Either way the hint corrupts
/// recovery.
///
/// So the hint is discarded entirely. This walks the frame region from
/// `frames_start` to the NATURAL end of CRC-valid frames: it keeps decoding
/// frames until one fails — truncated, an over-large `claimed_len`, a
/// `frame_decode` error, or one that would read past `file_len` — and returns
/// the cursor at that first failure. The ONLY bound is `file_len` (the real
/// source/file length). Footer bytes after the real frames never decode as a
/// CRC-valid frame, so the scan stops naturally at the true frame-region end no
/// matter where the garbage hint pointed. This recovers ALL CRC-valid frames
/// (availability) without admitting any non-CRC-valid data (integrity).
///
/// This must ONLY be used for an untrusted boundary. For a CRC-authenticated
/// SDX3 footer the offset is authoritative and a bad frame before it is genuine
/// mid-stream corruption that must still FailClosed (see the scan loops).
///
/// # Errors
/// Returns [`StoreError::Io`] on seek/read failure.
pub(crate) fn crc_valid_frames_end<R: Read + Seek>(
    source: &mut R,
    frames_start: u64,
    file_len: u64,
) -> Result<u64, StoreError> {
    let mut cursor = frames_start;
    source
        .seek(SeekFrom::Start(frames_start))
        .map_err(StoreError::Io)?;

    loop {
        if cursor >= file_len {
            // Walked every byte of the source as CRC-valid frames (no footer, or
            // the frames run to EOF): the real frames end at the source end.
            return Ok(file_len);
        }
        // Need at least an 8-byte frame header before the source end.
        if file_len.saturating_sub(cursor) < 8 {
            return Ok(cursor);
        }
        let mut header = [0u8; 8];
        if source.read_exact(&mut header).is_err() {
            return Ok(cursor);
        }
        let claimed_len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        if claimed_len > MAX_FRAME_PAYLOAD as u64 {
            // Over-large payload claim: not a real frame, clean end here.
            return Ok(cursor);
        }
        let frame_tail = match cursor
            .checked_add(8)
            .and_then(|b| b.checked_add(claimed_len))
        {
            Some(tail) => tail,
            None => return Ok(cursor),
        };
        if frame_tail > file_len {
            // A real frame cannot extend past the end of the source.
            return Ok(cursor);
        }
        let payload_len = usize::try_from(claimed_len).unwrap_or(usize::MAX);
        let mut frame = vec![0u8; 8 + payload_len];
        frame[..8].copy_from_slice(&header);
        if source.read_exact(&mut frame[8..]).is_err() {
            return Ok(cursor);
        }
        match frame_decode(&frame) {
            Ok((_, frame_size)) => {
                cursor = match cursor.checked_add(frame_size as u64) {
                    Some(next) => next,
                    None => return Ok(cursor),
                };
            }
            Err(_) => {
                // First non-CRC-valid frame (footer bytes or genuine corruption)
                // is the clean end of real frames.
                return Ok(cursor);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn needs_rotation_tracks_written_bytes_threshold() {
        let dir = TempDir::new().expect("tmpdir");
        let mut segment: Segment<Active> =
            Segment::create_with_created_ns(dir.path(), 1, 0).expect("create segment");
        let frame = frame_encode(&serde_json::json!({"payload": "rotation-threshold"}))
            .expect("encode frame");

        assert!(
            !segment.needs_rotation(1024),
            "PROPERTY: a fresh segment must not report rotation before any frames are written"
        );

        segment.write_frame(&frame).expect("write frame");

        assert!(
            segment.needs_rotation(1),
            "PROPERTY: needs_rotation(max_bytes=1) must flip true after any real frame write"
        );
        assert!(
            !segment.needs_rotation(1024),
            "PROPERTY: needs_rotation must stay false below the threshold"
        );
    }

    #[test]
    fn create_with_created_ns_fsyncs_content_and_directory_entry() {
        let dir = TempDir::new().expect("tmpdir");
        let segment_id = 42u64;
        let created_ns = 1_234_567i64;
        {
            // Drop the segment immediately after create so only fsynced bytes
            // remain; nothing else writes to or flushes the file.
            let _segment: Segment<Active> =
                Segment::create_with_created_ns(dir.path(), segment_id, created_ns)
                    .expect("create segment");
        }

        // The directory entry must be present (dir fsync) and the header bytes
        // durable (file fsync): reopen and round-trip magic + header. The reopen
        // succeeding is itself the directory-entry-visibility proof — open_file
        // fails if the freshly-created entry is not visible — so no separate
        // read_dir scan is needed (and store-layer code must not touch the
        // filesystem directly outside src/store/platform).
        let path = dir.path().join(segment_filename(segment_id));
        let mut file = crate::store::platform::fs::open_file(&path).expect("reopen segment");
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).expect("read magic");
        assert_eq!(&magic, SEGMENT_MAGIC, "PROPERTY: magic must be durable");

        let mut header_len_buf = [0u8; 4];
        file.read_exact(&mut header_len_buf)
            .expect("read header_len");
        let header_len = u32::from_be_bytes(header_len_buf) as usize;
        let mut header_buf = vec![0u8; header_len];
        file.read_exact(&mut header_buf).expect("read header");
        let header: SegmentHeader =
            crate::encoding::from_bytes(&header_buf).expect("decode header");

        assert_eq!(
            header.segment_id, segment_id,
            "PROPERTY: segment_id must round-trip after create + reopen, proving content is fsynced"
        );
        assert_eq!(header.version, 1, "PROPERTY: version must round-trip");
        assert_eq!(
            header.created_ns, created_ns,
            "PROPERTY: created_ns must round-trip"
        );
    }

    /// Build a minimal in-memory buffer whose last 16 bytes are a SIDX trailer
    /// with the given `string_table_offset`, valid magic, and zero entry_count.
    fn sidx_trailer_buf(total_len: usize, string_table_offset: u64) -> Vec<u8> {
        assert!(total_len >= 16, "buffer must hold the 16-byte trailer");
        let mut bytes = vec![0u8; total_len];
        let trailer_start = total_len - 16;
        bytes[trailer_start..trailer_start + 8].copy_from_slice(&string_table_offset.to_le_bytes());
        bytes[trailer_start + 8..trailer_start + 12].copy_from_slice(&0u32.to_le_bytes());
        bytes[trailer_start + 12..trailer_start + 16]
            .copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC);
        bytes
    }

    #[test]
    fn detect_sidx_boundary_rejects_offset_past_trailer() {
        let bytes = sidx_trailer_buf(64, 63); // offset past file_len - 16 = 48
        let file_len = bytes.len() as u64;
        let mut cursor = std::io::Cursor::new(bytes);
        let result = detect_sidx_boundary(&mut cursor, file_len, 7);
        assert!(
            matches!(result, Err(StoreError::CorruptSegment { segment_id: 7, .. })),
            "PROPERTY: a string_table_offset past file_len - 16 must surface CorruptSegment, not silently over-run; got {result:?}"
        );
    }

    #[test]
    fn detect_sidx_boundary_accepts_offset_at_max() {
        let file_len = 64u64;
        let max_offset = file_len - 16; // empty string table boundary, valid
        let bytes = sidx_trailer_buf(
            usize::try_from(file_len).expect("file_len fits usize"),
            max_offset,
        );
        let mut cursor = std::io::Cursor::new(bytes);
        let result = detect_sidx_boundary(&mut cursor, file_len, 7);
        assert_eq!(
            result
                .expect("must not error at the max boundary")
                .map(|b| b.frames_end),
            Some(max_offset),
            "PROPERTY: offset == file_len - 16 is the empty-string-table boundary and must be accepted"
        );
    }

    #[test]
    fn detect_sidx_boundary_recognizes_legacy_sdx2_magic() {
        // A pre-0.8.3 segment ends in the legacy `SDX2` magic with the same
        // 16-byte trailer geometry. detect_sidx_boundary must recognize it as a
        // footer BOUNDARY (so the frame scan stops at string_table_offset)
        // even though read_footer refuses to TRUST its un-CRC'd content.
        let file_len = 64u64;
        let max_offset = file_len - 16;
        let mut bytes = vec![0u8; usize::try_from(file_len).expect("file_len fits usize")];
        let trailer_start = bytes.len() - 16;
        bytes[trailer_start..trailer_start + 8].copy_from_slice(&max_offset.to_le_bytes());
        bytes[trailer_start + 8..trailer_start + 12].copy_from_slice(&0u32.to_le_bytes());
        bytes[trailer_start + 12..trailer_start + 16]
            .copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC_LEGACY_SDX2);
        let mut cursor = std::io::Cursor::new(bytes);
        let result = detect_sidx_boundary(&mut cursor, file_len, 7).expect("must not error");
        assert_eq!(
            result,
            Some(SidxBoundary {
                frames_end: max_offset,
                // A legacy SDX2 footer carries no CRC, so its offset is recognized
                // as a boundary but is NEVER trusted.
                trusted: false,
            }),
            "PROPERTY: a legacy SDX2 trailer must be recognized as a frame-region boundary"
        );
        assert!(
            !result.expect("boundary present").trusted,
            "PROPERTY: an un-CRC'd SDX2 boundary must be flagged untrusted"
        );
    }

    #[test]
    fn detect_sidx_boundary_no_magic_returns_none() {
        // A tail without the SIDX magic must read as "no footer", not error.
        let bytes = vec![0u8; 64];
        let file_len = bytes.len() as u64;
        let mut cursor = std::io::Cursor::new(bytes);
        let result = detect_sidx_boundary(&mut cursor, file_len, 7).expect("must not error");
        assert_eq!(result, None, "PROPERTY: absent SIDX magic must return None");
    }

    #[test]
    fn detect_sidx_boundary_tiny_file_returns_none() {
        let bytes = vec![0xAA; 8]; // < 16-byte trailer
        let file_len = bytes.len() as u64;
        let mut cursor = std::io::Cursor::new(bytes);
        let result = detect_sidx_boundary(&mut cursor, file_len, 7).expect("must not error");
        assert_eq!(
            result, None,
            "PROPERTY: a file smaller than the trailer must return None"
        );
    }
}
