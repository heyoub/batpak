use crate::store::stats::ParentDirSyncEvidence;
use crate::store::{StoreError, SyncMode};
use std::fs::File;
use std::path::Path;

/// Persist `named_temp` as `final_path` with parent-directory fsync.
///
/// The tempfile's contents must already be fsynced by the caller (this
/// function only handles the rename + directory-entry durability). On
/// unix the parent directory is opened and `sync_all`-ed after the
/// rename, so a crash immediately after this returns cannot lose the
/// directory entry that points at the new inode. On non-unix targets
/// `File::sync_all` on a directory is not meaningful, so the parent
/// fsync is skipped — the asymmetry is platform-level, not a shortcut:
/// windows POSIX semantics for directory fsync simply do not exist.
pub(crate) fn persist_temp_with_parent_sync(
    named_temp: tempfile::NamedTempFile,
    final_path: &Path,
    _admission: ParentDirSyncAdmission,
) -> std::io::Result<()> {
    // Fsync the temp file one more time defensively — callers that construct
    // their own NamedTempFile can rely on this helper being the durability
    // boundary.
    {
        let handle = named_temp.as_file();
        handle.sync_all()?;
    }

    named_temp
        .persist(final_path)
        .map_err(|error| error.error)?;

    sync_parent_dir_io(final_path)?;
    Ok(())
}

pub(crate) fn sync_parent_dir(path: &Path) -> Result<(), StoreError> {
    let evidence = crate::store::platform::evidence::parent_dir_sync_evidence();
    let _admission = admit_parent_dir_sync(evidence)?;
    sync_parent_dir_io(path).map_err(StoreError::Io)
}

fn sync_parent_dir_io(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            // Opening the parent dir read-only and calling sync_all is the
            // POSIX-idiomatic way to flush the directory entry for the
            // renamed-in file. Without this, a crash after the rename
            // returns can leave the rename itself undone on disk.
            let dir = File::open(parent)?;
            dir.sync_all()?;
        }
    }
    #[cfg(not(unix))]
    {
        // Non-unix: File::sync_all on a directory handle is not meaningful
        // and returns an error on some platforms. The rename itself is
        // the durability point the OS provides here.
        let _ = path;
    }
    Ok(())
}

pub(crate) fn sync_file_with_mode(file: &File, mode: &SyncMode) -> Result<(), StoreError> {
    match mode {
        SyncMode::SyncAll => file.sync_all().map_err(StoreError::Io),
        SyncMode::SyncData => file.sync_data().map_err(StoreError::Io),
    }
}

pub(crate) fn sync_file_all_io(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ParentDirSyncAdmission {
    _private: (),
}

pub(crate) fn admit_parent_dir_sync(
    evidence: ParentDirSyncEvidence,
) -> Result<ParentDirSyncAdmission, StoreError> {
    match evidence {
        ParentDirSyncEvidence::UnixFsync | ParentDirSyncEvidence::RenameOnly => {}
        ParentDirSyncEvidence::Unknown | ParentDirSyncEvidence::ProbeFailed => {
            return Err(StoreError::PlatformAdmissionFailed {
                capability: "parent directory sync",
                reason: format!("parent directory sync evidence {evidence:?} is not admissible"),
            });
        }
    };
    Ok(ParentDirSyncAdmission { _private: () })
}

pub(crate) fn admit_current_parent_dir_sync() -> Result<ParentDirSyncAdmission, StoreError> {
    admit_parent_dir_sync(crate::store::platform::evidence::parent_dir_sync_evidence())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn sync_file_with_mode_surfaces_platform_sync_errors() -> Result<(), Box<dyn Error>> {
        let file = File::open("/dev/null")?;

        assert!(
            matches!(
                sync_file_with_mode(&file, &SyncMode::SyncAll),
                Err(StoreError::Io(_))
            ),
            "PROPERTY: sync_file_with_mode must map platform sync errors to StoreError::Io, not report success"
        );
        Ok(())
    }
}
