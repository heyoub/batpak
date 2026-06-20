pub(crate) mod id;
pub(crate) mod recovery_manifest;
pub(crate) mod scan;
pub(crate) mod sidx;

#[cfg(test)]
mod boundary_tests;

#[cfg(test)]
mod manifest_recovery_tests;

#[cfg(test)]
mod mod_tests;

pub(crate) use id::SegmentId;
pub(crate) use recovery_manifest::resolve_untrusted_frames_end;
// Corroboration internals are surfaced to the inline manifest-recovery test
// island only; production code reaches them through `resolve_untrusted_frames_end`.
#[cfg(test)]
pub(crate) use recovery_manifest::{
    corroborate_untrusted_entries, RecoveredFrame, RecoveredFrameMap, UntrustedRecovery,
};

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
    /// Filesystem backend through which this segment's create + fsync durability
    /// boundary is routed. Production is [`RealFs`]; a deterministic-simulation
    /// backend ([`SimFs`]) interposes here so it can drop fsyncs and truncate the
    /// segment file to its last durable length on a simulated crash.
    ///
    /// [`RealFs`]: crate::store::platform::fs::RealFs
    /// [`SimFs`]: crate::store::sim::fs::SimFs
    fs: std::sync::Arc<dyn crate::store::platform::fs::StoreFs>,
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
    /// Create new active segment over the production [`RealFs`] filesystem.
    ///
    /// Public convenience over [`Segment::create_with_created_ns_on`]: it pins the
    /// production [`RealFs`] backend so the public signature does not expose the
    /// crate-private [`StoreFs`] seam. The store's writer/lifecycle paths call the
    /// fs-bearing variant directly so a deterministic-simulation backend can
    /// interpose on the create + fsync durability boundary.
    ///
    /// [`RealFs`]: crate::store::platform::fs::RealFs
    /// [`StoreFs`]: crate::store::platform::fs::StoreFs
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the segment file cannot be created or the header cannot be written.
    /// Returns `StoreError::Serialization` if the segment header cannot be serialized.
    pub fn create_with_created_ns(
        dir: &std::path::Path,
        segment_id: u64,
        created_ns: i64,
    ) -> Result<Self, StoreError> {
        let fs: std::sync::Arc<dyn crate::store::platform::fs::StoreFs> =
            std::sync::Arc::new(crate::store::platform::fs::RealFs);
        Self::create_with_created_ns_on(dir, segment_id, created_ns, &fs)
    }

    /// Create new active segment over the supplied [`StoreFs`] backend.
    ///
    /// The fs-bearing internal variant: the create + initial fsync are routed
    /// through `fs`, so a simulation backend records this segment's durable length
    /// (and may drop the fsync) at the durability boundary. Production callers
    /// pass `config.fs()`.
    ///
    /// [`StoreFs`]: crate::store::platform::fs::StoreFs
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the segment file cannot be created or the header cannot be written.
    /// Returns `StoreError::Serialization` if the segment header cannot be serialized.
    pub(crate) fn create_with_created_ns_on(
        dir: &std::path::Path,
        segment_id: u64,
        created_ns: i64,
        fs: &std::sync::Arc<dyn crate::store::platform::fs::StoreFs>,
    ) -> Result<Self, StoreError> {
        let path = dir.join(segment_filename(segment_id));
        let mut file = fs.create_new_file(&path)?;

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
        // platform/fs.rs. Routed through `fs` so a sim backend records the
        // create's durable length (and can drop the fsync under its schedule).
        fs.sync_file_all(&file, &path).map_err(StoreError::Io)?;
        fs.sync_parent_dir(&path)?;

        Ok(Self {
            header,
            path,
            file: Some(file),
            written_bytes: (4 + 4 + header_bytes.len()) as u64, // magic + len + header
            fs: std::sync::Arc::clone(fs),
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
        // So the hint is discarded entirely and recovery routes through the
        // SIDX-manifest path (`resolve_untrusted_frames_end`): it walks the
        // CRC-valid frames bounded only by `file_len` (truncation-proof, still
        // FailClosed on mid-stream corruption), AND corroborates the
        // CRC-independent SIDX entry table against the recovered frames. If a
        // corroborated entry attests to a committed frame at/after the recovered
        // prefix end that the source is missing (torn last committed frame under a
        // corrupt footer — round-7), the copy FailCloses instead of silently
        // merging a segment that dropped a committed event. Compaction copy of a
        // sealed source is strict, so its fall-back posture is FailClosed; with no
        // corroborated manifest it still recovers the CRC-valid prefix.
        let copy_end = if untrusted_boundary {
            resolve_untrusted_frames_end(&mut source, frames_start, file_len, 0, true)?
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
            // Routed through `fs` so a sim backend records this segment's durable
            // length at the sync boundary (and may drop it under its schedule).
            self.fs.sync_file_with_mode(f, &self.path, mode)?;
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
            fs: self.fs,
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

    // Trust is determined FIRST, before any validation of the offset value.
    //
    // GOVERNING PRINCIPLE: an unauthenticated SIDX trailer offset must NEVER cause
    // a hard failure OR data loss — it always degrades to CRC-valid-frame recovery.
    // So the offset's *value* may be validated (and turned into a hard error) only
    // once it is known TRUSTED. An UNTRUSTED offset is FULLY INERT: never validated
    // as an error, never used as a boundary; callers discard `frames_end` for an
    // untrusted boundary and recover via `crc_valid_frames_end` (bounded only by
    // `file_len`).
    //
    // Only a CRC-valid SDX3 footer authenticates the offset. A CRC-fail SDX3
    // footer, a legacy SDX2 footer (no CRC), a torn/truncated footer, or a
    // no-footer segment whose last bytes coincidentally equal the magic all yield
    // `Ok(None)` -> UNTRUSTED. The CRC check is bounded (chunked hashing in
    // `read_layout`). An authenticated offset must equal the trailer offset read
    // above, else it is not trusted.
    let trusted =
        crate::store::segment::sidx::authenticated_string_table_offset(source, segment_id)?
            == Some(string_table_offset);

    // Upper-bound check — TRUSTED offsets ONLY. The string table cannot start
    // inside/past the 16-byte trailer. `file_len - 16` cannot underflow (checked
    // `file_len >= TRAILER_LEN` above); `offset == file_len - 16` (empty table)
    // stays valid. The lower bound is validated per call site. Gated on `trusted`
    // so it can NEVER hard-error an untrusted footer: a CRC-authenticated offset
    // is consistent by construction (read_layout already proved it <= entries_start
    // <= file_len - 16), so this never fires for a trusted offset, while a garbage
    // untrusted offset of ANY shape (0, mid-frame, file_len, > file_len-16, huge,
    // torn) downgrades to CRC-valid-frame recovery, not CorruptSegment (round-6 P1).
    if trusted {
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
    }

    Ok(Some(SidxBoundary {
        frames_end: string_table_offset,
        trusted,
    }))
}

/// Attempt to decode a single CRC-valid frame starting at byte `at` in `source`,
/// reading no farther than `file_len`. Returns the consumed frame size on a clean
/// decode, or `None` if no real frame begins exactly at `at` (truncated header, a
/// zero or over-large `claimed_len`, a tail past `file_len`, a short read, or a
/// CRC/decode failure). A returned `Some` means the bytes at `at` are a genuine
/// frame: the `claimed_len` is in `1..=MAX_FRAME_PAYLOAD` AND the CRC32 over the
/// payload matches, which makes a false positive astronomically unlikely.
///
/// A zero `claimed_len` is rejected on purpose: `frame_encode` always serializes a
/// non-empty `FramePayload` (it carries at least `event`/`entity`/`scope`), so a
/// real frame's payload is never empty. Admitting a zero-length frame would also
/// let any run of 8 zero bytes (common inside the SIDX footer's entry table —
/// e.g. `prev_hash: [0; 32]`) read as a CRC-valid empty frame (the CRC32 of an
/// empty payload is 0), which is the one systematic false-positive the resync
/// look-ahead must not trip on.
///
/// # Errors
/// Returns [`StoreError::Io`] only on a seek failure; a short/EOF read is treated
/// as "no frame here" (`Ok(None)`), not an error.
pub(super) fn try_decode_frame_at<R: Read + Seek>(
    source: &mut R,
    at: u64,
    file_len: u64,
) -> Result<Option<u64>, StoreError> {
    // Need at least an 8-byte frame header before the source end.
    if file_len.saturating_sub(at) < 8 {
        return Ok(None);
    }
    source.seek(SeekFrom::Start(at)).map_err(StoreError::Io)?;
    let mut header = [0u8; 8];
    if source.read_exact(&mut header).is_err() {
        return Ok(None);
    }
    let claimed_len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
    if claimed_len == 0 || claimed_len > MAX_FRAME_PAYLOAD as u64 {
        return Ok(None);
    }
    let frame_tail = match at.checked_add(8).and_then(|b| b.checked_add(claimed_len)) {
        Some(tail) => tail,
        None => return Ok(None),
    };
    if frame_tail > file_len {
        // A real frame cannot extend past the end of the source.
        return Ok(None);
    }
    let payload_len = usize::try_from(claimed_len).unwrap_or(usize::MAX);
    let mut frame = vec![0u8; 8 + payload_len];
    frame[..8].copy_from_slice(&header);
    if source.read_exact(&mut frame[8..]).is_err() {
        return Ok(None);
    }
    match frame_decode(&frame) {
        Ok((_, frame_size)) => Ok(Some(frame_size as u64)),
        Err(_) => Ok(None),
    }
}

/// Look-ahead resync: starting at byte `from`, scan toward `file_len` for ANY
/// CRC-valid frame. Returns `true` if one is found.
///
/// This is the integrity check that distinguishes "the bytes at the first
/// non-decodable position are footer/torn-tail" (nothing valid follows → `false`)
/// from "the bytes at that position are MID-STREAM corruption" (a CRC-valid frame
/// still follows → `true`). A CRC-valid frame after the corruption proves the
/// corruption is interior, not the true end of the frame region, so recovery must
/// fail closed instead of silently truncating to the prefix.
///
/// Cost: the scan only covers the tail region after the last CRC-valid frame.
/// In the common (benign) case that tail is just the footer — a handful of bytes
/// — so the byte-by-byte resync is cheap and bounded by `file_len`. Alignment is
/// NOT assumed: a CRC-valid frame may begin at any byte, so every offset from
/// `from` to `file_len - 8` is probed. Correctness (never miss a real later
/// frame) is worth more than skipping offsets here. The 256 MiB
/// `MAX_FRAME_PAYLOAD` claim-cap plus the CRC make a false-positive resync onto
/// random bytes astronomically unlikely, so a `true` result is a real frame.
///
/// # Errors
/// Returns [`StoreError::Io`] on a seek failure inside the probe.
pub(super) fn crc_valid_frame_exists_after<R: Read + Seek>(
    source: &mut R,
    from: u64,
    file_len: u64,
) -> Result<bool, StoreError> {
    let mut probe = from;
    while file_len.saturating_sub(probe) >= 8 {
        if try_decode_frame_at(source, probe, file_len)?.is_some() {
            return Ok(true);
        }
        probe += 1;
    }
    Ok(false)
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
/// recovery. So the hint is discarded entirely and the ONLY bound is `file_len`.
///
/// This walks the frame region from `frames_start`, decoding frames until one
/// fails at some position P (truncated, an over-large `claimed_len`, a
/// `frame_decode`/CRC error, or a tail past `file_len`). P is the FIRST
/// non-decodable position. The remaining question is whether P is the true end
/// of the frame region (footer/torn-tail bytes follow) or MID-STREAM corruption
/// (a corrupt frame with valid frames still after it):
///
/// - **P is the true end** iff NOTHING decodable as a CRC-valid frame exists
///   between P and `file_len`. Footer bytes, the string table, the trailer, and a
///   genuinely torn last frame never resync to a CRC-valid frame, so the prefix
///   `[frames_start..P]` is exactly the durable frame region → return `Ok(P)`.
///
/// - **P is mid-stream corruption** iff a CRC-valid frame still EXISTS after P
///   (before `file_len`). Silently returning `P` here would drop the corrupt
///   frame AND every later CRC-valid event — converting interior corruption into
///   a clean EOF, which a trusted/no-footer scan would FailClosed on. So this
///   FailCloses too: return `Err(CorruptSegment)`. The look-ahead resync
///   (`crc_valid_frame_exists_after`) is what decides this; CRC makes a
///   false-positive resync astronomically unlikely.
///
/// This recovers ALL CRC-valid frames in the benign cases (availability) without
/// ever converting interior corruption into silent truncation (integrity), and
/// composes with torn-tail handling: a genuinely incomplete LAST frame has
/// nothing valid after it, so it falls in the "true end" branch and the prefix
/// recovers.
///
/// This must ONLY be used for an untrusted boundary. For a CRC-authenticated
/// SDX3 footer the offset is authoritative and a bad frame before it is genuine
/// mid-stream corruption that must still FailClosed (see the scan loops).
///
/// # Errors
/// Returns [`StoreError::Io`] on seek/read failure, or
/// [`StoreError::CorruptSegment`] when a CRC-valid frame is found after the first
/// non-decodable position (mid-stream corruption).
///
/// This is the prefix-only primitive: it returns just the recovery stop offset P.
/// The production untrusted path uses [`crc_valid_frames_end_with_map`] (which
/// additionally builds the recovered-frame map `R` for SIDX-manifest
/// corroboration); this thin wrapper preserves the original signature for the
/// round-5/6 unit tests that pin the mid-stream-corruption / torn-tail behavior
/// directly, and is the single source of truth for that walk.
//
// Production code reaches the walk via `crc_valid_frames_end_with_map`; this
// prefix-only wrapper is exercised only by the boundary unit tests, so it is
// gated `#[cfg(test)]` (no dead code in the production lib).
#[cfg(test)]
pub(crate) fn crc_valid_frames_end<R: Read + Seek>(
    source: &mut R,
    frames_start: u64,
    file_len: u64,
    segment_id: u64,
) -> Result<u64, StoreError> {
    let (stop, _recovered) = recovery_manifest::crc_valid_frames_end_with_map(
        source,
        frames_start,
        file_len,
        segment_id,
    )?;
    Ok(stop)
}
