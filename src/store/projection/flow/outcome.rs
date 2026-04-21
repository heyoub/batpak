use crate::store::config::duration_micros;

/// Outcome returned by the internal `project_inner` pipeline.
///
/// Bundles the projected state with the generation at which the state was
/// materialized. The generation is honest — it is:
///   * `slot.generation` on a group-local cache hit,
///   * `plan.generation` (sampled before replay started) on a replay path, or
///   * the probed entity generation on the empty/no-replay-plan path.
///
/// `ProjectionWatcher` persists the returned generation after each successful
/// `recv()`, so a subsequent relevant append cannot be “consumed” while the
/// caller still holds stale state.
#[derive(Debug)]
pub(crate) struct ProjectionOutcome<T> {
    state: Option<T>,
    returned_generation: u64,
}

impl<T> ProjectionOutcome<T> {
    pub(super) fn empty(returned_generation: u64) -> Self {
        Self {
            state: None,
            returned_generation,
        }
    }

    pub(super) fn new(state: Option<T>, returned_generation: u64) -> Self {
        Self {
            state,
            returned_generation,
        }
    }

    /// Consume the outcome and return `(generation, state)`.
    pub(crate) fn into_parts(self) -> (u64, Option<T>) {
        (self.returned_generation, self.state)
    }

    /// Consume just the state, discarding the generation bookkeeping.
    pub(crate) fn into_state(self) -> Option<T> {
        self.state
    }
}

/// Per-phase timing breakdown for the projection pipeline.
/// Only populated when the caller opts in via `project_timed()`.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectionTimings {
    pub plan_build_us: u64,
    pub group_local_lookup_us: u64,
    pub cache_key_build_us: u64,
    pub prefetch_us: u64,
    pub external_cache_probe_us: u64,
    /// Batch read from disk (frame decode + msgpack deser, no coordinate build).
    pub disk_read_us: u64,
    /// Legacy: was StoredEvent -> Event extraction. Now always 0 since
    /// `read_events_batch` returns `Event` directly, skipping coordinates.
    pub event_extract_us: u64,
    pub replay_fold_us: u64,
    pub cache_store_us: u64,
    pub total_us: u64,
}

pub(super) fn record_total_time(
    timings: &mut Option<&mut ProjectionTimings>,
    started_at: std::time::Instant,
) {
    if let Some(t) = timings.as_deref_mut() {
        t.total_us = duration_micros(started_at.elapsed());
    }
}

pub(super) fn record_external_cache_probe_time(
    timings: &mut Option<&mut ProjectionTimings>,
    started_at: std::time::Instant,
) {
    if let Some(t) = timings.as_deref_mut() {
        t.external_cache_probe_us = duration_micros(started_at.elapsed());
    }
}

pub(super) fn finish_projection<T>(
    timings: &mut Option<&mut ProjectionTimings>,
    started_at: std::time::Instant,
    state: Option<T>,
    returned_generation: u64,
) -> ProjectionOutcome<T> {
    record_total_time(timings, started_at);
    ProjectionOutcome::new(state, returned_generation)
}

pub(super) fn finish_empty_projection<T>(
    timings: &mut Option<&mut ProjectionTimings>,
    started_at: std::time::Instant,
    returned_generation: u64,
) -> ProjectionOutcome<T> {
    record_total_time(timings, started_at);
    ProjectionOutcome::empty(returned_generation)
}
