use crate::store::{StoreError, StoreLockMode};
use std::fs::{File, OpenOptions};
use std::path::Path;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

pub(crate) const STORE_LOCK_FILENAME: &str = ".batpak.lock";

/// Lifetime-held directory lock for a store root.
///
/// The underlying `File` owns the OS lock. Keeping this guard inside `Store`
/// guarantees the open mode remains reserved for the handle's full lifetime.
pub(crate) struct StoreDirLock {
    _file: File,
}

impl StoreDirLock {
    pub(crate) fn acquire(data_dir: &Path, mode: StoreLockMode) -> Result<Self, StoreError> {
        let canonical_dir = std::fs::canonicalize(data_dir).map_err(StoreError::Io)?;
        let path = canonical_dir.join(STORE_LOCK_FILENAME);
        let file = open_lock_file(&path)?;

        // Wave 1 hardening is intentionally exclusive-only. Read-only handles
        // are rejected while any live owner exists until shared semantics are
        // explicitly designed and tested.
        let lock_result = file.try_lock();

        match lock_result {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::StoreLocked {
                path: canonical_dir,
                mode,
            }),
            Err(std::fs::TryLockError::Error(error)) => Err(StoreError::Io(error)),
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File, StoreError> {
    #[cfg(unix)]
    {
        return OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(StoreError::Io);
    }

    #[cfg(not(unix))]
    {
        crate::store::cold_start::reject_symlink_leaf(path, "store lock file")?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(StoreError::Io)
    }
}
