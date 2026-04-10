use crate::store::RestartPolicy;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "test-support")]
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

/// Memory layout strategy for the secondary query index.
///
/// - `AoS`: Default. No secondary index — queries use DashMap (struct-per-entry).
///   Best for point lookups and write-heavy workloads.
/// - `SoA`: Parallel sorted arrays per field. Replaces `by_fact` and `scope_entities`
///   DashMaps. Best for scan queries (`by_fact`, `by_scope`). Up to 10x faster for
///   analytical workloads.
/// - `AoSoA8/16/64`: Tiled SoA with cache-line-aligned tiles. Replaces scan DashMaps.
///   Best for SIMD compute and projection replay. Tile size determines vectorization width:
///   8 fills AVX (256-bit), 16 fills AVX-512 or Apple M-series cache line, 64 fills
///   a full x86 cache line of u64s. Const-generic — compiler fully monomorphizes each variant.
#[derive(Clone, Debug, Default)]
pub enum IndexLayout {
    /// Struct-per-entry in DashMap. Current behavior.
    #[default]
    AoS,
    /// Parallel sorted arrays. Replaces by_fact + scope_entities DashMaps.
    SoA,
    /// 8-element tiles. Fits AVX register (256-bit).
    AoSoA8,
    /// 16-element tiles. Fits AVX-512 or Apple M-series cache line (128 bytes).
    AoSoA16,
    /// 64-element tiles. Fits full x86 cache line of u64s.
    AoSoA64,
    /// Hybrid: AoS outer (entity groups via HashMap), SoA inner (events within
    /// each entity stored as parallel arrays). Best for entity-local queries
    /// (stream, project) where per-entity iteration should be cache-friendly.
    /// Matches the ECS archetype pattern: entity lookup is O(1) hash,
    /// event scan within entity is columnar.
    SoAoS,
}

/// StoreConfig: all settings for a Store instance.
/// No Default — callers must provide data_dir via `StoreConfig::new(path)`.
/// Manual Clone and Debug impls because `clock` field is `Arc<dyn Fn>`.
pub struct StoreConfig {
    /// Directory where segment files (.fbat) are stored.
    pub data_dir: PathBuf,
    /// Maximum bytes per segment file before rotation.
    pub segment_max_bytes: u64,
    /// Number of events between periodic fsyncs.
    pub sync_every_n_events: u32,
    /// Maximum number of open segment file descriptors.
    pub fd_budget: usize,
    /// Capacity of the flume channel between callers and the writer thread.
    pub writer_channel_capacity: usize,
    /// Capacity of each subscriber's broadcast channel.
    pub broadcast_capacity: usize,
    /// Maximum size in bytes of the LMDB cache map.
    pub cache_map_size_bytes: usize,
    /// Writer auto-restart policy on panic. `Once` allows 1 restart, `Bounded`
    /// allows N restarts within a time window. See: writer.rs writer_thread_main().
    pub restart_policy: RestartPolicy,
    /// Maximum number of queued append commands drained during shutdown.
    pub shutdown_drain_limit: usize,
    /// Optional writer thread stack size. None = OS default (~8MB on Linux).
    pub writer_stack_size: Option<usize>,
    /// Injectable clock for deterministic testing. Returns microseconds since epoch.
    /// None = std::time::SystemTime::now() (production default).
    pub clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Sync mode: SyncAll (data+metadata, default) or SyncData (data only, faster).
    pub sync_mode: SyncMode,
    /// Maximum Append commands drained per writer loop iteration before issuing
    /// a single fsync (group commit). Default: 1 (per-event sync, backward-compatible).
    /// Set higher for throughput. When > 1, all appends MUST include an idempotency
    /// key — the store returns `StoreError::IdempotencyRequired` otherwise.
    /// Set to 0 for unbounded drain (drain all pending before syncing).
    pub group_commit_max_batch: u32,
    /// Memory layout for the secondary query index. Default: AoS (DashMap only).
    /// SoA and AoSoA variants replace by_fact + scope_entities DashMaps with
    /// cache-friendly columnar storage.
    pub index_layout: IndexLayout,
    /// Enable incremental projection: when the `EventSourced` impl opts in via
    /// `supports_incremental_apply() -> true`, load cached state and apply only
    /// events newer than the cached watermark instead of full replay.
    pub incremental_projection: bool,
    /// Write an index checkpoint on close (and after compact) for fast cold start.
    /// Default: true. Set to false for ephemeral test stores.
    pub enable_checkpoint: bool,
    /// Maximum number of items in a single batch append. Default: 256.
    /// Bounded to prevent unbounded memory use and writer stalls.
    pub batch_max_size: u32,
    /// Maximum total payload bytes in a single batch append. Default: 1MB.
    /// Bounded to prevent pathological batch sizes.
    pub batch_max_bytes: u32,
    /// Fault injector for testing failure scenarios.
    /// Only available with the `test-support` feature.
    #[cfg(feature = "test-support")]
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
            sync_every_n_events: 1000,
            fd_budget: 64,
            writer_channel_capacity: 4096,
            broadcast_capacity: 8192,
            cache_map_size_bytes: 64 * 1024 * 1024,
            restart_policy: RestartPolicy::default(),
            shutdown_drain_limit: 1024,
            writer_stack_size: None,
            batch_max_size: 256,
            batch_max_bytes: 1024 * 1024, // 1MB
            clock: None,
            sync_mode: SyncMode::default(),
            group_commit_max_batch: 1,
            index_layout: IndexLayout::default(),
            incremental_projection: false,
            enable_checkpoint: true,
            #[cfg(feature = "test-support")]
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
        if self.writer_channel_capacity == 0 {
            return Err(crate::store::StoreError::Configuration(
                "writer_channel_capacity must be > 0 (0 creates a rendezvous channel that deadlocks)".into(),
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
        if self.batch_max_size == 0 || self.batch_max_size > 4096 {
            return Err(crate::store::StoreError::Configuration(
                "batch_max_size must be 1..=4096".into(),
            ));
        }
        if self.batch_max_bytes == 0 || self.batch_max_bytes > 16 * 1024 * 1024 {
            return Err(crate::store::StoreError::Configuration(
                "batch_max_bytes must be 1..=16MB".into(),
            ));
        }
        Ok(())
    }

    /// Set the maximum segment file size in bytes before rotation.
    pub fn with_segment_max_bytes(mut self, segment_max_bytes: u64) -> Self {
        self.segment_max_bytes = segment_max_bytes;
        self
    }

    /// Set how many events are written between periodic fsyncs.
    pub fn with_sync_every_n_events(mut self, sync_every_n_events: u32) -> Self {
        self.sync_every_n_events = sync_every_n_events;
        self
    }

    /// Set the maximum number of concurrently open segment file descriptors.
    pub fn with_fd_budget(mut self, fd_budget: usize) -> Self {
        self.fd_budget = fd_budget;
        self
    }

    /// Set the capacity of the writer command channel.
    pub fn with_writer_channel_capacity(mut self, writer_channel_capacity: usize) -> Self {
        self.writer_channel_capacity = writer_channel_capacity;
        self
    }

    /// Set the per-subscriber broadcast channel capacity.
    pub fn with_broadcast_capacity(mut self, broadcast_capacity: usize) -> Self {
        self.broadcast_capacity = broadcast_capacity;
        self
    }

    /// Set the LMDB cache map size in bytes.
    pub fn with_cache_map_size_bytes(mut self, cache_map_size_bytes: usize) -> Self {
        self.cache_map_size_bytes = cache_map_size_bytes;
        self
    }

    /// Set the writer thread restart policy on panic.
    pub fn with_restart_policy(mut self, restart_policy: RestartPolicy) -> Self {
        self.restart_policy = restart_policy;
        self
    }

    /// Set how many pending appends the writer drains before shutting down.
    pub fn with_shutdown_drain_limit(mut self, shutdown_drain_limit: usize) -> Self {
        self.shutdown_drain_limit = shutdown_drain_limit;
        self
    }

    /// Set an explicit stack size for the writer thread; `None` uses the OS default.
    pub fn with_writer_stack_size(mut self, writer_stack_size: Option<usize>) -> Self {
        self.writer_stack_size = writer_stack_size;
        self
    }

    /// Override the clock with a custom function returning microseconds since epoch (for testing).
    pub fn with_clock(mut self, clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>) -> Self {
        self.clock = clock;
        self
    }

    /// Set the fsync strategy used after writes.
    pub fn with_sync_mode(mut self, sync_mode: SyncMode) -> Self {
        self.sync_mode = sync_mode;
        self
    }

    /// Set maximum appends batched before a single fsync (group commit).
    /// Default: 1 (per-event, backward-compatible). When > 1, all appends
    /// must include an idempotency key for crash safety.
    pub fn with_group_commit_max_batch(mut self, group_commit_max_batch: u32) -> Self {
        self.group_commit_max_batch = group_commit_max_batch;
        self
    }

    /// Set the memory layout for the secondary query index.
    pub fn with_index_layout(mut self, index_layout: IndexLayout) -> Self {
        self.index_layout = index_layout;
        self
    }

    /// Enable or disable incremental projection for types that support it.
    pub fn with_incremental_projection(mut self, incremental_projection: bool) -> Self {
        self.incremental_projection = incremental_projection;
        self
    }

    /// Enable or disable index checkpoint on close.
    pub fn with_enable_checkpoint(mut self, enable_checkpoint: bool) -> Self {
        self.enable_checkpoint = enable_checkpoint;
        self
    }

    /// Set maximum items per batch append. Default: 256.
    pub fn with_batch_max_size(mut self, batch_max_size: u32) -> Self {
        self.batch_max_size = batch_max_size;
        self
    }

    /// Set maximum total payload bytes per batch append. Default: 1MB.
    pub fn with_batch_max_bytes(mut self, batch_max_bytes: u32) -> Self {
        self.batch_max_bytes = batch_max_bytes;
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
            sync_every_n_events: self.sync_every_n_events,
            fd_budget: self.fd_budget,
            writer_channel_capacity: self.writer_channel_capacity,
            broadcast_capacity: self.broadcast_capacity,
            cache_map_size_bytes: self.cache_map_size_bytes,
            restart_policy: self.restart_policy.clone(),
            shutdown_drain_limit: self.shutdown_drain_limit,
            writer_stack_size: self.writer_stack_size,
            clock: self.clock.clone(),
            sync_mode: self.sync_mode.clone(),
            group_commit_max_batch: self.group_commit_max_batch,
            index_layout: self.index_layout.clone(),
            incremental_projection: self.incremental_projection,
            enable_checkpoint: self.enable_checkpoint,
            batch_max_size: self.batch_max_size,
            batch_max_bytes: self.batch_max_bytes,
            #[cfg(feature = "test-support")]
            fault_injector: self.fault_injector.clone(),
        }
    }
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConfig")
            .field("data_dir", &self.data_dir)
            .field("segment_max_bytes", &self.segment_max_bytes)
            .field("sync_every_n_events", &self.sync_every_n_events)
            .field("fd_budget", &self.fd_budget)
            .field("writer_channel_capacity", &self.writer_channel_capacity)
            .field("broadcast_capacity", &self.broadcast_capacity)
            .field("cache_map_size_bytes", &self.cache_map_size_bytes)
            .field("restart_policy", &self.restart_policy)
            .field("shutdown_drain_limit", &self.shutdown_drain_limit)
            .field("writer_stack_size", &self.writer_stack_size)
            .field("clock", &self.clock.as_ref().map(|_| "<fn>"))
            .field("sync_mode", &self.sync_mode)
            .field("group_commit_max_batch", &self.group_commit_max_batch)
            .field("index_layout", &self.index_layout)
            .field("incremental_projection", &self.incremental_projection)
            .field("enable_checkpoint", &self.enable_checkpoint)
            .field("batch_max_size", &self.batch_max_size)
            .field("batch_max_bytes", &self.batch_max_bytes)
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
