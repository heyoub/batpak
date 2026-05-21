use super::{Clock, MonotonicClock, StoreConfig, SystemClock};
use crate::store::cold_start::ColdStartPolicy;
use crate::store::signing::ReceiptSigningRegistry;
use crate::store::StoreError;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct ValidatedStoreConfig {
    pub(crate) pressure_retry_threshold: usize,
    pub(crate) require_idempotency_keys: bool,
    pub(crate) incremental_projection: bool,
    pub(crate) cold_start: ColdStartPolicy,
    pub(crate) shutdown_drain_limit: usize,
    pub(crate) group_commit_drain_budget: u32,
    pub(crate) signing_registry: ReceiptSigningRegistry,
    clock: Arc<dyn Clock>,
}

impl StoreConfig {
    /// Build the validated runtime policy derived from the caller-provided config.
    ///
    /// # Errors
    /// Returns `StoreError::Configuration` for invalid field values.
    pub(crate) fn validated(&self) -> Result<ValidatedStoreConfig, StoreError> {
        if self.segment_max_bytes == 0 {
            return Err(StoreError::Configuration(
                "segment_max_bytes must be > 0".into(),
            ));
        }
        if self.writer.channel_capacity == 0 {
            return Err(StoreError::Configuration(
                "writer.channel_capacity must be > 0 (0 creates a rendezvous channel that deadlocks)".into(),
            ));
        }
        if self.writer.pressure_retry_threshold_pct == 0
            || self.writer.pressure_retry_threshold_pct > 100
        {
            return Err(StoreError::Configuration(
                "writer.pressure_retry_threshold_pct must be 1..=100".into(),
            ));
        }
        if self.fd_budget == 0 {
            return Err(StoreError::Configuration("fd_budget must be > 0".into()));
        }
        if self.broadcast_capacity == 0 {
            return Err(StoreError::Configuration(
                "broadcast_capacity must be > 0 (0 creates rendezvous channels that starve subscribers)".into(),
            ));
        }
        if self.single_append_max_bytes == 0 || self.single_append_max_bytes > 64 * 1024 * 1024 {
            return Err(StoreError::Configuration(
                "single_append_max_bytes must be 1..=64MB".into(),
            ));
        }
        if self.batch.max_size == 0 || self.batch.max_size > 4096 {
            return Err(StoreError::Configuration(
                "batch.max_size must be 1..=4096".into(),
            ));
        }
        if self.batch.max_bytes == 0 || self.batch.max_bytes > 16 * 1024 * 1024 {
            return Err(StoreError::Configuration(
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
            signing_registry: ReceiptSigningRegistry::from_keys(&self.signing_keys),
            clock: Arc::new(MonotonicClock::wrap(
                self.clock
                    .clone()
                    .unwrap_or_else(|| Arc::new(SystemClock::new())),
            )),
        })
    }
}

impl std::fmt::Debug for ValidatedStoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatedStoreConfig")
            .field("pressure_retry_threshold", &self.pressure_retry_threshold)
            .field("require_idempotency_keys", &self.require_idempotency_keys)
            .field("incremental_projection", &self.incremental_projection)
            .field("cold_start", &self.cold_start)
            .field("shutdown_drain_limit", &self.shutdown_drain_limit)
            .field("group_commit_drain_budget", &self.group_commit_drain_budget)
            .field("signing_registry", &"<registry>")
            .field("clock", &"<monotonic>")
            .finish()
    }
}

impl ValidatedStoreConfig {
    /// Runtime clock source used by open stores and projection/cache freshness.
    ///
    /// Any configured custom clock is wrapped in [`MonotonicClock`] during
    /// validation so direct field assignment cannot bypass the non-decreasing
    /// runtime invariant.
    pub(crate) fn now_us(&self) -> i64 {
        self.clock.now_us()
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    pub(crate) fn clock_arc(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    pub(crate) fn now_wall_ns(&self) -> i64 {
        self.clock.now_wall_ns()
    }

    pub(crate) fn now_mono_ns(&self) -> i64 {
        self.clock.now_mono_ns()
    }

    pub(crate) fn process_boot_ns(&self) -> u64 {
        self.clock.process_boot_ns()
    }

    /// Projection/cache metadata clock source.
    ///
    /// Projection freshness math and cache row timestamps must never persist a
    /// negative wall-clock value. Clamp malformed custom clocks to zero and log
    /// the boundary violation instead of propagating invalid metadata.
    pub(crate) fn cache_now_us(&self) -> i64 {
        let now_us = self.now_us();
        match now_us.cmp(&0) {
            std::cmp::Ordering::Less => {
                tracing::error!(
                    raw_us = now_us,
                    "custom clock returned a negative value; clamping projection/cache metadata timestamp to zero"
                );
                0
            }
            std::cmp::Ordering::Equal | std::cmp::Ordering::Greater => now_us,
        }
    }
}
