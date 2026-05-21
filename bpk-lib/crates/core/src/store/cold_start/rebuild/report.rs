use super::load_status::OpenIndexLoadStatus;
use std::collections::BTreeMap;

/// Which cold-start restore strategy was actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OpenIndexPath {
    /// Restored from the mmap snapshot (`index.fbati`) plus tail replay.
    Mmap,
    /// Restored from the checkpoint (`index.ckpt`) plus tail replay.
    Checkpoint,
    /// Full rebuild from segment files (parallel SIDX + sequential active).
    Rebuild,
}

/// Diagnostic output from `open_index()`. Hard truth, not logs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct OpenIndexReport {
    /// Which restore strategy was selected and completed.
    pub path: OpenIndexPath,
    /// Number of entries restored from the snapshot (mmap or checkpoint body).
    pub restored_entries: usize,
    /// Number of entries replayed from the tail after the snapshot watermark.
    pub tail_entries: usize,
    /// Wall-clock microseconds for the entire open_index() call.
    pub elapsed_us: u64,
    /// Microseconds spent in `RestorePlanner::build()` (snapshot load, tail
    /// collection, or full rebuild planning).
    #[serde(default)]
    pub phase_plan_build_us: u64,
    /// Microseconds to replace the string interner from the restore plan.
    #[serde(default)]
    pub phase_interner_us: u64,
    /// Microseconds to install sorted index entries and routing overlays.
    #[serde(default)]
    pub phase_restore_index_us: u64,
    /// Microseconds to load and apply cancelled visibility ranges, if any.
    #[serde(default)]
    pub phase_hidden_ranges_us: u64,
    /// Whether `index.fbati` was not tried, missing, invalid, or used.
    #[serde(default)]
    pub mmap_load_status: OpenIndexLoadStatus,
    /// Diagnostic reason when `index.fbati` was present but invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mmap_invalid_reason: Option<String>,
    /// Whether `index.ckpt` was not tried, missing, invalid, or used.
    #[serde(default)]
    pub checkpoint_load_status: OpenIndexLoadStatus,
    /// Diagnostic reason when `index.ckpt` was present but invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_invalid_reason: Option<String>,
    /// Number of unknown reserved system-kind SIDX/mmap values that fell back
    /// to `DATA` during reopen.
    pub unknown_reserved_system_kind_fallbacks: usize,
    /// Histogram of raw reserved system-kind values encountered during this reopen.
    pub unknown_reserved_system_kind_histogram: BTreeMap<u16, usize>,
    /// Number of unknown reserved effect-kind SIDX/mmap values that fell back
    /// to `EFFECT_ERROR` during reopen.
    pub unknown_reserved_effect_kind_fallbacks: usize,
    /// Histogram of raw reserved effect-kind values encountered during this reopen.
    pub unknown_reserved_effect_kind_histogram: BTreeMap<u16, usize>,
    /// Cumulative number of unknown reserved system-kind fallbacks persisted
    /// through this store's cold-start snapshots, including this reopen.
    pub cumulative_unknown_reserved_system_kind_fallbacks: usize,
    /// Cumulative histogram of raw reserved system-kind values persisted through
    /// this store's cold-start snapshots, including this reopen.
    pub cumulative_unknown_reserved_system_kind_histogram: BTreeMap<u16, usize>,
    /// Cumulative number of unknown reserved effect-kind fallbacks persisted
    /// through this store's cold-start snapshots, including this reopen.
    pub cumulative_unknown_reserved_effect_kind_fallbacks: usize,
    /// Cumulative histogram of raw reserved effect-kind values persisted
    /// through this store's cold-start snapshots, including this reopen.
    pub cumulative_unknown_reserved_effect_kind_histogram: BTreeMap<u16, usize>,
}
