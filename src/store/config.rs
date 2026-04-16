use crate::store::RestartPolicy;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "dangerous-test-hooks")]
use crate::store::fault::FaultInjector;

/// Sync strategy for segment fsync.
#[derive(Clone, Debug, Default)]
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

    /// Base AoS maps plus the tiled AoSoA64 overlay.
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
    /// Maximum total payload bytes in a single batch append.
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
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            topology: IndexTopology::default(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        }
    }
}

/// StoreConfig: all settings for a Store instance.
/// No Default — callers must provide data_dir via `StoreConfig::new(path)`.
/// Manual Clone and Debug impls because `clock` field is `Arc<dyn Fn>`.
pub struct StoreConfig {
    /// Directory where segment files (.fbat) are stored.
    pub data_dir: PathBuf,
    /// Maximum bytes per segment file before rotation.
    pub segment_max_bytes: u64,
    /// Maximum number of open segment file descriptors.
    pub fd_budget: usize,
    /// Capacity of each subscriber's broadcast channel.
    pub broadcast_capacity: usize,
    /// Maximum serialized payload size for a single append operation.
    pub single_append_max_bytes: u32,
    /// Batch append limits and group-commit behavior.
    pub batch: BatchConfig,
    /// Writer thread channel, stack, restart, and shutdown-drain configuration.
    pub writer: WriterConfig,
    /// fsync strategy and cadence.
    pub sync: SyncConfig,
    /// Secondary query index topology, projection, and checkpoint configuration.
    pub index: IndexConfig,
    /// Injectable clock for deterministic testing. Returns microseconds since epoch.
    /// None = std::time::SystemTime::now() (production default).
    pub clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Fault injector for testing failure scenarios.
    /// Only available with the `dangerous-test-hooks` feature.
    #[cfg(feature = "dangerous-test-hooks")]
    pub fault_injector: Option<Arc<dyn FaultInjector>>,
}

impl StoreConfig {
    /// Create a StoreConfig with required data_dir and sensible defaults.
    /// All numeric defaults are documented. Override fields after construction
    /// to tune for your deployment (embedded, server, CLI).
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            segment_max_bytes: 256 * 1024 * 1024,
            fd_budget: 64,
            broadcast_capacity: 8192,
            single_append_max_bytes: 16 * 1024 * 1024,
            batch: BatchConfig::default(),
            writer: WriterConfig::default(),
            sync: SyncConfig::default(),
            index: IndexConfig::default(),
            clock: None,
            #[cfg(feature = "dangerous-test-hooks")]
            fault_injector: None,
        }
    }

    /// Validate config fields. Returns an error for values that would cause
    /// silent breakage (deadlocks, infinite rotation, etc.).
    ///
    /// # Errors
    /// Returns `StoreError::Configuration` for invalid field values.
    pub(crate) fn validate(&self) -> Result<(), crate::store::StoreError> {
        if self.segment_max_bytes == 0 {
            return Err(crate::store::StoreError::Configuration(
                "segment_max_bytes must be > 0".into(),
            ));
        }
        if self.writer.channel_capacity == 0 {
            return Err(crate::store::StoreError::Configuration(
                "writer.channel_capacity must be > 0 (0 creates a rendezvous channel that deadlocks)".into(),
            ));
        }
        if self.writer.pressure_retry_threshold_pct == 0
            || self.writer.pressure_retry_threshold_pct > 100
        {
            return Err(crate::store::StoreError::Configuration(
                "writer.pressure_retry_threshold_pct must be 1..=100".into(),
            ));
        }
        if self.fd_budget == 0 {
            return Err(crate::store::StoreError::Configuration(
                "fd_budget must be > 0".into(),
            ));
        }
        if self.broadcast_capacity == 0 {
            return Err(crate::store::StoreError::Configuration(
                "broadcast_capacity must be > 0 (0 creates rendezvous channels that starve subscribers)".into(),
            ));
        }
        if self.single_append_max_bytes == 0 || self.single_append_max_bytes > 64 * 1024 * 1024 {
            return Err(crate::store::StoreError::Configuration(
                "single_append_max_bytes must be 1..=64MB".into(),
            ));
        }
        if self.batch.max_size == 0 || self.batch.max_size > 4096 {
            return Err(crate::store::StoreError::Configuration(
                "batch.max_size must be 1..=4096".into(),
            ));
        }
        if self.batch.max_bytes == 0 || self.batch.max_bytes > 16 * 1024 * 1024 {
            return Err(crate::store::StoreError::Configuration(
                "batch.max_bytes must be 1..=16MB".into(),
            ));
        }
        // group_commit_max_batch: 0 = unbounded drain (writer drains all pending
        // appends before syncing); 1 = per-event sync (default single-event behavior);
        // N > 1 = drain up to N-1 additional appends before syncing.
        // All values are valid; no range check needed.
        Ok(())
    }

    /// Set the maximum segment file size in bytes before rotation.
    pub fn with_segment_max_bytes(mut self, segment_max_bytes: u64) -> Self {
        self.segment_max_bytes = segment_max_bytes;
        self
    }

    /// Set how many events are written between periodic fsyncs.
    pub fn with_sync_every_n_events(mut self, sync_every_n_events: u32) -> Self {
        self.sync.every_n_events = sync_every_n_events;
        self
    }

    /// Set the maximum number of concurrently open segment file descriptors.
    pub fn with_fd_budget(mut self, fd_budget: usize) -> Self {
        self.fd_budget = fd_budget;
        self
    }

    /// Set the capacity of the writer command channel.
    pub fn with_writer_channel_capacity(mut self, writer_channel_capacity: usize) -> Self {
        self.writer.channel_capacity = writer_channel_capacity;
        self
    }

    /// Set the soft-pressure threshold used by `try_submit*`.
    pub fn with_writer_pressure_retry_threshold_pct(
        mut self,
        pressure_retry_threshold_pct: u8,
    ) -> Self {
        self.writer.pressure_retry_threshold_pct = pressure_retry_threshold_pct;
        self
    }

    /// Set the per-subscriber broadcast channel capacity.
    pub fn with_broadcast_capacity(mut self, broadcast_capacity: usize) -> Self {
        self.broadcast_capacity = broadcast_capacity;
        self
    }

    /// Set the maximum serialized payload size for a single append.
    pub fn with_single_append_max_bytes(mut self, single_append_max_bytes: u32) -> Self {
        self.single_append_max_bytes = single_append_max_bytes;
        self
    }

    /// Set the writer thread restart policy on panic.
    pub fn with_restart_policy(mut self, restart_policy: RestartPolicy) -> Self {
        self.writer.restart_policy = restart_policy;
        self
    }

    /// Set how many pending appends the writer drains before shutting down.
    pub fn with_shutdown_drain_limit(mut self, shutdown_drain_limit: usize) -> Self {
        self.writer.shutdown_drain_limit = shutdown_drain_limit;
        self
    }

    /// Set an explicit stack size for the writer thread; `None` uses the OS default.
    pub fn with_writer_stack_size(mut self, writer_stack_size: Option<usize>) -> Self {
        self.writer.stack_size = writer_stack_size;
        self
    }

    /// Override the clock with a custom function returning microseconds since epoch (for testing).
    pub fn with_clock(mut self, clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>) -> Self {
        self.clock = clock;
        self
    }

    /// Set the fsync strategy used after writes.
    pub fn with_sync_mode(mut self, sync_mode: SyncMode) -> Self {
        self.sync.mode = sync_mode;
        self
    }

    /// Set maximum appends batched before a single fsync.
    /// Default: 1 (per-event sync). When > 1, all appends
    /// must include an idempotency key for crash safety.
    pub fn with_group_commit_max_batch(mut self, group_commit_max_batch: u32) -> Self {
        self.batch.group_commit_max_batch = group_commit_max_batch;
        self
    }

    /// Set the explicit in-memory scan topology.
    pub fn with_index_topology(mut self, index_topology: IndexTopology) -> Self {
        self.index.topology = index_topology;
        self
    }

    /// Enable or disable incremental projection for types that support it.
    pub fn with_incremental_projection(mut self, incremental_projection: bool) -> Self {
        self.index.incremental_projection = incremental_projection;
        self
    }

    /// Enable or disable index checkpoint on close.
    pub fn with_enable_checkpoint(mut self, enable_checkpoint: bool) -> Self {
        self.index.enable_checkpoint = enable_checkpoint;
        self
    }

    /// Enable or disable the mmap-first index artifact on close/open.
    pub fn with_enable_mmap_index(mut self, enable_mmap_index: bool) -> Self {
        self.index.enable_mmap_index = enable_mmap_index;
        self
    }

    /// Set maximum items per batch append. Default: 256.
    pub fn with_batch_max_size(mut self, batch_max_size: u32) -> Self {
        self.batch.max_size = batch_max_size;
        self
    }

    /// Set maximum total payload bytes per batch append. Default: 1MB.
    pub fn with_batch_max_bytes(mut self, batch_max_bytes: u32) -> Self {
        self.batch.max_bytes = batch_max_bytes;
        self
    }

    /// Get current timestamp in microseconds, using the injectable clock if set.
    pub(crate) fn now_us(&self) -> i64 {
        match &self.clock {
            Some(f) => f(),
            None => now_us(),
        }
    }
}

impl Clone for StoreConfig {
    fn clone(&self) -> Self {
        Self {
            data_dir: self.data_dir.clone(),
            segment_max_bytes: self.segment_max_bytes,
            fd_budget: self.fd_budget,
            broadcast_capacity: self.broadcast_capacity,
            single_append_max_bytes: self.single_append_max_bytes,
            batch: self.batch.clone(),
            writer: self.writer.clone(),
            sync: self.sync.clone(),
            index: self.index.clone(),
            clock: self.clock.clone(),
            #[cfg(feature = "dangerous-test-hooks")]
            fault_injector: self.fault_injector.clone(),
        }
    }
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConfig")
            .field("data_dir", &self.data_dir)
            .field("segment_max_bytes", &self.segment_max_bytes)
            .field("fd_budget", &self.fd_budget)
            .field("broadcast_capacity", &self.broadcast_capacity)
            .field("single_append_max_bytes", &self.single_append_max_bytes)
            .field("batch", &self.batch)
            .field("writer", &self.writer)
            .field("sync", &self.sync)
            .field("index", &self.index)
            .field("clock", &self.clock.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

pub(crate) fn now_us() -> i64 {
    // Unix epoch micros fit in i64 for any practical lifetime of this project.
    #[allow(clippy::cast_possible_truncation)]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64
    }
}
