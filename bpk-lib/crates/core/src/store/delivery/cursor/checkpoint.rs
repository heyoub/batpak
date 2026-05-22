use super::Cursor;
use crate::store::delivery::observation::CheckpointId;
use crate::store::platform;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

/// Durable cursor checkpoint.
///
/// Written atomically to `{data_dir}/cursors/{id}.ckpt` via tempfile +
/// parent-directory fsync after every successful batch so a cursor with a
/// `checkpoint_id` resumes from the durable position after a process
/// restart. `process_boot_ns` reserves space for monotonic-clock
/// cross-checks without wiring any clock dependency today — set to
/// `None` when that wiring is not required.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CursorCheckpoint {
    /// Global sequence of the last delivered event.
    ///
    /// When `started` is true, a subsequent poll returns events strictly
    /// after this position.
    pub position: u64,
    /// Whether the cursor has delivered at least one event. A fresh
    /// cursor starts at position 0 with `started = false` so that
    /// global_sequence 0 (a legitimate value) is not skipped.
    pub started: bool,
    /// Process-boot monotonic clock value at the time of the last save.
    /// Reserved for monotonic-clock integration; `None` when not wired.
    pub process_boot_ns: Option<u64>,
    /// Stable identity of the region this checkpoint belongs to.
    ///
    /// Old checkpoints may deserialize with `None`; startup treats that as
    /// a mismatch and fails closed instead of silently resuming an
    /// unscoped checkpoint against an arbitrary region.
    #[serde(default)]
    pub region_identity: Option<String>,
}

impl CursorCheckpoint {
    pub(super) fn from_checkpoint(position: u64, started: bool, region_identity: String) -> Self {
        Self {
            position,
            started,
            process_boot_ns: None,
            region_identity: Some(region_identity),
        }
    }
}

fn cursor_checkpoint_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("cursors")
}

pub(super) fn cursor_checkpoint_path(data_dir: &Path, id: &CheckpointId) -> PathBuf {
    cursor_checkpoint_dir(data_dir).join(format!("{}.ckpt", id.as_str()))
}

#[derive(Clone, Debug)]
pub(super) struct CursorDurableBinding {
    pub(super) data_dir: PathBuf,
    pub(super) id: CheckpointId,
}

impl Cursor {
    /// Load a persisted cursor checkpoint, or `Ok(None)` if none exists.
    ///
    /// # Errors
    /// Returns an I/O error if the checkpoint file exists but cannot be
    /// read. A decoding error yields `io::ErrorKind::InvalidData` so
    /// durable-resume callers can fail closed instead of silently
    /// rewinding to position 0.
    pub fn load_checkpoint(
        data_dir: &Path,
        id: &CheckpointId,
    ) -> std::io::Result<Option<CursorCheckpoint>> {
        let path = cursor_checkpoint_path(data_dir, id);
        let bytes = match platform::fs::read(&path) {
            Ok(b) => b,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match crate::encoding::from_bytes::<CursorCheckpoint>(&bytes) {
            Ok(ckpt) => Ok(Some(ckpt)),
            Err(error) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("cursor checkpoint decode failed: {error}"),
            )),
        }
    }

    /// Persist a cursor checkpoint atomically with a parent-directory
    /// fsync. The cursor-directory is created lazily if it does not
    /// already exist.
    ///
    /// # Errors
    /// Returns any I/O error from temp-file creation, write, fsync, or
    /// rename. Encoding errors are surfaced as `io::Error` with kind
    /// `Other`.
    pub fn save_checkpoint(
        data_dir: &Path,
        id: &CheckpointId,
        ckpt: &CursorCheckpoint,
    ) -> std::io::Result<()> {
        let dir = cursor_checkpoint_dir(data_dir);
        platform::fs::create_dir_all(&dir)?;
        let bytes =
            crate::encoding::to_bytes(ckpt).map_err(|e| std::io::Error::other(e.to_string()))?;
        let final_path = cursor_checkpoint_path(data_dir, id);

        let mut tmp = NamedTempFile::new_in(&dir)?;
        {
            use std::io::Write;
            tmp.write_all(&bytes)?;
            tmp.flush()?;
        }
        // Fsync the temp contents before rename; `persist_temp_with_parent_sync`
        // does a defensive fsync too, but doing it here keeps the
        // durability boundary explicit.
        crate::store::platform::sync::sync_file_all_io(tmp.as_file())?;
        let admission = crate::store::platform::sync::admit_current_parent_dir_sync()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        crate::store::platform::sync::persist_temp_with_parent_sync(tmp, &final_path, admission)?;
        Ok(())
    }
}
