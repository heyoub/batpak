use crate::event::EventSourced;
use crate::store::columnar::CachedProjectionSlot;
use crate::store::index::ProjectionReplayPlan;
use crate::store::{projection, Freshness, Store, StoreError};
use std::any::TypeId;
use std::hash::{Hash, Hasher};

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

/// Pure function: decide which projection strategy to use from known metadata.
/// No I/O, no side effects — makes the decision tree unit-testable.
fn compute_strategy(
    has_replay_plan: bool,
    group_local_slot: Option<&CachedProjectionSlot>,
    is_group_local_fresh: bool,
    supports_incremental: bool,
    incremental_enabled: bool,
    cache_is_noop: bool,
) -> ProjectionStrategy {
    if !has_replay_plan {
        return ProjectionStrategy::Empty;
    }
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

pub(crate) fn projection_cache_key<T>(entity: &str) -> Vec<u8>
where
    T: EventSourced<serde_json::Value> + 'static,
{
    // Cache key: entity + \0 + type_id_hash(u64 LE) + schema_version(u64 LE)
    // TypeId ensures different EventSourced types never collide on the same entity.
    // Vec pre-allocated to exact size: entity.len() + 1 + 8 + 8 = entity.len() + 17.
    let schema_v = T::schema_version();
    let type_disc = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        TypeId::of::<T>().hash(&mut h);
        h.finish()
    };
    let mut cache_key = Vec::with_capacity(entity.len() + 17);
    cache_key.extend_from_slice(entity.as_bytes());
    cache_key.push(0);
    cache_key.extend_from_slice(&type_disc.to_le_bytes());
    cache_key.extend_from_slice(&schema_v.to_le_bytes());
    cache_key
}

pub(crate) fn project<T, State>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
) -> Result<Option<T>, StoreError>
where
    T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    project_inner(store, entity, freshness, None)
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
    T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    project_inner(store, entity, freshness, Some(out))
}

/// Shared projection executor. Optional timing sink gated behind `timings.is_some()`.
fn project_inner<T, State>(
    store: &Store<State>,
    entity: &str,
    freshness: &Freshness,
    mut timings: Option<&mut ProjectionTimings>,
) -> Result<Option<T>, StoreError>
where
    T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let t_start = std::time::Instant::now();

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
    let replay_plan = store.index.projection_replay_plan(entity, relevant_kinds);
    let has_replay_plan = replay_plan.is_some();
    if let Some(t) = timings.as_deref_mut() {
        t.plan_build_us = t_start.elapsed().as_micros() as u64;
    }

    // Early-out metadata (only meaningful when replay plan exists).
    let (watermark, entity_generation, cached_at_us, type_id, cache_key) =
        if let Some(ref plan) = replay_plan {
            let wm = plan.watermark;
            let gen = plan.generation;
            let ts = store.config.now_us();
            let tid = TypeId::of::<T>();

            // 1b: Cache key construction
            let t_cache_key = std::time::Instant::now();
            let ck = projection_cache_key::<T>(entity);
            if let Some(t) = timings.as_deref_mut() {
                t.cache_key_build_us = t_cache_key.elapsed().as_micros() as u64;
            }
            (wm, gen, ts, tid, ck)
        } else {
            // Values unused when strategy is Empty; provide defaults to avoid Option overhead.
            (0, 0, 0, TypeId::of::<T>(), Vec::new())
        };

    // 1c: Group-local cache slot + freshness
    let t_group = std::time::Instant::now();
    let group_local_slot = if has_replay_plan {
        store.index.cached_projection(entity, type_id)
    } else {
        None
    };
    let is_group_local_fresh = group_local_slot
        .as_ref()
        .map(|slot| match freshness {
            Freshness::Consistent => {
                slot.watermark == watermark && slot.generation == entity_generation
            }
            Freshness::MaybeStale { max_stale_ms } => {
                let age_us = cached_at_us.saturating_sub(slot.cached_at_us).max(0);
                age_us < (*max_stale_ms as i64) * 1000
                    && slot.generation == entity_generation
                    && slot.watermark <= watermark
            }
        })
        .unwrap_or(false);
    if let Some(t) = timings.as_deref_mut() {
        t.group_local_lookup_us = t_group.elapsed().as_micros() as u64;
    }

    // ── Phase 2: Compute strategy ─────────────────────────────────────

    let strategy = compute_strategy(
        has_replay_plan,
        group_local_slot.as_ref(),
        is_group_local_fresh,
        T::supports_incremental_apply(),
        store.config.index.incremental_projection,
        store.cache.capabilities().is_noop,
    );

    tracing::debug!(
        target: "batpak::flow",
        flow = "project",
        entity,
        ?strategy,
    );

    // ── Phase 3: Dispatch ─────────────────────────────────────────────

    match strategy {
        ProjectionStrategy::Empty => {
            if let Some(t) = timings.as_deref_mut() {
                t.total_us = t_start.elapsed().as_micros() as u64;
            }
            Ok(None)
        }

        ProjectionStrategy::GroupLocalHit => {
            // compute_strategy only returns GroupLocalHit when slot is Some + fresh.
            let slot = group_local_slot.expect("GroupLocalHit requires a slot");
            match serde_json::from_slice::<T>(&slot.bytes) {
                Ok(value) => {
                    if let Some(t) = timings.as_deref_mut() {
                        t.total_us = t_start.elapsed().as_micros() as u64;
                    }
                    Ok(Some(value))
                }
                Err(e) => {
                    tracing::warn!(
                        entity,
                        "group-local projection cache deserialize failed (falling back): {e}"
                    );
                    // Fallback: full replay (skip external cache since we already had a slot).
                    let plan = replay_plan.expect("GroupLocalHit requires a plan");
                    execute_full_replay::<T, State>(
                        store,
                        entity,
                        &plan,
                        &cache_key,
                        watermark,
                        cached_at_us,
                        type_id,
                        t_start,
                        &mut timings,
                    )
                }
            }
        }

        ProjectionStrategy::GroupLocalIncremental => {
            let slot = group_local_slot.expect("GroupLocalIncremental requires a slot");
            let plan = replay_plan.expect("GroupLocalIncremental requires a plan");
            match serde_json::from_slice::<T>(&slot.bytes) {
                Ok(mut cached_state) => {
                    let cached_watermark = slot.watermark;
                    for item in plan
                        .items
                        .iter()
                        .filter(|i| i.global_sequence > cached_watermark)
                    {
                        let event = store.reader.read_event_only(&item.disk_pos)?;
                        cached_state.apply_event(&event);
                    }
                    // Store back to both caches.
                    if let Ok(new_bytes) = serde_json::to_vec(&cached_state) {
                        let new_meta = projection::CacheMeta {
                            watermark,
                            cached_at_us,
                        };
                        if let Err(e) = store.cache.put(&cache_key, &new_bytes, new_meta) {
                            tracing::warn!("incremental cache put failed: {e}");
                        }
                        let _ = store.index.store_cached_projection(
                            entity,
                            type_id,
                            new_bytes,
                            watermark,
                            cached_at_us,
                        );
                    }
                    if let Some(t) = timings.as_deref_mut() {
                        t.total_us = t_start.elapsed().as_micros() as u64;
                    }
                    Ok(Some(cached_state))
                }
                Err(e) => {
                    tracing::warn!(
                        entity,
                        "group-local incremental deser failed, falling back to full replay: {e}"
                    );
                    execute_full_replay::<T, State>(
                        store,
                        entity,
                        &plan,
                        &cache_key,
                        watermark,
                        cached_at_us,
                        type_id,
                        t_start,
                        &mut timings,
                    )
                }
            }
        }

        ProjectionStrategy::ExternalCacheThenReplay => {
            let plan = replay_plan.expect("ExternalCacheThenReplay requires a plan");
            execute_external_cache_path::<T, State>(
                store,
                entity,
                &plan,
                &cache_key,
                freshness,
                watermark,
                cached_at_us,
                type_id,
                t_start,
                &mut timings,
            )
        }

        ProjectionStrategy::DirectReplay => {
            let plan = replay_plan.expect("DirectReplay requires a plan");
            execute_full_replay::<T, State>(
                store,
                entity,
                &plan,
                &cache_key,
                watermark,
                cached_at_us,
                type_id,
                t_start,
                &mut timings,
            )
        }
    }
}

/// External cache probe with incremental apply and fresh-hit paths, then fallback to full replay.
// cold path -- keep out of the hot dispatch to reduce instruction cache pressure
#[inline(never)]
fn execute_external_cache_path<T, State>(
    store: &Store<State>,
    entity: &str,
    replay_plan: &ProjectionReplayPlan,
    cache_key: &[u8],
    freshness: &Freshness,
    watermark: u64,
    cached_at_us: i64,
    type_id: TypeId,
    t_start: std::time::Instant,
    timings: &mut Option<&mut ProjectionTimings>,
) -> Result<Option<T>, StoreError>
where
    T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    // Prefetch
    let t_prefetch = std::time::Instant::now();
    let predicted_meta = projection::CacheMeta {
        watermark,
        cached_at_us,
    };
    if store.cache.capabilities().supports_prefetch {
        if let Err(error) = store.cache.prefetch(cache_key, predicted_meta) {
            tracing::warn!("cache prefetch failed (non-fatal): {error}");
        }
    }
    if let Some(t) = timings.as_deref_mut() {
        t.prefetch_us = t_prefetch.elapsed().as_micros() as u64;
    }

    // External cache probe
    let t_ext = std::time::Instant::now();
    match store.cache.get(cache_key) {
        Ok(Some((bytes, meta))) => {
            if let Some(t) = timings.as_deref_mut() {
                t.external_cache_probe_us = t_ext.elapsed().as_micros() as u64;
            }
            let is_fresh = match freshness {
                Freshness::Consistent => meta.watermark == watermark,
                Freshness::MaybeStale { max_stale_ms } => {
                    let age_us = store
                        .config
                        .now_us()
                        .saturating_sub(meta.cached_at_us)
                        .max(0);
                    age_us < (*max_stale_ms as i64) * 1000
                }
            };

            if !is_fresh
                && T::supports_incremental_apply()
                && store.config.index.incremental_projection
            {
                let cached_watermark = meta.watermark;
                let delta_entries: Vec<_> = replay_plan
                    .items
                    .iter()
                    .filter(|item| item.global_sequence > cached_watermark)
                    .collect();
                if let Ok(mut cached_state) = serde_json::from_slice::<T>(&bytes) {
                    for de in &delta_entries {
                        let event = store.reader.read_event_only(&de.disk_pos)?;
                        cached_state.apply_event(&event);
                    }
                    if let Ok(new_bytes) = serde_json::to_vec(&cached_state) {
                        let new_meta = projection::CacheMeta {
                            watermark,
                            cached_at_us,
                        };
                        if let Err(e) = store.cache.put(cache_key, &new_bytes, new_meta) {
                            tracing::warn!("incremental cache put failed: {e}");
                        }
                        let _ = store.index.store_cached_projection(
                            entity,
                            type_id,
                            new_bytes,
                            watermark,
                            cached_at_us,
                        );
                    }
                    if let Some(t) = timings.as_deref_mut() {
                        t.total_us = t_start.elapsed().as_micros() as u64;
                    }
                    return Ok(Some(cached_state));
                }
                tracing::warn!(
                    entity,
                    "incremental projection deser failed, falling back to full replay"
                );
            }

            if is_fresh {
                match serde_json::from_slice::<T>(&bytes) {
                    Ok(value) => {
                        let _ = store.index.store_cached_projection(
                            entity,
                            type_id,
                            bytes,
                            meta.watermark,
                            meta.cached_at_us,
                        );
                        if let Some(t) = timings.as_deref_mut() {
                            t.total_us = t_start.elapsed().as_micros() as u64;
                        }
                        return Ok(Some(value));
                    }
                    Err(e) => {
                        tracing::warn!("cache deserialize failed (falling back to replay): {e}");
                    }
                }
            }
        }
        Ok(None) => {
            if let Some(t) = timings.as_deref_mut() {
                t.external_cache_probe_us = t_ext.elapsed().as_micros() as u64;
            }
        }
        Err(e) => {
            if let Some(t) = timings.as_deref_mut() {
                t.external_cache_probe_us = t_ext.elapsed().as_micros() as u64;
            }
            tracing::warn!("cache get failed (falling back to replay): {e}");
        }
    }

    // Fallback: full replay
    execute_full_replay::<T, State>(
        store,
        entity,
        replay_plan,
        cache_key,
        watermark,
        cached_at_us,
        type_id,
        t_start,
        timings,
    )
}

/// Full replay from disk: batch-read events, fold, and store back to cache.
// cold path -- keep out of the hot dispatch to reduce instruction cache pressure
#[inline(never)]
fn execute_full_replay<T, State>(
    store: &Store<State>,
    entity: &str,
    replay_plan: &ProjectionReplayPlan,
    cache_key: &[u8],
    watermark: u64,
    cached_at_us: i64,
    type_id: TypeId,
    t_start: std::time::Instant,
    timings: &mut Option<&mut ProjectionTimings>,
) -> Result<Option<T>, StoreError>
where
    T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    // Full replay -- batch-read filtered events from disk.
    // Uses read_events_batch which skips Coordinate construction (pure waste
    // for projection replay that only needs the event payload).
    let t_disk = std::time::Instant::now();
    let positions: Vec<&crate::store::DiskPos> = replay_plan
        .items
        .iter()
        .map(|item| &item.disk_pos)
        .collect();
    let events = store.reader.read_events_batch(&positions)?;
    if let Some(t) = timings.as_deref_mut() {
        t.disk_read_us = t_disk.elapsed().as_micros() as u64;
        // No separate extraction step -- read_events_batch returns Event directly.
        t.event_extract_us = 0;
    }

    let t_fold = std::time::Instant::now();
    let result = T::from_events(&events);
    if let Some(t) = timings.as_deref_mut() {
        t.replay_fold_us = t_fold.elapsed().as_micros() as u64;
    }

    if result.is_none() && !events.is_empty() {
        tracing::debug!(
            entity,
            event_count = events.len(),
            "projection returned None despite non-empty filtered event stream"
        );
    }

    // Cache store-back
    let t_store = std::time::Instant::now();
    if let Some(ref value) = result {
        if let Ok(bytes) = serde_json::to_vec(value) {
            let meta = projection::CacheMeta {
                watermark,
                cached_at_us,
            };
            if let Err(error) = store.cache.put(cache_key, &bytes, meta) {
                tracing::warn!("cache put failed (non-fatal): {error}");
            }
            let _ = store.index.store_cached_projection(
                entity,
                type_id,
                bytes,
                watermark,
                cached_at_us,
            );
        }
    }
    if let Some(t) = timings.as_deref_mut() {
        t.cache_store_us = t_store.elapsed().as_micros() as u64;
        t.total_us = t_start.elapsed().as_micros() as u64;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind};
    use crate::store::StoreConfig;
    use tempfile::TempDir;

    #[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
    struct Counter;

    impl EventSourced<serde_json::Value> for Counter {
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
            .map(|entry| crate::store::index::ProjectionReplayItem {
                global_sequence: entry.global_sequence,
                disk_pos: entry.disk_pos,
            })
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
        assert_eq!(plan.items, legacy_items);

        store.close().expect("close");
    }

    #[test]
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
        // No replay plan -> Empty regardless of other inputs
        assert_eq!(
            compute_strategy(false, None, false, false, false, false),
            ProjectionStrategy::Empty,
        );
        assert_eq!(
            compute_strategy(false, None, true, true, true, true),
            ProjectionStrategy::Empty,
        );

        let slot = CachedProjectionSlot {
            bytes: vec![],
            watermark: 42,
            generation: 1,
            cached_at_us: 100,
        };

        // Slot present + fresh -> GroupLocalHit
        assert_eq!(
            compute_strategy(true, Some(&slot), true, false, false, false),
            ProjectionStrategy::GroupLocalHit,
        );
        assert_eq!(
            compute_strategy(true, Some(&slot), true, true, true, true),
            ProjectionStrategy::GroupLocalHit,
        );

        // Slot present + stale + incremental supported + enabled -> GroupLocalIncremental
        assert_eq!(
            compute_strategy(true, Some(&slot), false, true, true, false),
            ProjectionStrategy::GroupLocalIncremental,
        );
        assert_eq!(
            compute_strategy(true, Some(&slot), false, true, true, true),
            ProjectionStrategy::GroupLocalIncremental,
        );

        // Slot present + stale + incremental NOT supported -> falls through to cache check
        assert_eq!(
            compute_strategy(true, Some(&slot), false, false, true, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
        assert_eq!(
            compute_strategy(true, Some(&slot), false, true, false, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
        assert_eq!(
            compute_strategy(true, Some(&slot), false, false, false, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );

        // Slot present + stale + incremental NOT supported + noop cache -> DirectReplay
        assert_eq!(
            compute_strategy(true, Some(&slot), false, false, false, true),
            ProjectionStrategy::DirectReplay,
        );

        // No slot + noop cache -> DirectReplay
        assert_eq!(
            compute_strategy(true, None, false, false, false, true),
            ProjectionStrategy::DirectReplay,
        );

        // No slot + real cache -> ExternalCacheThenReplay
        assert_eq!(
            compute_strategy(true, None, false, false, false, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
        assert_eq!(
            compute_strategy(true, None, false, true, true, false),
            ProjectionStrategy::ExternalCacheThenReplay,
        );
    }
}
