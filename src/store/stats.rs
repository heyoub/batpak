use std::path::PathBuf;

use crate::store::RestartPolicy;

/// Lightweight runtime statistics snapshot for the store.
#[derive(Clone, Debug)]
#[must_use]
pub struct StoreStats {
    /// Total number of events currently held in the in-memory index.
    pub event_count: usize,
    /// Current value of the global monotonic sequence counter.
    pub global_sequence: u64,
}

/// Detailed diagnostic snapshot of the store's internal configuration and state.
#[derive(Clone, Debug)]
#[must_use]
pub struct StoreDiagnostics {
    /// Total number of events currently held in the in-memory index.
    pub event_count: usize,
    /// Current value of the global monotonic sequence counter (allocator).
    pub global_sequence: u64,
    /// Current visibility watermark (exclusive upper bound).
    /// Entries with `global_sequence < visible_sequence` are returned by read methods.
    pub visible_sequence: u64,
    /// Filesystem path to the directory containing segment files.
    pub data_dir: PathBuf,
    /// Maximum segment file size in bytes before rotation.
    pub segment_max_bytes: u64,
    /// Maximum number of concurrently open segment file descriptors.
    pub fd_budget: usize,
    /// Writer thread restart policy used on panic.
    pub restart_policy: RestartPolicy,
    /// Active index layout name (AoS, SoA, AoSoA8, AoSoA16, AoSoA64, SoAoS).
    pub index_layout: &'static str,
    /// Number of tiles in the columnar index (0 for non-tiled layouts).
    pub tile_count: usize,
}
