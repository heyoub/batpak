use super::{checkpoint_entries_to_index_entries, CheckpointEntry};
use crate::store::cold_start::{FileLoad, ReservedKindFallbackStats};
use crate::store::index::{recommended_restore_chunk_count, RoutingSummary};
use crate::store::platform::fs::{read, write_file_atomically};
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Magic bytes at the start of every checkpoint file.
pub(super) const CHECKPOINT_MAGIC: &[u8; 6] = b"FBATCK";

/// Format version stored in the checkpoint header.
/// v6: v5 plus receipt-extension maps in checkpoint entries.
/// v2 checkpoints remain readable as a fallback; v1 is rejected.
pub(super) const CHECKPOINT_VERSION: u16 = 6;

/// Final checkpoint filename inside the data directory.
pub(crate) const CHECKPOINT_FILENAME: &str = "index.ckpt";

const HEADER_LEN: usize = 6 + 2 + 4;

/// Checkpoint format v2: includes interner snapshot + InternId-based entries.
#[derive(Serialize, Deserialize)]
pub(super) struct CheckpointDataV2 {
    pub(super) global_sequence: u64,
    pub(super) watermark_segment_id: u64,
    pub(super) watermark_offset: u64,
    /// Interner snapshot: ordered list of interned strings (index = InternId).
    /// The sentinel (empty string at index 0) is included.
    pub(super) interner_strings: Vec<String>,
    pub(super) entries: Vec<CheckpointEntry>,
}

/// Checkpoint format v3: v2 plus additive routing/chunk summaries.
#[derive(Serialize, Deserialize)]
pub(super) struct CheckpointDataV3 {
    pub(super) global_sequence: u64,
    pub(super) watermark_segment_id: u64,
    pub(super) watermark_offset: u64,
    pub(super) interner_strings: Vec<String>,
    pub(super) routing: RoutingSummary,
    pub(super) entries: Vec<CheckpointEntry>,
}

/// Checkpoint format v4: v3 plus DAG lane/depth inside each entry.
#[derive(Serialize, Deserialize)]
pub(super) struct CheckpointDataV4 {
    pub(super) global_sequence: u64,
    pub(super) watermark_segment_id: u64,
    pub(super) watermark_offset: u64,
    pub(super) interner_strings: Vec<String>,
    pub(super) routing: RoutingSummary,
    pub(super) entries: Vec<CheckpointEntry>,
}

/// Checkpoint format v6: v5 plus receipt-extension maps in entries.
#[derive(Serialize, Deserialize)]
pub(super) struct CheckpointDataV6 {
    pub(super) global_sequence: u64,
    pub(super) watermark_segment_id: u64,
    pub(super) watermark_offset: u64,
    pub(super) interner_strings: Vec<String>,
    pub(super) routing: RoutingSummary,
    #[serde(default)]
    pub(super) reserved_kind_fallbacks: ReservedKindFallbackStats,
    pub(super) entries: Vec<CheckpointEntry>,
}

pub(super) struct LoadedCheckpointFile {
    pub(super) path: PathBuf,
    pub(super) version: u16,
    pub(super) body: Vec<u8>,
}

pub(super) struct DecodedCheckpointData {
    pub(super) entries: Vec<CheckpointEntry>,
    pub(super) interner_strings: Vec<String>,
    pub(super) watermark_segment_id: u64,
    pub(super) watermark_offset: u64,
    pub(super) global_sequence: u64,
    pub(super) routing: RoutingSummary,
    pub(super) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

pub(super) fn read_checkpoint_file(data_dir: &Path) -> FileLoad<LoadedCheckpointFile> {
    let path = data_dir.join(CHECKPOINT_FILENAME);

    let raw = match read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return FileLoad::Missing,
        Err(error) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                error = %error,
                "failed to read checkpoint file"
            );
            return FileLoad::Invalid {
                reason: format!("read failed: {error}"),
            };
        }
    };

    if raw.len() < HEADER_LEN {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            len = raw.len(),
            "checkpoint file too short to contain a valid header"
        );
        return FileLoad::Invalid {
            reason: format!("checkpoint file too short: {} bytes", raw.len()),
        };
    }

    if &raw[..6] != CHECKPOINT_MAGIC.as_ref() {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            "checkpoint file has wrong magic bytes — ignoring"
        );
        return FileLoad::Invalid {
            reason: "wrong magic bytes".to_owned(),
        };
    }

    let version = u16::from_le_bytes([raw[6], raw[7]]);
    let stored_crc = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let body = raw[HEADER_LEN..].to_vec();
    let computed_crc = crc32fast::hash(&body);
    if stored_crc != computed_crc {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            stored = stored_crc,
            computed = computed_crc,
            "checkpoint CRC mismatch — file is corrupt, ignoring"
        );
        return FileLoad::Invalid {
            reason: format!("crc mismatch: stored {stored_crc}, computed {computed_crc}"),
        };
    }

    FileLoad::Loaded(LoadedCheckpointFile {
        path,
        version,
        body,
    })
}

pub(super) fn decode_checkpoint_data(
    path: &Path,
    version: u16,
    body: &[u8],
) -> Option<DecodedCheckpointData> {
    match version {
        2 => {
            let data: CheckpointDataV2 =
                decode_body(path, body, "checkpoint deserialisation failed — ignoring")?;
            let routing = RoutingSummary::from_sorted_entries(
                &checkpoint_entries_to_index_entries(&data.entries, &data.interner_strings).ok()?,
                recommended_restore_chunk_count(data.entries.len()),
            );
            Some(DecodedCheckpointData {
                entries: data.entries,
                interner_strings: data.interner_strings,
                watermark_segment_id: data.watermark_segment_id,
                watermark_offset: data.watermark_offset,
                global_sequence: data.global_sequence,
                routing,
                cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
            })
        }
        3 => {
            let data: CheckpointDataV3 =
                decode_body(path, body, "checkpoint deserialisation failed — ignoring")?;
            Some(DecodedCheckpointData {
                entries: data.entries,
                interner_strings: data.interner_strings,
                watermark_segment_id: data.watermark_segment_id,
                watermark_offset: data.watermark_offset,
                global_sequence: data.global_sequence,
                routing: data.routing,
                cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
            })
        }
        4 => {
            let data: CheckpointDataV4 =
                decode_body(path, body, "checkpoint deserialisation failed — ignoring")?;
            Some(DecodedCheckpointData {
                entries: data.entries,
                interner_strings: data.interner_strings,
                watermark_segment_id: data.watermark_segment_id,
                watermark_offset: data.watermark_offset,
                global_sequence: data.global_sequence,
                routing: data.routing,
                cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
            })
        }
        5 | 6 => {
            let data: CheckpointDataV6 =
                decode_body(path, body, "checkpoint deserialisation failed — ignoring")?;
            Some(DecodedCheckpointData {
                entries: data.entries,
                interner_strings: data.interner_strings,
                watermark_segment_id: data.watermark_segment_id,
                watermark_offset: data.watermark_offset,
                global_sequence: data.global_sequence,
                routing: data.routing,
                cumulative_reserved_kind_fallbacks: data.reserved_kind_fallbacks,
            })
        }
        _ => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                version,
                expected = CHECKPOINT_VERSION,
                "unsupported checkpoint version — ignoring"
            );
            None
        }
    }
}

pub(super) fn decode_checkpoint_snapshot_v6(path: &Path, body: &[u8]) -> Option<CheckpointDataV6> {
    decode_body(
        path,
        body,
        "checkpoint snapshot deserialisation failed — ignoring",
    )
}

pub(super) fn encode_checkpoint_body<T: Serialize + ?Sized>(
    body: &T,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    crate::encoding::to_bytes(body)
}

pub(super) fn write_checkpoint_file(data_dir: &Path, body: &[u8]) -> Result<(), StoreError> {
    let crc: u32 = crc32fast::hash(body);
    let final_path = data_dir.join(CHECKPOINT_FILENAME);
    write_file_atomically(data_dir, &final_path, "checkpoint", |file| {
        let mut w = BufWriter::new(file);

        w.write_all(CHECKPOINT_MAGIC)?;
        w.write_all(&CHECKPOINT_VERSION.to_le_bytes())?;
        w.write_all(&crc.to_le_bytes())?;
        w.write_all(body)?;
        w.flush()?;
        Ok(())
    })?;
    Ok(())
}

fn decode_body<T: for<'de> Deserialize<'de>>(
    path: &Path,
    body: &[u8],
    message: &'static str,
) -> Option<T> {
    match crate::encoding::from_bytes(body) {
        Ok(data) => Some(data),
        Err(error) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                error = %error,
                message
            );
            None
        }
    }
}
