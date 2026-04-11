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
        if let Some((entries, interner_strings, watermark, stored_allocator)) =
            crate::store::checkpoint::try_load_checkpoint(data_dir)
        {
            tracing::info!(
                "checkpoint v2 loaded: {} entries, {} interner strings, watermark segment {} offset {}, allocator {}",
                entries.len(),
                interner_strings.len(),
                watermark.watermark_segment_id,
                watermark.watermark_offset,
                stored_allocator,
            );
            crate::store::checkpoint::restore_from_checkpoint(
                index,
                entries,
                &interner_strings,
                stored_allocator,
            )?;
            // Replay segments newer than the watermark.
            replay_tail_segments(index, reader, data_dir, &watermark)?;
            return Ok(());
        }
        tracing::debug!("no valid checkpoint, performing full index rebuild");
    }
    rebuild_from_segments(index, reader, data_dir)
}

/// Build an `IndexEntry` from a `ScannedIndexEntry`, sourcing `global_sequence`
/// from the SIDX footer if available, otherwise asking the cursor to synthesize
/// the next free slot. This keeps sparse `global_sequence` values from disk
/// preserved verbatim across cold-start rebuilds.
fn entry_from_scan(
    index: &StoreIndex,
    cursor: &mut crate::store::index::ReplayCursor<'_>,
    se: crate::store::reader::ScannedIndexEntry,
) -> Result<IndexEntry, StoreError> {
    let coord = Coordinate::new(&se.entity, &se.scope)?;
    let entity_id = index.interner.intern(&se.entity);
    let scope_id = index.interner.intern(&se.scope);
    let clock = se.header.position.sequence;
    // SIDX-stored sequence wins; otherwise synthesize from the cursor's
    // running maximum (active segment / footerless slow path).
    let global_sequence = se
        .global_sequence
        .unwrap_or_else(|| cursor.synthesize_next());
    Ok(IndexEntry {
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
        global_sequence,
    })
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

    // Cross-segment batch recovery state persists across segment scans.
    let mut batch_state = crate::store::reader::BatchRecoveryState::default();

    let mut cursor = index.begin_replay();
    // Tail replay continues from wherever the checkpoint restore left the
    // allocator — pass the current value as the synthesis floor so any
    // synthesized sequences advance from there.
    let allocator_floor = index.global_sequence();

    let scan_result = (|| -> Result<(), StoreError> {
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

            let scanned = reader.scan_segment_index(&dir_entry.path(), Some(&mut batch_state))?;
            for se in scanned {
                // Skip frames already in the checkpoint
                if seg_id == watermark.watermark_segment_id
                    && se.offset < watermark.watermark_offset
                {
                    continue;
                }
                let entry = entry_from_scan(index, &mut cursor, se)?;
                cursor.insert(entry);
            }
        }
        Ok(())
    })();

    match scan_result {
        Ok(()) => {
            // All tail entries are now in the index. Restore allocator (preserving
            // both the checkpoint allocator floor and any sparse SIDX-preserved
            // sequences) and publish atomically.
            cursor.commit(allocator_floor);
            Ok(())
        }
        Err(e) => {
            cursor.abort();
            Err(e)
        }
    }
}

/// Scan all segment files in `data_dir`, rebuild the in-memory index from their contents.
/// Used by both cold-start (`Store::open_with_cache`) and post-compaction index rebuild.
/// Handles cross-segment batch recovery using BatchRecoveryState.
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

    // Cross-segment batch recovery state persists across segment scans.
    let mut batch_state = crate::store::reader::BatchRecoveryState::default();

    let mut cursor = index.begin_replay();

    let scan_result = (|| -> Result<(), StoreError> {
        for dir_entry in &entries {
            let scanned = reader.scan_segment_index(&dir_entry.path(), Some(&mut batch_state))?;
            for se in scanned {
                let entry = entry_from_scan(index, &mut cursor, se)?;
                cursor.insert(entry);
            }
        }
        Ok(())
    })();

    match scan_result {
        Ok(()) => {
            // Full rebuild complete. No allocator hint — preserve SIDX
            // sequences as-is and advance the allocator past the maximum seen.
            cursor.commit(0);
            Ok(())
        }
        Err(e) => {
            cursor.abort();
            Err(e)
        }
    }
}
