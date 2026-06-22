use crate::coordinate::Coordinate;
use crate::event::{EventKind, StoredEvent};
use crate::store::cold_start::latest_segment_watermark;
use crate::store::file_classification::StoreFileKind;
use crate::store::fork_report::{
    destination_path_digest as fork_destination_path_digest, fork_evidence_report, ForkFinding,
    ForkOptions, ForkReport, ForkReportInput,
};
use crate::store::lifecycle_close::write_cold_start_artifacts_on_close;
use crate::store::lifecycle_fork::{fork_entry, ForkAccumulator};
use crate::store::platform::fs as platform_fs;
use crate::store::segment::scan as reader;
use crate::store::segment::{self, Active, FramePayload};
use crate::store::snapshot_report::{
    destination_path_digest, snapshot_evidence_report, SnapshotEvidenceReport, SnapshotFileKind,
    SnapshotFinding, SnapshotReportInput,
};
use crate::store::write::control::AppendSubmission;
use crate::store::{
    AppendOptions, Closed, CompactionConfig, CompactionStrategy, Open, Store, StoreDiagnostics,
    StoreError, StoreStats, WriterPressure,
};
use serde::Serialize;

#[derive(Serialize)]
struct CloseLifecyclePayload {
    wall_ms: u64,
    global_sequence: u64,
}

fn append_close_completed_event(store: &Store<Open>) -> Result<(), StoreError> {
    let close_hlc = store.watermark_handle.lock().snapshot().visible_hlc;
    let coord = Coordinate::new("batpak:store", "batpak:lifecycle")?;
    let submission = AppendSubmission::with_options(
        AppendOptions::default().with_idempotency(crate::id::IdempotencyKey::from(
            crate::id::generate_v7_id_with_clock(store.runtime.clock()),
        )),
        store.runtime.clock(),
    );
    submission.validate_route(store)?;
    submission.validate_idempotency(store)?;

    let payload = CloseLifecyclePayload {
        wall_ms: close_hlc.wall_ms,
        global_sequence: close_hlc.global_sequence,
    };
    let event = submission.build_event(
        &payload,
        EventKind::SYSTEM_CLOSE_COMPLETED,
        super::timestamp_us_for_hlc(close_hlc)?,
    )?;

    let (tx, rx) = flume::bounded(1);
    let command = submission.into_command(coord, EventKind::SYSTEM_CLOSE_COMPLETED, event, tx);
    store
        .writer_handle()?
        .tx
        .send(command)
        .map_err(|_| StoreError::WriterCrashed)?;
    crate::store::recv_writer_reply(&rx)?;
    Ok(())
}

pub(crate) fn sync(store: &Store<Open>) -> Result<(), StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "sync");
    let (tx, rx) = flume::bounded(1);
    store
        .writer_handle()?
        .tx
        .send(crate::store::write::writer::WriterCommand::Sync { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    crate::store::recv_writer_reply(&rx)
}

pub(crate) fn snapshot(
    store: &Store<Open>,
    dest: &std::path::Path,
) -> Result<SnapshotEvidenceReport, StoreError> {
    tracing::debug!(
        target: "batpak::flow",
        flow = "snapshot",
        destination = %dest.display()
    );
    let _lifecycle = store.lifecycle_gate.lock();
    // Hold a private visibility fence for the duration of the snapshot so
    // concurrent unfenced appends are rejected and a user-held fence cannot
    // race hidden writes into the copied segment set.
    let snapshot_fence = store.begin_visibility_fence()?;
    let fence_token = snapshot_fence.token();
    sync(store)?;
    // Flush the durable idempotency store to disk so the snapshot copies a
    // current `index.idemp`. It is a correctness authority, so a snapshot that
    // dropped it would silently lose cross-compaction dedup memory.
    // justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
    store.index.idemp.flush(&store.config.data_dir)?;
    let (source_watermark_segment_id, source_watermark_offset) =
        latest_segment_watermark(&store.config.data_dir)?;
    platform_fs::reject_symlink_leaf(dest, "snapshot destination")?;
    platform_fs::create_dir_all(dest).map_err(StoreError::Io)?;
    let cleared_artifact_count = clear_snapshot_store_artifacts(store.config.fs().as_ref(), dest)?;
    let entries = platform_fs::read_dir(&store.config.data_dir).map_err(StoreError::Io)?;
    let mut copied_segment_ids_sorted = Vec::new();
    let mut copied_visibility_ranges_present = false;
    let mut copied_pending_compaction_marker_present = false;
    let mut copied_idempotency_store_present = false;
    let mut findings = Vec::new();
    if cleared_artifact_count > 0 {
        findings.push(SnapshotFinding::DestinationCleared {
            artifact_count: cleared_artifact_count,
        });
    }
    for entry in entries {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        let source_kind = StoreFileKind::from_path(&path);
        if let Some(file_kind) = snapshot_source_file_kind(&source_kind) {
            let dest_path = dest.join(entry.file_name());
            platform_fs::reject_symlink_leaf(&dest_path, "snapshot entry")?;
            platform_fs::copy(&path, &dest_path).map_err(StoreError::Io)?;
            match file_kind {
                SnapshotFileKind::Segment => {
                    if let Some(segment_id) = source_kind.segment_id() {
                        copied_segment_ids_sorted.push(segment_id.as_u64());
                    }
                }
                SnapshotFileKind::VisibilityRanges => {
                    copied_visibility_ranges_present = true;
                }
                SnapshotFileKind::PendingCompactionMarker => {
                    copied_pending_compaction_marker_present = true;
                }
                SnapshotFileKind::IdempotencyStore => {
                    // The dedup authority. Recorded in evidence identity (v2) so
                    // a snapshot that lost it cannot masquerade as one that
                    // carried it. justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
                    copied_idempotency_store_present = true;
                }
            }
        }
    }
    snapshot_fence.cancel()?;
    findings.push(SnapshotFinding::FenceTokenCancelled);
    findings.push(SnapshotFinding::CopyByteHashUnavailable {
        reason:
            "snapshot v1 records structural file identity; per-file byte hash table is out of scope"
                .to_string(),
        file_kind: SnapshotFileKind::Segment,
    });
    let report = snapshot_evidence_report(SnapshotReportInput {
        fence_token,
        source_watermark_segment_id,
        source_watermark_offset,
        copied_segment_ids_sorted,
        copied_visibility_ranges_present,
        copied_pending_compaction_marker_present,
        copied_idempotency_store_present,
        destination_path_digest: destination_path_digest(dest),
        findings,
    })?;
    Ok(report)
}

pub(crate) fn fork(
    store: &Store<Open>,
    dest: &std::path::Path,
    options: ForkOptions,
) -> Result<ForkReport, StoreError> {
    tracing::debug!(
        target: "batpak::flow",
        flow = "fork",
        destination = %dest.display()
    );
    let _lifecycle = store.lifecycle_gate.lock();
    let fork_fence = store.begin_visibility_fence()?;
    let fence_token = fork_fence.token();
    sync(store)?;
    store.index.idemp.flush(&store.config.data_dir)?;
    let (source_watermark_segment_id, source_watermark_offset) =
        latest_segment_watermark(&store.config.data_dir)?;
    let active_segment_id = source_watermark_segment_id;

    platform_fs::reject_symlink_leaf(dest, "fork destination")?;
    platform_fs::create_dir_all(dest).map_err(StoreError::Io)?;
    // Refuse forking a store onto its own data dir: the clear pass below would
    // delete the live store's files before copying (data loss).
    let dest_canon = platform_fs::canonicalize(dest).map_err(StoreError::Io)?;
    let src_canon = platform_fs::canonicalize(&store.config.data_dir).map_err(StoreError::Io)?;
    if dest_canon == src_canon {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "fork destination {} resolves to the source data directory",
                dest.display()
            ),
        )));
    }
    let cleared_artifact_count = clear_fork_store_artifacts(dest)?;
    let entries = platform_fs::read_dir(&store.config.data_dir).map_err(StoreError::Io)?;

    let mut acc = ForkAccumulator::default();
    if cleared_artifact_count > 0 {
        acc.findings.push(ForkFinding::DestinationCleared {
            artifact_count: cleared_artifact_count,
        });
    }

    for entry in entries {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        let source_kind = StoreFileKind::from_path(&path);
        let file_name = entry.file_name();
        let file_name_display = file_name.to_string_lossy().into_owned();
        let dest_path = dest.join(&file_name);
        platform_fs::reject_symlink_leaf(&dest_path, "fork entry")?;

        fork_entry(
            &mut acc,
            &path,
            &source_kind,
            file_name_display,
            &dest_path,
            active_segment_id,
            &options,
        )?;
    }

    fork_fence.cancel()?;
    acc.findings.push(ForkFinding::FenceTokenCancelled);
    fork_evidence_report(ForkReportInput {
        fence_token,
        source_watermark_segment_id,
        source_watermark_offset,
        active_segment_id,
        shared_segment_ids_sorted: acc.shared_segment_ids_sorted,
        deep_copied_segment_ids_sorted: acc.deep_copied_segment_ids_sorted,
        strategy_counts: acc.strategy_counts,
        copied_visibility_ranges_present: acc.copied_visibility_ranges_present,
        copied_pending_compaction_marker_present: acc.copied_pending_compaction_marker_present,
        copied_idempotency_store_present: acc.copied_idempotency_store_present,
        destination_path_digest: fork_destination_path_digest(dest),
        findings: acc.findings,
    })
    .map_err(StoreError::from)
}

fn snapshot_source_file_kind(file_kind: &StoreFileKind) -> Option<SnapshotFileKind> {
    if !file_kind.should_copy_into_snapshot() {
        return None;
    }
    match file_kind {
        StoreFileKind::Segment(_) => Some(SnapshotFileKind::Segment),
        StoreFileKind::VisibilityRanges => Some(SnapshotFileKind::VisibilityRanges),
        StoreFileKind::IdempotencyStore => Some(SnapshotFileKind::IdempotencyStore),
        StoreFileKind::PendingCompactionMarker => Some(SnapshotFileKind::PendingCompactionMarker),
        StoreFileKind::MalformedSegment(_)
        | StoreFileKind::Checkpoint
        | StoreFileKind::MmapIndex
        | StoreFileKind::CompactSource
        | StoreFileKind::CursorDirectory
        | StoreFileKind::Other => None,
    }
}

fn snapshot_destination_should_clear(path: &std::path::Path) -> bool {
    StoreFileKind::from_path(path).should_clear_from_snapshot_destination()
}

fn remove_file_if_present(path: &std::path::Path) -> Result<bool, StoreError> {
    platform_fs::remove_file_if_present(path).map_err(StoreError::Io)
}

fn remove_dir_all_if_present(path: &std::path::Path) -> Result<bool, StoreError> {
    platform_fs::remove_dir_all_if_present(path).map_err(StoreError::Io)
}

fn clear_snapshot_store_artifacts(
    fs: &dyn crate::store::platform::fs::StoreFs,
    dest: &std::path::Path,
) -> Result<usize, StoreError> {
    let entries = fs.read_dir(dest).map_err(StoreError::Io)?;
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        if snapshot_destination_should_clear(&path) {
            removed += usize::from(remove_file_if_present(&path)?);
            continue;
        }

        if path.is_dir() && StoreFileKind::from_path(&path) == StoreFileKind::CursorDirectory {
            removed += usize::from(remove_dir_all_if_present(&path)?);
        }
    }
    Ok(removed)
}

fn clear_fork_store_artifacts(dest: &std::path::Path) -> Result<usize, StoreError> {
    let entries = platform_fs::read_dir(dest).map_err(StoreError::Io)?;
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        if !StoreFileKind::from_path(&path).should_clear_from_fork_destination() {
            continue;
        }
        let is_dir = platform_fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_dir())
            .unwrap_or(false);
        if is_dir {
            removed += usize::from(remove_dir_all_if_present(&path)?);
        } else {
            removed += usize::from(remove_file_if_present(&path)?);
        }
    }
    Ok(removed)
}

fn rollback_compaction_disk_state(
    data_dir: &std::path::Path,
    merged_path: &std::path::Path,
    compact_source_path: Option<&std::path::Path>,
) -> Result<(), StoreError> {
    platform_fs::remove_file_if_present(merged_path).map_err(StoreError::Io)?;
    if let Some(temp_source_path) = compact_source_path {
        platform_fs::rename(temp_source_path, merged_path).map_err(StoreError::Io)?;
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
    rollback_compaction_disk_state(ctx.data_dir, ctx.merged_path, ctx.compact_source_path)?;
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
    let frame_payload = FramePayload {
        event: entry.event,
        entity: entry.entity,
        scope: entry.scope,
        receipt_extensions: entry.receipt_extensions,
    };
    let frame = segment::frame_encode(&frame_payload)?;
    merged_segment.write_frame(&frame)?;
    Ok(())
}

/// If a sealed source shares the merged segment's id, move it aside to a
/// `.compact-src` temp path so the new merged segment can claim the canonical
/// name without clobbering its own input. Records the temp path so the caller
/// can clean it up after the swap. Extracted from
/// [`materialize_compacted_segment`] to keep that fn within its complexity budget
/// once the segment-create call carries the `StoreFs` seam argument.
fn relocate_merged_source_if_present(
    store: &Store<Open>,
    sealed: &mut [(u64, std::path::PathBuf)],
    merged_id: u64,
    compact_source_path: &mut Option<std::path::PathBuf>,
) -> Result<(), StoreError> {
    if let Some((_, source_path)) = sealed.iter_mut().find(|(seg_id, _)| *seg_id == merged_id) {
        let temp_source_path = store.config.data_dir.join(format!(
            "{merged_id:06}.{}.compact-src",
            segment::SEGMENT_EXTENSION
        ));
        remove_file_if_present(&temp_source_path)?;
        platform_fs::rename(&*source_path, &temp_source_path).map_err(StoreError::Io)?;
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

    remove_file_if_present(merged_path)?;
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
    // Second drain. We are about to read the on-disk state to build a fresh
    // index off-side — no writer traffic must race past this point.
    sync(store)?;

    // ── OFF-SIDE INDEX BUILD (FREEZE-4 step 1) ────────────────────────
    //
    // Build the replacement index in a sibling allocation. The live index is
    // untouched — readers keep serving pre-compact state; a concurrent
    // cursor/query observes no mid-rebuild cleared view.
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

/// Compact sealed segments.
///
/// # F6 / FREEZE-4 swap sketch
///
/// ```text
///   sync()                              (1) drain writer
///   scan on-disk segments
///   if sealed.len() < min_segments { return Skipped }
///   merge/retain/tombstone into merged_segment       (disk-side work)
///   sync()                              (2) second drain after disk ops
///
///   // OFF-SIDE INDEX BUILD (no mutation of live index)
///   fresh = StoreIndex::with_config(index_cfg)
///   rebuild_from_segments(&fresh, reader, data_dir)
///   if let Some(ranges) = load_cancelled_ranges(data_dir)? {
///       fresh.restore_cancelled_visibility_ranges(ranges)
///   }
///   // If rebuild or hidden-range reload fails here, the live index is still valid.
///   // Callers observe CompactionOutcome::Failed { reason }.
///
///   // SINGLE PUBLISH POINT
///   live.replace_contents_from_fresh(fresh)   (3) swap under write lock
///
///   // CLEANUP (only after the live swap has committed)
///   delete_old_sealed_segments()
///   delete_compact_source_tempfile()
///   delete_pending_compaction_marker()
///   write_cold_start_artifacts_on_close()     (4) refresh fastpaths
/// ```
///
/// The old index is valid and readable at every point before step (3). The
/// new index is live from step (3) onward. A reader either observes one or
/// the other — never a partially cleared or half-rebuilt view.
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
    let _lifecycle = store.lifecycle_gate.lock();
    sync(store)?;

    // Single read_dir: collect all segment IDs and paths, then partition.
    let mut all_segments: Vec<(u64, std::path::PathBuf)> = Vec::new();
    for entry in platform_fs::read_dir(&store.config.data_dir).map_err(StoreError::Io)? {
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
        // Skip signal: zero segments removed, zero bytes reclaimed. No
        // merged_id is fabricated from a zero fallback — the early return
        // happens before any merged-file path is derived, so there is no
        // way for compaction to overwrite segment 0.
        let result = segment::CompactionResult {
            outcome: segment::CompactionOutcome::Skipped,
            segments_removed: 0,
            bytes_reclaimed: 0,
        };
        let report =
            crate::store::compaction_report::report_skipped(config, active_segment_id, &sealed)?;
        return Ok((result, report));
    }

    // sealed.len() >= config.min_segments >= 1 here, so sealed[0] is safe
    // without any unwrap_or fallback. The merged_id is always a real sealed
    // segment id that we are about to replace.
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

    // Pre-swap preparation: materialize the merged segment on disk, then do
    // the second drain + off-side rebuild into a sibling index.
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
            });
        }
    };

    // ── SINGLE SWAP POINT (FREEZE-4 step 2) ───────────────────────────
    //
    // Atomically adopt the fresh index as the live one. Under the
    // `swap_gate` write guard: readers either hold the old index (already
    // in progress on the read guard) or the new one.
    store.index.replace_contents_from_fresh(fresh_index)?;

    // ── SEGMENT CLEANUP AFTER SWAP (FREEZE-4 step 5) ──────────────────
    //
    // Now that the live index reflects the merged segment, it is safe to
    // delete the compacted sealed files. If the process crashes between
    // swap and cleanup, the pending-compaction marker keeps cold-start
    // from re-indexing the superseded sealed sources on next open.
    let mut bytes_reclaimed = 0_u64;
    let mut segments_removed = 0_usize;
    for (_, path) in &sealed {
        if let Ok(meta) = platform_fs::metadata(path) {
            bytes_reclaimed += meta.len();
        }
        platform_fs::remove_file(path).map_err(StoreError::Io)?;
        segments_removed += 1;
    }

    if let Some(temp_source_path) = compact_source_path {
        remove_file_if_present(&temp_source_path)?;
    }
    crate::store::cold_start::rebuild::clear_pending_compaction(&store.config.data_dir)?;

    // Apply the window-priority idempotency eviction at the current frontier,
    // THEN flush. The idemp store is a separate authority from `by_id`, so the
    // index swap above did not touch it: every keyed entry survives compaction
    // even as the underlying event frames are evicted. Eviction here only trims
    // keys that aged out of the window (and out-of-window surplus under the soft
    // cap); within-window keys are structurally guaranteed to remain.
    // justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
    let frontier = store.index.global_sequence();
    // Flag entries whose event frames the retention merge just dropped, before
    // trimming aged-out keys. The reconstruction tuple stays intact; only the
    // disk_pos segment id is stamped with the evicted sentinel.
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

    // Persist the durable idempotency store FIRST and MANDATORILY: compaction
    // has already deleted the source frames, so losing the (now-evicted) sidecar
    // on a crash/reopen would lose within-window dedup memory. Its error is
    // PROPAGATED, never downgraded — it is a correctness primitive, not a
    // fast-open cache. justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
    store.index.idemp.flush(&store.config.data_dir)?;

    // Refresh cold-start artifacts after post-compact rebuild so the next open
    // can take the fast path. This is best-effort: a failed mmap/checkpoint only
    // costs a slower next open, so its error stays a warning.
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

pub(crate) fn close(mut store: Store<Open>) -> Result<Closed, StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "close");
    let _lifecycle = store.lifecycle_gate.lock();
    if let Err(error) = append_close_completed_event(&store) {
        tracing::warn!(
            target: "batpak::flow",
            flow = "close",
            "failed to append SYSTEM_CLOSE_COMPLETED lifecycle event: {error}"
        );
    }

    let (tx, rx) = flume::bounded(1);
    store
        .writer_handle()?
        .tx
        .send(crate::store::write::writer::WriterCommand::Shutdown { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    let result = crate::store::recv_writer_reply(&rx);

    result?;
    store.state.0.join()?;

    // Persist the durable idempotency store FIRST and MANDATORILY, ahead of the
    // best-effort cold-start artifacts. It is a correctness primitive that must
    // survive even when checkpoint/mmap artifacts are disabled or fail.
    // justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
    store.index.idemp.flush(&store.config.data_dir)?;

    // Write cold-start artifacts after writer shutdown (all data fsynced).
    // Explicit close() is the honest durable path, so artifact write failures
    // must surface to the caller instead of being downgraded to a warning.
    write_cold_start_artifacts_on_close(&store)?;

    store.should_shutdown_on_drop = false;
    Ok(Closed)
}

pub(crate) fn stats<State: crate::store::StoreState>(store: &Store<State>) -> StoreStats {
    StoreStats {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
    }
}

pub(crate) fn diagnostics<State: crate::store::StoreState>(
    store: &Store<State>,
) -> StoreDiagnostics {
    let frontier = store.watermark_handle.lock().snapshot_view();
    StoreDiagnostics {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
        visible_sequence: store.index.visible_sequence(),
        data_dir: store.config.data_dir.clone(),
        segment_max_bytes: store.config.segment_max_bytes,
        fd_budget: store.config.fd_budget,
        restart_policy: store.config.writer.restart_policy.clone(),
        writer_pressure: store
            .state
            .writer_queue_len()
            .map(|queue_len| WriterPressure {
                queue_len,
                capacity: store.config.writer.channel_capacity,
            })
            .unwrap_or(WriterPressure {
                queue_len: 0,
                capacity: 0,
            }),
        frontier,
        index_topology: store.index.topology_name(),
        tile_count: store.index.tile_count(),
        open_report: store.open_report.clone(),
        platform_evidence: crate::store::platform::evidence::collect_for_store_path(
            &store.config.data_dir,
            store.runtime.clock(),
        ),
    }
}
