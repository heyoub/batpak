mod cache_identity;
mod external_cache;
mod fusion;
mod outcome;
mod replay_input;
mod state_contract;
mod strategy;

use crate::event::{EventSourced, ProjectionInput};
use crate::store::index::columnar::CachedProjectionSlot;
use crate::store::index::{ProjectionCacheStoreStatus, ProjectionReplayItem};
use crate::store::{Freshness, HlcPoint, Store, StoreError};
use external_cache::execute_external_cache_path;
use std::any::TypeId;
use std::collections::BTreeMap;

pub(crate) use cache_identity::projection_cache_key;
#[cfg(test)]
pub(crate) use fusion::{fused_replay_batch_reads, reset_fused_replay_batch_reads};
pub(crate) use fusion::{project_fused2, project_fused3};
use outcome::{
    elapsed_us, finish_empty_projection, finish_projection, record_external_cache_probe_time,
    ProjectionFinishObservation,
};
pub(crate) use outcome::{
    ProjectionCacheObservation, ProjectionObservedFreshness, ProjectionOutcome, ProjectionTimings,
};
#[doc(hidden)]
pub use replay_input::ReplayInput;
use state_contract::validate_projection_state;
#[cfg(test)]
use strategy::{compute_strategy, ProjectionStrategy};
use strategy::{
    replay_execution, PreparedProjection, ProjectionDispatch, ProjectionPreparation, ReplayContext,
    ReplayExecution,
};

fn decode_cached_state<T>(entity: &str, bytes: &[u8], warning: &str) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    match serde_json::from_slice::<T>(bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(entity, error = %error, "{}", warning);
            None
        }
    }
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectionExternalCacheStoreOutcome {
    Stored,
    PutFailed,
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectionIndexCacheStoreOutcome {
    Stored,
    MissingEntity,
    UnsupportedTopology,
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectionCacheStoreOutcome {
    Stored {
        external: ProjectionExternalCacheStoreOutcome,
        index: ProjectionIndexCacheStoreOutcome,
    },
    SerializationFailed,
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupLocalProjectionFreshness {
    Missing,
    Fresh,
    Stale,
}

impl GroupLocalProjectionFreshness {
    fn is_fresh(self) -> bool {
        matches!(self, Self::Fresh)
    }
}

fn group_local_projection_freshness(
    slot: Option<&CachedProjectionSlot>,
    replay: &ReplayContext,
    freshness: &Freshness,
) -> GroupLocalProjectionFreshness {
    let Some(slot) = slot else {
        return GroupLocalProjectionFreshness::Missing;
    };

    let unchanged = slot.watermark == replay.watermark && slot.generation == replay.plan.generation;
    match freshness {
        Freshness::Consistent => {
            if unchanged {
                GroupLocalProjectionFreshness::Fresh
            } else {
                GroupLocalProjectionFreshness::Stale
            }
        }
        Freshness::MaybeStale { max_stale_ms: _ } => {
            // `slot.watermark == replay.watermark` — a slot with a lower
            // watermark can legitimately happen if the replay plan advanced,
            // but treating it as fresh would return a state that omits newer
            // events. Equality here is the honest invariant.
            //
            // The age-based branch (`age_us < max_stale_ms * 1000`) is
            // omitted because the group-local slot stores only wall-clock
            // `cached_at_us` — a regression-prone basis for age comparison.
            // Until the slot carries a monotonic counterpart, MaybeStale
            // collapses to the same invariant as `Consistent` for
            // group-local: hit only when state is unchanged.
            //
            // justifies: INV-CACHE-CAPABILITIES-EXPLICIT; legacy-cache rows lack monotonic time in src/store/projection/flow.rs;
            // conservatively treat as stale for MaybeStale.
            if unchanged {
                GroupLocalProjectionFreshness::Fresh
            } else {
                GroupLocalProjectionFreshness::Stale
            }
        }
    }
}

fn fallback_to_full_replay<T, I, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
    replay: &ReplayContext,
    started_at_ns: i64,
    timings: &mut Option<&mut ProjectionTimings>,
) -> Result<ProjectionOutcome<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
    execute_full_replay::<T, I, State>(
        store,
        replay_execution(entity, freshness, replay, started_at_ns),
        ProjectionCacheObservation::Miss,
        ProjectionObservedFreshness::Fresh,
        timings,
    )
}

fn input_frontier_for_sequence<State: crate::store::StoreState>(
    store: &Store<State>,
    sequence: u64,
) -> Option<HlcPoint> {
    store.index.hlc_for_global_sequence(sequence)
}

fn finish_observation<State: crate::store::StoreState>(
    store: &Store<State>,
    applied_sequence: u64,
    cache_status: ProjectionCacheObservation,
    observed_freshness: ProjectionObservedFreshness,
) -> ProjectionFinishObservation {
    ProjectionFinishObservation {
        applied_sequence,
        cache_status,
        observed_freshness,
        input_frontier: input_frontier_for_sequence(store, applied_sequence),
    }
}

pub(crate) fn project<T, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
) -> Result<Option<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    T::Input: ReplayInput,
{
    Ok(project_inner::<T, T::Input, State>(store, entity, freshness, None)?.into_state())
}

pub(crate) fn project_outcome<T, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
) -> Result<ProjectionOutcome<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    T::Input: ReplayInput,
{
    project_inner::<T, T::Input, State>(store, entity, freshness, None)
}

pub(crate) fn project_if_changed<T, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    last_seen_generation: u64,
    freshness: &Freshness,
) -> Result<Option<(u64, Option<T>)>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    T::Input: ReplayInput,
{
    let consistent_freshness = Freshness::Consistent;
    let effective_freshness = match freshness {
        Freshness::Consistent => freshness,
        // `project_if_changed` returns a generation token that callers often
        // persist as a watermark. Serving a MaybeStale cache row here would
        // let stale state travel with a newer generation and silently consume
        // a later relevant append. Keep this path generation-honest by
        // normalising to `Consistent`.
        Freshness::MaybeStale { .. } => &consistent_freshness,
    };
    let current_generation = store.entity_generation(entity).unwrap_or(0);
    if current_generation == last_seen_generation {
        return Ok(None);
    }
    // Do NOT return `current_generation` — that is the generation as of the
    // change-detection probe, not the generation at which the returned state
    // was materialized. A cache-hit path may return state stamped at an
    // earlier generation; a replay path stamps at `plan.generation` sampled
    // before replay started. Returning the honest value here prevents
    // `ProjectionWatcher` from "consuming" a relevant append while the caller
    // is still holding stale state. See F5.
    let outcome = project_inner::<T, T::Input, State>(store, entity, effective_freshness, None)?;
    Ok(Some(outcome.into_parts()))
}

/// Same as `project()` but captures per-phase timings into `out`.
/// The measured path IS the real path — same code, same branches.
#[cfg(test)]
pub(crate) fn project_timed<T, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
    out: &mut ProjectionTimings,
) -> Result<Option<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    T::Input: ReplayInput,
{
    Ok(project_inner::<T, T::Input, State>(store, entity, freshness, Some(out))?.into_state())
}

/// Shared projection executor. Optional timing sink gated behind `timings.is_some()`.
fn project_inner<T, I, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
    mut timings: Option<&mut ProjectionTimings>,
) -> Result<ProjectionOutcome<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
    let t_start = store.runtime.now_mono_ns();
    let observed_generation = store.entity_generation(entity).unwrap_or(0);

    tracing::debug!(
        target: "batpak::flow",
        flow = "project",
        entity,
        freshness = match freshness {
            Freshness::Consistent => "consistent",
            Freshness::MaybeStale { .. } => "maybe_stale",
        }
    );

    let preparation =
        prepare_projection::<T, State>(store, entity, freshness, t_start, timings.as_deref_mut());

    let dispatch = match preparation {
        ProjectionPreparation::Empty => ProjectionDispatch::Empty,
        ProjectionPreparation::Planned(prepared) => prepared.dispatch::<T>(
            store.runtime.incremental_projection,
            store.cache.capabilities().is_noop,
        ),
    };

    tracing::debug!(
        target: "batpak::flow",
        flow = "project",
        entity,
        strategy = ?dispatch.strategy(),
    );
    let replay_items = replay_items_for_dispatch(&dispatch);

    // Each branch returns a `ProjectionOutcome<T>` whose `returned_generation`
    // is the generation at which the returned state was actually materialized:
    //   * Cache hit  → slot.generation (the generation stamped on that cache row)
    //   * Any replay path → plan.generation (sampled at plan-build, BEFORE the
    //     replay stream executed — this is the honest upper bound of what the
    //     returned state saw)
    //
    // See F5: `ProjectionWatcher` persists the returned value as its
    // `last_delivered_generation`; if we returned a fresher token than the
    // state actually reflects, a subsequent relevant append would be silently
    // "consumed" against stale data.

    let outcome = match dispatch {
        ProjectionDispatch::Empty => {
            validate_projection_state::<T>(entity, None)?;
            Ok(finish_empty_projection(
                &mut timings,
                store.runtime.clock(),
                t_start,
                observed_generation,
            ))
        }

        ProjectionDispatch::GroupLocalHit { slot, replay } => {
            if let Some(value) = decode_cached_state::<T>(
                entity,
                &slot.bytes,
                "group-local projection cache deserialize failed (falling back)",
            ) {
                validate_projection_state::<T>(entity, Some(&value))?;
                Ok(finish_projection(
                    &mut timings,
                    store.runtime.clock(),
                    t_start,
                    Some(value),
                    slot.generation,
                    finish_observation(
                        store,
                        slot.watermark,
                        ProjectionCacheObservation::Hit,
                        ProjectionObservedFreshness::Fresh,
                    ),
                ))
            } else {
                fallback_to_full_replay::<T, I, State>(
                    store,
                    entity,
                    freshness,
                    &replay,
                    t_start,
                    &mut timings,
                )
            }
        }

        ProjectionDispatch::GroupLocalIncremental { slot, replay } => {
            if let Some(mut cached_state) = decode_cached_state::<T>(
                entity,
                &slot.bytes,
                "group-local incremental deser failed, falling back to full replay",
            ) {
                let execution = replay_execution(entity, freshness, &replay, t_start);
                apply_incremental_events::<T, I, State>(
                    store,
                    &execution,
                    &mut cached_state,
                    slot.watermark,
                )?;
                validate_projection_state::<T>(execution.entity, Some(&cached_state))?;
                observe_projection_cache_store_outcome(
                    "group_local_incremental",
                    execution.entity,
                    store_projection_value(store, &execution, &cached_state),
                );
                Ok(finish_projection(
                    &mut timings,
                    store.runtime.clock(),
                    t_start,
                    Some(cached_state),
                    replay.plan.generation,
                    finish_observation(
                        store,
                        replay.watermark,
                        ProjectionCacheObservation::Hit,
                        ProjectionObservedFreshness::Fresh,
                    ),
                ))
            } else {
                fallback_to_full_replay::<T, I, State>(
                    store,
                    entity,
                    freshness,
                    &replay,
                    t_start,
                    &mut timings,
                )
            }
        }

        ProjectionDispatch::ExternalCacheThenReplay { replay } => {
            execute_external_cache_path::<T, I, State>(
                store,
                replay_execution(entity, freshness, &replay, t_start),
                ProjectionCacheObservation::Miss,
                &mut timings,
            )
        }

        ProjectionDispatch::DirectReplay { replay } => execute_full_replay::<T, I, State>(
            store,
            replay_execution(entity, freshness, &replay, t_start),
            ProjectionCacheObservation::Bypassed,
            ProjectionObservedFreshness::Fresh,
            &mut timings,
        ),
    }?;

    tracing::trace!(
        target: "batpak::projection",
        flow = "project",
        entity,
        cache_status = ?outcome.cache_status(),
        observed_freshness = ?outcome.observed_freshness(),
        total_us = elapsed_us(store.runtime.clock(), t_start),
        returned_generation = outcome.returned_generation(),
    );

    notify_projection_applied::<T, State>(store, entity, &outcome, &replay_items);
    Ok(outcome)
}

/// Phase 1 of [`project_inner`]: build the replay plan, fire the early cache
/// prefetch, and probe the group-local slot, recording the per-phase timings.
/// Extracted verbatim from `project_inner` to keep that function within budget.
fn prepare_projection<T, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
    t_start: i64,
    mut timings: Option<&mut ProjectionTimings>,
) -> ProjectionPreparation
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    // 1a: Build replay plan
    let relevant_kinds = T::relevant_event_kinds();
    let preparation = match store.index.projection_replay_plan(entity, relevant_kinds) {
        None => ProjectionPreparation::Empty,
        Some(plan) => {
            let t_cache_key = store.runtime.now_mono_ns();
            let replay = ReplayContext {
                watermark: plan.watermark,
                cached_at_us: store.runtime.cache_now_us(),
                cached_at_mono_ns: store.runtime.now_mono_ns(),
                process_boot_ns: store.runtime.process_boot_ns(),
                type_id: TypeId::of::<T>(),
                cache_key: projection_cache_key::<T>(entity),
                plan,
            };
            if let Some(t) = timings.as_deref_mut() {
                t.cache_key_build_us = elapsed_us(store.runtime.clock(), t_cache_key);
            }

            // Fire prefetch early so I/O overlaps with group-local CPU work.
            let t_prefetch = store.runtime.now_mono_ns();
            if store.cache.capabilities().supports_prefetch {
                let predicted_meta = super::CacheMeta {
                    watermark: replay.watermark,
                    cached_at_us: replay.cached_at_us,
                    cached_at_mono_ns: Some(replay.cached_at_mono_ns),
                    process_boot_ns: Some(replay.process_boot_ns),
                };
                if let Err(error) = store.cache.prefetch(&replay.cache_key, predicted_meta) {
                    tracing::warn!("cache prefetch failed (non-fatal): {error}");
                }
            }
            if let Some(t) = timings.as_deref_mut() {
                t.prefetch_us = elapsed_us(store.runtime.clock(), t_prefetch);
            }

            let t_group = store.runtime.now_mono_ns();
            let group_local_slot = store.index.cached_projection(entity, replay.type_id);
            let group_local_fresh =
                group_local_projection_freshness(group_local_slot.as_ref(), &replay, freshness)
                    .is_fresh();
            if let Some(t) = timings.as_deref_mut() {
                t.group_local_lookup_us = elapsed_us(store.runtime.clock(), t_group);
            }

            ProjectionPreparation::Planned(PreparedProjection {
                replay,
                group_local_slot,
                group_local_fresh,
            })
        }
    };
    if let Some(t) = timings {
        t.plan_build_us = elapsed_us(store.runtime.clock(), t_start);
    }
    preparation
}

fn replay_items_for_dispatch(dispatch: &ProjectionDispatch) -> Vec<ProjectionReplayItem> {
    match dispatch {
        ProjectionDispatch::Empty => Vec::new(),
        ProjectionDispatch::GroupLocalHit { replay, .. }
        | ProjectionDispatch::GroupLocalIncremental { replay, .. }
        | ProjectionDispatch::ExternalCacheThenReplay { replay }
        | ProjectionDispatch::DirectReplay { replay } => replay.plan.items.clone(),
    }
}

fn notify_projection_applied<T, State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    outcome: &ProjectionOutcome<T>,
    replay_items: &[ProjectionReplayItem],
) where
    T: 'static,
{
    if let Some(sequence) = outcome.applied_sequence() {
        let projection_id = super::registry::ProjectionRegistry::id_for_type::<T>(entity);
        let mut lanes = BTreeMap::<u32, HlcPoint>::new();
        for item in replay_items
            .iter()
            .filter(|item| item.global_sequence <= sequence)
        {
            lanes
                .entry(item.lane)
                .and_modify(|current| *current = (*current).max_by_sequence(item.point))
                .or_insert(item.point);
        }
        for (lane, point) in lanes {
            store
                .projection_registry
                .notify_applied_on_lane(projection_id.clone(), lane, point);
        }
    }
}

/// Full replay from disk: batch-read events, fold, and store back to cache.
// cold path -- keep out of the hot dispatch to reduce instruction cache pressure
#[inline(never)]
fn execute_full_replay<T, I, State: crate::store::StoreState>(
    store: &Store<State>,
    execution: ReplayExecution<'_>,
    cache_status: ProjectionCacheObservation,
    observed_freshness: ProjectionObservedFreshness,
    timings: &mut Option<&mut ProjectionTimings>,
) -> Result<ProjectionOutcome<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
    // `plan.generation` was sampled at plan-build, BEFORE the replay stream
    // executed. That is the honest upper bound for what the returned state
    // reflects — returning a fresher `entity_generation` here would risk
    // silently "consuming" a later append in the watcher bump path (F5).
    let plan_generation = execution.replay.plan.generation;

    // Full replay -- batch-read filtered events from disk.
    // Uses the projection's replay-input lane, which always skips Coordinate
    // construction and may leave payloads as raw MessagePack bytes.
    let t_disk = store.runtime.now_mono_ns();
    let positions: Vec<&crate::store::index::DiskPos> = execution
        .replay
        .plan
        .items
        .iter()
        .map(|item| &item.disk_pos)
        .collect();
    let events = I::read_batch(&store.reader, &positions)?;
    if let Some(t) = timings.as_deref_mut() {
        t.disk_read_us = elapsed_us(store.runtime.clock(), t_disk);
        // No separate extraction step -- replay lanes return Event directly.
        t.event_extract_us = 0;
    }

    let t_fold = store.runtime.now_mono_ns();
    let result = T::from_events(&events);
    if let Some(t) = timings.as_deref_mut() {
        t.replay_fold_us = elapsed_us(store.runtime.clock(), t_fold);
    }
    validate_projection_state::<T>(execution.entity, result.as_ref())?;

    if result.is_none() && !events.is_empty() {
        tracing::debug!(
            execution.entity,
            event_count = events.len(),
            "projection returned None despite non-empty filtered event stream"
        );
    }

    // Cache store-back
    let t_store = store.runtime.now_mono_ns();
    if let Some(ref value) = result {
        observe_projection_cache_store_outcome(
            "full_replay",
            execution.entity,
            store_projection_value(store, &execution, value),
        );
    }
    if let Some(t) = timings.as_deref_mut() {
        t.cache_store_us = elapsed_us(store.runtime.clock(), t_store);
    }

    Ok(finish_projection(
        timings,
        store.runtime.clock(),
        execution.started_at_ns,
        result,
        plan_generation,
        finish_observation(
            store,
            execution.replay.watermark,
            cache_status,
            observed_freshness,
        ),
    ))
}

/// Fold post-watermark events onto a decoded cached state.
///
/// Correctness rests on the [`EventSourced::supports_incremental_apply`]
/// contract: `from_events` must equal a fold over `apply_event`. Only call
/// this for types whose `supports_incremental_apply` returns `true`. The
/// incremental-vs-full-replay equivalence is witnessed by
/// `crates/core/tests/projection_incremental_xcheck.rs::incremental_apply_matches_full_replay_cross_check` (audit R9).
fn apply_incremental_events<T, I, State: crate::store::StoreState>(
    store: &Store<State>,
    execution: &ReplayExecution<'_>,
    cached_state: &mut T,
    cached_watermark: u64,
) -> Result<(), StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
    for item in execution
        .replay
        .plan
        .items
        .iter()
        .filter(|item| item.global_sequence > cached_watermark)
    {
        let event = I::read_one(&store.reader, &item.disk_pos)?;
        cached_state.apply_event(&event);
    }
    Ok(())
}

fn store_projection_value<T, State: crate::store::StoreState>(
    store: &Store<State>,
    execution: &ReplayExecution<'_>,
    value: &T,
) -> ProjectionCacheStoreOutcome
where
    T: serde::Serialize,
{
    let bytes = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                target: "batpak::projection",
                flow = "cache_store",
                entity = execution.entity,
                error = %error,
                "projection cache serialization failed; skipping cache store-back"
            );
            return ProjectionCacheStoreOutcome::SerializationFailed;
        }
    };

    // G6: stamp `cached_at_*` at the moment the bytes are actually
    // handed to `ProjectionCache::put`, not at the moment the plan was
    // built. Plan-build time can be microseconds to milliseconds earlier
    // — anything that depends on "how old is this cache row" must see
    // the real put timestamp, not the plan-build timestamp.
    //
    // Wall-clock and monotonic metadata both flow through the runtime
    // clock; tests that install a full `Clock` can control wall time,
    // monotonic age, and process-epoch evidence from one seam.
    let meta = super::CacheMeta {
        watermark: execution.replay.watermark,
        cached_at_us: store.runtime.cache_now_us(),
        cached_at_mono_ns: Some(store.runtime.now_mono_ns()),
        process_boot_ns: Some(store.runtime.process_boot_ns()),
    };
    let external = if let Err(error) = store.cache.put(&execution.replay.cache_key, &bytes, meta) {
        tracing::warn!("cache put failed (non-fatal): {error}");
        ProjectionExternalCacheStoreOutcome::PutFailed
    } else {
        ProjectionExternalCacheStoreOutcome::Stored
    };
    let index = store_index_cached_projection(
        store,
        execution.entity,
        execution.replay.type_id,
        bytes,
        execution.replay.watermark,
    );
    ProjectionCacheStoreOutcome::Stored { external, index }
}

fn observe_projection_cache_store_outcome(
    flow: &'static str,
    entity: &str,
    outcome: ProjectionCacheStoreOutcome,
) {
    tracing::trace!(
        target: "batpak::projection",
        flow,
        entity,
        outcome = ?outcome,
        "projection cache store outcome"
    );
}

fn store_index_cached_projection<State: crate::store::StoreState>(
    store: &Store<State>,
    entity: &str,
    type_id: TypeId,
    bytes: Vec<u8>,
    watermark: u64,
) -> ProjectionIndexCacheStoreOutcome {
    match store
        .index
        .store_cached_projection(entity, type_id, bytes, watermark)
    {
        ProjectionCacheStoreStatus::Stored => ProjectionIndexCacheStoreOutcome::Stored,
        ProjectionCacheStoreStatus::MissingEntity => {
            ProjectionIndexCacheStoreOutcome::MissingEntity
        }
        ProjectionCacheStoreStatus::UnsupportedTopology => {
            ProjectionIndexCacheStoreOutcome::UnsupportedTopology
        }
    }
}

#[cfg(test)]
mod fusion_tests;
#[cfg(test)]
mod state_contract_tests;
#[cfg(test)]
mod tests;
