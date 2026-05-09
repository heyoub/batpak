use crate::store::config::duration_micros;
use crate::store::HlcPoint;

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
    applied_sequence: Option<u64>,
    cache_status: ProjectionCacheObservation,
    observed_freshness: ProjectionObservedFreshness,
    input_frontier: Option<HlcPoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionCacheObservation {
    Hit,
    Miss,
    Bypassed,
    Unavailable { reason: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionObservedFreshness {
    Fresh,
    StaleAllowed,
    NotApplicable,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ProjectionFinishObservation {
    pub applied_sequence: u64,
    pub cache_status: ProjectionCacheObservation,
    pub observed_freshness: ProjectionObservedFreshness,
    pub input_frontier: Option<HlcPoint>,
}

impl<T> ProjectionOutcome<T> {
    pub(super) fn empty(
        returned_generation: u64,
        cache_status: ProjectionCacheObservation,
        observed_freshness: ProjectionObservedFreshness,
        input_frontier: Option<HlcPoint>,
    ) -> Self {
        Self {
            state: None,
            returned_generation,
            applied_sequence: None,
            cache_status,
            observed_freshness,
            input_frontier,
        }
    }

    pub(super) fn new(
        state: Option<T>,
        returned_generation: u64,
        applied_sequence: Option<u64>,
        cache_status: ProjectionCacheObservation,
        observed_freshness: ProjectionObservedFreshness,
        input_frontier: Option<HlcPoint>,
    ) -> Self {
        Self {
            state,
            returned_generation,
            applied_sequence,
            cache_status,
            observed_freshness,
            input_frontier,
        }
    }

    pub(super) fn applied_sequence(&self) -> Option<u64> {
        self.applied_sequence
    }

    /// Consume the outcome and return `(generation, state)`.
    pub(crate) fn into_parts(self) -> (u64, Option<T>) {
        (self.returned_generation, self.state)
    }

    /// Consume just the state, discarding the generation bookkeeping.
    pub(crate) fn into_state(self) -> Option<T> {
        self.state
    }

    pub(crate) fn cache_status(&self) -> ProjectionCacheObservation {
        self.cache_status
    }

    pub(crate) fn observed_freshness(&self) -> ProjectionObservedFreshness {
        self.observed_freshness
    }

    pub(crate) fn input_frontier(&self) -> Option<HlcPoint> {
        self.input_frontier
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
    observation: ProjectionFinishObservation,
) -> ProjectionOutcome<T> {
    record_total_time(timings, started_at);
    ProjectionOutcome::new(
        state,
        returned_generation,
        Some(observation.applied_sequence),
        observation.cache_status,
        observation.observed_freshness,
        observation.input_frontier,
    )
}

pub(super) fn finish_empty_projection<T>(
    timings: &mut Option<&mut ProjectionTimings>,
    started_at: std::time::Instant,
    returned_generation: u64,
) -> ProjectionOutcome<T> {
    record_total_time(timings, started_at);
    ProjectionOutcome::empty(
        returned_generation,
        ProjectionCacheObservation::Bypassed,
        ProjectionObservedFreshness::NotApplicable,
        None,
    )
}
