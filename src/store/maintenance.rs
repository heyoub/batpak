use crate::coordinate::Coordinate;
use crate::event::{EventKind, StoredEvent};
use crate::store::reader;
use crate::store::segment::{self, Active, FramePayload};
use crate::store::{
    CompactionConfig, CompactionStrategy, Store, StoreDiagnostics, StoreError, StoreStats,
};

pub(crate) fn sync(store: &Store) -> Result<(), StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "sync");
    let (tx, rx) = flume::bounded(1);
    store
        .writer
        .tx
        .send(crate::store::writer::WriterCommand::Sync { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    rx.recv().map_err(|_| StoreError::WriterCrashed)?
}

pub(crate) fn snapshot(store: &Store, dest: &std::path::Path) -> Result<(), StoreError> {
    tracing::debug!(
        target: "batpak::flow",
        flow = "snapshot",
        destination = %dest.display()
    );
    sync(store)?;
    std::fs::create_dir_all(dest).map_err(StoreError::Io)?;
    let entries = std::fs::read_dir(&store.config.data_dir).map_err(StoreError::Io)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .map(|ext| ext == segment::SEGMENT_EXTENSION)
            .unwrap_or(false)
        {
            let dest_path = dest.join(entry.file_name());
            std::fs::copy(&path, &dest_path).map_err(StoreError::Io)?;
        }
    }
    Ok(())
}

pub(crate) fn compact(
    store: &Store,
    config: &CompactionConfig,
) -> Result<segment::CompactionResult, StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "compact");
    sync(store)?;

    // Single read_dir: collect all segment IDs and paths, then partition.
    let mut all_segments: Vec<(u64, std::path::PathBuf)> =
        std::fs::read_dir(&store.config.data_dir)
            .map_err(StoreError::Io)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let ext_ok = path
                    .extension()
                    .map(|ext| ext == segment::SEGMENT_EXTENSION)
                    .unwrap_or(false);
                if !ext_ok {
                    return None;
                }
                let seg_id = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.parse::<u64>().ok())?;
                Some((seg_id, path))
            })
            .collect();
    all_segments.sort_by_key(|(id, _)| *id);

    let active_segment_id = all_segments.last().map(|(id, _)| *id).unwrap_or(0);
    let mut sealed: Vec<(u64, std::path::PathBuf)> = all_segments
        .into_iter()
        .filter(|(id, _)| *id < active_segment_id)
        .collect();

    if sealed.len() < config.min_segments {
        return Ok(segment::CompactionResult {
            segments_removed: 0,
            bytes_reclaimed: 0,
        });
    }

    let merged_id = sealed[0].0;
    let merged_path = store
        .config
        .data_dir
        .join(segment::segment_filename(merged_id));
    let mut compact_source_path = None;

    for (seg_id, _) in &sealed {
        store.reader.evict_segment(*seg_id);
    }

    if let Some((_, source_path)) = sealed.iter_mut().find(|(seg_id, _)| *seg_id == merged_id) {
        let temp_source_path = store.config.data_dir.join(format!(
            "{merged_id:06}.{}.compact-src",
            segment::SEGMENT_EXTENSION
        ));
        let _ = std::fs::remove_file(&temp_source_path);
        std::fs::rename(&*source_path, &temp_source_path).map_err(StoreError::Io)?;
        *source_path = temp_source_path.clone();
        compact_source_path = Some(temp_source_path);
    }

    let _ = std::fs::remove_file(&merged_path);
    let mut merged_segment = segment::Segment::<Active>::create(&store.config.data_dir, merged_id)?;
    match &config.strategy {
        CompactionStrategy::Merge => {
            for (_, path) in &sealed {
                merged_segment.append_frames_from_segment(path)?;
            }
        }
        CompactionStrategy::Retention(predicate) => {
            let mut all_events: Vec<reader::ScannedEntry> = Vec::new();
            for (_, path) in &sealed {
                all_events.extend(store.reader.scan_segment(path)?);
            }
            for entry in all_events {
                let coord = Coordinate::new(&entry.entity, &entry.scope)?;
                let stored = StoredEvent {
                    coordinate: coord,
                    event: entry.event.clone(),
                };
                if predicate(&stored) {
                    let frame_payload = FramePayload {
                        event: entry.event,
                        entity: entry.entity,
                        scope: entry.scope,
                    };
                    let frame = segment::frame_encode(&frame_payload)?;
                    merged_segment.write_frame(&frame)?;
                }
            }
        }
        CompactionStrategy::Tombstone(predicate) => {
            let mut all_events: Vec<reader::ScannedEntry> = Vec::new();
            for (_, path) in &sealed {
                all_events.extend(store.reader.scan_segment(path)?);
            }
            let tombstone_kind = EventKind::TOMBSTONE;
            for mut entry in all_events {
                let coord = Coordinate::new(&entry.entity, &entry.scope)?;
                let stored = StoredEvent {
                    coordinate: coord,
                    event: entry.event.clone(),
                };
                if !predicate(&stored) {
                    entry.event.header.event_kind = tombstone_kind;
                }
                let frame_payload = FramePayload {
                    event: entry.event,
                    entity: entry.entity,
                    scope: entry.scope,
                };
                let frame = segment::frame_encode(&frame_payload)?;
                merged_segment.write_frame(&frame)?;
            }
        }
    }

    merged_segment.sync_with_mode(&store.config.sync.mode)?;
    let _sealed_segment = merged_segment.seal();

    let mut bytes_reclaimed = 0_u64;
    let mut segments_removed = 0_usize;
    for (_, path) in &sealed {
        if let Ok(meta) = std::fs::metadata(path) {
            bytes_reclaimed += meta.len();
        }
        std::fs::remove_file(path).map_err(StoreError::Io)?;
        segments_removed += 1;
    }

    if let Some(temp_source_path) = compact_source_path {
        let _ = std::fs::remove_file(temp_source_path);
    }

    sync(store)?;
    store.index.clear();
    crate::store::index_rebuild::rebuild_from_segments(
        &store.index,
        &store.reader,
        &store.config.data_dir,
    )?;

    // Write checkpoint after post-compact rebuild so next open is fast.
    if store.config.index.enable_checkpoint {
        if let Err(e) = write_checkpoint_on_close(store) {
            tracing::warn!("post-compaction checkpoint write failed: {e}");
        }
    }

    Ok(segment::CompactionResult {
        segments_removed,
        bytes_reclaimed,
    })
}

pub(crate) fn close(store: Store) -> Result<(), StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "close");
    let (tx, rx) = flume::bounded(1);
    store
        .writer
        .tx
        .send(crate::store::writer::WriterCommand::Shutdown { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    let result = rx.recv().map_err(|_| StoreError::WriterCrashed)?;

    // Write index checkpoint after writer shutdown (all data fsynced).
    if store.config.index.enable_checkpoint {
        if let Err(e) = write_checkpoint_on_close(&store) {
            tracing::warn!("failed to write checkpoint on close: {e}");
            // Non-fatal: next open will fall back to full segment scan.
        }
    }

    drop(store);
    result
}

/// Determine watermark from the latest segment file and write checkpoint.
fn write_checkpoint_on_close(store: &Store) -> Result<(), StoreError> {
    let (seg_id, offset) = find_latest_segment_watermark(&store.config.data_dir)?;
    crate::store::checkpoint::write_checkpoint(&store.index, &store.config.data_dir, seg_id, offset)
}

/// Scan data_dir for the highest-numbered .fbat file and return (segment_id, file_size).
fn find_latest_segment_watermark(data_dir: &std::path::Path) -> Result<(u64, u64), StoreError> {
    let mut max: Option<(u64, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(data_dir).map_err(StoreError::Io)? {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        let ext_ok = path
            .extension()
            .map(|ext| ext == crate::store::segment::SEGMENT_EXTENSION)
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        if let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if max.as_ref().map(|(m, _)| id > *m).unwrap_or(true) {
                max = Some((id, path));
            }
        }
    }
    match max {
        Some((id, path)) => {
            let offset = std::fs::metadata(&path).map_err(StoreError::Io)?.len();
            Ok((id, offset))
        }
        None => Ok((0, 0)),
    }
}

pub(crate) fn stats(store: &Store) -> StoreStats {
    StoreStats {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
    }
}

pub(crate) fn diagnostics(store: &Store) -> StoreDiagnostics {
    // Extract tile stats from columnar index (0 for non-columnar layouts).
    let (index_layout, tile_count) = match &store.index.scan {
        crate::store::columnar::ScanIndex::Maps { .. } => ("AoS", 0),
        crate::store::columnar::ScanIndex::Columnar(ci) => (ci.layout_name(), ci.tile_count()),
    };
    StoreDiagnostics {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
        visible_sequence: store.index.visible_sequence(),
        data_dir: store.config.data_dir.clone(),
        segment_max_bytes: store.config.segment_max_bytes,
        fd_budget: store.config.fd_budget,
        restart_policy: store.config.writer.restart_policy.clone(),
        index_layout,
        tile_count,
    }
}
