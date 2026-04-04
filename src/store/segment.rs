use crate::event::Event;
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
// serde(with) resolves via string path — no explicit wire import needed.

/// Segment file format: magic(4) + header_len(4 BE) + header(msgpack) + frames
/// Frame: \[len:u32 BE\]\[crc32:u32 BE\]\[msgpack\]
/// Files named: {segment_id:06}.fbat. Sequential u64.
/// [SPEC:src/store/segment.rs]
pub const SEGMENT_MAGIC: &[u8; 4] = b"FBAT";
/// File extension used for all segment files (without the leading dot).
pub const SEGMENT_EXTENSION: &str = "fbat";

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
pub struct FramePayload<P> {
    /// The event data stored in this frame.
    pub event: Event<P>,
    /// Entity name string (e.g. `"user:42"`).
    pub entity: String,
    /// Scope name string (e.g. `"profile"`).
    pub scope: String,
}

#[derive(Serialize)]
pub(crate) struct FramePayloadRef<'a, P> {
    pub event: &'a Event<P>,
    pub entity: &'a str,
    pub scope: &'a str,
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

/// Result returned by a compaction run.
#[derive(Debug)]
pub struct CompactionResult {
    /// Number of sealed segment files that were merged and removed.
    pub segments_removed: usize,
    /// Total bytes freed by removing the merged segment files.
    pub bytes_reclaimed: u64,
}

/// frame_encode: serialize data to msgpack, wrap in \[len:u32 BE\]\[crc32:u32 BE\]\[msgpack\]
/// \[SPEC:WIRE FORMAT DECISIONS — ALWAYS rmp_serde::to_vec_named()\]
/// \[DEP:rmp_serde::to_vec_named\] → `Result<Vec<u8>, encode::Error>`
/// \[DEP:crc32fast::hash\] → u32
///
/// # Errors
/// Returns `StoreError::Serialization` if the data cannot be serialized to MessagePack.
pub fn frame_encode<T: serde::Serialize>(data: &T) -> Result<Vec<u8>, StoreError> {
    let msgpack =
        rmp_serde::to_vec_named(data).map_err(|e| StoreError::Serialization(Box::new(e)))?;
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
    if buf.len() < 8 + len {
        return Err(FrameDecodeError::Truncated {
            expected_len: 8 + len,
            available: buf.len(),
        });
    }
    let msgpack = &buf[8..8 + len];
    let actual_crc = crc32fast::hash(msgpack);
    if actual_crc != expected_crc {
        return Err(FrameDecodeError::CrcMismatch {
            expected: expected_crc,
            actual: actual_crc,
        });
    }
    Ok((msgpack, 8 + len))
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
    pub fn create(dir: &std::path::Path, segment_id: u64) -> Result<Self, StoreError> {
        let path = dir.join(segment_filename(segment_id));
        let mut file = std::fs::File::create_new(&path).map_err(StoreError::Io)?;

        let header = SegmentHeader {
            version: 1,
            flags: 0,
            #[allow(clippy::cast_possible_truncation)] // won't overflow i64 until year 2262
            created_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64,
            segment_id,
        };

        // Write magic + header_len(u32 BE) + header(msgpack)
        file.write_all(SEGMENT_MAGIC).map_err(StoreError::Io)?;
        let header_bytes =
            rmp_serde::to_vec_named(&header).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        #[allow(clippy::cast_possible_truncation)] // msgpack header is always small
        let header_len = (header_bytes.len() as u32).to_be_bytes();
        file.write_all(&header_len).map_err(StoreError::Io)?;
        file.write_all(&header_bytes).map_err(StoreError::Io)?;

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
        let mut source = std::fs::File::open(path).map_err(StoreError::Io)?;
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
        source
            .seek(SeekFrom::Start(8 + header_len))
            .map_err(StoreError::Io)?;

        let offset = self.written_bytes;
        if let Some(ref mut destination) = self.file {
            let copied = std::io::copy(&mut source, destination).map_err(StoreError::Io)?;
            self.written_bytes += copied;
        }
        Ok(offset)
    }

    /// Returns `true` if the segment has reached or exceeded `max_bytes` and should be rotated.
    pub fn needs_rotation(&self, max_bytes: u64) -> bool {
        self.written_bytes >= max_bytes
    }

    /// Deprecated: hardcodes SyncAll, ignoring user's SyncMode config.
    /// Use `sync_with_mode(&config.sync_mode)` instead.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] if the filesystem sync fails.
    #[deprecated(note = "Use sync_with_mode(&config.sync_mode) to respect user's SyncMode config")]
    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.sync_with_mode(&crate::store::SyncMode::SyncAll)
    }

    /// Flush the segment file to durable storage using the specified sync mode.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the OS-level sync call fails.
    pub fn sync_with_mode(&mut self, mode: &crate::store::SyncMode) -> Result<(), StoreError> {
        if let Some(ref f) = self.file {
            match mode {
                crate::store::SyncMode::SyncAll => f.sync_all().map_err(StoreError::Io)?,
                crate::store::SyncMode::SyncData => f.sync_data().map_err(StoreError::Io)?,
            }
        }
        Ok(())
    }

    /// Write a SIDX footer to the end of this segment before sealing.
    /// The footer enables fast cold-start index rebuild by storing compact
    /// binary entries instead of requiring full msgpack frame deserialization.
    ///
    /// # Errors
    /// Returns `StoreError::Io` or `StoreError::Serialization` if writing fails.
    pub(crate) fn write_sidx_footer(
        &mut self,
        collector: &crate::store::sidx::SidxEntryCollector,
    ) -> Result<(), StoreError> {
        if let Some(ref mut f) = self.file {
            collector.write_footer(f)?;
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
