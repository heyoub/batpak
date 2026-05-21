use super::{checkpoint_entries_to_index_entries, format};
use crate::store::cold_start::{
    validate_watermark_segment, FileLoad, ReservedKindFallbackStats, WatermarkInfo,
    WatermarkValidationError,
};
use crate::store::index::{restore_chunk_ranges, IndexEntry, RoutingSummary};
use crate::store::StoreError;
use rayon::prelude::*;
use std::path::Path;

pub(crate) struct LoadedCheckpointData {
    pub(crate) entries: Vec<super::CheckpointEntry>,
    pub(crate) interner_strings: Vec<String>,
    pub(crate) watermark: WatermarkInfo,
    pub(crate) stored_allocator: u64,
    pub(crate) routing: RoutingSummary,
    pub(crate) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

pub(crate) struct LoadedCheckpointSnapshot {
    pub(crate) entries: Vec<IndexEntry>,
    pub(crate) interner_strings: Vec<String>,
    pub(crate) watermark: WatermarkInfo,
    pub(crate) stored_allocator: u64,
    pub(crate) routing: RoutingSummary,
    pub(crate) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
    pub(crate) receipt_extensions_hydrated: bool,
}

/// Try to load a checkpoint from `<data_dir>/index.ckpt`.
///
/// Returns `None` — and emits a `tracing::warn!` — on any of:
/// - File not found (normal on first start).
/// - Bad magic bytes.
/// - Bad version number.
/// - CRC32 mismatch (corruption).
/// - Msgpack deserialisation error.
/// - The watermark segment file referenced in the checkpoint does not exist on
///   disk (indicates the data directory was modified externally after the
///   checkpoint was written).
///
/// On success returns the decoded checkpoint body plus routing summary.
/// `stored_allocator` is the `global_sequence` allocator position at checkpoint time,
/// which may be higher than `entries.len()` due to burned batch slots.
#[cfg(test)]
pub(crate) fn try_load_checkpoint(data_dir: &Path) -> Option<LoadedCheckpointData> {
    let loaded = match format::read_checkpoint_file(data_dir) {
        FileLoad::Loaded(loaded) => loaded,
        FileLoad::Missing | FileLoad::Invalid { .. } => return None,
    };
    decode_checkpoint_data(data_dir, &loaded.path, loaded.version, &loaded.body)
}

pub(crate) fn try_load_checkpoint_snapshot(data_dir: &Path) -> Option<LoadedCheckpointSnapshot> {
    let raw = match format::read_checkpoint_file(data_dir) {
        FileLoad::Loaded(raw) => raw,
        FileLoad::Missing => return None,
        FileLoad::Invalid { reason } => {
            tracing::debug!(
                target: "batpak::checkpoint",
                %reason,
                "checkpoint fast path skipped after invalid checkpoint file"
            );
            return None;
        }
    };
    if raw.version == format::CHECKPOINT_VERSION {
        return decode_checkpoint_snapshot_v6(data_dir, &raw.path, &raw.body);
    }

    let loaded = decode_checkpoint_data(data_dir, &raw.path, raw.version, &raw.body)?;
    let chunk_ranges = restore_chunk_ranges(loaded.entries.len(), &loaded.routing);

    let mut per_chunk = chunk_ranges
        .into_par_iter()
        .enumerate()
        .map(|(chunk_idx, (start, len))| {
            let end = start
                .checked_add(len)
                .ok_or_else(|| StoreError::ser_msg("checkpoint restore chunk range overflowed"))?;
            let slice = loaded.entries.get(start..end).ok_or_else(|| {
                StoreError::ser_msg("checkpoint restore chunk range out of bounds")
            })?;
            let rebuilt = checkpoint_entries_to_index_entries(slice, &loaded.interner_strings)?;
            Ok::<_, StoreError>((chunk_idx, rebuilt))
        })
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    per_chunk.sort_by_key(|(chunk_idx, _)| *chunk_idx);

    let mut rebuilt_entries = Vec::with_capacity(loaded.entries.len());
    for (_, chunk_entries) in per_chunk {
        rebuilt_entries.extend(chunk_entries);
    }

    Some(LoadedCheckpointSnapshot {
        entries: rebuilt_entries,
        interner_strings: loaded.interner_strings,
        watermark: loaded.watermark,
        stored_allocator: loaded.stored_allocator,
        routing: loaded.routing,
        cumulative_reserved_kind_fallbacks: loaded.cumulative_reserved_kind_fallbacks,
        receipt_extensions_hydrated: false,
    })
}

fn decode_checkpoint_data(
    data_dir: &Path,
    path: &Path,
    version: u16,
    body: &[u8],
) -> Option<LoadedCheckpointData> {
    let data = format::decode_checkpoint_data(path, version, body)?;

    let watermark = validate_checkpoint_watermark(
        data_dir,
        path,
        data.watermark_segment_id,
        data.watermark_offset,
    )?;

    tracing::debug!(
        target: "batpak::checkpoint",
        entries = data.entries.len(),
        global_sequence = data.global_sequence,
        watermark_segment_id = data.watermark_segment_id,
        watermark_offset = data.watermark_offset,
        "checkpoint loaded successfully"
    );

    Some(LoadedCheckpointData {
        entries: data.entries,
        interner_strings: data.interner_strings,
        watermark,
        stored_allocator: data.global_sequence,
        routing: data.routing,
        cumulative_reserved_kind_fallbacks: data.cumulative_reserved_kind_fallbacks,
    })
}

fn decode_checkpoint_snapshot_v6(
    data_dir: &Path,
    path: &Path,
    body: &[u8],
) -> Option<LoadedCheckpointSnapshot> {
    let data = format::decode_checkpoint_snapshot_v6(path, body)?;

    let watermark = validate_checkpoint_watermark(
        data_dir,
        path,
        data.watermark_segment_id,
        data.watermark_offset,
    )?;

    tracing::debug!(
        target: "batpak::checkpoint",
        entries = data.entries.len(),
        global_sequence = data.global_sequence,
        watermark_segment_id = data.watermark_segment_id,
        watermark_offset = data.watermark_offset,
        "checkpoint snapshot loaded successfully"
    );

    let entries = match checkpoint_entries_to_index_entries(&data.entries, &data.interner_strings) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                error = %error,
                "checkpoint snapshot entry rebuild failed — ignoring"
            );
            return None;
        }
    };

    Some(LoadedCheckpointSnapshot {
        entries,
        interner_strings: data.interner_strings,
        watermark,
        stored_allocator: data.global_sequence,
        routing: data.routing,
        cumulative_reserved_kind_fallbacks: data.reserved_kind_fallbacks,
        receipt_extensions_hydrated: true,
    })
}

fn validate_checkpoint_watermark(
    data_dir: &Path,
    path: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
) -> Option<WatermarkInfo> {
    match validate_watermark_segment(data_dir, watermark_segment_id, watermark_offset) {
        Ok(()) => {}
        Err(WatermarkValidationError::MissingSegment { path: seg_path }) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                missing_segment = %seg_path.display(),
                "watermark segment referenced by checkpoint is missing — ignoring checkpoint"
            );
            return None;
        }
        Err(WatermarkValidationError::OffsetPastTail {
            path: seg_path,
            file_len,
            watermark_offset,
        }) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                watermark_segment = %seg_path.display(),
                file_len,
                watermark_offset,
                "checkpoint watermark points past the segment tail"
            );
            return None;
        }
    }

    Some(WatermarkInfo {
        watermark_segment_id,
        watermark_offset,
    })
}
