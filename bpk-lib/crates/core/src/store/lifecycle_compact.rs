//! Sealed-segment compaction with off-side index rebuild and single swap point.

use super::sync;
use crate::coordinate::Coordinate;
use crate::event::{Event, EventKind, StoredEvent};
use crate::store::file_classification::StoreFileKind;
use crate::store::lifecycle_close::write_cold_start_artifacts_on_close;
use crate::store::platform::fs::StoreFs;
use crate::store::segment::scan as reader;
use crate::store::segment::{self, Active, FramePayload};
use crate::store::{CompactionConfig, CompactionStrategy, Open, Store, StoreError};

pub(crate) fn compact(
    store: &Store<Open>,
    config: &CompactionConfig,
) -> Result<
    (
        segment::CompactionResult,
        crate::store::compaction_report::CompactionReportBody,
    ),
    StoreError,
> {
    tracing::debug!(target: "batpak::flow", flow = "compact");
    let fs = store.config.fs();
    let _lifecycle = store.lifecycle_gate.lock();
    sync(store)?;

    let mut all_segments: Vec<(u64, std::path::PathBuf)> = Vec::new();
    for entry in fs
        .read_dir(&store.config.data_dir)
        .map_err(StoreError::Io)?
    {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        let seg_id = match StoreFileKind::from_path(&path) {
            StoreFileKind::Segment(segment_id) => segment_id.as_u64(),
            StoreFileKind::MalformedSegment(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping malformed segment filename"
                );
                continue;
            }
            StoreFileKind::VisibilityRanges
            | StoreFileKind::Checkpoint
            | StoreFileKind::MmapIndex
            | StoreFileKind::IdempotencyStore
            | StoreFileKind::PendingCompactionMarker
            | StoreFileKind::CompactSource
            | StoreFileKind::CursorDirectory
            | StoreFileKind::Other => continue,
        };
        all_segments.push((seg_id, path));
    }
    all_segments.sort_by_key(|(id, _)| *id);

    let active_segment_id = all_segments.last().map(|(id, _)| *id).unwrap_or(0);
    let mut sealed: Vec<(u64, std::path::PathBuf)> = all_segments
        .into_iter()
        .filter(|(id, _)| *id < active_segment_id)
        .collect();

    if sealed.len() < config.min_segments {
        let result = segment::CompactionResult {
            outcome: segment::CompactionOutcome::Skipped,
            segments_removed: 0,
            bytes_reclaimed: 0,
        };
        let report =
            crate::store::compaction_report::report_skipped(config, active_segment_id, &sealed)?;
        return Ok((result, report));
    }

    let merged_id = sealed[0].0;
    let merged_path = store
        .config
        .data_dir
        .join(segment::segment_filename(merged_id));
    let source_segment_ids: Vec<u64> = sealed.iter().map(|(seg_id, _)| *seg_id).collect();
    let mut compact_source_path = None;

    crate::store::cold_start::rebuild::write_pending_compaction(
        &store.config.data_dir,
        merged_id,
        &source_segment_ids,
    )?;

    let fresh_index = match materialize_compacted_segment(
        store,
        &config.strategy,
        &mut sealed,
        merged_id,
        &merged_path,
        &mut compact_source_path,
    )
    .and_then(|_| rebuild_fresh_compaction_index(store))
    {
        Ok(fresh_index) => fresh_index,
        Err(error) => {
            return failed_compaction_with_rollback(&FailedCompactionCtx {
                config,
                active_segment_id,
                sealed: &sealed,
                merged_segment_id: merged_id,
                data_dir: &store.config.data_dir,
                merged_path: &merged_path,
                compact_source_path: compact_source_path.as_deref(),
                error: &error,
                context: "compaction pre-swap phase failed",
                fs: fs.as_ref(),
            });
        }
    };

    store.index.replace_contents_from_fresh(fresh_index)?;

    let mut bytes_reclaimed = 0_u64;
    let mut segments_removed = 0_usize;
    for (_, path) in &sealed {
        if let Ok(meta) = fs.metadata(path) {
            bytes_reclaimed += meta.len();
        }
        fs.remove_file(path).map_err(StoreError::Io)?;
        segments_removed += 1;
    }

    if let Some(temp_source_path) = compact_source_path {
        fs.remove_file_if_present(&temp_source_path)
            .map_err(StoreError::Io)?;
    }
    crate::store::cold_start::rebuild::clear_pending_compaction(&store.config.data_dir)?;

    let frontier = store.index.global_sequence();
    store.index.mark_idemp_evicted_against_live();
    let eviction = store.index.idemp.evict(frontier);
    tracing::debug!(
        target: "batpak::idemp",
        flow = "compact",
        frontier,
        aged_out = eviction.aged_out,
        cap_trimmed = eviction.cap_trimmed_out_of_window,
        within_window_exceeds_cap = eviction.within_window_exceeds_cap,
        remaining = eviction.remaining,
        "applied window-priority idempotency eviction after compaction"
    );

    store.index.idemp.flush(&store.config.data_dir)?;

    if let Err(e) = write_cold_start_artifacts_on_close(store) {
        tracing::warn!("post-compaction cold-start artifact write failed: {e}");
    }

    let result = segment::CompactionResult {
        outcome: segment::CompactionOutcome::Performed,
        segments_removed,
        bytes_reclaimed,
    };
    let report = crate::store::compaction_report::report_for_run(
        config,
        active_segment_id,
        &sealed,
        Some(merged_id),
        &result,
        Some(&merged_path),
    )?;
    Ok((result, report))
}

fn rollback_compaction_disk_state(
    data_dir: &std::path::Path,
    merged_path: &std::path::Path,
    compact_source_path: Option<&std::path::Path>,
    fs: &dyn StoreFs,
) -> Result<(), StoreError> {
    fs.remove_file_if_present(merged_path)
        .map_err(StoreError::Io)?;
    if let Some(temp_source_path) = compact_source_path {
        fs.rename(temp_source_path, merged_path)
            .map_err(StoreError::Io)?;
    }
    crate::store::cold_start::rebuild::clear_pending_compaction(data_dir)?;
    Ok(())
}

struct FailedCompactionCtx<'a> {
    config: &'a CompactionConfig,
    active_segment_id: u64,
    sealed: &'a [(u64, std::path::PathBuf)],
    merged_segment_id: u64,
    data_dir: &'a std::path::Path,
    merged_path: &'a std::path::Path,
    compact_source_path: Option<&'a std::path::Path>,
    error: &'a StoreError,
    context: &'a str,
    fs: &'a dyn StoreFs,
}

fn failed_compaction_with_rollback(
    ctx: &FailedCompactionCtx<'_>,
) -> Result<
    (
        segment::CompactionResult,
        crate::store::compaction_report::CompactionReportBody,
    ),
    StoreError,
> {
    rollback_compaction_disk_state(
        ctx.data_dir,
        ctx.merged_path,
        ctx.compact_source_path,
        ctx.fs,
    )?;
    let reason = format!("{}; disk layout rolled back: {}", ctx.context, ctx.error);
    tracing::error!(target: "batpak::flow", flow = "compact", error = %ctx.error, "{reason}");
    let result = segment::CompactionResult {
        outcome: segment::CompactionOutcome::Failed {
            reason: reason.clone(),
        },
        segments_removed: 0,
        bytes_reclaimed: 0,
    };
    let report = crate::store::compaction_report::report_for_run(
        ctx.config,
        ctx.active_segment_id,
        ctx.sealed,
        Some(ctx.merged_segment_id),
        &result,
        None,
    )?;
    Ok((result, report))
}

fn scan_sealed_entries(
    store: &Store<Open>,
    sealed: &[(u64, std::path::PathBuf)],
) -> Result<Vec<reader::ScannedEntry>, StoreError> {
    let mut all_events = Vec::new();
    for (_, path) in sealed {
        all_events.extend(store.reader.scan_segment(path)?);
    }
    Ok(all_events)
}

fn scanned_entry_as_stored_event(
    entry: &reader::ScannedEntry,
) -> Result<StoredEvent<serde_json::Value>, StoreError> {
    Ok(StoredEvent {
        coordinate: Coordinate::new(&entry.entity, &entry.scope)?,
        event: entry.event.clone(),
    })
}

fn write_scanned_entry(
    merged_segment: &mut segment::Segment<Active>,
    entry: reader::ScannedEntry,
) -> Result<(), StoreError> {
    // Re-emit the survivor's ORIGINAL payload BYTES, never the decoded Value.
    // `entry.event.payload` is the `serde_json::Value` view kept only for the
    // keep/drop predicate; serializing THAT writes a msgpack MAP where the
    // reader's `FramePayload<Vec<u8>>` decode expects raw bytes, making every
    // survivor unreadable ("invalid type: map, expected a sequence"). Rebuilding
    // the frame from `entry.payload_bytes` re-encodes only the outer frame
    // envelope — the user payload is carried verbatim — so a kept frame is
    // byte-identical to the original and its `event_hash` (blake3 over
    // `event.payload`) is byte-stable across compaction. The Tombstone path's
    // in-place `event_kind` mutation rides through `entry.event.header` here.
    let event = Event {
        header: entry.event.header,
        payload: entry.payload_bytes,
        hash_chain: entry.event.hash_chain,
    };
    let frame_payload = FramePayload {
        event,
        entity: entry.entity,
        scope: entry.scope,
        receipt_extensions: entry.receipt_extensions,
    };
    let frame = segment::frame_encode(&frame_payload)?;
    merged_segment.write_frame(&frame)?;
    Ok(())
}

fn relocate_merged_source_if_present(
    store: &Store<Open>,
    sealed: &mut [(u64, std::path::PathBuf)],
    merged_id: u64,
    compact_source_path: &mut Option<std::path::PathBuf>,
) -> Result<(), StoreError> {
    if let Some((_, source_path)) = sealed.iter_mut().find(|(seg_id, _)| *seg_id == merged_id) {
        let fs = store.config.fs();
        let temp_source_path = store.config.data_dir.join(format!(
            "{merged_id:06}.{}.compact-src",
            segment::SEGMENT_EXTENSION
        ));
        fs.remove_file_if_present(&temp_source_path)
            .map_err(StoreError::Io)?;
        fs.rename(&*source_path, &temp_source_path)
            .map_err(StoreError::Io)?;
        *source_path = temp_source_path.clone();
        *compact_source_path = Some(temp_source_path);
    }
    Ok(())
}

fn materialize_compacted_segment(
    store: &Store<Open>,
    strategy: &CompactionStrategy,
    sealed: &mut [(u64, std::path::PathBuf)],
    merged_id: u64,
    merged_path: &std::path::Path,
    compact_source_path: &mut Option<std::path::PathBuf>,
) -> Result<(), StoreError> {
    for (seg_id, _) in sealed.iter() {
        store.reader.evict_segment(*seg_id);
    }

    relocate_merged_source_if_present(store, sealed, merged_id, compact_source_path)?;

    store
        .config
        .fs()
        .remove_file_if_present(merged_path)
        .map_err(StoreError::Io)?;
    let mut merged_segment = segment::Segment::<Active>::create_with_created_ns_on(
        &store.config.data_dir,
        merged_id,
        store.runtime.now_wall_ns(),
        store.config.fs(),
    )?;
    match strategy {
        CompactionStrategy::Merge => {
            for (_, path) in sealed.iter() {
                merged_segment.append_frames_from_segment(path)?;
            }
        }
        CompactionStrategy::Retention(predicate) => {
            for entry in scan_sealed_entries(store, sealed)? {
                if predicate(&scanned_entry_as_stored_event(&entry)?) {
                    write_scanned_entry(&mut merged_segment, entry)?;
                }
            }
        }
        CompactionStrategy::Tombstone(predicate) => {
            let tombstone_kind = EventKind::TOMBSTONE;
            for mut entry in scan_sealed_entries(store, sealed)? {
                if !predicate(&scanned_entry_as_stored_event(&entry)?) {
                    entry.event.header.event_kind = tombstone_kind;
                }
                write_scanned_entry(&mut merged_segment, entry)?;
            }
        }
    }

    merged_segment.sync_with_mode(&store.config.sync.mode)?;
    let _sealed_segment = merged_segment.seal();
    Ok(())
}

fn rebuild_fresh_compaction_index(
    store: &Store<Open>,
) -> Result<crate::store::index::StoreIndex, StoreError> {
    sync(store)?;

    let fresh_index = crate::store::index::StoreIndex::with_config(&store.config.index);
    crate::store::cold_start::rebuild::rebuild_from_segments(
        &fresh_index,
        &store.reader,
        &store.config.data_dir,
    )?;
    if let Some(ranges) =
        crate::store::hidden_ranges::load_cancelled_ranges(&store.config.data_dir)?
    {
        fresh_index.restore_cancelled_visibility_ranges(ranges);
    }

    Ok(fresh_index)
}
