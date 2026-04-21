use crate::event::EventSourced;
use crate::store::index::columnar::CachedProjectionSlot;
use crate::store::index::ProjectionReplayPlan;
use crate::store::Freshness;
use std::any::TypeId;

/// Internal dispatch strategy for a single project() call.
/// Computed once from known metadata; makes the decision tree explicit and testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionStrategy {
    /// No replay plan exists — the entity has no matching events.
    Empty,
    /// Group-local cache hit is fresh; deserialize and return.
    GroupLocalHit,
    /// Group-local slot exists but is stale; apply delta events incrementally.
    GroupLocalIncremental,
    /// Probe the external cache first, then fall back to full replay.
    ExternalCacheThenReplay,
    /// Skip external cache entirely and go straight to disk replay.
    DirectReplay,
}

#[derive(Debug, Clone)]
pub(super) struct ReplayContext {
    pub(super) plan: ProjectionReplayPlan,
    pub(super) cache_key: Vec<u8>,
    pub(super) watermark: u64,
    /// Wall-clock µs-since-epoch captured at plan build. Used as the
    /// prefetch-hint predicted timestamp so backends can warm the right
    /// row; NOT used as the `cached_at_us` stamp written into the real
    /// cache row. The honest put-time stamp is taken inside
    /// `store_projection_value` right before `ProjectionCache::put`
    /// (see G6). Survives across process restarts via the cache format;
    /// not monotonic on its own.
    pub(super) cached_at_us: i64,
    /// Monotonic ns-since-process-anchor captured at plan build. Only
    /// meaningful within the producing process; readers compare
    /// `process_boot_ns` before trusting age deltas. Same rationale as
    /// `cached_at_us`: used for prefetch prediction, NOT as the stamp
    /// written at put time.
    pub(super) cached_at_mono_ns: i64,
    /// This process's monotonic-epoch marker. Stamped on every cached value
    /// produced by this replay so subsequent reads can detect cross-process
    /// monotonic-clock comparisons.
    pub(super) process_boot_ns: u64,
    pub(super) type_id: TypeId,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedProjection {
    pub(super) replay: ReplayContext,
    pub(super) group_local_slot: Option<CachedProjectionSlot>,
    pub(super) group_local_fresh: bool,
}

#[derive(Debug, Clone)]
pub(super) enum ProjectionPreparation {
    Empty,
    Planned(PreparedProjection),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ReplayExecution<'a> {
    pub(super) entity: &'a str,
    pub(super) freshness: &'a Freshness,
    pub(super) replay: &'a ReplayContext,
    pub(super) started_at: std::time::Instant,
}

#[derive(Debug, Clone)]
pub(super) enum ProjectionDispatch {
    Empty,
    GroupLocalHit {
        slot: CachedProjectionSlot,
        replay: ReplayContext,
    },
    GroupLocalIncremental {
        slot: CachedProjectionSlot,
        replay: ReplayContext,
    },
    ExternalCacheThenReplay {
        replay: ReplayContext,
    },
    DirectReplay {
        replay: ReplayContext,
    },
}

impl ProjectionDispatch {
    pub(super) fn strategy(&self) -> ProjectionStrategy {
        match self {
            Self::Empty => ProjectionStrategy::Empty,
            Self::GroupLocalHit { .. } => ProjectionStrategy::GroupLocalHit,
            Self::GroupLocalIncremental { .. } => ProjectionStrategy::GroupLocalIncremental,
            Self::ExternalCacheThenReplay { .. } => ProjectionStrategy::ExternalCacheThenReplay,
            Self::DirectReplay { .. } => ProjectionStrategy::DirectReplay,
        }
    }
}

pub(super) fn replay_execution<'a>(
    entity: &'a str,
    freshness: &'a Freshness,
    replay: &'a ReplayContext,
    started_at: std::time::Instant,
) -> ReplayExecution<'a> {
    ReplayExecution {
        entity,
        freshness,
        replay,
        started_at,
    }
}

impl PreparedProjection {
    pub(super) fn dispatch<T: EventSourced>(
        self,
        incremental_enabled: bool,
        cache_is_noop: bool,
    ) -> ProjectionDispatch {
        let strategy = compute_strategy(
            self.group_local_slot.as_ref(),
            self.group_local_fresh,
            T::supports_incremental_apply(),
            incremental_enabled,
            cache_is_noop,
        );

        match (strategy, self.group_local_slot) {
            (ProjectionStrategy::GroupLocalHit, Some(slot)) => ProjectionDispatch::GroupLocalHit {
                slot,
                replay: self.replay,
            },
            (ProjectionStrategy::GroupLocalIncremental, Some(slot)) => {
                ProjectionDispatch::GroupLocalIncremental {
                    slot,
                    replay: self.replay,
                }
            }
            (ProjectionStrategy::ExternalCacheThenReplay, _) => {
                ProjectionDispatch::ExternalCacheThenReplay {
                    replay: self.replay,
                }
            }
            (ProjectionStrategy::DirectReplay, _) => ProjectionDispatch::DirectReplay {
                replay: self.replay,
            },
            (ProjectionStrategy::Empty, _) => ProjectionDispatch::Empty,
            // `compute_strategy()` only selects group-local strategies when a slot exists.
            _ => ProjectionDispatch::DirectReplay {
                replay: self.replay,
            },
        }
    }
}

/// Pure function: decide which projection strategy to use from known metadata.
/// No I/O, no side effects — makes the decision tree unit-testable.
pub(super) fn compute_strategy(
    group_local_slot: Option<&CachedProjectionSlot>,
    is_group_local_fresh: bool,
    supports_incremental: bool,
    incremental_enabled: bool,
    cache_is_noop: bool,
) -> ProjectionStrategy {
    if group_local_slot.is_some() {
        if is_group_local_fresh {
            return ProjectionStrategy::GroupLocalHit;
        }
        if supports_incremental && incremental_enabled {
            return ProjectionStrategy::GroupLocalIncremental;
        }
    }
    if cache_is_noop {
        return ProjectionStrategy::DirectReplay;
    }
    ProjectionStrategy::ExternalCacheThenReplay
}
