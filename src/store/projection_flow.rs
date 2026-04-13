use crate::event::EventSourced;
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
    pub disk_read_us: u64,
    pub replay_fold_us: u64,
    pub cache_store_us: u64,
    pub total_us: u64,
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

    // Phase 1: Build replay plan
    let relevant_kinds = T::relevant_event_kinds();
    let Some(replay_plan) = store.index.projection_replay_plan(entity, relevant_kinds) else {
        if let Some(t) = timings.as_deref_mut() {
            t.total_us = t_start.elapsed().as_micros() as u64;
        }
        return Ok(None);
    };
    if let Some(t) = timings.as_deref_mut() {
        t.plan_build_us = t_start.elapsed().as_micros() as u64;
    }

    let watermark = replay_plan.watermark;
    let entity_generation = replay_plan.generation;
    let cached_at_us = store.config.now_us();
    let type_id = TypeId::of::<T>();

    // Phase 2: Cache key construction
    let t_cache_key = std::time::Instant::now();
    let cache_key = projection_cache_key::<T>(entity);
    if let Some(t) = timings.as_deref_mut() {
        t.cache_key_build_us = t_cache_key.elapsed().as_micros() as u64;
    }

    // Phase 3: Group-local cache check
    let t_group = std::time::Instant::now();
    if let Some(slot) = store.index.cached_projection(entity, type_id) {
        if let Some(t) = timings.as_deref_mut() {
            t.group_local_lookup_us = t_group.elapsed().as_micros() as u64;
        }
        let is_fresh = match freshness {
            Freshness::Consistent => {
                slot.watermark == watermark && slot.generation == entity_generation
            }
            Freshness::MaybeStale { max_stale_ms } => {
                let age_us = cached_at_us.saturating_sub(slot.cached_at_us).max(0);
                age_us < (*max_stale_ms as i64) * 1000
                    && slot.generation == entity_generation
                    && slot.watermark <= watermark
            }
        };
        if is_fresh {
            match serde_json::from_slice::<T>(&slot.bytes) {
                Ok(value) => {
                    if let Some(t) = timings.as_deref_mut() {
                        t.total_us = t_start.elapsed().as_micros() as u64;
                    }
                    return Ok(Some(value));
                }
                Err(e) => {
                    tracing::warn!(
                        entity,
                        "group-local projection cache deserialize failed (falling back): {e}"
                    );
                }
            }
        }
    } else if let Some(t) = timings.as_deref_mut() {
        t.group_local_lookup_us = t_group.elapsed().as_micros() as u64;
    }

    // Phase 4: External cache prefetch
    let t_prefetch = std::time::Instant::now();
    let predicted_meta = projection::CacheMeta {
        watermark,
        cached_at_us,
    };
    if store.cache.capabilities().supports_prefetch {
        if let Err(error) = store.cache.prefetch(&cache_key, predicted_meta) {
            tracing::warn!("cache prefetch failed (non-fatal): {error}");
        }
    }
    if let Some(t) = timings.as_deref_mut() {
        t.prefetch_us = t_prefetch.elapsed().as_micros() as u64;
    }

    // Phase 5: External cache probe
    let t_ext = std::time::Instant::now();
    match store.cache.get(&cache_key) {
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
                        let stored = store.reader.read_entry(&de.disk_pos)?;
                        cached_state.apply_event(&stored.event);
                    }
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

    // Phase 6: Full replay — batch-read filtered entries from disk
    let t_disk = std::time::Instant::now();
    let positions: Vec<&crate::store::DiskPos> = replay_plan
        .items
        .iter()
        .map(|item| &item.disk_pos)
        .collect();
    let stored_events = store.reader.read_entries_batch(&positions)?;
    let events: Vec<_> = stored_events.into_iter().map(|s| s.event).collect();
    if let Some(t) = timings.as_deref_mut() {
        t.disk_read_us = t_disk.elapsed().as_micros() as u64;
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

    // Phase 7: Cache store-back
    let t_store = std::time::Instant::now();
    if let Some(ref value) = result {
        if let Ok(bytes) = serde_json::to_vec(value) {
            let meta = projection::CacheMeta {
                watermark,
                cached_at_us,
            };
            if let Err(error) = store.cache.put(&cache_key, &bytes, meta) {
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
        eprintln!("  disk_read:            {:>8} us", timings.disk_read_us);
        eprintln!("  replay_fold:          {:>8} us", timings.replay_fold_us);
        eprintln!("  cache_store:          {:>8} us", timings.cache_store_us);
        eprintln!("  total:                {:>8} us", timings.total_us);
        let accounted = timings.plan_build_us
            + timings.cache_key_build_us
            + timings.group_local_lookup_us
            + timings.prefetch_us
            + timings.external_cache_probe_us
            + timings.disk_read_us
            + timings.replay_fold_us
            + timings.cache_store_us;
        eprintln!(
            "  unaccounted:          {:>8} us",
            timings.total_us.saturating_sub(accounted)
        );

        assert!(timings.total_us > 0, "total must be positive");
        store.close().expect("close");
    }
}
