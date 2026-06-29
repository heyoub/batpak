use super::*;

/// External cache probe with incremental apply and fresh-hit paths, then fallback to full replay.
// cold path -- keep out of the hot dispatch to reduce instruction cache pressure
#[inline(never)]
pub(super) fn execute_external_cache_path<T, I, State: crate::store::StoreState>(
    store: &Store<State>,
    execution: ReplayExecution<'_>,
    mut fallback_cache_status: ProjectionCacheObservation,
    timings: &mut Option<&mut ProjectionTimings>,
) -> Result<ProjectionOutcome<T>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
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
            let is_fresh = cache_row_is_fresh(store, &execution, &meta);

            if let Some(outcome) = try_finish_incremental_cache_row::<T, I, State>(
                store,
                &execution,
                timings,
                &bytes,
                &meta,
                is_fresh,
                plan_generation,
            )? {
                return Ok(outcome);
            }

            if let Some(outcome) = try_finish_fresh_cache_row::<T, State>(
                store,
                &execution,
                timings,
                bytes,
                &meta,
                is_fresh,
                plan_generation,
            )? {
                return Ok(outcome);
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

    execute_full_replay::<T, I, State>(
        store,
        execution,
        fallback_cache_status,
        ProjectionObservedFreshness::Fresh,
        timings,
    )
}

fn cache_row_is_fresh<State: crate::store::StoreState>(
    store: &Store<State>,
    execution: &ReplayExecution<'_>,
    meta: &super::super::CacheMeta,
) -> bool {
    match execution.freshness {
        Freshness::Consistent => meta.watermark == execution.replay.watermark,
        Freshness::MaybeStale { max_stale_ms } => {
            let now_us = store.runtime.cache_now_us();
            let age_us = now_us.saturating_sub(meta.cached_at_us).max(0);
            age_us < (*max_stale_ms as i64) * 1000
        }
    }
}

fn try_finish_incremental_cache_row<T, I, State: crate::store::StoreState>(
    store: &Store<State>,
    execution: &ReplayExecution<'_>,
    timings: &mut Option<&mut ProjectionTimings>,
    bytes: &[u8],
    meta: &super::super::CacheMeta,
    is_fresh: bool,
    plan_generation: u64,
) -> Result<Option<ProjectionOutcome<T>>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    I: ReplayInput<Payload = <T::Input as ProjectionInput>::Payload>,
{
    if is_fresh
        || meta.watermark > execution.replay.watermark
        || !T::supports_incremental_apply()
        || !store.runtime.incremental_projection
    {
        return Ok(None);
    }
    let Some(mut cached_state) = decode_cached_state::<T>(
        execution.entity,
        bytes,
        "incremental projection deser failed, falling back to full replay",
    ) else {
        return Ok(None);
    };
    apply_incremental_events::<T, I, State>(store, execution, &mut cached_state, meta.watermark)?;
    validate_projection_state::<T>(execution.entity, Some(&cached_state))?;
    observe_projection_cache_store_outcome(
        "incremental",
        execution.entity,
        store_projection_value(store, execution, &cached_state),
    );
    Ok(Some(finish_projection(
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
    )))
}

fn try_finish_fresh_cache_row<T, State: crate::store::StoreState>(
    store: &Store<State>,
    execution: &ReplayExecution<'_>,
    timings: &mut Option<&mut ProjectionTimings>,
    bytes: Vec<u8>,
    meta: &super::super::CacheMeta,
    is_fresh: bool,
    plan_generation: u64,
) -> Result<Option<ProjectionOutcome<T>>, StoreError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    if !is_fresh || meta.watermark > execution.replay.watermark {
        return Ok(None);
    }
    let Some(value) = decode_cached_state::<T>(
        execution.entity,
        &bytes,
        "cache deserialize failed (falling back to replay)",
    ) else {
        return Ok(None);
    };
    validate_projection_state::<T>(execution.entity, Some(&value))?;
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
    let observed_freshness = if meta.watermark == execution.replay.watermark {
        ProjectionObservedFreshness::Fresh
    } else {
        ProjectionObservedFreshness::StaleAllowed
    };
    Ok(Some(finish_projection(
        timings,
        store.runtime.clock(),
        execution.started_at_ns,
        Some(value),
        plan_generation,
        finish_observation(
            store,
            meta.watermark,
            ProjectionCacheObservation::Hit,
            observed_freshness,
        ),
    )))
}
