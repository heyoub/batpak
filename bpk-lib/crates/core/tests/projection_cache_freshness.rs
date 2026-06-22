//! Cache-FRESHNESS window semantics for projection reads: the watermark and
//! age-window decisions that pick between serving a cached value and forcing a
//! fresh replay under `Freshness::MaybeStale` and `Freshness::Consistent`.
//!
//! Integration tests: `cargo test --test projection_cache_freshness`
//! Backend mechanics live in `projection_cache`; corruption fall-backs live in
//! `projection_cache_corruption`.
//!
//! PROVES: FM-009 (Polite Downgrade — MaybeStale stale-window semantics stay
//!   honest) and LAW-001 (No Fake Success — a stale cache must not serve wrong
//!   data).
//! CATCHES: drift where MaybeStale ignores its age window, where a reopened
//!   stale watermark is trusted under Consistent, where project_if_changed
//!   pairs stale bytes with a new generation, or where an empty entity touches
//!   the cache at all.
//! SEEDED: deterministic watermark-advancing appends plus a clock-driven
//!   age-boundary fixture and a counting cache that pins zero cache traffic.
//! INVARIANTS: INV-CLOCK-NOW-US-LIVE (freshness semantics),
//!   INV-REPLAY-LANE-SELECTION (replay path selection).

use batpak_testkit::projection_cache as pc_support;

use batpak::store::projection::{CacheMeta, ProjectionCache};
use batpak::store::StoreError;
use pc_support::*;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct CacheProbeCounters {
    gets: AtomicUsize,
    puts: AtomicUsize,
    prefetches: AtomicUsize,
}

struct CountingCache {
    counters: Arc<CacheProbeCounters>,
}

impl ProjectionCache for CountingCache {
    fn capabilities(&self) -> batpak::store::projection::CacheCapabilities {
        batpak::store::projection::CacheCapabilities {
            is_noop: false,
            supports_prefetch: true,
        }
    }

    fn get(&self, _key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        self.counters.gets.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }

    fn put(&self, _key: &[u8], _value: &[u8], _meta: CacheMeta) -> Result<(), StoreError> {
        self.counters.puts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn delete_prefix(&self, _prefix: &[u8]) -> Result<u64, StoreError> {
        Ok(0)
    }

    fn sync(&self) -> Result<(), StoreError> {
        Ok(())
    }

    fn prefetch(&self, _key: &[u8], _predicted_meta: CacheMeta) -> Result<(), StoreError> {
        self.counters.prefetches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

const MAYBE_STALE_GENERATION_KIND: batpak::prelude::EventKind =
    batpak::prelude::EventKind::custom(0xF, 0x51);

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct MaybeStaleGenerationCounter {
    count: u32,
}

impl batpak::prelude::EventSourced for MaybeStaleGenerationCounter {
    type Input = batpak::prelude::JsonValueInput;

    fn from_events(events: &[batpak::prelude::Event<serde_json::Value>]) -> Option<Self> {
        Some(Self {
            count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
        })
    }

    fn apply_event(&mut self, _event: &batpak::prelude::Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [batpak::prelude::EventKind] {
        &[MAYBE_STALE_GENERATION_KIND]
    }
}

#[test]
fn freshness_maybe_stale_serves_stale_cache_within_window() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, NativeCache, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");

    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Store::open_with_cache(config, Box::new(cache)).expect("open store");

    let coord = Coordinate::new("entity:besteff1", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    let _ = store
        .append(&coord, kind, &serde_json::json!({"x": 1}))
        .expect("append 1");
    let _ = store
        .append(&coord, kind, &serde_json::json!({"x": 2}))
        .expect("append 2");

    // Project with Consistent to populate cache
    let result: Option<MaybeStaleCounter> = store
        .project("entity:besteff1", &Freshness::Consistent)
        .expect("project consistent");
    assert_eq!(result, Some(MaybeStaleCounter { count: 2 }));

    // Append a third event — cache is now stale
    let _ = store
        .append(&coord, kind, &serde_json::json!({"x": 3}))
        .expect("append 3");

    // MaybeStale with large window should serve the stale cached value
    let result_best: Option<MaybeStaleCounter> = store
        .project(
            "entity:besteff1",
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("project maybe stale");
    assert_eq!(
        result_best,
        Some(MaybeStaleCounter { count: 2 }),
        "FRESHNESS BEST EFFORT: with large stale window, should serve cached value (count=2) \
         even though a 3rd event was appended.\n\
         Investigate: src/store/mod.rs project() MaybeStale branch."
    );

    // MaybeStale with zero window should force re-replay
    let result_strict: Option<MaybeStaleCounter> = store
        .project(
            "entity:besteff1",
            &Freshness::MaybeStale { max_stale_ms: 0 },
        )
        .expect("project maybe stale strict");
    assert_eq!(
        result_strict,
        Some(MaybeStaleCounter { count: 3 }),
        "FRESHNESS BEST EFFORT ZERO: with max_stale_ms=0, cache should always be considered \
         stale, forcing a full replay (count=3)."
    );

    store.close().expect("close");
}

#[test]
fn project_if_changed_never_pairs_maybe_stale_cache_with_new_generation() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, NativeCache, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");

    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Store::open_with_cache(config, Box::new(cache)).expect("open store");

    let coord = Coordinate::new("entity:generation-honesty", "scope:test").expect("coord");
    let _ = store
        .append(
            &coord,
            MAYBE_STALE_GENERATION_KIND,
            &serde_json::json!({"x": 1}),
        )
        .expect("append 1");
    let _ = store
        .append(
            &coord,
            MAYBE_STALE_GENERATION_KIND,
            &serde_json::json!({"x": 2}),
        )
        .expect("append 2");

    let seeded: Option<MaybeStaleGenerationCounter> = store
        .project("entity:generation-honesty", &Freshness::Consistent)
        .expect("seed cache");
    assert_eq!(seeded, Some(MaybeStaleGenerationCounter { count: 2 }));

    let baseline_generation = store
        .entity_generation("entity:generation-honesty")
        .expect("baseline generation");

    let _ = store
        .append(
            &coord,
            MAYBE_STALE_GENERATION_KIND,
            &serde_json::json!({"x": 3}),
        )
        .expect("append 3");

    let changed = store
        .project_if_changed::<MaybeStaleGenerationCounter>(
            "entity:generation-honesty",
            baseline_generation,
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("project_if_changed")
        .expect("changed projection");

    assert!(
        changed.0 > baseline_generation,
        "generation should advance after the third relevant append"
    );
    assert_eq!(
        changed.1,
        Some(MaybeStaleGenerationCounter { count: 3 }),
        "PROPERTY: project_if_changed must not return stale cache bytes together with a newer generation token.\n\
         Investigate: src/store/projection/flow.rs project_if_changed() MaybeStale path."
    );

    store.close().expect("close");
}

#[test]
fn empty_projection_surface_skips_cache_for_no_replay_plan() {
    use batpak::store::{Freshness, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let counters = Arc::new(CacheProbeCounters::default());
    let cache = CountingCache {
        counters: Arc::clone(&counters),
    };
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Store::open_with_cache(config, Box::new(cache)).expect("open store");

    let consistent: Option<MaybeStaleCounter> = store
        .project("entity:no-events", &Freshness::Consistent)
        .expect("project empty consistent");
    let maybe_stale: Option<MaybeStaleCounter> = store
        .project(
            "entity:no-events",
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("project empty maybe stale");
    let unchanged = store
        .project_if_changed::<MaybeStaleCounter>("entity:no-events", 0, &Freshness::Consistent)
        .expect("project_if_changed empty");

    assert_eq!(
        consistent, None,
        "EMPTY PROJECTION SURFACE: project() on an entity with no replay plan should return None."
    );
    assert_eq!(
        maybe_stale, None,
        "EMPTY PROJECTION SURFACE: MaybeStale must not invent a cache-backed value for an empty entity."
    );
    assert_eq!(
        unchanged, None,
        "EMPTY PROJECTION SURFACE: project_if_changed() should report no change for a never-seen entity."
    );
    assert_eq!(
        counters.gets.load(Ordering::SeqCst),
        0,
        "EMPTY PROJECTION SURFACE: no replay plan should skip external cache get entirely."
    );
    assert_eq!(
        counters.prefetches.load(Ordering::SeqCst),
        0,
        "EMPTY PROJECTION SURFACE: no replay plan should skip cache prefetch entirely."
    );
    assert_eq!(
        counters.puts.load(Ordering::SeqCst),
        0,
        "EMPTY PROJECTION SURFACE: no replay plan should not populate cache."
    );

    store.close().expect("close");
}

#[test]
fn consistent_replays_when_reopened_native_cache_row_is_stale() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let coord = Coordinate::new("entity:consistent-stale-cache", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);

    {
        let store = Store::open_with_native_cache(config.clone(), &cache_path).expect("open store");
        let _ = store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        let _ = store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");
        let seeded: Option<MaybeStaleCounter> = store
            .project("entity:consistent-stale-cache", &Freshness::Consistent)
            .expect("seed cache");
        assert_eq!(seeded, Some(MaybeStaleCounter { count: 2 }));
        store.close().expect("close seeded store");
    }

    {
        let store = Store::open_with_native_cache(config, &cache_path).expect("reopen store");
        let _ = store
            .append(&coord, kind, &serde_json::json!({"x": 3}))
            .expect("append 3");
        let result: Option<MaybeStaleCounter> = store
            .project("entity:consistent-stale-cache", &Freshness::Consistent)
            .expect("project after stale cache");
        assert_eq!(
            result,
            Some(MaybeStaleCounter { count: 3 }),
            "CONSISTENT STALE CACHE HONESTY: after reopen, a populated external cache row with an older watermark must be bypassed and replayed."
        );
        store.close().expect("close");
    }
}

#[test]
fn freshness_maybe_stale_replays_at_exact_age_boundary() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, NativeCache, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");
    let now_us = Arc::new(AtomicI64::new(1_000_000));
    let clock = Arc::clone(&now_us);

    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1)
        .with_clock_fn(move || clock.load(Ordering::SeqCst));
    let store = Store::open_with_cache(config, Box::new(cache)).expect("open store");

    let coord = Coordinate::new("entity:maybe-stale-boundary", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    let _ = store
        .append(&coord, kind, &serde_json::json!({"x": 1}))
        .expect("append 1");
    let _ = store
        .append(&coord, kind, &serde_json::json!({"x": 2}))
        .expect("append 2");

    let seeded: Option<MaybeStaleCounter> = store
        .project("entity:maybe-stale-boundary", &Freshness::Consistent)
        .expect("seed cache");
    assert_eq!(seeded, Some(MaybeStaleCounter { count: 2 }));

    let _ = store
        .append(&coord, kind, &serde_json::json!({"x": 3}))
        .expect("append 3");

    now_us.store(1_005_000, Ordering::SeqCst);
    let result: Option<MaybeStaleCounter> = store
        .project(
            "entity:maybe-stale-boundary",
            &Freshness::MaybeStale { max_stale_ms: 5 },
        )
        .expect("project maybe stale at boundary");
    assert_eq!(
        result,
        Some(MaybeStaleCounter { count: 3 }),
        "MAYBE STALE BOUNDARY HONESTY: when cache age equals max_stale_ms exactly, the strict '<' boundary must force replay rather than serve the stale row."
    );

    store.close().expect("close");
}
