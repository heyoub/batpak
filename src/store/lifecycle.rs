use crate::coordinate::Coordinate;
use crate::event::{EventKind, StoredEvent};
use crate::store::cold_start::{
    latest_segment_watermark, reject_symlink_leaf, ColdStartArtifactKind,
};
use crate::store::segment::scan as reader;
use crate::store::segment::{self, Active, FramePayload};
use crate::store::{
    Closed, CompactionConfig, CompactionStrategy, Open, Store, StoreDiagnostics, StoreError,
    StoreStats, WriterPressure,
};

pub(crate) fn sync(store: &Store<Open>) -> Result<(), StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "sync");
    let (tx, rx) = flume::bounded(1);
    store
        .writer_ref()
        .tx
        .send(crate::store::write::writer::WriterCommand::Sync { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    rx.recv().map_err(|_| StoreError::WriterCrashed)?
}

pub(crate) fn snapshot(store: &Store<Open>, dest: &std::path::Path) -> Result<(), StoreError> {
    tracing::debug!(
        target: "batpak::flow",
        flow = "snapshot",
        destination = %dest.display()
    );
    sync(store)?;
    reject_symlink_leaf(dest, "snapshot destination")?;
    std::fs::create_dir_all(dest).map_err(StoreError::Io)?;
    let entries = std::fs::read_dir(&store.config.data_dir).map_err(StoreError::Io)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_segment = path
            .extension()
            .map(|ext| ext == segment::SEGMENT_EXTENSION)
            .unwrap_or(false);
        let is_visibility_metadata = path
            .file_name()
            .map(|name| name == crate::store::hidden_ranges::VISIBILITY_RANGES_FILENAME)
            .unwrap_or(false);
        if is_segment || is_visibility_metadata {
            let dest_path = dest.join(entry.file_name());
            reject_symlink_leaf(&dest_path, "snapshot entry")?;
            std::fs::copy(&path, &dest_path).map_err(StoreError::Io)?;
        }
    }
    Ok(())
}

pub(crate) fn compact(
    store: &Store<Open>,
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
        // Skip signal: zero segments removed, zero bytes reclaimed. No
        // merged_id is fabricated from a zero fallback — the early return
        // happens before any merged-file path is derived, so there is no
        // way for compaction to overwrite segment 0.
        return Ok(segment::CompactionResult {
            outcome: segment::CompactionOutcome::Skipped,
            segments_removed: 0,
            bytes_reclaimed: 0,
        });
    }

    // sealed.len() >= config.min_segments >= 1 here, so sealed[0] is safe
    // without any unwrap_or fallback. The merged_id is always a real sealed
    // segment id that we are about to replace.
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
    crate::store::cold_start::rebuild::rebuild_from_segments(
        &store.index,
        &store.reader,
        &store.config.data_dir,
    )?;
    crate::store::cold_start::rebuild::restore_cancelled_visibility_ranges(
        &store.index,
        &store.config.data_dir,
    );

    // Refresh cold-start artifacts after post-compact rebuild so the next open
    // can take the fast path.
    if let Err(e) = write_cold_start_artifacts_on_close(store) {
        tracing::warn!("post-compaction cold-start artifact write failed: {e}");
    }

    Ok(segment::CompactionResult {
        outcome: segment::CompactionOutcome::Performed,
        segments_removed,
        bytes_reclaimed,
    })
}

pub(crate) fn close(mut store: Store<Open>) -> Result<Closed, StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "close");
    let (tx, rx) = flume::bounded(1);
    store
        .writer_ref()
        .tx
        .send(crate::store::write::writer::WriterCommand::Shutdown { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    let result = rx.recv().map_err(|_| StoreError::WriterCrashed)?;

    result?;

    // Write cold-start artifacts after writer shutdown (all data fsynced).
    // Explicit close() is the honest durable path, so artifact write failures
    // must surface to the caller instead of being downgraded to a warning.
    write_cold_start_artifacts_on_close(&store)?;

    store.should_shutdown_on_drop = false;
    Ok(Closed)
}

/// Determine watermark from the latest segment file and write the fastest
/// available cold-start artifact. When mmap is enabled it is strictly
/// preferred over checkpoint — writing both is redundant work that doubles
/// close() cost at high event counts.
fn write_cold_start_artifacts_on_close(store: &Store<Open>) -> Result<(), StoreError> {
    let (seg_id, offset) = latest_segment_watermark(&store.config.data_dir)?;
    match store.runtime.cold_start.write_target() {
        Some(ColdStartArtifactKind::MmapIndex) => {
            crate::store::cold_start::mmap::write_mmap_index(
                &store.index,
                &store.config.data_dir,
                seg_id,
                offset,
            )?;
        }
        Some(ColdStartArtifactKind::Checkpoint) => {
            crate::store::cold_start::checkpoint::write_checkpoint(
                &store.index,
                &store.config.data_dir,
                seg_id,
                offset,
            )?;
        }
        None => {}
    }
    Ok(())
}

pub(crate) fn stats<State>(store: &Store<State>) -> StoreStats {
    StoreStats {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
    }
}

pub(crate) fn diagnostics<State>(store: &Store<State>) -> StoreDiagnostics {
    StoreDiagnostics {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
        visible_sequence: store.index.visible_sequence(),
        data_dir: store.config.data_dir.clone(),
        segment_max_bytes: store.config.segment_max_bytes,
        fd_budget: store.config.fd_budget,
        restart_policy: store.config.writer.restart_policy.clone(),
        writer_pressure: store
            .writer
            .as_ref()
            .map(|writer| WriterPressure {
                queue_len: writer.tx.len(),
                capacity: store.config.writer.channel_capacity,
            })
            .unwrap_or(WriterPressure {
                queue_len: 0,
                capacity: 0,
            }),
        index_topology: store.index.topology_name(),
        tile_count: store.index.tile_count(),
        open_report: store.open_report.clone(),
    }
}
