//! Shared filesystem helpers for lifecycle snapshot/fork/compact paths.

use crate::store::platform::fs as platform_fs;
use crate::store::StoreError;

pub(super) fn remove_file_if_present(path: &std::path::Path) -> Result<bool, StoreError> {
    platform_fs::remove_file_if_present(path).map_err(StoreError::Io)
}

pub(super) fn remove_dir_all_if_present(path: &std::path::Path) -> Result<bool, StoreError> {
    platform_fs::remove_dir_all_if_present(path).map_err(StoreError::Io)
}
