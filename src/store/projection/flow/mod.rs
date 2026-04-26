mod cache_identity;
mod outcome;
mod replay_input;
mod strategy;

use crate::event::{EventSourced, ProjectionInput};
use crate::store::config::duration_micros;
use crate::store::{Freshness, Store, StoreError};
use std::any::TypeId;

pub(crate) use cache_identity::projection_cache_key;
pub(crate) use outcome::ProjectionTimings;
use outcome::{
    finish_empty_projection, finish_projection, record_external_cache_probe_time, ProjectionOutcome,
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

fn fallback_to_full_replay<T, I, State>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
    replay: &ReplayContext,
    started_at: std::time::Instant,
    timings: &mut Option<&mut ProjectionTimings>,
) -> Result<ProjectionOutcome<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
    execute_full_replay::<T, I, State>(
        store,
        replay_execution(entity, freshness, replay, started_at),
        timings,
    )
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
    let t_start = std::time::Instant::now();
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
            let t_cache_key = std::time::Instant::now();
            let replay = ReplayContext {
                watermark: plan.watermark,
                cached_at_us: store.runtime.cache_now_us(),
                cached_at_mono_ns: crate::store::config::now_mono_ns(),
                process_boot_ns: crate::store::config::process_boot_ns(),
                type_id: TypeId::of::<T>(),
                cache_key: projection_cache_key::<T>(entity),
                plan,
            };
            if let Some(t) = timings.as_deref_mut() {
                t.cache_key_build_us = duration_micros(t_cache_key.elapsed());
            }

            // Fire prefetch early so I/O overlaps with group-local CPU work.
            let t_prefetch = std::time::Instant::now();
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
                t.prefetch_us = duration_micros(t_prefetch.elapsed());
            }

            let t_group = std::time::Instant::now();
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
                t.group_local_lookup_us = duration_micros(t_group.elapsed());
            }

            ProjectionPreparation::Planned(PreparedProjection {
                replay,
                group_local_slot,
                group_local_fresh,
            })
        }
    };
    if let Some(t) = timings.as_deref_mut() {
        t.plan_build_us = duration_micros(t_start.elapsed());
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
                    t_start,
                    Some(value),
                    slot.generation,
                    slot.watermark,
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
                store_projection_value(store, &execution, &cached_state);
                Ok(finish_projection(
                    &mut timings,
                    t_start,
                    Some(cached_state),
                    replay.plan.generation,
                    replay.watermark,
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
                &mut timings,
            )
        }

        ProjectionDispatch::DirectReplay { replay } => execute_full_replay::<T, I, State>(
            store,
            replay_execution(entity, freshness, &replay, t_start),
            &mut timings,
        ),
    }?;

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

    let t_ext = std::time::Instant::now();
    match store.cache.get(&execution.replay.cache_key) {
        Ok(Some((bytes, meta))) => {
            record_external_cache_probe_time(timings, t_ext);
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
                    store_projection_value(store, &execution, &cached_state);
                    return Ok(finish_projection(
                        timings,
                        execution.started_at,
                        Some(cached_state),
                        plan_generation,
                        execution.replay.watermark,
                    ));
                }
            }

            if is_fresh {
                if let Some(value) = decode_cached_state::<T>(
                    execution.entity,
                    &bytes,
                    "cache deserialize failed (falling back to replay)",
                ) {
                    let _ = store.index.store_cached_projection(
                        execution.entity,
                        execution.replay.type_id,
                        bytes,
                        meta.watermark,
                    );
                    return Ok(finish_projection(
                        timings,
                        execution.started_at,
                        Some(value),
                        plan_generation,
                        meta.watermark,
                    ));
                }
            }
        }
        Ok(None) => record_external_cache_probe_time(timings, t_ext),
        Err(e) => {
            record_external_cache_probe_time(timings, t_ext);
            tracing::warn!("cache get failed (falling back to replay): {e}");
        }
    }

    // Fallback: full replay
    execute_full_replay::<T, I, State>(store, execution, timings)
}

/// Full replay from disk: batch-read events, fold, and store back to cache.
// cold path -- keep out of the hot dispatch to reduce instruction cache pressure
#[inline(never)]
fn execute_full_replay<T, I, State>(
    store: &Store<State>,
    execution: ReplayExecution<'_>,
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
    let t_disk = std::time::Instant::now();
    let positions: Vec<&crate::store::DiskPos> = execution
        .replay
        .plan
        .items
        .iter()
        .map(|item| &item.disk_pos)
        .collect();
    let events = I::read_batch(&store.reader, &positions)?;
    if let Some(t) = timings.as_deref_mut() {
        t.disk_read_us = duration_micros(t_disk.elapsed());
        // No separate extraction step -- replay lanes return Event directly.
        t.event_extract_us = 0;
    }

    let t_fold = std::time::Instant::now();
    let result = T::from_events(&events);
    if let Some(t) = timings.as_deref_mut() {
        t.replay_fold_us = duration_micros(t_fold.elapsed());
    }

    if result.is_none() && !events.is_empty() {
        tracing::debug!(
            execution.entity,
            event_count = events.len(),
            "projection returned None despite non-empty filtered event stream"
        );
    }

    // Cache store-back
    let t_store = std::time::Instant::now();
    if let Some(ref value) = result {
        store_projection_value(store, &execution, value);
    }
    if let Some(t) = timings.as_deref_mut() {
        t.cache_store_us = duration_micros(t_store.elapsed());
    }

    Ok(finish_projection(
        timings,
        execution.started_at,
        result,
        plan_generation,
        execution.replay.watermark,
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
) where
    T: serde::Serialize,
{
    if let Ok(bytes) = serde_json::to_vec(value) {
        // G6: stamp `cached_at_*` at the moment the bytes are actually
        // handed to `ProjectionCache::put`, not at the moment the plan was
        // built. Plan-build time can be microseconds to milliseconds earlier
        // — anything that depends on "how old is this cache row" must see
        // the real put timestamp, not the plan-build timestamp.
        //
        // Wall-clock (`now_us`) flows through the injected `MonotonicClock`
        // wrapper, so the age observable in the `MaybeStale` path above is
        // the same clock a test controls via `StoreConfig::with_clock`.
        // `cached_at_mono_ns` + `process_boot_ns` stay pinned to the
        // hardware monotonic anchor for the lifetime of this process —
        // they are the cross-process-mismatch detector, not the age basis.
        let meta = super::CacheMeta {
            watermark: execution.replay.watermark,
            cached_at_us: store.runtime.cache_now_us(),
            cached_at_mono_ns: Some(crate::store::config::now_mono_ns()),
            process_boot_ns: Some(crate::store::config::process_boot_ns()),
        };
        if let Err(error) = store.cache.put(&execution.replay.cache_key, &bytes, meta) {
            tracing::warn!("cache put failed (non-fatal): {error}");
        }
        let _ = store.index.store_cached_projection(
            execution.entity,
            execution.replay.type_id,
            bytes,
            execution.replay.watermark,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind};
    use crate::store::index::columnar::CachedProjectionSlot;
    use crate::store::StoreConfig;
    use tempfile::TempDir;

    #[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
    struct Counter;

    impl EventSourced for Counter {
        type Input = crate::event::JsonValueInput;

        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {}

        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            (!events.is_empty()).then_some(Self)
        }

        fn relevant_event_kinds() -> &'static [EventKind] {
            static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
            &KINDS
        }
    }

    #[test]
    fn projection_replay_plan_matches_legacy_stream_filtering() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = crate::coordinate::Coordinate::new("entity:proj", "scope:test").expect("coord");
        let kept = EventKind::custom(0xF, 1);
        let skipped = EventKind::custom(0xF, 2);

        for (kind, payload) in [
            (kept, serde_json::json!({"n": 1})),
            (skipped, serde_json::json!({"n": 2})),
            (kept, serde_json::json!({"n": 3})),
        ] {
            store.append(&coord, kind, &payload).expect("append");
        }

        let plan = store
            .index
            .projection_replay_plan("entity:proj", Counter::relevant_event_kinds())
            .expect("projection plan");

        let legacy_entries = store.index.stream("entity:proj");
        let legacy_entries: Vec<_> = legacy_entries
            .into_iter()
            .filter(|entry| Counter::relevant_event_kinds().contains(&entry.kind))
            .collect();
        let legacy_items: Vec<_> = legacy_entries
            .iter()
            .map(|entry| (entry.global_sequence, entry.disk_pos))
            .collect();
        let planned_items: Vec<_> = plan
            .items
            .iter()
            .map(|item| (item.global_sequence, item.disk_pos))
            .collect();
        let legacy_watermark = legacy_entries
            .last()
            .map(|entry| entry.global_sequence)
            .expect("legacy filtered entries");

        assert_eq!(plan.watermark, legacy_watermark);
        assert_eq!(
            plan.generation,
            store.index.entity_generation("entity:proj").unwrap_or(0)
        );
        assert_eq!(planned_items, legacy_items);

        store.close().expect("close");
    }

    #[test]
    // justifies: INV-OBSERVABILITY-FAILURE-PATHS; diagnostic test in src/store/projection/flow.rs reports cold-path breakdown on stderr; the eprintln is the observable artefact of the test.
    #[allow(clippy::print_stderr)]
    fn projection_timings_cold_path_breakdown() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord =
            crate::coordinate::Coordinate::new("entity:timed", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        for i in 0..1_000u32 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }

        // Close and reopen to get a true cold path
        store.close().expect("close");
        let store = Store::open(StoreConfig::new(dir.path())).expect("reopen");

        let mut timings = ProjectionTimings::default();
        let result: Option<Counter> =
            project_timed(&store, "entity:timed", &Freshness::Consistent, &mut timings)
                .expect("project_timed");
        assert!(result.is_some(), "projection must produce a value");

        // Print breakdown for diagnostic purposes (visible with --nocapture)
        eprintln!("=== Projection Cold Path Breakdown (1k events) ===");
        eprintln!("  plan_build:           {:>8} us", timings.plan_build_us);
        eprintln!(
            "  cache_key_build:      {:>8} us",
            timings.cache_key_build_us
        );
        eprintln!(
            "  group_local_lookup:   {:>8} us",
            timings.group_local_lookup_us
        );
        eprintln!("  prefetch:             {:>8} us", timings.prefetch_us);
        eprintln!(
            "  external_cache_probe: {:>8} us",
            timings.external_cache_probe_us
        );
        eprintln!(
            "  disk_read:            {:>8} us  (frame decode + deser, no coord build)",
            timings.disk_read_us
        );
        eprintln!(
            "  event_extract:        {:>8} us  (now 0 -- events returned directly)",
            timings.event_extract_us
        );
        eprintln!("  replay_fold:          {:>8} us", timings.replay_fold_us);
        eprintln!("  cache_store:          {:>8} us", timings.cache_store_us);
        eprintln!("  total:                {:>8} us", timings.total_us);
        let accounted = timings.plan_build_us
            + timings.cache_key_build_us
            + timings.group_local_lookup_us
            + timings.prefetch_us
            + timings.external_cache_probe_us
            + timings.disk_read_us
            + timings.event_extract_us
            + timings.replay_fold_us
            + timings.cache_store_us;
        eprintln!(
            "  unaccounted:          {:>8} us",
            timings.total_us.saturating_sub(accounted)
        );

        assert!(timings.total_us > 0, "total must be positive");
        store.close().expect("close");
    }

    #[test]
    fn compute_strategy_exhaustive() {
        let slot = CachedProjectionSlot {
            bytes: vec![],
            watermark: 42,
            generation: 1,
        };

        // Slot present + fresh -> GroupLocalHit
        assert_eq!(
            compute_strategy(Some(&slot), true, false, false, false),
            ProjectionStrategy::GroupLocalHit,
        );
        assert_eq!(
            compute_strategy(Some(&slot), true, true, true, true),
            ProjectionStrategy::GroupLocalHit,
        );

        // Slot present + stale + incremental supported + enabled -> GroupLocalIncremental
        assert_eq!(
            compute_strategy(Some(&slot), false, true, true, false),
            ProjectionStrategy::GroupLocalIncremental,
        );
        assert_eq!(
            compute_strategy(Some(&slot), false, true, true, true),
            ProjectionStrategy::GroupLocalIncremental,
        );

        // Slot present + stale + incremental disabled -> falls through to cache check
        assert_eq!(
            compute_strategy(Some(&slot), false, true, false, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
        assert_eq!(
            compute_strategy(Some(&slot), false, true, false, true),
            ProjectionStrategy::DirectReplay,
        );

        // Slot present + stale + incremental NOT supported -> falls through to cache check
        assert_eq!(
            compute_strategy(Some(&slot), false, false, false, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
        assert_eq!(
            compute_strategy(Some(&slot), false, false, true, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
        assert_eq!(
            compute_strategy(Some(&slot), false, false, false, true),
            ProjectionStrategy::DirectReplay,
        );

        // No slot + noop cache -> DirectReplay
        assert_eq!(
            compute_strategy(None, false, false, false, true),
            ProjectionStrategy::DirectReplay,
        );

        // No slot + real cache -> ExternalCacheThenReplay
        assert_eq!(
            compute_strategy(None, false, false, false, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
        assert_eq!(
            compute_strategy(None, false, true, true, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
    }
}
