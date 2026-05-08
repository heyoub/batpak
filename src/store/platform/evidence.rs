use crate::store::stats::{
    ActiveSegmentReadEvidence, ClockEvidence, HostEvidenceSummary, LockLeafSymlinkProtection,
    MmapAdmissionSummary, MmapEvidence, ParentDirSyncAdmissionSummary, ParentDirSyncEvidence,
    PlatformAdmissionSummary, PlatformEvidenceSummary, StoreLockAdmissionSummary,
    StorePathEvidenceSummary, StorePathStatusEvidence,
};
use memmap2::MmapOptions;
use std::io::Write;
use std::path::Path;

pub(crate) fn collect_for_store_path(data_dir: &Path) -> PlatformEvidenceSummary {
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
            process_clock_epoch_marker_ns: crate::store::platform::clock::process_boot_ns(),
            monotonic_clock: ClockEvidence::ProcessLocalInstantAnchor,
        },
        store_path,
        admission,
    }
}

pub(crate) fn path_status(data_dir: &Path) -> StorePathStatusEvidence {
    match std::fs::metadata(data_dir) {
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
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            MmapEvidence::ObservedUnsupported
        }
        Err(_) => MmapEvidence::ProbeFailed,
    }
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
}
