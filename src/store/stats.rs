use std::cmp::Ordering;
use std::path::PathBuf;

use crate::store::cold_start::rebuild::OpenIndexReport;
use crate::store::RestartPolicy;

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
    /// The durable watermark.
    Durable,
    /// The applied watermark.
    Applied,
    /// The visible watermark.
    Visible,
}

impl WatermarkKind {
    pub(crate) fn current(self, snapshot: WatermarkSnapshot) -> HlcPoint {
        match self {
            Self::Durable => snapshot.durable_hlc,
            Self::Applied => snapshot.applied_hlc,
            Self::Visible => snapshot.visible_hlc,
        }
    }
}

/// Coherent point-in-time copy of the internal frontier watermarks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct WatermarkSnapshot {
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

/// Operator-facing frontier view with the current internal watermark surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct FrontierView {
    /// Highest HLC whose ordering coordinate has been assigned.
    pub accepted_hlc: HlcPoint,
    /// Highest HLC whose frame write returned successfully.
    pub written_hlc: HlcPoint,
    /// Highest HLC whose containing segment range has been synced.
    pub durable_hlc: HlcPoint,
    /// Highest HLC currently visible to query readers.
    pub current_visible_hlc: HlcPoint,
    /// Highest HLC consumed by registered in-process projections.
    pub applied_hlc: HlcPoint,
    /// Highest HLC for which broadcast artifacts were attempted.
    pub emitted_hlc: HlcPoint,
    /// Signed sequence-unit gap between visible and durable at snapshot time.
    pub visible_minus_durable_seq: i64,
    /// Real elapsed age of the oldest currently undurable write, if any.
    pub oldest_pending_write_age_ms: Option<u64>,
}

impl From<WatermarkSnapshot> for FrontierView {
    fn from(snapshot: WatermarkSnapshot) -> Self {
        Self {
            accepted_hlc: snapshot.accepted_hlc,
            written_hlc: snapshot.written_hlc,
            durable_hlc: snapshot.durable_hlc,
            current_visible_hlc: snapshot.visible_hlc,
            applied_hlc: snapshot.applied_hlc,
            emitted_hlc: snapshot.emitted_hlc,
            visible_minus_durable_seq: (snapshot.visible_hlc.global_sequence as i64)
                - (snapshot.durable_hlc.global_sequence as i64),
            oldest_pending_write_age_ms: snapshot.oldest_pending_write_age_ms,
        }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// Active scan topology label (`aos`, `scan`, `entity-local`, `tiled`, `all`, or `hybrid`).
    pub index_topology: &'static str,
    /// Number of tiles in the columnar index (0 for non-tiled layouts).
    pub tile_count: usize,
    /// Structured report from the cold-start open path, if available.
    pub open_report: Option<OpenIndexReport>,
}
