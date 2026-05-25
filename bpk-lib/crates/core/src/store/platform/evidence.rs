use crate::store::stats::{
    ActiveSegmentReadEvidence, ClockEvidence, HostEvidenceSummary, LockLeafSymlinkProtection,
    MmapAdmissionSummary, MmapEvidence, ParentDirSyncAdmissionSummary, ParentDirSyncEvidence,
    PlatformAdmissionSummary, PlatformEvidenceSummary, StoreLockAdmissionSummary,
    StorePathEvidenceSummary, StorePathStatusEvidence,
};
use crate::store::Clock;
use memmap2::MmapOptions;
use std::io::Write;
use std::path::Path;

pub(crate) fn collect_for_store_path(
    data_dir: &Path,
    clock: &dyn Clock,
) -> PlatformEvidenceSummary {
    let mmap_evidence = mmap_evidence_for_store_path(data_dir);
    let store_path = StorePathEvidenceSummary {
        path_status: path_status(data_dir),
        parent_dir_sync: parent_dir_sync_evidence(),
        lock_leaf_symlink_protection: lock_leaf_symlink_protection(),
        mmap_index: mmap_evidence,
        sealed_segment_mmap: mmap_evidence,
        active_segment_read: active_segment_read_evidence(),
    };
    let admission = admission_from_store_path(&store_path);
    PlatformEvidenceSummary {
        host: HostEvidenceSummary {
            process_clock_epoch_marker_ns: clock.process_boot_ns(),
            monotonic_clock: ClockEvidence::ProcessLocalInstantAnchor,
        },
        store_path,
        admission,
    }
}

pub(crate) fn path_status(data_dir: &Path) -> StorePathStatusEvidence {
    match super::fs::metadata(data_dir) {
        Ok(metadata) if metadata.is_dir() => StorePathStatusEvidence::ObservedDirectory,
        Ok(_) => StorePathStatusEvidence::ObservedUnsupportedNotDirectory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            StorePathStatusEvidence::UnknownMissing
        }
        Err(error) => StorePathStatusEvidence::ProbeFailed {
            reason: error.to_string(),
        },
    }
}

pub(crate) fn parent_dir_sync_evidence() -> ParentDirSyncEvidence {
    #[cfg(unix)]
    {
        ParentDirSyncEvidence::UnixFsync
    }
    #[cfg(not(unix))]
    {
        ParentDirSyncEvidence::RenameOnly
    }
}

pub(crate) fn lock_leaf_symlink_protection() -> LockLeafSymlinkProtection {
    #[cfg(unix)]
    {
        LockLeafSymlinkProtection::AtomicNoFollow
    }
    #[cfg(not(unix))]
    {
        LockLeafSymlinkProtection::BestEffortCheckThenOpen
    }
}

pub(crate) fn active_segment_read_evidence() -> ActiveSegmentReadEvidence {
    #[cfg(unix)]
    {
        ActiveSegmentReadEvidence::UnixReadAt
    }
    #[cfg(not(unix))]
    {
        ActiveSegmentReadEvidence::LockedSeekRead
    }
}

pub(crate) fn mmap_evidence_for_store_path(data_dir: &Path) -> MmapEvidence {
    match path_status(data_dir) {
        StorePathStatusEvidence::ObservedDirectory => {}
        StorePathStatusEvidence::ObservedUnsupportedNotDirectory => {
            return MmapEvidence::ObservedUnsupported;
        }
        StorePathStatusEvidence::UnknownMissing => return MmapEvidence::Unknown,
        StorePathStatusEvidence::ProbeFailed { .. } => return MmapEvidence::ProbeFailed,
    }

    let Ok(mut probe) = tempfile::NamedTempFile::new_in(data_dir) else {
        return MmapEvidence::ProbeFailed;
    };
    if probe.write_all(&[0]).and_then(|()| probe.flush()).is_err() {
        return MmapEvidence::ProbeFailed;
    }
    // SAFETY: the probe file is private to this function, one byte long, and
    // dropped immediately after the map attempt. This observes the target mmap
    // mechanism; it does not establish store semantics.
    match unsafe { MmapOptions::new().len(1).map(probe.as_file()) } {
        Ok(_map) => MmapEvidence::FileBacked,
        Err(error) => mmap_map_error_evidence(&error),
    }
}

fn mmap_map_error_evidence(error: &std::io::Error) -> MmapEvidence {
    if matches!(
        error.kind(),
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
    ) {
        return MmapEvidence::ObservedUnsupported;
    }
    MmapEvidence::ProbeFailed
}

pub(crate) fn admission_from_store_path(
    store_path: &StorePathEvidenceSummary,
) -> PlatformAdmissionSummary {
    PlatformAdmissionSummary {
        store_lock: match store_path.lock_leaf_symlink_protection {
            LockLeafSymlinkProtection::AtomicNoFollow => StoreLockAdmissionSummary::AtomicNoFollow,
            LockLeafSymlinkProtection::BestEffortCheckThenOpen => {
                StoreLockAdmissionSummary::BestEffortCheckThenOpen
            }
            LockLeafSymlinkProtection::Unknown
            | LockLeafSymlinkProtection::ObservedUnsupported
            | LockLeafSymlinkProtection::ProbeFailed => StoreLockAdmissionSummary::Rejected,
        },
        parent_dir_sync: match store_path.parent_dir_sync {
            ParentDirSyncEvidence::UnixFsync => ParentDirSyncAdmissionSummary::UnixFsync,
            ParentDirSyncEvidence::RenameOnly => ParentDirSyncAdmissionSummary::RenameOnly,
            ParentDirSyncEvidence::Unknown | ParentDirSyncEvidence::ProbeFailed => {
                ParentDirSyncAdmissionSummary::Rejected
            }
        },
        mmap_index: mmap_admission(store_path.mmap_index),
        sealed_segment_mmap: mmap_admission(store_path.sealed_segment_mmap),
    }
}

fn mmap_admission(evidence: MmapEvidence) -> MmapAdmissionSummary {
    match evidence {
        MmapEvidence::FileBacked => MmapAdmissionSummary::FileBacked,
        MmapEvidence::Unknown | MmapEvidence::ObservedUnsupported | MmapEvidence::ProbeFailed => {
            MmapAdmissionSummary::Rejected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::stats::{
        ActiveSegmentReadEvidence, LockLeafSymlinkProtection, ParentDirSyncEvidence,
        StorePathEvidenceSummary,
    };
    use std::error::Error;

    #[test]
    fn path_status_distinguishes_missing_paths_from_probe_failures() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("missing-store-path");

        assert_eq!(
            path_status(&missing),
            StorePathStatusEvidence::UnknownMissing,
            "PROPERTY: a missing store path is unknown/missing evidence, not a probe failure"
        );
        Ok(())
    }

    #[test]
    fn path_status_distinguishes_regular_files_from_directories() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let file_path = dir.path().join("store-file");
        std::fs::write(&file_path, b"not a directory")?;

        assert_eq!(
            path_status(&file_path),
            StorePathStatusEvidence::ObservedUnsupportedNotDirectory,
            "PROPERTY: an existing non-directory path must be unsupported evidence, not accepted as a store directory"
        );
        Ok(())
    }

    #[test]
    fn mmap_evidence_keeps_missing_paths_unknown() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("missing-store-path");

        assert_eq!(
            mmap_evidence_for_store_path(&missing),
            MmapEvidence::Unknown,
            "PROPERTY: mmap evidence for a missing path must stay Unknown, not ProbeFailed"
        );
        Ok(())
    }

    #[test]
    fn mmap_evidence_rejects_non_directory_store_paths() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let file_path = dir.path().join("store-file");
        std::fs::write(&file_path, b"not a directory")?;

        assert_eq!(
            mmap_evidence_for_store_path(&file_path),
            MmapEvidence::ObservedUnsupported,
            "PROPERTY: mmap probing must not create temp probes under a non-directory store path"
        );
        Ok(())
    }

    #[test]
    fn admission_from_store_path_maps_each_evidence_family_independently() {
        let store_path = StorePathEvidenceSummary {
            path_status: StorePathStatusEvidence::ObservedDirectory,
            parent_dir_sync: ParentDirSyncEvidence::ProbeFailed,
            lock_leaf_symlink_protection: LockLeafSymlinkProtection::ObservedUnsupported,
            mmap_index: MmapEvidence::FileBacked,
            sealed_segment_mmap: MmapEvidence::Unknown,
            active_segment_read: ActiveSegmentReadEvidence::ProbeFailed,
        };

        let admission = admission_from_store_path(&store_path);

        assert_eq!(
            admission.store_lock,
            StoreLockAdmissionSummary::Rejected,
            "PROPERTY: unsupported lock evidence must fail only the lock admission"
        );
        assert_eq!(
            admission.parent_dir_sync,
            ParentDirSyncAdmissionSummary::Rejected,
            "PROPERTY: failed parent-dir sync evidence must fail only the sync admission"
        );
        assert_eq!(
            admission.mmap_index,
            MmapAdmissionSummary::FileBacked,
            "PROPERTY: file-backed mmap evidence remains admitted even when other families reject"
        );
        assert_eq!(
            admission.sealed_segment_mmap,
            MmapAdmissionSummary::Rejected,
            "PROPERTY: unknown sealed-segment mmap evidence must be rejected"
        );
    }

    #[test]
    fn mmap_map_error_classifies_unsupported_and_permission_as_observed_unsupported() {
        assert_eq!(
            mmap_map_error_evidence(&std::io::Error::from(std::io::ErrorKind::Unsupported)),
            MmapEvidence::ObservedUnsupported,
            "PROPERTY: unsupported mmap must be descriptive evidence, not a probe failure"
        );
        assert_eq!(
            mmap_map_error_evidence(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            MmapEvidence::ObservedUnsupported,
            "PROPERTY: permission-denied mmap must be descriptive evidence, not a probe failure"
        );
        assert_eq!(
            mmap_map_error_evidence(&std::io::Error::from(std::io::ErrorKind::Other)),
            MmapEvidence::ProbeFailed,
            "PROPERTY: unrelated mmap errors must remain probe failures"
        );
    }
}
