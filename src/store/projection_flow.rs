use crate::event::EventSourced;
use crate::store::{projection, Freshness, Store, StoreError};

pub(crate) fn project<T>(
    store: &Store,
    entity: &str,
    freshness: &Freshness,
) -> Result<Option<T>, StoreError>
where
    T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned,
{
    tracing::debug!(
        target: "batpak::flow",
        flow = "project",
        entity,
        freshness = match freshness {
            Freshness::Consistent => "consistent",
            Freshness::BestEffort { .. } => "best_effort",
        }
    );
    let entries = store.index.stream(entity);
    if entries.is_empty() {
        return Ok(None);
    }

    // Filter by relevant_event_kinds() — hard filter at the index level.
    // Empty slice = no filter = replay all events.
    let relevant_kinds = T::relevant_event_kinds();
    let entries: Vec<_> = if relevant_kinds.is_empty() {
        entries
    } else {
        entries
            .into_iter()
            .filter(|e| relevant_kinds.contains(&e.kind))
            .collect()
    };
    if entries.is_empty() {
        return Ok(None);
    }

    let watermark = entries.last().map(|e| e.global_sequence).unwrap_or(0);

    // Schema-versioned cache key: entity\0v{schema_version}
    // Different projection types (or schema versions) get separate cache slots.
    let schema_v = T::schema_version();
    let cache_key: Vec<u8> = if schema_v == 0 {
        entity.as_bytes().to_vec()
    } else {
        let mut key = entity.as_bytes().to_vec();
        key.push(0); // null separator
        key.extend_from_slice(b"v");
        key.extend_from_slice(schema_v.to_string().as_bytes());
        key
    };

    let predicted_meta = projection::CacheMeta {
        watermark,
        cached_at_us: store.config.now_us(),
    };
    if store.cache.capabilities().supports_prefetch {
        if let Err(error) = store.cache.prefetch(&cache_key, predicted_meta) {
            tracing::warn!("cache prefetch failed (non-fatal): {error}");
        }
    }

    match store.cache.get(&cache_key) {
        Ok(Some((bytes, meta))) => {
            let is_fresh = match freshness {
                Freshness::Consistent => meta.watermark == watermark,
                Freshness::BestEffort { max_stale_ms } => {
                    let age_us = store
                        .config
                        .now_us()
                        .saturating_sub(meta.cached_at_us)
                        .max(0);
                    age_us < (*max_stale_ms as i64) * 1000
                }
            };

            // Incremental apply path: if the cache is stale but we have a baseline,
            // and the projection type opts in, apply only delta events.
            if !is_fresh
                && T::supports_incremental_apply()
                && store.config.incremental_projection
            {
                let cached_watermark = meta.watermark;
                // Delta: entries with global_sequence > cached watermark
                let delta_entries: Vec<_> = entries
                    .iter()
                    .filter(|e| e.global_sequence > cached_watermark)
                    .collect();
                if let Ok(mut cached_state) = serde_json::from_slice::<T>(&bytes) {
                    // Read only delta events from disk
                    for de in &delta_entries {
                        let stored = store.reader.read_entry(&de.disk_pos)?;
                        cached_state.apply_event(&stored.event);
                    }
                    // Write back updated state
                    if let Ok(new_bytes) = serde_json::to_vec(&cached_state) {
                        let new_meta = projection::CacheMeta {
                            watermark,
                            cached_at_us: store.config.now_us(),
                        };
                        if let Err(e) = store.cache.put(&cache_key, &new_bytes, new_meta) {
                            tracing::warn!("incremental cache put failed: {e}");
                        }
                    }
                    return Ok(Some(cached_state));
                }
                // If deser fails, fall through to full replay
            }

            if is_fresh {
                match serde_json::from_slice::<T>(&bytes) {
                    Ok(value) => return Ok(Some(value)),
                    Err(e) => {
                        tracing::warn!("cache deserialize failed (falling back to replay): {e}");
                    }
                }
            }
        }
        Ok(None) => { /* cache miss — expected, fall through to replay */ }
        Err(e) => {
            tracing::warn!("cache get failed (falling back to replay): {e}");
        }
    }

    // Full replay: batch-read all filtered entries from disk.
    let positions: Vec<&crate::store::DiskPos> = entries.iter().map(|e| &e.disk_pos).collect();
    let stored_events = store.reader.read_entries_batch(&positions)?;
    let events: Vec<_> = stored_events.into_iter().map(|s| s.event).collect();
    let result = T::from_events(&events);

    if let Some(ref value) = result {
        if let Ok(bytes) = serde_json::to_vec(value) {
            let meta = projection::CacheMeta {
                watermark,
                cached_at_us: store.config.now_us(),
            };
            if let Err(error) = store.cache.put(&cache_key, &bytes, meta) {
                tracing::warn!("cache put failed (non-fatal): {error}");
            }
        }
    }

    Ok(result)
}
