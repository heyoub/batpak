use crate::store::{platform, StoreError, StoreLockMode};
use std::fs::File;
use std::path::Path;

pub(crate) const STORE_LOCK_FILENAME: &str = ".batpak.lock";

/// Lifetime-held directory lock for a store root.
///
/// The underlying `File` owns the OS lock. Keeping this guard inside `Store`
/// guarantees the open mode remains reserved for the handle's full lifetime.
///
/// # Boundary (advisory lock)
/// This is an **advisory** OS lock (`flock`-class via `File::try_lock`): it only
/// excludes other processes that also take this lock through `acquire`. A
/// process that opens the store path WITHOUT cooperating can still read — and,
/// critically, `mmap` — a sealed segment that this owner assumes is immutable,
/// which violates the mmap immutability SAFETY contract (undefined behavior).
/// Advisory locks are also unreliable on networked filesystems (NFS/CIFS).
/// Single-process exclusion is covered; cross-process behavior is not yet
/// exercised by a two-process test (0.8.3 audit C3).
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
