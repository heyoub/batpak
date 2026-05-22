use crate::store::{platform, StoreError, StoreLockMode};
use std::fs::File;
use std::path::Path;

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
        let canonical_dir = platform::fs::canonicalize(data_dir).map_err(StoreError::Io)?;
        let path = canonical_dir.join(STORE_LOCK_FILENAME);
        let file = crate::store::platform::lock::open_store_lock_file(&path)?;

        // Store opens are intentionally exclusive. Read-only handles are
        // rejected while any live owner exists until shared semantics are
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
