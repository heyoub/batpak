use crate::store::stats::{LockLeafSymlinkProtection, StoreLockAdmissionSummary};
use crate::store::StoreError;
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub(crate) fn open_store_lock_file(path: &Path) -> Result<File, StoreError> {
    let evidence = crate::store::platform::evidence::lock_leaf_symlink_protection();
    let _admission = admit_store_lock(evidence)?;
    #[cfg(unix)]
    {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(StoreError::Io)
    }

    #[cfg(not(unix))]
    {
        // Best-effort on non-Unix: std has no O_NOFOLLOW equivalent here, so
        // this is check-then-open rather than the Unix branch's atomic open.
        crate::store::platform::fs::reject_symlink_leaf(path, "store lock file")?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(StoreError::Io)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StoreLockAdmission {
    _private: (),
}

pub(crate) fn admit_store_lock(
    evidence: LockLeafSymlinkProtection,
) -> Result<StoreLockAdmission, StoreError> {
    let summary = match evidence {
        LockLeafSymlinkProtection::AtomicNoFollow => StoreLockAdmissionSummary::AtomicNoFollow,
        LockLeafSymlinkProtection::BestEffortCheckThenOpen => {
            StoreLockAdmissionSummary::BestEffortCheckThenOpen
        }
        LockLeafSymlinkProtection::Unknown
        | LockLeafSymlinkProtection::ObservedUnsupported
        | LockLeafSymlinkProtection::ProbeFailed => {
            return Err(StoreError::PlatformAdmissionFailed {
                capability: "store lock symlink-leaf protection",
                reason: format!("lock evidence {evidence:?} is not admissible"),
            });
        }
    };
    let _ = summary;
    Ok(StoreLockAdmission { _private: () })
}
