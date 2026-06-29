use crate::event::EventPayloadValidation;
pub(crate) use crate::store::platform::clock::{
    clock_from_fn, wall_ms_from_timestamp_us, Clock, MonotonicClock, SystemClock,
};
pub(crate) use crate::store::platform::fs::{RealFs, StoreFs};
pub(crate) use crate::store::platform::spawn::{Spawn, ThreadSpawn};
use crate::store::signing::SigningKey;
use crate::store::RestartPolicy;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "dangerous-test-hooks")]
use crate::store::fault::FaultInjector;

mod types;
mod validation;

pub use crate::store::index::idemp::{IdempotencyRetention, OverflowPolicy};
pub(crate) use types::WriterMode;
pub use types::{
    BatchConfig, IndexConfig, IndexTopology, OpenReportObserver, SyncConfig, SyncMode, WriterConfig,
};
pub(crate) use validation::ValidatedStoreConfig;

/// StoreConfig: all settings for a Store instance.
/// No Default — callers must provide data_dir via `StoreConfig::new(path)`.
/// Manual Clone and Debug impls because `clock` field is `Arc<dyn Clock>`.
pub struct StoreConfig {
    /// Directory where segment files (.fbat) are stored.
    pub(crate) data_dir: PathBuf,
    /// Maximum bytes per segment file before rotation.
    pub(crate) segment_max_bytes: u64,
    /// Maximum number of open segment file descriptors.
    pub(crate) fd_budget: usize,
    /// Capacity of each subscriber's broadcast channel.
    pub(crate) broadcast_capacity: usize,
    /// Maximum serialized payload plus encoded receipt-extension size for a
    /// single append operation.
    pub(crate) single_append_max_bytes: u32,
    /// Batch append limits and group-commit behavior.
    pub(crate) batch: BatchConfig,
    /// Writer thread channel, stack, restart, and shutdown-drain configuration.
    pub(crate) writer: WriterConfig,
    /// How the writer pipeline is driven (threaded vs. cooperative inline).
    pub(crate) writer_mode: WriterMode,
    /// fsync strategy and cadence.
    pub(crate) sync: SyncConfig,
    /// Secondary query index topology, projection, and checkpoint configuration.
    pub(crate) index: IndexConfig,
    /// Injectable clock for deterministic testing. None = SystemClock.
    pub(crate) clock: Option<Arc<dyn Clock>>,
    /// Spawner for store background threads. Defaults to [`ThreadSpawn`]
    /// (one OS thread per spawn, identical to direct `std::thread` usage).
    /// A deterministic simulation backend can be installed via
    /// [`StoreConfig::with_spawner`].
    pub(crate) spawner: Arc<dyn Spawn>,
    /// Filesystem backend for store data-path operations. Defaults to
    /// [`RealFs`] (every op delegates to `std::fs` via the platform free fns,
    /// identical to direct usage). A deterministic simulation backend can be
    /// installed via [`StoreConfig::with_fs`].
    pub(crate) fs: Arc<dyn StoreFs>,
    /// Optional callback fired once after a successful open completes.
    pub(crate) open_report_observer: Option<OpenReportObserver>,
    /// Optional platform profile record that must match current platform evidence at open.
    pub(crate) platform_profile_path: Option<PathBuf>,
    /// Signing keys known to this store. The last configured key signs new
    /// receipts; earlier keys remain available for verification.
    pub(crate) signing_keys: Vec<SigningKey>,
    /// Payload-registry collision policy applied during `Store::open`.
    pub(crate) event_payload_validation: EventPayloadValidation,
    /// Fault injector for testing failure scenarios.
    /// Only available with the `dangerous-test-hooks` feature.
    #[cfg(feature = "dangerous-test-hooks")]
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "dangerous-test-hooks"))
    )]
    pub(crate) fault_injector: Option<Arc<dyn FaultInjector>>,
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
            writer_mode: WriterMode::default(),
            sync: SyncConfig::default(),
            index: IndexConfig::default(),
            clock: None,
            spawner: Arc::new(ThreadSpawn),
            fs: Arc::new(RealFs),
            open_report_observer: None,
            platform_profile_path: None,
            signing_keys: Vec::new(),
            event_payload_validation: EventPayloadValidation::default(),
            #[cfg(feature = "dangerous-test-hooks")]
            fault_injector: None,
        }
        // Funnel the default spawner through the builder so the install seam is
        // exercised on every construction; a deterministic-sim backend swaps it
        // in via the same builder without touching any spawn site.
        .with_spawner(Arc::new(ThreadSpawn))
        // Funnel the default filesystem backend through the builder too, so the
        // install seam is exercised on every construction; a deterministic-sim
        // backend swaps it in via the same builder without touching call sites.
        .with_fs(Arc::new(RealFs))
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

    /// Set the maximum serialized payload plus encoded receipt-extension size
    /// for a single append.
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
    /// The runtime installs the monotonic wrapper during validation/open so
    /// direct field assignment (`config.clock = ...`) and builder use follow
    /// the same path.
    ///
    /// **Observable scope.** The injected clock controls both the wall-clock
    /// reads used by internal timestamping AND the `Freshness::MaybeStale`
    /// age comparison in the projection pipeline. Tests may fast-forward the
    /// injected clock to observe age-based cache invalidation: a cached
    /// projection returned from an earlier `project()` becomes stale once
    /// the clock advances past `max_stale_ms`, forcing a re-project on the
    /// next call. See G6.
    ///
    /// Negative timestamps are rejected at append/batch execution time with
    /// `StoreError::InvalidClock` rather than being truncated or panicking.
    pub fn with_clock(mut self, clock: Option<Arc<dyn Clock>>) -> Self {
        self.clock = clock;
        self
    }

    /// Install a microsecond wall-clock closure for deterministic tests.
    ///
    /// This is an adapter for older closure-based tests. New callers that need
    /// control over monotonic or boot-epoch observations should implement
    /// [`Clock`] and pass it through [`StoreConfig::with_clock`].
    pub fn with_clock_fn<F>(mut self, clock: F) -> Self
    where
        F: Fn() -> i64 + Send + Sync + 'static,
    {
        self.clock = Some(clock_from_fn(Arc::new(clock)));
        self
    }

    /// Install a callback that observes the structured open report.
    pub fn with_open_report_observer(mut self, observer: Option<OpenReportObserver>) -> Self {
        self.open_report_observer = observer;
        self
    }

    /// Set a platform profile that must verify during store open.
    pub fn with_platform_profile_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.platform_profile_path = Some(path.into());
        self
    }

    /// Clear any configured platform profile.
    pub fn without_platform_profile_path(mut self) -> Self {
        self.platform_profile_path = None;
        self
    }

    /// Add a signing key to the receipt-signature registry.
    pub fn with_signing_key(mut self, signing_key: SigningKey) -> Self {
        self.signing_keys.push(signing_key);
        self
    }

    /// Set the open-time payload-registry collision policy.
    pub fn with_event_payload_validation(mut self, validation: EventPayloadValidation) -> Self {
        self.event_payload_validation = validation;
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

    /// Set the growth-bound policy for the durable idempotency store.
    ///
    /// Default is the window-priority [`IdempotencyRetention::Hybrid`]: a keyed
    /// retry whose original commit is within the window is ALWAYS a no-op,
    /// regardless of compaction or load. See [`IdempotencyRetention`].
    pub fn with_idempotency_retention(mut self, retention: IdempotencyRetention) -> Self {
        self.index.idempotency_retention = retention;
        self
    }

    /// Set the escalation policy when within-window keys alone exceed the soft
    /// cap (residual pigeonhole). Default [`OverflowPolicy::Warn`].
    pub fn with_idempotency_overflow(mut self, overflow: OverflowPolicy) -> Self {
        self.index.idempotency_overflow = overflow;
        self
    }

    /// Set maximum items per batch append. Default: 256.
    pub fn with_batch_max_size(mut self, batch_max_size: u32) -> Self {
        self.batch.max_size = batch_max_size;
        self
    }

    /// Set maximum total payload plus encoded receipt-extension bytes per batch append.
    /// Default: 1MB.
    pub fn with_batch_max_bytes(mut self, batch_max_bytes: u32) -> Self {
        self.batch.max_bytes = batch_max_bytes;
        self
    }

    /// Directory where segment files (`.fbat`) are stored.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Maximum bytes per segment file before rotation.
    pub fn segment_max_bytes(&self) -> u64 {
        self.segment_max_bytes
    }

    /// Maximum number of concurrently open segment file descriptors.
    pub fn fd_budget(&self) -> usize {
        self.fd_budget
    }

    /// Capacity of each subscriber broadcast channel.
    pub fn broadcast_capacity(&self) -> usize {
        self.broadcast_capacity
    }

    /// Maximum serialized payload plus encoded receipt-extension size for a single append.
    pub fn single_append_max_bytes(&self) -> u32 {
        self.single_append_max_bytes
    }

    /// Batch append limits and group-commit behavior.
    pub fn batch(&self) -> &BatchConfig {
        &self.batch
    }

    /// Writer thread channel, stack, restart, and shutdown-drain configuration.
    pub fn writer(&self) -> &WriterConfig {
        &self.writer
    }

    /// fsync strategy and cadence.
    pub fn sync(&self) -> &SyncConfig {
        &self.sync
    }

    /// Secondary query index topology, projection, and checkpoint configuration.
    pub fn index(&self) -> &IndexConfig {
        &self.index
    }

    /// Whether a custom clock has been configured.
    pub fn has_custom_clock(&self) -> bool {
        self.clock.is_some()
    }

    /// Install a custom spawner for store background threads.
    ///
    /// Production uses the default [`ThreadSpawn`] (one OS thread per spawn).
    /// A deterministic simulation backend installs an alternate [`Spawn`] here
    /// without touching any spawn site.
    pub(crate) fn with_spawner(mut self, spawner: Arc<dyn Spawn>) -> Self {
        self.spawner = spawner;
        self
    }

    /// The configured spawner for store background threads.
    pub(crate) fn spawner(&self) -> &Arc<dyn Spawn> {
        &self.spawner
    }

    /// Select how the writer pipeline is driven.
    ///
    /// Production uses the default [`WriterMode::Threaded`] (a dedicated writer
    /// thread). The cooperative mode runs the writer inline on the calling
    /// thread with NO writer thread, for deterministic simulation, and is only
    /// available under `dangerous-test-hooks`.
    #[cfg(feature = "dangerous-test-hooks")]
    pub(crate) fn with_writer_mode(mut self, writer_mode: WriterMode) -> Self {
        self.writer_mode = writer_mode;
        self
    }

    /// How the writer pipeline is driven.
    pub(crate) fn writer_mode(&self) -> WriterMode {
        self.writer_mode
    }

    /// Install a custom filesystem backend for store data-path operations.
    ///
    /// Production uses the default [`RealFs`] (every op delegates to `std::fs`).
    /// A deterministic simulation backend installs an alternate [`StoreFs`] here
    /// without touching routed call sites.
    pub(crate) fn with_fs(mut self, fs: Arc<dyn StoreFs>) -> Self {
        self.fs = fs;
        self
    }

    /// The configured filesystem backend for store data-path operations.
    pub(crate) fn fs(&self) -> &Arc<dyn StoreFs> {
        &self.fs
    }

    /// Optional platform profile path.
    pub fn platform_profile_path(&self) -> Option<&Path> {
        self.platform_profile_path.as_deref()
    }

    /// Payload-registry collision policy applied during `Store::open`.
    pub fn event_payload_validation(&self) -> EventPayloadValidation {
        self.event_payload_validation
    }

    /// Configure a fault injector for dangerous test hooks.
    #[cfg(feature = "dangerous-test-hooks")]
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "dangerous-test-hooks"))
    )]
    pub fn with_fault_injector(mut self, injector: Option<Arc<dyn FaultInjector>>) -> Self {
        self.fault_injector = injector;
        self
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
            writer_mode: self.writer_mode,
            sync: self.sync.clone(),
            index: self.index.clone(),
            clock: self.clock.clone(),
            spawner: Arc::clone(&self.spawner),
            fs: Arc::clone(&self.fs),
            open_report_observer: self.open_report_observer.clone(),
            platform_profile_path: self.platform_profile_path.clone(),
            signing_keys: self.signing_keys.clone(),
            event_payload_validation: self.event_payload_validation,
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
            .field("writer_mode", &self.writer_mode)
            .field("sync", &self.sync)
            .field("index", &self.index)
            .field("clock", &self.clock.as_ref().map(|_| "<clock>"))
            .field("spawner", &"<spawner>")
            .field("fs", &"<fs>")
            .field(
                "open_report_observer",
                &self.open_report_observer.as_ref().map(|_| "<observer>"),
            )
            .field("platform_profile_path", &self.platform_profile_path)
            .field("signing_keys", &self.signing_keys.len())
            .field("event_payload_validation", &self.event_payload_validation)
            .finish()
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

#[cfg(test)]
mod tests;
