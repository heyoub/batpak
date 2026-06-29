//! Point-in-time store snapshot (deep copy of share-safe substrate files).

use super::sync;
use crate::store::cold_start::latest_segment_watermark;
use crate::store::file_classification::StoreFileKind;
use crate::store::snapshot_report::{
    destination_path_digest, snapshot_evidence_report, SnapshotEvidenceReport, SnapshotFileKind,
    SnapshotFinding, SnapshotReportInput,
};
use crate::store::{Open, Store, StoreError};

pub(crate) fn snapshot(
    store: &Store<Open>,
    dest: &std::path::Path,
) -> Result<SnapshotEvidenceReport, StoreError> {
    tracing::debug!(
        target: "batpak::flow",
        flow = "snapshot",
        destination = %dest.display()
    );
    let fs = store.config.fs();
    let _lifecycle = store.lifecycle_gate.lock();
    let snapshot_fence = store.begin_visibility_fence()?;
    let fence_token = snapshot_fence.token();
    sync(store)?;
    store.index.idemp.flush(&store.config.data_dir)?;
    let (source_watermark_segment_id, source_watermark_offset) =
        latest_segment_watermark(&store.config.data_dir)?;
    fs.reject_symlink_leaf(dest, "snapshot destination")?;
    fs.create_dir_all(dest).map_err(StoreError::Io)?;
    let cleared_artifact_count = clear_snapshot_store_artifacts(fs.as_ref(), dest)?;
    let entries = fs
        .read_dir(&store.config.data_dir)
        .map_err(StoreError::Io)?;
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
            fs.reject_symlink_leaf(&dest_path, "snapshot entry")?;
            fs.copy(&path, &dest_path).map_err(StoreError::Io)?;
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
    Ok(snapshot_evidence_report(SnapshotReportInput {
        fence_token,
        source_watermark_segment_id,
        source_watermark_offset,
        copied_segment_ids_sorted,
        copied_visibility_ranges_present,
        copied_pending_compaction_marker_present,
        copied_idempotency_store_present,
        destination_path_digest: destination_path_digest(dest),
        findings,
    })?)
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

pub(super) fn snapshot_destination_should_clear(path: &std::path::Path) -> bool {
    StoreFileKind::from_path(path).should_clear_from_snapshot_destination()
}

pub(super) fn clear_snapshot_store_artifacts(
    fs: &dyn crate::store::platform::fs::StoreFs,
    dest: &std::path::Path,
) -> Result<usize, StoreError> {
    use super::lifecycle_fs::{remove_dir_all_if_present, remove_file_if_present};

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
