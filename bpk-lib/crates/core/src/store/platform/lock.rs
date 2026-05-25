use crate::store::stats::LockLeafSymlinkProtection;
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
    match evidence {
        LockLeafSymlinkProtection::AtomicNoFollow
        | LockLeafSymlinkProtection::BestEffortCheckThenOpen => {}
        LockLeafSymlinkProtection::Unknown
        | LockLeafSymlinkProtection::ObservedUnsupported
        | LockLeafSymlinkProtection::ProbeFailed => {
            return Err(StoreError::PlatformAdmissionFailed {
                capability: "store lock symlink-leaf protection",
                reason: format!("lock evidence {evidence:?} is not admissible"),
            });
        }
    };
    Ok(StoreLockAdmission { _private: () })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_lock_admission_accepts_only_reported_lock_protection_modes() {
        for evidence in [
            LockLeafSymlinkProtection::AtomicNoFollow,
            LockLeafSymlinkProtection::BestEffortCheckThenOpen,
        ] {
            assert!(
                admit_store_lock(evidence).is_ok(),
                "PROPERTY: lock evidence {evidence:?} must be admissible"
            );
        }

        for evidence in [
            LockLeafSymlinkProtection::Unknown,
            LockLeafSymlinkProtection::ObservedUnsupported,
            LockLeafSymlinkProtection::ProbeFailed,
        ] {
            let err = match admit_store_lock(evidence) {
                Ok(_) => panic!("PROPERTY: lock evidence {evidence:?} must reject"),
                Err(error) => error,
            };
            assert!(
                matches!(
                    err,
                    StoreError::PlatformAdmissionFailed {
                        capability: "store lock symlink-leaf protection",
                        ..
                    }
                ),
                "expected store-lock admission failure for {evidence:?}, got {err:?}"
            );
        }
    }
}
