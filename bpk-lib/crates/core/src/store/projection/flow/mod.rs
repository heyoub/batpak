mod cache_identity;
mod outcome;
mod replay_input;
mod strategy;

use crate::event::{EventSourced, ProjectionInput};
use crate::store::index::ProjectionCacheStoreStatus;
use crate::store::{Clock, Freshness, HlcPoint, Store, StoreError};
use std::any::TypeId;

pub(crate) use cache_identity::projection_cache_key;
use outcome::{
    finish_empty_projection, finish_projection, record_external_cache_probe_time,
    ProjectionFinishObservation,
};
pub(crate) use outcome::{
    ProjectionCacheObservation, ProjectionObservedFreshness, ProjectionOutcome, ProjectionTimings,
};
#[doc(hidden)]
pub use replay_input::ReplayInput;
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

fn elapsed_us(clock: &dyn Clock, started_at_ns: i64) -> u64 {
    u64::try_from(clock.now_mono_ns().saturating_sub(started_at_ns).max(0) / 1_000)
        .unwrap_or(u64::MAX)
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

fn fallback_to_full_replay<T, I, State>(
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

fn input_frontier_for_sequence<State>(store: &Store<State>, sequence: u64) -> Option<HlcPoint> {
    store.index.hlc_for_global_sequence(sequence)
}

fn finish_observation<State>(
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

pub(crate) fn project<T, State>(
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

pub(crate) fn project_outcome<T, State>(
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

pub(crate) fn project_if_changed<T, State>(
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
pub(crate) fn project_timed<T, State>(
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
fn project_inner<T, I, State>(
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

    // ── Phase 1: Gather metadata ──────────────────────────────────────

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
            let group_local_fresh = group_local_slot
                .as_ref()
                .map(|slot| match freshness {
                    Freshness::Consistent => {
                        slot.watermark == replay.watermark
                            && slot.generation == replay.plan.generation
                    }
                    Freshness::MaybeStale { max_stale_ms: _ } => {
                        // `slot.watermark == replay.watermark` — a slot with a
                        // lower watermark can legitimately happen if the replay
                        // plan advanced, but treating it as fresh would return
                        // a state that omits the newer events. Equality here
                        // is the honest invariant.
                        //
                        // The age-based branch (`age_us < max_stale_ms * 1000`)
                        // is omitted because the group-local slot stores only
                        // wall-clock `cached_at_us` — a regression-prone basis
                        // for age comparison. Until the slot carries a
                        // monotonic counterpart, MaybeStale collapses to the
                        // same invariant as `Consistent` for group-local: hit
                        // only when state is unchanged.
                        //
                        // justifies: INV-CACHE-CAPABILITIES-EXPLICIT; legacy-cache rows lack monotonic time in src/store/projection/flow.rs;
                        // conservatively treat as stale for MaybeStale.
                        slot.watermark == replay.watermark
                            && slot.generation == replay.plan.generation
                    }
                })
                .unwrap_or(false);
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
    if let Some(t) = timings.as_deref_mut() {
        t.plan_build_us = elapsed_us(store.runtime.clock(), t_start);
    }

    // ── Phase 2: Compute strategy ─────────────────────────────────────

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

    // ── Phase 3: Dispatch ─────────────────────────────────────────────
    //
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
        ProjectionDispatch::Empty => Ok(finish_empty_projection(
            &mut timings,
            store.runtime.clock(),
            t_start,
            observed_generation,
        )),

        ProjectionDispatch::GroupLocalHit { slot, replay } => {
            if let Some(value) = decode_cached_state::<T>(
                entity,
                &slot.bytes,
                "group-local projection cache deserialize failed (falling back)",
            ) {
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

    notify_projection_applied::<T, State>(store, entity, &outcome);
    Ok(outcome)
}

fn notify_projection_applied<T, State>(
    store: &Store<State>,
    entity: &str,
    outcome: &ProjectionOutcome<T>,
) where
    T: 'static,
{
    if let Some(sequence) = outcome.applied_sequence() {
        if let Some(point) = store.index.hlc_for_global_sequence(sequence) {
            store.projection_registry.notify_applied(
                super::registry::ProjectionRegistry::id_for_type::<T>(entity),
                point,
            );
        }
    }
}

/// External cache probe with incremental apply and fresh-hit paths, then fallback to full replay.
// cold path -- keep out of the hot dispatch to reduce instruction cache pressure
#[inline(never)]
fn execute_external_cache_path<T, I, State>(
    store: &Store<State>,
    execution: ReplayExecution<'_>,
    mut fallback_cache_status: ProjectionCacheObservation,
    timings: &mut Option<&mut ProjectionTimings>,
) -> Result<ProjectionOutcome<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
    // Prefetch already fired in Phase 1c (before group-local check).
    // External cache probe

    // `plan.generation` was sampled BEFORE the replay stream executed and is
    // the honest generation for any state served from this path — see F5.
    let plan_generation = execution.replay.plan.generation;

    let t_ext = store.runtime.now_mono_ns();
    let cache_row = store.cache.get(&execution.replay.cache_key);
    let probe_us = elapsed_us(store.runtime.clock(), t_ext);
    let probe_outcome = match &cache_row {
        Ok(Some(_)) => "some",
        Ok(None) => "none",
        Err(_) => "error",
    };
    tracing::trace!(
        target: "batpak::projection",
        flow = "external_cache_probe",
        entity = execution.entity,
        outcome = probe_outcome,
        probe_us,
    );
    match cache_row {
        Ok(Some((bytes, meta))) => {
            record_external_cache_probe_time(timings, store.runtime.clock(), t_ext);
            let is_fresh = match execution.freshness {
                Freshness::Consistent => meta.watermark == execution.replay.watermark,
                Freshness::MaybeStale { max_stale_ms } => {
                    // Age-based freshness runs through the Store's monotonic
                    // clock — which is derived from the injected wall clock
                    // via `MonotonicClock` (see `StoreConfig::with_clock`).
                    // This makes fast-forwarded test clocks observable in the
                    // MaybeStale path: a test that advances the injected
                    // clock past `max_stale_ms` forces a re-project on the
                    // next call. See G6.
                    //
                    // The comparison is against `cached_at_us` on the cache
                    // meta, which is stamped at `ProjectionCache::put` time
                    // (not plan-build time) so "age" means actual time since
                    // the bytes were written, not since the plan was drawn.
                    let now_us = store.runtime.cache_now_us();
                    let age_us = now_us.saturating_sub(meta.cached_at_us).max(0);
                    age_us < (*max_stale_ms as i64) * 1000
                }
            };

            if !is_fresh && T::supports_incremental_apply() && store.runtime.incremental_projection
            {
                if let Some(mut cached_state) = decode_cached_state::<T>(
                    execution.entity,
                    &bytes,
                    "incremental projection deser failed, falling back to full replay",
                ) {
                    apply_incremental_events::<T, I, State>(
                        store,
                        &execution,
                        &mut cached_state,
                        meta.watermark,
                    )?;
                    observe_projection_cache_store_outcome(
                        "incremental",
                        execution.entity,
                        store_projection_value(store, &execution, &cached_state),
                    );
                    return Ok(finish_projection(
                        timings,
                        store.runtime.clock(),
                        execution.started_at_ns,
                        Some(cached_state),
                        plan_generation,
                        finish_observation(
                            store,
                            execution.replay.watermark,
                            ProjectionCacheObservation::Hit,
                            ProjectionObservedFreshness::Fresh,
                        ),
                    ));
                }
            }

            if is_fresh {
                if let Some(value) = decode_cached_state::<T>(
                    execution.entity,
                    &bytes,
                    "cache deserialize failed (falling back to replay)",
                ) {
                    let index_outcome = store_index_cached_projection(
                        store,
                        execution.entity,
                        execution.replay.type_id,
                        bytes,
                        meta.watermark,
                    );
                    tracing::trace!(
                        target: "batpak::projection",
                        flow = "group_local_cache_warm",
                        entity = execution.entity,
                        outcome = ?index_outcome,
                    );
                    return Ok(finish_projection(
                        timings,
                        store.runtime.clock(),
                        execution.started_at_ns,
                        Some(value),
                        plan_generation,
                        finish_observation(
                            store,
                            meta.watermark,
                            ProjectionCacheObservation::Hit,
                            if meta.watermark == execution.replay.watermark {
                                ProjectionObservedFreshness::Fresh
                            } else {
                                ProjectionObservedFreshness::StaleAllowed
                            },
                        ),
                    ));
                }
            }
        }
        Ok(None) => {
            fallback_cache_status = ProjectionCacheObservation::Miss;
            record_external_cache_probe_time(timings, store.runtime.clock(), t_ext);
        }
        Err(e) => {
            fallback_cache_status = ProjectionCacheObservation::Unavailable {
                reason: "cache_get_failed",
            };
            record_external_cache_probe_time(timings, store.runtime.clock(), t_ext);
            tracing::warn!("cache get failed (falling back to replay): {e}");
        }
    }

    // Fallback: full replay
    execute_full_replay::<T, I, State>(
        store,
        execution,
        fallback_cache_status,
        ProjectionObservedFreshness::Fresh,
        timings,
    )
}

/// Full replay from disk: batch-read events, fold, and store back to cache.
// cold path -- keep out of the hot dispatch to reduce instruction cache pressure
#[inline(never)]
fn execute_full_replay<T, I, State>(
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

fn apply_incremental_events<T, I, State>(
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

fn store_projection_value<T, State>(
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

fn store_index_cached_projection<State>(
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
mod tests;
