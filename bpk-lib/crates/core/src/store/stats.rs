use std::cmp::Ordering;
use std::path::PathBuf;

use crate::store::cold_start::rebuild::OpenIndexReport;
use crate::store::RestartPolicy;
use serde::{Deserialize, Serialize};

/// Hybrid logical clock point used by frontier instrumentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct HlcPoint {
    /// HLC wall-clock milliseconds.
    pub wall_ms: u64,
    /// Globally monotonic sequence assigned by the writer.
    pub global_sequence: u64,
}

impl HlcPoint {
    /// Origin point used before any event has been accepted.
    pub const ORIGIN: Self = Self {
        wall_ms: 0,
        global_sequence: 0,
    };

    pub(crate) fn covers_sequence(self, target: Self) -> bool {
        self.global_sequence >= target.global_sequence
    }

    pub(crate) fn max_by_sequence(self, other: Self) -> Self {
        match self.global_sequence.cmp(&other.global_sequence) {
            Ordering::Less => other,
            Ordering::Greater => self,
            Ordering::Equal => self.max(other),
        }
    }

    pub(crate) fn min_by_sequence(self, other: Self) -> Self {
        match self.global_sequence.cmp(&other.global_sequence) {
            Ordering::Less => self,
            Ordering::Greater => other,
            Ordering::Equal => self.min(other),
        }
    }
}

impl Ord for HlcPoint {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.global_sequence.cmp(&other.global_sequence))
    }
}

impl PartialOrd for HlcPoint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Frontier watermark identifiers accepted by synchronous wait APIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[must_use]
pub enum WatermarkKind {
    /// The accepted watermark.
    Accepted,
    /// The written watermark.
    Written,
    /// The durable watermark.
    Durable,
    /// The applied watermark.
    Applied,
    /// The visible watermark.
    Visible,
    /// The emitted watermark.
    Emitted,
}

impl WatermarkKind {
    pub(crate) fn current(self, snapshot: WatermarkSnapshot) -> HlcPoint {
        match self {
            Self::Accepted => snapshot.accepted_hlc,
            Self::Written => snapshot.written_hlc,
            Self::Durable => snapshot.durable_hlc,
            Self::Applied => snapshot.applied_hlc,
            Self::Visible => snapshot.visible_hlc,
            Self::Emitted => snapshot.emitted_hlc,
        }
    }
}

/// Coherent point-in-time copy of the internal frontier watermarks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(crate) struct WatermarkSnapshot {
    /// Highest HLC whose ordering coordinate has been assigned.
    pub accepted_hlc: HlcPoint,
    /// Highest HLC whose frame write returned successfully.
    pub written_hlc: HlcPoint,
    /// Highest HLC covered by a successful sync.
    pub durable_hlc: HlcPoint,
    /// Highest HLC currently visible to query readers.
    pub visible_hlc: HlcPoint,
    /// Highest HLC consumed by an in-process projection fold.
    pub applied_hlc: HlcPoint,
    /// Highest HLC for which broadcast artifacts were attempted.
    pub emitted_hlc: HlcPoint,
    /// Real elapsed age of the oldest currently undurable write, if any.
    pub oldest_pending_write_age_ms: Option<u64>,
}

impl Default for WatermarkSnapshot {
    fn default() -> Self {
        Self {
            accepted_hlc: HlcPoint::ORIGIN,
            written_hlc: HlcPoint::ORIGIN,
            durable_hlc: HlcPoint::ORIGIN,
            visible_hlc: HlcPoint::ORIGIN,
            applied_hlc: HlcPoint::ORIGIN,
            emitted_hlc: HlcPoint::ORIGIN,
            oldest_pending_write_age_ms: None,
        }
    }
}

/// Per-lane operator-facing frontier view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct LaneFrontierView {
    /// Opaque DAG lane id.
    pub lane: u32,
    /// Highest HLC whose ordering coordinate has been assigned on this lane.
    pub accepted_hlc: HlcPoint,
    /// Highest HLC whose frame write returned successfully on this lane.
    pub written_hlc: HlcPoint,
    /// Highest lane HLC covered by the global physical durable point.
    pub durable_hlc: HlcPoint,
    /// Highest HLC currently visible to lane-scoped query readers.
    pub visible_hlc: HlcPoint,
    /// Highest HLC consumed by registered in-process projections for this lane.
    pub applied_hlc: HlcPoint,
    /// Highest HLC for which broadcast artifacts were attempted on this lane.
    pub emitted_hlc: HlcPoint,
    /// Signed sequence-unit gap between visible and durable at snapshot time.
    pub visible_minus_durable_seq: i64,
}

/// Operator-facing frontier view with the current internal watermark surface.
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct FrontierView {
    /// Highest HLC whose ordering coordinate has been assigned.
    pub accepted_hlc: HlcPoint,
    /// Highest HLC whose frame write returned successfully.
    pub written_hlc: HlcPoint,
    /// Highest HLC whose containing segment range has been synced.
    pub durable_hlc: HlcPoint,
    /// Highest HLC currently visible to query readers.
    pub visible_hlc: HlcPoint,
    /// Highest HLC consumed by registered in-process projections.
    pub applied_hlc: HlcPoint,
    /// Highest HLC for which broadcast artifacts were attempted.
    pub emitted_hlc: HlcPoint,
    /// Signed sequence-unit gap between visible and durable at snapshot time.
    pub visible_minus_durable_seq: i64,
    /// Per-lane logical frontier views, sorted by lane id.
    pub lanes: Vec<LaneFrontierView>,
    /// Real elapsed age of the oldest currently undurable write, if any.
    pub oldest_pending_write_age_ms: Option<u64>,
}

impl FrontierView {
    /// Return the logical frontier for one exact DAG lane.
    #[must_use]
    pub fn lane(&self, lane: u32) -> Option<LaneFrontierView> {
        self.lanes.iter().copied().find(|view| view.lane == lane)
    }
}

/// Lightweight runtime statistics snapshot for the store.
#[derive(Clone, Debug)]
#[must_use]
pub struct StoreStats {
    /// Total number of events currently held in the in-memory index.
    pub event_count: usize,
    /// Current value of the global monotonic sequence counter.
    pub global_sequence: u64,
}

/// Snapshot of writer mailbox pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[must_use]
pub struct WriterPressure {
    /// Number of queued commands currently waiting in the writer mailbox.
    pub queue_len: usize,
    /// Configured bounded capacity of the writer mailbox.
    pub capacity: usize,
}

impl WriterPressure {
    /// Fraction of mailbox capacity currently in use.
    pub fn utilization(&self) -> f64 {
        if self.capacity == 0 {
            return 0.0;
        }
        self.queue_len as f64 / self.capacity as f64
    }

    /// Number of free command slots remaining before the mailbox is full.
    pub fn headroom(&self) -> usize {
        self.capacity.saturating_sub(self.queue_len)
    }

    /// True when the mailbox has no queued commands.
    pub fn is_idle(&self) -> bool {
        self.queue_len == 0
    }
}

/// Diagnostic summary of target-sensitive machine-contact posture reported by
/// the private store platform backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub struct PlatformEvidenceSummary {
    /// Host/process-level clock evidence.
    pub host: HostEvidenceSummary,
    /// Store-path and file-operation evidence used by store admission paths.
    pub store_path: StorePathEvidenceSummary,
    /// Store-admitted interpretation of the descriptive evidence.
    pub admission: PlatformAdmissionSummary,
}

/// Host/process-level platform evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub struct HostEvidenceSummary {
    /// Process-local monotonic-clock epoch marker.
    pub process_clock_epoch_marker_ns: u64,
    /// Source used for process-local monotonic freshness metadata.
    pub monotonic_clock: ClockEvidence,
}

/// Store-path and file-operation platform posture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub struct StorePathEvidenceSummary {
    /// Cheap path inspection result for the configured store directory.
    pub path_status: StorePathStatusEvidence,
    /// Parent-directory sync behavior available to atomic persistence helpers.
    pub parent_dir_sync: ParentDirSyncEvidence,
    /// Symlink-leaf protection available for the store lock file.
    pub lock_leaf_symlink_protection: LockLeafSymlinkProtection,
    /// mmap posture for the cold-start index file.
    pub mmap_index: MmapEvidence,
    /// mmap posture for immutable sealed segments.
    pub sealed_segment_mmap: MmapEvidence,
    /// Active-segment positional read posture.
    pub active_segment_read: ActiveSegmentReadEvidence,
}

/// Store-path inspection evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum StorePathStatusEvidence {
    /// The configured store path exists and is a directory.
    ObservedDirectory,
    /// The configured store path does not exist yet.
    UnknownMissing,
    /// The configured store path exists but is not a directory.
    ObservedUnsupportedNotDirectory,
    /// Metadata inspection failed before a stable conclusion was available.
    ProbeFailed {
        /// Human-readable metadata inspection failure.
        reason: String,
    },
}

/// Clock source evidence exposed by the platform backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum ClockEvidence {
    /// A process-local `Instant` anchor is available for monotonic metadata.
    ProcessLocalInstantAnchor,
    /// The clock source has not been inspected.
    Unknown,
    /// Clock probing failed.
    ProbeFailed,
}

/// Parent-directory sync evidence for atomic file replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum ParentDirSyncEvidence {
    /// Unix-style parent-directory fsync is used after rename.
    UnixFsync,
    /// The target has no meaningful directory-fsync surface; rename is the OS boundary.
    RenameOnly,
    /// Parent-directory sync support has not been inspected.
    Unknown,
    /// Parent-directory sync probing failed.
    ProbeFailed,
}

/// Store-lock symlink-leaf protection evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum LockLeafSymlinkProtection {
    /// Unix `O_NOFOLLOW` rejects symlink leaves atomically during lock-file open.
    AtomicNoFollow,
    /// Non-Unix check-then-open fallback; useful evidence, not atomic protection.
    BestEffortCheckThenOpen,
    /// Lock symlink-leaf behavior has not been inspected.
    Unknown,
    /// Lock symlink-leaf behavior is unsupported.
    ObservedUnsupported,
    /// Lock symlink-leaf probing failed.
    ProbeFailed,
}

/// mmap evidence for a specific store use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum MmapEvidence {
    /// File-backed mmap is the admitted mechanism for this use.
    FileBacked,
    /// mmap support has not been inspected.
    Unknown,
    /// mmap is not supported for this use.
    ObservedUnsupported,
    /// mmap probing failed.
    ProbeFailed,
}

/// Active segment positional read evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum ActiveSegmentReadEvidence {
    /// Unix `pread`-style positional reads avoid mutating the file cursor.
    UnixReadAt,
    /// Non-Unix active reads use locked seek+read against the cached descriptor.
    LockedSeekRead,
    /// Active read posture has not been inspected.
    Unknown,
    /// Active read probing failed.
    ProbeFailed,
}

/// Store admission summary derived from platform evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub struct PlatformAdmissionSummary {
    /// Store lock admission.
    pub store_lock: StoreLockAdmissionSummary,
    /// Parent-directory sync admission.
    pub parent_dir_sync: ParentDirSyncAdmissionSummary,
    /// mmap index admission.
    pub mmap_index: MmapAdmissionSummary,
    /// Sealed-segment mmap admission.
    pub sealed_segment_mmap: MmapAdmissionSummary,
}

/// Admitted store-lock posture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum StoreLockAdmissionSummary {
    /// Atomic Unix no-follow lock-file open is admitted.
    AtomicNoFollow,
    /// Best-effort non-Unix check-then-open is admitted and reported.
    BestEffortCheckThenOpen,
    /// Store lock admission failed.
    Rejected,
}

/// Admitted parent-directory sync posture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum ParentDirSyncAdmissionSummary {
    /// Unix parent-directory fsync is admitted.
    UnixFsync,
    /// Rename-only non-Unix posture is admitted and reported.
    RenameOnly,
    /// Parent-directory sync admission failed.
    Rejected,
}

/// Admitted mmap posture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
#[non_exhaustive]
pub enum MmapAdmissionSummary {
    /// File-backed mmap is admitted for this use.
    FileBacked,
    /// mmap admission failed.
    Rejected,
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
    /// Current writer mailbox pressure snapshot.
    pub writer_pressure: WriterPressure,
    /// Narrow frontier observability view.
    pub frontier: FrontierView,
    /// Active scan topology label (`aos`, `scan`, `entity-local`, `tiled`,
    /// `tiled-simd`, `all`, or `hybrid`).
    pub index_topology: &'static str,
    /// Number of tiles in the columnar index (0 for non-tiled layouts).
    pub tile_count: usize,
    /// Structured report from the cold-start open path, if available.
    pub open_report: Option<OpenIndexReport>,
    /// Platform evidence summary reported by the private store platform backend.
    pub platform_evidence: PlatformEvidenceSummary,
}
