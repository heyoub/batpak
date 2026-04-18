use crate::store::cold_start::ColdStartPolicy;
use crate::store::RestartPolicy;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

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
    /// Enable the experimental AoSoA64Simd mixed-kind tiled overlay.
    ///
    /// Unlike `tiles64` (kind-homogeneous tiles + tile-skip), `tiles64_simd`
    /// uses mixed-kind tiles with an inline `[u16; 64]` kinds array designed
    /// for auto-vectorizable comparison. These two overlays are mutually
    /// exclusive in practice — enable one or the other, not both.
    tiles64_simd: bool,
}

impl IndexTopology {
    /// Base AoS maps only.
    pub fn aos() -> Self {
        Self {
            soa: false,
            entity_groups: false,
            tiles64: false,
            tiles64_simd: false,
        }
    }

    /// Base AoS maps plus the broad-scan SoA overlay.
    pub fn scan() -> Self {
        Self {
            soa: true,
            entity_groups: false,
            tiles64: false,
            tiles64_simd: false,
        }
    }

    /// Base AoS maps plus the entity-local SoAoS overlay.
    pub fn entity_local() -> Self {
        Self {
            soa: false,
            entity_groups: true,
            tiles64: false,
            tiles64_simd: false,
        }
    }

    /// Base AoS maps plus the tiled AoSoA64 overlay (kind-homogeneous, tile-skip).
    pub fn tiled() -> Self {
        Self {
            soa: false,
            entity_groups: false,
            tiles64: true,
            tiles64_simd: false,
        }
    }

    /// Base AoS maps plus the experimental AoSoA64Simd overlay (mixed-kind, inline
    /// kinds array, auto-vectorizable scan). Benchmarked head-to-head against `tiled`.
    pub fn tiled_simd() -> Self {
        Self {
            soa: false,
            entity_groups: false,
            tiles64: false,
            tiles64_simd: true,
        }
    }

    /// Base AoS maps plus every supported overlay.
    pub fn all() -> Self {
        Self {
            soa: true,
            entity_groups: true,
            tiles64: true,
            tiles64_simd: false,
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

    /// Enable or disable the experimental AoSoA64Simd overlay.
    pub fn with_tiles64_simd(mut self, enabled: bool) -> Self {
        self.tiles64_simd = enabled;
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

    pub(crate) fn tiles64_simd_enabled(&self) -> bool {
        self.tiles64_simd
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

#[derive(Clone, Debug)]
pub(crate) struct ValidatedStoreConfig {
    pub(crate) pressure_retry_threshold: usize,
    pub(crate) require_idempotency_keys: bool,
    pub(crate) incremental_projection: bool,
    pub(crate) cold_start: ColdStartPolicy,
    pub(crate) shutdown_drain_limit: usize,
    pub(crate) group_commit_drain_budget: u32,
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

    /// Build the validated runtime policy derived from the caller-provided config.
    ///
    /// # Errors
    /// Returns `StoreError::Configuration` for invalid field values.
    pub(crate) fn validated(&self) -> Result<ValidatedStoreConfig, crate::store::StoreError> {
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
        let pressure_retry_threshold = self
            .writer
            .channel_capacity
            .saturating_mul(usize::from(self.writer.pressure_retry_threshold_pct))
            .div_ceil(100)
            .max(1);
        let group_commit_drain_budget = if self.batch.group_commit_max_batch == 0 {
            u32::MAX
        } else if self.batch.group_commit_max_batch == 1 {
            0
        } else {
            self.batch.group_commit_max_batch.saturating_sub(1)
        };

        Ok(ValidatedStoreConfig {
            pressure_retry_threshold,
            require_idempotency_keys: self.batch.group_commit_max_batch > 1,
            incremental_projection: self.index.incremental_projection,
            cold_start: ColdStartPolicy::new(
                self.index.enable_checkpoint,
                self.index.enable_mmap_index,
            ),
            shutdown_drain_limit: self.writer.shutdown_drain_limit,
            group_commit_drain_budget,
        })
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

    /// Install a custom clock for deterministic testing.
    ///
    /// `with_clock` installs a [`MonotonicClock`] wrapper — a user-supplied clock
    /// that regresses is clamped non-decreasingly; regressions log a
    /// `tracing::error!` event but do not panic. Callers pass an unwrapped
    /// `Arc<dyn Fn() -> i64>`; the wrapping happens here so the invariant
    /// cannot be bypassed.
    pub fn with_clock(mut self, clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>) -> Self {
        self.clock = clock.map(|raw| {
            let wrapped = MonotonicClock::wrap(raw);
            let f: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || wrapped.now_us());
            f
        });
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

/// Returns microseconds since Unix epoch, saturating to `i64::MAX` if the system
/// clock is beyond year ~292,277 (treat the max value as a clock-malfunction
/// signal). No panic; cache staleness checks downstream see a saturated value
/// and force a replay rather than poisoning the process.
pub(crate) fn now_us() -> i64 {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    i64::try_from(micros).unwrap_or(i64::MAX)
}

/// Process-wide monotonic anchor. Captured on first call; subsequent calls read
/// the elapsed nanoseconds from `Instant::now()` relative to this anchor.
///
/// The anchor couples two facts:
///   1. `anchor_instant`: the `Instant` captured at first call.
///   2. `anchor_boot_ns`: a u64 marker that identifies *this* process's
///      monotonic epoch. Any cached monotonic value persisted to disk and then
///      read back by a different process MUST compare its `process_boot_ns`
///      against this value — mismatch means the monotonic value belongs to a
///      different process's clock and cannot be trusted.
struct MonotonicAnchor {
    anchor_instant: Instant,
    anchor_boot_ns: u64,
}

impl MonotonicAnchor {
    fn get() -> &'static Self {
        use std::sync::OnceLock;
        static ANCHOR: OnceLock<MonotonicAnchor> = OnceLock::new();
        ANCHOR.get_or_init(|| {
            // The boot marker is the wall-clock time at anchor creation, encoded
            // as nanoseconds since Unix epoch and saturated to u64. Two processes
            // booting in the same nanosecond on the same machine would collide,
            // which is acceptable (they would both re-project on mismatch anyway).
            let wall_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let anchor_boot_ns = u64::try_from(wall_ns).unwrap_or(u64::MAX);
            MonotonicAnchor {
                anchor_instant: Instant::now(),
                anchor_boot_ns,
            }
        })
    }
}

/// Returns monotonic nanoseconds since the process-wide anchor. Guaranteed
/// non-decreasing within a single process; meaningless across processes
/// (use [`process_boot_ns`] to detect cross-process comparisons).
///
/// Saturates to `i64::MAX` if the process has been alive for more than
/// ~292 years.
pub(crate) fn now_mono_ns() -> i64 {
    let anchor = MonotonicAnchor::get();
    let elapsed = anchor.anchor_instant.elapsed().as_nanos();
    i64::try_from(elapsed).unwrap_or(i64::MAX)
}

/// Returns this process's monotonic epoch marker. Two processes never share
/// this value (except in the vanishingly unlikely case of same-nanosecond
/// boot); a monotonic value read from disk whose `process_boot_ns` does not
/// match the current one belongs to a prior process and cannot be compared
/// against [`now_mono_ns`].
pub(crate) fn process_boot_ns() -> u64 {
    MonotonicAnchor::get().anchor_boot_ns
}

/// Non-decreasing wrapper around a user-supplied `Fn() -> i64` clock.
///
/// A user clock that regresses (e.g. NTP jump, manual reset) would poison age
/// comparisons — a slot cached at `now=1000` and read at `now=500` would look
/// like it's `-500` µs old, and a naive check can misclassify it. This wrapper
/// clamps each observed value to `max(last, new)`: once we see a value, we
/// never return anything smaller. Regressions emit `tracing::error!` with the
/// previous and new values and return the previous value — the user's clock
/// is broken, but the store keeps running.
#[derive(Clone)]
pub(crate) struct MonotonicClock {
    inner: Arc<dyn Fn() -> i64 + Send + Sync>,
    last: Arc<AtomicI64>,
}

impl MonotonicClock {
    /// Wrap a user-supplied clock function. The returned handle is cloneable
    /// and stores shared state (`AtomicI64`) in an `Arc`, so clones observe the
    /// same non-decreasing sequence.
    pub(crate) fn wrap(inner: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            inner,
            last: Arc::new(AtomicI64::new(i64::MIN)),
        }
    }

    /// Sample the wrapped clock and return a value that is never smaller than
    /// any value previously returned by this [`MonotonicClock`] (or any clone
    /// of it). A regression is logged at `error` level.
    pub(crate) fn now_us(&self) -> i64 {
        let raw = (self.inner)();
        // Compare-and-swap loop: install `raw` if it's newer than `last`,
        // otherwise report a regression and keep the old value.
        loop {
            let prev = self.last.load(Ordering::Acquire);
            if raw >= prev {
                match self
                    .last
                    .compare_exchange(prev, raw, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => return raw,
                    Err(_) => continue, // another thread stored a newer value; retry
                }
            } else {
                tracing::error!("user clock regressed: prev={} new={}", prev, raw);
                return prev;
            }
        }
    }
}

/// Convert an [`Instant::elapsed`] duration to microseconds as `u64`.
///
/// `Duration::as_micros()` returns `u128`; the cast to `u64` would overflow
/// after ~584,942 years of elapsed time. Caps at `u64::MAX` rather than
/// panicking — a saturating ceiling is more useful than a crash for telemetry.
#[inline]
pub(crate) fn duration_micros(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}
