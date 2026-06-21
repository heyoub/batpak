//! Carrier + algebra for [`crate::store::lifecycle::fork`]'s directory walk.
//!
//! `fork` is a fold: it walks the source data directory and folds each entry
//! into a [`ForkAccumulator`] via the [`fork_entry`] algebra, then projects the
//! carrier into a [`ForkReportInput`]. Holding that machinery here keeps `fork`
//! itself a thin fold and keeps `lifecycle.rs` under the file-size cap.

use crate::store::file_classification::{ForkStrategy, StoreFileKind};
use crate::store::fork_report::{ForkCopyStrategy, ForkFinding, ForkOptions, ForkStrategyCounts};
use crate::store::platform::fs as platform_fs;
use crate::store::StoreError;

/// Carrier folded across `fork`'s directory walk: each [`fork_entry`] step
/// mutates this accumulator in place, and the final values are projected
/// straight into [`crate::store::fork_report::ForkReportInput`]. Holding the
/// report state here (rather than in a fistful of local `mut`s) is what keeps
/// `fork` itself a thin fold.
#[derive(Default)]
pub(super) struct ForkAccumulator {
    pub(super) shared_segment_ids_sorted: Vec<u64>,
    pub(super) deep_copied_segment_ids_sorted: Vec<u64>,
    pub(super) copied_visibility_ranges_present: bool,
    pub(super) copied_pending_compaction_marker_present: bool,
    pub(super) copied_idempotency_store_present: bool,
    pub(super) strategy_counts: ForkStrategyCounts,
    pub(super) findings: Vec<ForkFinding>,
}

/// Bounded presence-classifier for the `DeepCopyAlways` arm: lifts the
/// exhaustive `StoreFileKind` match out of [`fork_entry`] so the algebra stays
/// flat. Only the three substrate-singleton kinds flip a presence flag; every
/// other variant is a no-op.
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

/// The per-entry algebra of `fork`: classify one source file by its
/// [`ForkStrategy`], perform the (effectful) copy, and fold the outcome into
/// `acc`. Behavior-preserving extraction of `fork`'s former inline loop body.
pub(super) fn fork_entry(
    acc: &mut ForkAccumulator,
    path: &std::path::Path,
    source_kind: &StoreFileKind,
    file_name_display: String,
    dest_path: &std::path::Path,
    active_segment_id: u64,
    options: &ForkOptions,
) -> Result<(), StoreError> {
    match source_kind.fork_strategy(active_segment_id) {
        ForkStrategy::ShareIfPossible => {
            let used = platform_fs::cow_copy_file(
                path,
                dest_path,
                options.use_reflink,
                options.use_hardlink,
            )
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
            let used = platform_fs::cow_copy_file(path, dest_path, false, false)
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
            let used = platform_fs::cow_copy_file(path, dest_path, false, false)
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

fn fork_copy_strategy(used: platform_fs::CowStrategyUsed) -> ForkCopyStrategy {
    match used {
        platform_fs::CowStrategyUsed::Reflink => ForkCopyStrategy::Reflink,
        platform_fs::CowStrategyUsed::Hardlink => ForkCopyStrategy::Hardlink,
        platform_fs::CowStrategyUsed::DeepCopy => ForkCopyStrategy::DeepCopy,
    }
}
