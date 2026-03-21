use crate::event::Event;
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::io::Write;
// NOTE: No `use crate::wire::*` needed. serde(with) resolves via string path.

/// Segment file format: magic + header + frames.
/// Magic: b"FBAT" (4 bytes). Header: 32 bytes. Frame: [len:u32 BE][crc32:u32 BE][msgpack]
/// Files named: {segment_id:06}.fbat (e.g., 000001.fbat). Sequential u64.
/// [SPEC:src/store/segment.rs]
pub const SEGMENT_MAGIC: &[u8; 4] = b"FBAT";
pub const SEGMENT_HEADER_SIZE: usize = 32;

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

/// frame_encode: serialize data to msgpack, wrap in [len:u32 BE][crc32:u32 BE][msgpack]
/// [SPEC:WIRE FORMAT DECISIONS — ALWAYS rmp_serde::to_vec_named()]
/// [DEP:rmp_serde::to_vec_named] → Result<Vec<u8>, encode::Error>
/// [DEP:crc32fast::hash] → u32
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

/// frame_decode: read [len][crc][msgpack], verify CRC, return msgpack bytes.
/// Returns (msgpack_bytes, total_frame_size_consumed).
pub fn frame_decode(buf: &[u8]) -> Result<(&[u8], usize), StoreError> {
    if buf.len() < 8 {
        return Err(StoreError::CorruptSegment {
            segment_id: 0,
            detail: "frame too short for header".into(),
        });
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let expected_crc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if buf.len() < 8 + len {
        return Err(StoreError::CorruptSegment {
            segment_id: 0,
            detail: "frame truncated".into(),
        });
    }
    let msgpack = &buf[8..8 + len];
    let actual_crc = crc32fast::hash(msgpack);
    if actual_crc != expected_crc {
        return Err(StoreError::CrcMismatch {
            segment_id: 0,
            offset: 0,
        });
    }
    Ok((msgpack, 8 + len))
}

/// Segment naming helper.
pub fn segment_filename(segment_id: u64) -> String {
    format!("{:06}.fbat", segment_id)
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

        // Write magic + header
        file.write_all(SEGMENT_MAGIC).map_err(StoreError::Io)?;
        let header_bytes = rmp_serde::to_vec_named(&header)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        file.write_all(&header_bytes).map_err(StoreError::Io)?;

        Ok(Self {
            header,
            path,
            file: Some(file),
            written_bytes: (4 + header_bytes.len()) as u64,
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
        if let Some(ref f) = self.file {
            f.sync_all().map_err(StoreError::Io)?;
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
