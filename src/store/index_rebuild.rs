use crate::coordinate::Coordinate;
use crate::store::index::{DiskPos, IndexEntry, StoreIndex};
use crate::store::reader::Reader;
use crate::store::segment;
use crate::store::StoreError;
use std::path::Path;

/// Open the index using the fastest available path:
/// 1. Try loading a checkpoint file → if valid, restore from it + replay tail segments.
/// 2. Fall back to full segment scan if checkpoint is missing, corrupt, or stale.
pub(crate) fn open_index(
    index: &StoreIndex,
    reader: &Reader,
    data_dir: &Path,
    enable_checkpoint: bool,
) -> Result<(), StoreError> {
    if enable_checkpoint {
        if let Some((entries, watermark)) =
            crate::store::checkpoint::try_load_checkpoint(data_dir)
        {
            tracing::info!(
                "checkpoint loaded: {} entries, global_seq {}, watermark segment {} offset {}",
                entries.len(),
                watermark.global_sequence,
                watermark.watermark_segment_id,
                watermark.watermark_offset
            );
            crate::store::checkpoint::restore_from_checkpoint(index, entries)?;
            // Replay segments newer than the watermark.
            replay_tail_segments(index, reader, data_dir, &watermark)?;
            return Ok(());
        }
        tracing::debug!("no valid checkpoint, performing full index rebuild");
    }
    rebuild_from_segments(index, reader, data_dir)
}

/// Replay only segments with ID > watermark, or frames at offset >= watermark_offset
/// within the watermark segment itself.
fn replay_tail_segments(
    index: &StoreIndex,
    reader: &Reader,
    data_dir: &Path,
    watermark: &crate::store::checkpoint::WatermarkInfo,
) -> Result<(), StoreError> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(data_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == segment::SEGMENT_EXTENSION)
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for dir_entry in &entries {
        let seg_id = dir_entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        if seg_id < watermark.watermark_segment_id {
            continue; // Already in checkpoint
        }

        let scanned = reader.scan_segment_index(&dir_entry.path())?;
        for se in scanned {
            // Skip frames already in the checkpoint
            if seg_id == watermark.watermark_segment_id
                && se.offset < watermark.watermark_offset
            {
                continue;
            }
            let coord = Coordinate::new(&se.entity, &se.scope)?;
            let entity_id = index.interner.intern(&se.entity);
            let scope_id = index.interner.intern(&se.scope);
            let clock = se.header.position.sequence;
            let entry = IndexEntry {
                event_id: se.header.event_id,
                correlation_id: se.header.correlation_id,
                causation_id: se.header.causation_id,
                coord,
                entity_id,
                scope_id,
                kind: se.header.event_kind,
                wall_ms: se.header.position.wall_ms,
                clock,
                hash_chain: se.hash_chain,
                disk_pos: DiskPos {
                    segment_id: se.segment_id,
                    offset: se.offset,
                    length: se.length,
                },
                global_sequence: index.global_sequence(),
            };
            index.insert(entry);
        }
    }
    Ok(())
}

/// Scan all segment files in `data_dir`, rebuild the in-memory index from their contents.
/// Used by both cold-start (`Store::open_with_cache`) and post-compaction index rebuild.
pub(crate) fn rebuild_from_segments(
    index: &StoreIndex,
    reader: &Reader,
    data_dir: &Path,
) -> Result<(), StoreError> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(data_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == segment::SEGMENT_EXTENSION)
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for dir_entry in &entries {
        let scanned = reader.scan_segment_index(&dir_entry.path())?;
        for se in scanned {
            let coord = Coordinate::new(&se.entity, &se.scope)?;
            let entity_id = index.interner.intern(&se.entity);
            let scope_id = index.interner.intern(&se.scope);
            let clock = se.header.position.sequence;
            let entry = IndexEntry {
                event_id: se.header.event_id,
                correlation_id: se.header.correlation_id,
                causation_id: se.header.causation_id,
                coord,
                entity_id,
                scope_id,
                kind: se.header.event_kind,
                wall_ms: se.header.position.wall_ms,
                clock,
                hash_chain: se.hash_chain,
                disk_pos: DiskPos {
                    segment_id: se.segment_id,
                    offset: se.offset,
                    length: se.length,
                },
                global_sequence: index.global_sequence(),
            };
            index.insert(entry);
        }
    }

    Ok(())
}
