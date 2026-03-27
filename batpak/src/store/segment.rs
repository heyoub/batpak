use crate::event::Event;
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::io::Write;
// NOTE: No `use crate::wire::*` needed. serde(with) resolves via string path.

/// Segment file format: magic(4) + header_len(4 BE) + header(msgpack) + frames
/// Frame: \[len:u32 BE\]\[crc32:u32 BE\]\[msgpack\]
/// Files named: {segment_id:06}.fbat. Sequential u64.
/// [SPEC:src/store/segment.rs]
pub const SEGMENT_MAGIC: &[u8; 4] = b"FBAT";
pub const SEGMENT_EXTENSION: &str = "fbat";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentHeader {
    pub version: u16,
    pub flags: u16,
    pub created_ns: i64,
    pub segment_id: u64,
}

/// FramePayload: what gets serialized into each frame.
/// entity and scope are stored as strings (not Coordinate) because segments
/// are the persistence layer — they don't depend on the Coordinate type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FramePayload<P> {
    pub event: Event<P>,
    pub entity: String,
    pub scope: String,
}

/// Typestate for segment lifecycle.
pub struct Active;
pub struct Sealed;
pub struct Segment<State> {
    pub header: SegmentHeader,
    pub path: std::path::PathBuf,
    file: Option<std::fs::File>,
    written_bytes: u64,
    _state: std::marker::PhantomData<State>,
}

#[derive(Debug)]
pub struct CompactionResult {
    pub segments_removed: usize,
    pub bytes_reclaimed: u64,
}

/// frame_encode: serialize data to msgpack, wrap in \[len:u32 BE\]\[crc32:u32 BE\]\[msgpack\]
/// \[SPEC:WIRE FORMAT DECISIONS — ALWAYS rmp_serde::to_vec_named()\]
/// \[DEP:rmp_serde::to_vec_named\] → `Result<Vec<u8>, encode::Error>`
/// \[DEP:crc32fast::hash\] → u32
pub fn frame_encode<T: serde::Serialize>(data: &T) -> Result<Vec<u8>, StoreError> {
    let msgpack =
        rmp_serde::to_vec_named(data).map_err(|e| StoreError::Serialization(e.to_string()))?;
    let crc = crc32fast::hash(&msgpack);
    let len = msgpack.len() as u32;

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
    TooShort,
    Truncated {
        expected_len: usize,
        available: usize,
    },
    CrcMismatch {
        expected: u32,
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
    pub fn create(dir: &std::path::Path, segment_id: u64) -> Result<Self, StoreError> {
        let path = dir.join(segment_filename(segment_id));
        // Use OpenOptions (NOT File::create_new — requires Rust 1.77, MSRV is 1.75)
        // [SPEC:IMPLEMENTATION NOTES item 7 — MSRV workarounds]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(StoreError::Io)?;

        let header = SegmentHeader {
            version: 1,
            flags: 0,
            created_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64,
            segment_id,
        };

        // Write magic + header_len(u32 BE) + header(msgpack)
        file.write_all(SEGMENT_MAGIC).map_err(StoreError::Io)?;
        let header_bytes = rmp_serde::to_vec_named(&header)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
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
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<u64, StoreError> {
        let offset = self.written_bytes;
        if let Some(ref mut f) = self.file {
            f.write_all(frame).map_err(StoreError::Io)?;
        }
        self.written_bytes += frame.len() as u64;
        Ok(offset)
    }

    pub fn needs_rotation(&self, max_bytes: u64) -> bool {
        self.written_bytes >= max_bytes
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.sync_with_mode(&crate::store::SyncMode::SyncAll)
    }

    pub fn sync_with_mode(&mut self, mode: &crate::store::SyncMode) -> Result<(), StoreError> {
        if let Some(ref f) = self.file {
            match mode {
                crate::store::SyncMode::SyncAll => f.sync_all().map_err(StoreError::Io)?,
                crate::store::SyncMode::SyncData => f.sync_data().map_err(StoreError::Io)?,
            }
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
