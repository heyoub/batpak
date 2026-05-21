use crate::store::cold_start::FileLoad;

/// Status for a cold-start snapshot load attempt.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OpenIndexLoadStatus {
    /// The loader was not attempted for this open.
    #[default]
    NotTried,
    /// The loader ran and the snapshot file was absent.
    Missing,
    /// The loader found a snapshot file, but rejected it.
    Invalid,
    /// The loader accepted the snapshot and used it.
    Loaded,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SnapshotLoadDiagnostics {
    pub(super) mmap_status: OpenIndexLoadStatus,
    pub(super) mmap_invalid_reason: Option<String>,
    pub(super) checkpoint_status: OpenIndexLoadStatus,
    pub(super) checkpoint_invalid_reason: Option<String>,
}

impl SnapshotLoadDiagnostics {
    pub(super) fn record_mmap<T>(&mut self, load: &FileLoad<T>) {
        let (status, reason) = status_and_reason(load);
        self.mmap_status = status;
        self.mmap_invalid_reason = reason;
    }

    pub(super) fn record_checkpoint<T>(&mut self, load: &FileLoad<T>) {
        let (status, reason) = status_and_reason(load);
        self.checkpoint_status = status;
        self.checkpoint_invalid_reason = reason;
    }
}

fn status_and_reason<T>(load: &FileLoad<T>) -> (OpenIndexLoadStatus, Option<String>) {
    match load {
        FileLoad::Missing => (OpenIndexLoadStatus::Missing, None),
        FileLoad::Loaded(_) => (OpenIndexLoadStatus::Loaded, None),
        FileLoad::Invalid { reason } => (OpenIndexLoadStatus::Invalid, Some(reason.clone())),
    }
}
