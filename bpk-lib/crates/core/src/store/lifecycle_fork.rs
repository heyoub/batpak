//! Copy-on-write store fork orchestration.

use crate::store::cold_start::latest_segment_watermark;
use crate::store::file_classification::StoreFileKind;
use crate::store::fork_report::ForkCopyStrategy;
use crate::store::fork_report::{
    destination_path_digest as fork_destination_path_digest, fork_evidence_report, ForkFinding,
    ForkOptions, ForkReport, ForkReportInput,
};
use crate::store::platform::fs::StoreFs;
use crate::store::{Open, Store, StoreError};

use super::lifecycle_fs::{remove_dir_all_if_present, remove_file_if_present};
use super::sync;

/// Carrier folded across `fork`'s directory walk.
#[derive(Default)]
pub(super) struct ForkAccumulator {
    pub(super) shared_segment_ids_sorted: Vec<u64>,
    pub(super) deep_copied_segment_ids_sorted: Vec<u64>,
    pub(super) copied_visibility_ranges_present: bool,
    pub(super) copied_pending_compaction_marker_present: bool,
    pub(super) copied_idempotency_store_present: bool,
    pub(super) strategy_counts: crate::store::fork_report::ForkStrategyCounts,
    pub(super) findings: Vec<ForkFinding>,
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
    let fs = store.config.fs();
    let _lifecycle = store.lifecycle_gate.lock();
    let fork_fence = store.begin_visibility_fence()?;
    let fence_token = fork_fence.token();
    sync(store)?;
    store.index.idemp.flush(&store.config.data_dir)?;
    let (source_watermark_segment_id, source_watermark_offset) =
        latest_segment_watermark(&store.config.data_dir)?;
    let active_segment_id = source_watermark_segment_id;

    fs.reject_symlink_leaf(dest, "fork destination")?;
    fs.create_dir_all(dest).map_err(StoreError::Io)?;
    let dest_canon = fs.canonicalize(dest).map_err(StoreError::Io)?;
    let src_canon = fs
        .canonicalize(&store.config.data_dir)
        .map_err(StoreError::Io)?;
    if dest_canon == src_canon {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "fork destination {} resolves to the source data directory",
                dest.display()
            ),
        )));
    }
    let cleared_artifact_count = clear_fork_store_artifacts(fs.as_ref(), dest)?;
    let entries = fs
        .read_dir(&store.config.data_dir)
        .map_err(StoreError::Io)?;

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
        fs.reject_symlink_leaf(&dest_path, "fork entry")?;

        fork_entry(
            fs.as_ref(),
            &mut acc,
            ForkEntry {
                path: &path,
                source_kind: &source_kind,
                file_name_display,
                dest_path: &dest_path,
            },
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

fn clear_fork_store_artifacts(
    fs: &dyn StoreFs,
    dest: &std::path::Path,
) -> Result<usize, StoreError> {
    let entries = fs.read_dir(dest).map_err(StoreError::Io)?;
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        if !StoreFileKind::from_path(&path).should_clear_from_fork_destination() {
            continue;
        }
        let is_dir = fs
            .symlink_metadata(&path)
            .map_err(StoreError::Io)?
            .file_type()
            .is_dir();
        if is_dir {
            removed += usize::from(remove_dir_all_if_present(&path)?);
        } else {
            removed += usize::from(remove_file_if_present(&path)?);
        }
    }
    Ok(removed)
}

fn record_deep_copied_presence(acc: &mut ForkAccumulator, source_kind: &StoreFileKind) {
    match source_kind {
        StoreFileKind::VisibilityRanges => acc.copied_visibility_ranges_present = true,
        StoreFileKind::PendingCompactionMarker => {
            acc.copied_pending_compaction_marker_present = true;
        }
        StoreFileKind::IdempotencyStore => acc.copied_idempotency_store_present = true,
        StoreFileKind::Segment(_)
        | StoreFileKind::MalformedSegment(_)
        | StoreFileKind::Checkpoint
        | StoreFileKind::MmapIndex
        | StoreFileKind::CompactSource
        | StoreFileKind::CursorDirectory
        | StoreFileKind::Other => {}
    }
}

/// One directory entry being forked: its source path/kind, destination path, and
/// the display name used in findings. Bundled so `fork_entry` stays within the
/// argument-count budget.
pub(super) struct ForkEntry<'a> {
    pub(super) path: &'a std::path::Path,
    pub(super) source_kind: &'a StoreFileKind,
    pub(super) file_name_display: String,
    pub(super) dest_path: &'a std::path::Path,
}

pub(super) fn fork_entry(
    fs: &dyn StoreFs,
    acc: &mut ForkAccumulator,
    entry: ForkEntry<'_>,
    active_segment_id: u64,
    options: &ForkOptions,
) -> Result<(), StoreError> {
    use crate::store::file_classification::ForkStrategy;

    let ForkEntry {
        path,
        source_kind,
        file_name_display,
        dest_path,
    } = entry;

    match source_kind.fork_strategy(active_segment_id) {
        ForkStrategy::ShareIfPossible => {
            let used = fs
                .cow_copy_file(path, dest_path, options.copy_preference)
                .map_err(StoreError::Io)?;
            let strategy = fork_copy_strategy(used);
            acc.strategy_counts.record_copy(strategy);
            if let Some(segment_id) = source_kind.segment_id() {
                match strategy {
                    ForkCopyStrategy::Reflink | ForkCopyStrategy::Hardlink => {
                        acc.shared_segment_ids_sorted.push(segment_id.as_u64());
                    }
                    ForkCopyStrategy::DeepCopy => {
                        acc.deep_copied_segment_ids_sorted.push(segment_id.as_u64());
                    }
                }
            }
            acc.findings.push(ForkFinding::FileCopied {
                file_name: file_name_display,
                strategy,
            });
        }
        ForkStrategy::DeepCopyAlways => {
            let used = fs
                .cow_copy_file(path, dest_path, crate::store::CopyPreference::DeepCopyOnly)
                .map_err(StoreError::Io)?;
            let strategy = fork_copy_strategy(used);
            acc.strategy_counts.record_copy(strategy);
            if let Some(segment_id) = source_kind.segment_id() {
                acc.deep_copied_segment_ids_sorted.push(segment_id.as_u64());
            }
            record_deep_copied_presence(acc, source_kind);
            acc.findings.push(ForkFinding::FileCopied {
                file_name: file_name_display,
                strategy,
            });
        }
        ForkStrategy::CacheRegenerable if !options.exclude_caches => {
            let used = fs
                .cow_copy_file(path, dest_path, crate::store::CopyPreference::DeepCopyOnly)
                .map_err(StoreError::Io)?;
            let strategy = fork_copy_strategy(used);
            acc.strategy_counts.record_copy(strategy);
            acc.findings.push(ForkFinding::FileCopied {
                file_name: file_name_display,
                strategy,
            });
        }
        ForkStrategy::CacheRegenerable => {
            acc.strategy_counts.record_cache_regenerable();
            acc.findings.push(ForkFinding::CacheRegenerableExcluded {
                file_name: file_name_display,
            });
        }
        ForkStrategy::Exclude => {
            acc.strategy_counts.record_excluded();
            acc.findings.push(ForkFinding::FileExcluded {
                file_name: file_name_display,
                reason: "not part of the share-safe fork substrate".to_string(),
            });
        }
    }
    Ok(())
}

fn fork_copy_strategy(used: crate::store::platform::fs::CowStrategyUsed) -> ForkCopyStrategy {
    use crate::store::platform::fs::CowStrategyUsed;
    match used {
        CowStrategyUsed::Reflink => ForkCopyStrategy::Reflink,
        CowStrategyUsed::Hardlink => ForkCopyStrategy::Hardlink,
        CowStrategyUsed::DeepCopy => ForkCopyStrategy::DeepCopy,
    }
}
