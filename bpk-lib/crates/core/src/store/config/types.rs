use crate::store::cold_start::rebuild::OpenIndexReport;
use crate::store::index::idemp::{IdempotencyRetention, OverflowPolicy};
use crate::store::RestartPolicy;
use std::sync::Arc;

/// User-supplied hook fired after a successful store open completes.
pub type OpenReportObserver = Arc<dyn Fn(&OpenIndexReport) + Send + Sync>;

/// How the writer pipeline is driven.
///
/// `Threaded` is the production default: a dedicated OS (or sim) thread owns the
/// writer state and pulls commands off the channel. `Cooperative` runs the
/// writer inline on the calling thread — there is NO writer thread; the command
/// queue is pumped whenever a reply is awaited. Cooperative drive is a
/// deterministic-simulation primitive, kept `pub(crate)` (not public API).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WriterMode {
    /// Production: the writer runs on a spawned thread.
    #[default]
    Threaded,
    /// Single-threaded: the writer is driven inline by pumping the queue.
    ///
    /// Only constructible under `dangerous-test-hooks` (via
    /// `StoreConfig::with_writer_mode` / `Store::open_cooperative`): cooperative
    /// drive is a deterministic-simulation primitive, not a production path.
    #[cfg(feature = "dangerous-test-hooks")]
    Cooperative,
}

/// Sync strategy for segment fsync.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum SyncMode {
    /// sync_all: syncs data + metadata (safest, slower)
    #[default]
    SyncAll,
    /// sync_data: syncs data only (faster, sufficient for most use cases)
    SyncData,
}

/// Explicit in-memory scan topology.
///
/// Base AoS maps are always present. This type controls which additional
/// overlays are materialized alongside them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexTopology {
    /// Enable the SoA overlay for broad kind/scope scans.
    soa: bool,
    /// Enable the SoAoS entity-group overlay for entity-local queries.
    entity_groups: bool,
    /// Enable the AoSoA64 tiled overlay for replay/scanning hot loops.
    tiles64: bool,
}

impl IndexTopology {
    /// Base AoS maps only.
    pub fn aos() -> Self {
        Self {
            soa: false,
            entity_groups: false,
            tiles64: false,
        }
    }

    /// Base AoS maps plus the broad-scan SoA overlay.
    pub fn scan() -> Self {
        Self {
            soa: true,
            entity_groups: false,
            tiles64: false,
        }
    }

    /// Base AoS maps plus the entity-local SoAoS overlay.
    pub fn entity_local() -> Self {
        Self {
            soa: false,
            entity_groups: true,
            tiles64: false,
        }
    }

    /// Base AoS maps plus the tiled AoSoA64 overlay (kind-homogeneous, tile-skip).
    pub fn tiled() -> Self {
        Self {
            soa: false,
            entity_groups: false,
            tiles64: true,
        }
    }

    /// Base AoS maps plus every supported overlay.
    pub fn all() -> Self {
        Self {
            soa: true,
            entity_groups: true,
            tiles64: true,
        }
    }

    /// Enable or disable the SoA overlay.
    pub fn with_soa(mut self, enabled: bool) -> Self {
        self.soa = enabled;
        self
    }

    /// Enable or disable the SoAoS entity-group overlay.
    pub fn with_entity_groups(mut self, enabled: bool) -> Self {
        self.entity_groups = enabled;
        self
    }

    /// Enable or disable the AoSoA64 tiled overlay.
    pub fn with_tiles64(mut self, enabled: bool) -> Self {
        self.tiles64 = enabled;
        self
    }

    pub(crate) fn soa_enabled(&self) -> bool {
        self.soa
    }

    pub(crate) fn entity_groups_enabled(&self) -> bool {
        self.entity_groups
    }

    pub(crate) fn tiles64_enabled(&self) -> bool {
        self.tiles64
    }
}

impl Default for IndexTopology {
    fn default() -> Self {
        Self::aos()
    }
}

/// Batch append limits and group-commit behavior.
#[derive(Clone, Debug)]
pub struct BatchConfig {
    /// Maximum number of items in a single batch append.
    pub max_size: u32,
    /// Maximum total payload bytes plus encoded receipt-extension bytes in a
    /// single batch append.
    pub max_bytes: u32,
    /// Maximum Append commands drained per writer loop iteration before issuing
    /// a single fsync (group commit). Default: 1 (per-event sync). When > 1,
    /// all appends MUST include an idempotency key or `StoreError::IdempotencyRequired`
    /// is raised.
    pub group_commit_max_batch: u32,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_size: 256,
            max_bytes: 1024 * 1024,
            group_commit_max_batch: 1,
        }
    }
}

/// Writer thread channel, stack, restart, and shutdown-drain configuration.
#[derive(Clone, Debug)]
pub struct WriterConfig {
    /// Capacity of the flume channel between callers and the writer thread.
    pub channel_capacity: usize,
    /// Soft-pressure threshold, expressed as a percentage of channel capacity.
    /// `try_submit*` returns `Outcome::Retry` once the queued command count
    /// reaches this fraction of the mailbox.
    pub pressure_retry_threshold_pct: u8,
    /// Optional writer thread stack size. None = OS default.
    pub stack_size: Option<usize>,
    /// Writer auto-restart policy on panic.
    pub restart_policy: RestartPolicy,
    /// Maximum number of queued append commands drained during shutdown.
    pub shutdown_drain_limit: usize,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 4096,
            pressure_retry_threshold_pct: 75,
            stack_size: None,
            restart_policy: RestartPolicy::default(),
            shutdown_drain_limit: 1024,
        }
    }
}

/// fsync strategy and cadence.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// Sync mode: SyncAll (data+metadata, default) or SyncData (data only, faster).
    pub mode: SyncMode,
    /// Number of events between periodic fsyncs.
    pub every_n_events: u32,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            mode: SyncMode::default(),
            every_n_events: 1000,
        }
    }
}

/// Secondary query index layout, projection, and checkpoint configuration.
#[derive(Clone, Debug)]
pub struct IndexConfig {
    /// Active in-memory scan topology.
    pub topology: IndexTopology,
    /// Enable incremental projection apply (delta replay from cached watermark).
    pub incremental_projection: bool,
    /// Write an index checkpoint on close (and after compact) for fast cold start.
    pub enable_checkpoint: bool,
    /// Prefer the mmap index artifact on open before checkpoint / segment replay.
    pub enable_mmap_index: bool,
    /// Growth-bound policy for the durable idempotency store. Default is the
    /// window-priority [`IdempotencyRetention::Hybrid`]: a within-window keyed
    /// retry is always a no-op, with a generous soft cap + alarm.
    pub idempotency_retention: IdempotencyRetention,
    /// Escalation policy when within-window keys alone exceed the soft cap.
    /// Default [`OverflowPolicy::Warn`]: log loudly and proceed (the window
    /// always wins on correctness).
    pub idempotency_overflow: OverflowPolicy,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            topology: IndexTopology::default(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
            idempotency_retention: IdempotencyRetention::default(),
            idempotency_overflow: OverflowPolicy::default(),
        }
    }
}
