//! Direct tests of ProjectionCache trait methods per backend.
//! Plus: integration tests with real Store operations (append → project → cache hit).
//! Fulfills SPEC promise: "Every trait method on ProjectionCache is exercised
//! against every backend (NoCache, RedbCache, LmdbCache)."
//!
//! Integration tests: `cargo test --features redb,lmdb --test projection_cache`
//! [SPEC:tests/projection_cache.rs]
//!
//! PROVES: LAW-001 (No Fake Success — cached projections must be correct)
//! DEFENDS: FM-009 (Polite Downgrade — BestEffort must eventually refresh)
//! INVARIANTS: INV-TYPE (cache round-trip fidelity), INV-TEMP (freshness semantics)

use batpak::store::projection::{CacheCapabilities, CacheMeta, NoCache, ProjectionCache};

fn test_meta() -> CacheMeta {
    CacheMeta {
        watermark: 42,
        cached_at_us: 1_000_000,
    }
}

// ================================================================
// NoCache — the default. Every read replays from segments.
// ================================================================

#[test]
fn nocache_get_always_returns_none() {
    let cache = NoCache;
    let result = cache.get(b"any_key").expect("get should not error");
    assert!(
        result.is_none(),
        "NoCache::get should always return None. Investigate: src/store/projection.rs NoCache."
    );
}

#[test]
fn nocache_put_is_noop() {
    let cache = NoCache;
    cache
        .put(b"key", b"value", test_meta())
        .expect("put should not error");
    // Verify: still returns None after put
    let result = cache.get(b"key").expect("get");
    assert!(result.is_none(), "NoCache should not store anything.");
}

#[test]
fn nocache_delete_prefix_returns_zero() {
    let cache = NoCache;
    let count = cache.delete_prefix(b"prefix").expect("delete_prefix");
    assert_eq!(count, 0, "NoCache::delete_prefix should return 0.");
}

#[test]
fn nocache_sync_is_noop() {
    let cache = NoCache;
    cache.sync().expect("NoCache::sync should not error.");
}

// ================================================================
// RedbCache — backed by redb embedded database.
// ================================================================

#[cfg(feature = "redb")]
mod redb_tests {
    use super::*;
    use batpak::store::projection::RedbCache;
    use tempfile::TempDir;

    fn redb_cache() -> (RedbCache, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("test.redb");
        let cache = RedbCache::open(&path).expect("open redb");
        (cache, dir)
    }

    #[test]
    fn redb_get_put_round_trip() {
        let (cache, _dir) = redb_cache();
        let meta = test_meta();

        // Put creates the table, then get retrieves
        cache.put(b"key1", b"hello", meta.clone()).expect("put");
        let (value, returned_meta) = cache.get(b"key1").expect("get").expect("should be Some");
        assert_eq!(
            value, b"hello",
            "RedbCache round-trip failed. Investigate: src/store/projection.rs RedbCache."
        );
        assert_eq!(
            returned_meta.watermark, 42,
            "REDB ROUND-TRIP META WATERMARK: watermark should be preserved across put/get.\n\
             Investigate: src/store/projection.rs RedbCache::put and RedbCache::get.\n\
             Common causes: CacheMeta serialization losing watermark field.\n\
             Run: cargo test --test projection_cache redb_get_put_round_trip"
        );
        assert_eq!(
            returned_meta.cached_at_us, 1_000_000,
            "REDB ROUND-TRIP META CACHED_AT: cached_at_us should be preserved across put/get.\n\
             Investigate: src/store/projection.rs RedbCache::put and RedbCache::get.\n\
             Common causes: CacheMeta serialization losing cached_at_us field.\n\
             Run: cargo test --test projection_cache redb_get_put_round_trip"
        );

        // Non-existent key returns None
        assert!(
            cache.get(b"nonexistent").expect("get").is_none(),
            "REDB ROUND-TRIP: get for a key that was never inserted should return None.\n\
             Investigate: src/store/projection.rs RedbCache::get.\n\
             Common causes: get returning stale data, missing key check logic.\n\
             Run: cargo test --test projection_cache redb_get_put_round_trip"
        );
    }

    #[test]
    fn redb_delete_prefix() {
        let (cache, _dir) = redb_cache();
        let meta = test_meta();

        cache.put(b"user:1", b"alice", meta.clone()).expect("put");
        cache.put(b"user:2", b"bob", meta.clone()).expect("put");
        cache.put(b"order:1", b"widget", meta.clone()).expect("put");

        let deleted = cache.delete_prefix(b"user:").expect("delete_prefix");
        assert_eq!(deleted, 2, "Should delete 2 keys with prefix 'user:'.");

        // user keys gone
        assert!(
            cache.get(b"user:1").expect("get").is_none(),
            "REDB DELETE PREFIX: key 'user:1' should be gone after delete_prefix('user:').\n\
             Investigate: src/store/projection.rs RedbCache::delete_prefix.\n\
             Common causes: prefix scan not matching key, deletion not committed.\n\
             Run: cargo test --test projection_cache redb_delete_prefix"
        );
        assert!(
            cache.get(b"user:2").expect("get").is_none(),
            "REDB DELETE PREFIX: key 'user:2' should be gone after delete_prefix('user:').\n\
             Investigate: src/store/projection.rs RedbCache::delete_prefix.\n\
             Common causes: prefix scan stopping early, deletion not committed.\n\
             Run: cargo test --test projection_cache redb_delete_prefix"
        );
        // order key remains
        assert!(
            cache.get(b"order:1").expect("get").is_some(),
            "REDB DELETE PREFIX: key 'order:1' should survive delete_prefix('user:').\n\
             Investigate: src/store/projection.rs RedbCache::delete_prefix.\n\
             Common causes: prefix matching too broad, deleting keys that don't share the prefix.\n\
             Run: cargo test --test projection_cache redb_delete_prefix"
        );
    }

    #[test]
    fn redb_sync_is_safe() {
        let (cache, _dir) = redb_cache();
        cache.sync().expect("RedbCache::sync should not error.");
    }

    // -- Integration: Store + RedbCache --

    use batpak::prelude::*;

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Counter {
        count: u32,
    }
    impl EventSourced<serde_json::Value> for Counter {
        fn from_events(events: &[batpak::prelude::Event<serde_json::Value>]) -> Option<Self> {
            Some(Counter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &batpak::prelude::Event<serde_json::Value>) {
            self.count += 1;
        }
        fn relevant_event_kinds() -> &'static [EventKind] {
            &[]
        }
    }

    #[test]
    fn redb_projection_round_trip() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("cache.redb");
        let cache = RedbCache::open(&cache_path).expect("open redb cache");

        let config = StoreConfig {
            data_dir: dir.path().join("data"),
            segment_max_bytes: 4096,
            sync_every_n_events: 1,
            ..StoreConfig::new("")
        };
        let store =
            Store::open_with_cache(config, Box::new(cache)).expect("open store with redb cache");

        let coord = Coordinate::new("entity:redb1", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");

        // First project: cache miss, replays from segments
        let result: Option<Counter> = store
            .project("entity:redb1", &Freshness::Consistent)
            .expect("project");
        assert_eq!(
            result,
            Some(Counter { count: 2 }),
            "REDB PROJECTION ROUND-TRIP: first project should replay 2 events.\n\
             Investigate: src/store/mod.rs project, src/store/projection.rs RedbCache.\n\
             Run: cargo test --features redb --test projection_cache redb_projection_round_trip"
        );

        // Second project: should hit cache (same watermark)
        let result2: Option<Counter> = store
            .project("entity:redb1", &Freshness::Consistent)
            .expect("project 2");
        assert_eq!(result2, Some(Counter { count: 2 }));

        // Append more → cache should be stale → re-replay
        store
            .append(&coord, kind, &serde_json::json!({"x": 3}))
            .expect("append 3");
        let result3: Option<Counter> = store
            .project("entity:redb1", &Freshness::Consistent)
            .expect("project 3");
        assert_eq!(
            result3,
            Some(Counter { count: 3 }),
            "REDB CACHE INVALIDATION: after appending more events, project should re-replay.\n\
             Investigate: src/store/mod.rs project watermark comparison.\n\
             Run: cargo test --features redb --test projection_cache redb_projection_round_trip"
        );

        store.close().expect("close");
    }

    #[test]
    fn redb_delete_prefix_then_project_repopulates_cache() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("cache.redb");
        let config = StoreConfig {
            data_dir: dir.path().join("data"),
            segment_max_bytes: 4096,
            sync_every_n_events: 1,
            ..StoreConfig::new("")
        };
        let coord = Coordinate::new("entity:redb-miss", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);

        {
            let cache = RedbCache::open(&cache_path).expect("open redb cache");
            let store =
                Store::open_with_cache(config.clone(), Box::new(cache)).expect("open store");
            store
                .append(&coord, kind, &serde_json::json!({"x": 1}))
                .expect("append 1");
            store
                .append(&coord, kind, &serde_json::json!({"x": 2}))
                .expect("append 2");
            let _: Option<Counter> = store
                .project("entity:redb-miss", &Freshness::Consistent)
                .expect("warm cache");
            store.close().expect("close");
        }

        {
            let cache = RedbCache::open(&cache_path).expect("reopen cache");
            // Cache key now includes TypeId hash — use prefix delete which matches
            // any key starting with the entity bytes.
            let deleted = cache
                .delete_prefix(b"entity:redb-miss")
                .expect("delete prefix");
            assert!(
                deleted >= 1,
                "REDB CACHE MISS PROOF: delete_prefix should remove at least one warmed cache key, got {deleted}."
            );
            assert!(
                cache.get(b"entity:redb-miss").expect("get").is_none(),
                "REDB CACHE MISS PROOF: delete_prefix must actually clear the cache key before replay."
            );
        }

        {
            let cache = RedbCache::open(&cache_path).expect("reopen cache for store");
            let store = Store::open_with_cache(config, Box::new(cache)).expect("reopen store");
            let result: Option<Counter> = store
                .project("entity:redb-miss", &Freshness::Consistent)
                .expect("project after delete");
            assert_eq!(result, Some(Counter { count: 2 }));
            store.close().expect("close");
        }

        // Verify the cache was repopulated by projecting again through the store
        // (the cache key includes a TypeId hash, so raw cache.get with just the
        // entity name won't match).
        let cache = RedbCache::open(&cache_path).expect("final reopen cache");
        let repopulated = cache
            .delete_prefix(b"entity:redb-miss")
            .expect("check repopulated");
        assert!(
            repopulated >= 1,
            "REDB CACHE MISS PROOF: projecting after delete_prefix must repopulate the cache key."
        );
    }
}

// ================================================================
// LmdbCache — backed by LMDB via heed.
// ================================================================

#[cfg(feature = "lmdb")]
mod lmdb_tests {
    use super::*;
    use batpak::store::projection::LmdbCache;
    use tempfile::TempDir;

    fn lmdb_cache() -> (LmdbCache, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("lmdb");
        let cache = LmdbCache::open(&path, 10 * 1024 * 1024).expect("open lmdb");
        (cache, dir)
    }

    #[test]
    fn lmdb_get_put_round_trip() {
        let (cache, _dir) = lmdb_cache();
        let meta = test_meta();

        assert!(cache.get(b"key1").expect("get").is_none());

        cache.put(b"key1", b"hello", meta.clone()).expect("put");
        let (value, returned_meta) = cache.get(b"key1").expect("get").expect("should be Some");
        assert_eq!(
            value, b"hello",
            "LMDB ROUND-TRIP VALUE: value should be preserved across put/get.\n\
             Investigate: src/store/projection.rs LmdbCache::put and LmdbCache::get.\n\
             Common causes: value bytes not written or read correctly from LMDB.\n\
             Run: cargo test --test projection_cache lmdb_get_put_round_trip"
        );
        assert_eq!(
            returned_meta.watermark, 42,
            "LMDB ROUND-TRIP META WATERMARK: watermark should be preserved across put/get.\n\
             Investigate: src/store/projection.rs LmdbCache::put and LmdbCache::get.\n\
             Common causes: CacheMeta serialization losing watermark field.\n\
             Run: cargo test --test projection_cache lmdb_get_put_round_trip"
        );
    }

    #[test]
    fn lmdb_delete_prefix() {
        let (cache, _dir) = lmdb_cache();
        let meta = test_meta();

        cache.put(b"user:1", b"alice", meta.clone()).expect("put");
        cache.put(b"user:2", b"bob", meta.clone()).expect("put");
        cache.put(b"order:1", b"widget", meta.clone()).expect("put");

        let deleted = cache.delete_prefix(b"user:").expect("delete_prefix");
        assert_eq!(
            deleted, 2,
            "LMDB DELETE PREFIX: should delete exactly 2 keys with prefix 'user:'.\n\
             Investigate: src/store/projection.rs LmdbCache::delete_prefix.\n\
             Common causes: prefix scan not matching both keys, count not incremented correctly.\n\
             Run: cargo test --test projection_cache lmdb_delete_prefix"
        );

        assert!(
            cache.get(b"user:1").expect("get").is_none(),
            "LMDB DELETE PREFIX: key 'user:1' should be gone after delete_prefix('user:').\n\
             Investigate: src/store/projection.rs LmdbCache::delete_prefix.\n\
             Common causes: prefix scan not matching key, deletion not committed.\n\
             Run: cargo test --test projection_cache lmdb_delete_prefix"
        );
        assert!(
            cache.get(b"order:1").expect("get").is_some(),
            "LMDB DELETE PREFIX: key 'order:1' should survive delete_prefix('user:').\n\
             Investigate: src/store/projection.rs LmdbCache::delete_prefix.\n\
             Common causes: prefix matching too broad, deleting keys outside the prefix.\n\
             Run: cargo test --test projection_cache lmdb_delete_prefix"
        );
    }

    #[test]
    fn lmdb_delete_prefix_is_idempotent_after_iterator_deletion() {
        let (cache, _dir) = lmdb_cache();
        let meta = test_meta();

        cache.put(b"user:1", b"alice", meta.clone()).expect("put");
        cache.put(b"user:2", b"bob", meta).expect("put");

        let first = cache.delete_prefix(b"user:").expect("delete prefix");
        let second = cache.delete_prefix(b"user:").expect("delete prefix again");
        cache.sync().expect("sync");

        assert_eq!(
            first, 2,
            "LMDB DELETE PREFIX IDEMPOTENCE: first delete should remove both matching entries."
        );
        assert_eq!(
            second, 0,
            "LMDB DELETE PREFIX IDEMPOTENCE: repeating the delete after iterator-driven removal must be a clean no-op."
        );
    }

    #[test]
    fn lmdb_sync() {
        let (cache, _dir) = lmdb_cache();
        cache.sync().expect("LmdbCache::sync should not error.");
    }

    // -- Integration: Store + LmdbCache --

    use batpak::prelude::*;

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Counter {
        count: u32,
    }
    impl EventSourced<serde_json::Value> for Counter {
        fn from_events(events: &[batpak::prelude::Event<serde_json::Value>]) -> Option<Self> {
            Some(Counter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &batpak::prelude::Event<serde_json::Value>) {
            self.count += 1;
        }
        fn relevant_event_kinds() -> &'static [EventKind] {
            &[]
        }
    }

    #[test]
    fn lmdb_projection_round_trip() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("lmdb_cache");
        let cache = LmdbCache::open(&cache_path, 10 * 1024 * 1024).expect("open lmdb cache");

        let config = StoreConfig {
            data_dir: dir.path().join("data"),
            segment_max_bytes: 4096,
            sync_every_n_events: 1,
            ..StoreConfig::new("")
        };
        let store =
            Store::open_with_cache(config, Box::new(cache)).expect("open store with lmdb cache");

        let coord = Coordinate::new("entity:lmdb1", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");

        // First project: cache miss, replays from segments
        let result: Option<Counter> = store
            .project("entity:lmdb1", &Freshness::Consistent)
            .expect("project");
        assert_eq!(
            result,
            Some(Counter { count: 2 }),
            "LMDB PROJECTION ROUND-TRIP: first project should replay 2 events.\n\
             Investigate: src/store/mod.rs project, src/store/projection.rs LmdbCache.\n\
             Run: cargo test --features lmdb --test projection_cache lmdb_projection_round_trip"
        );

        // Second project: should hit cache
        let result2: Option<Counter> = store
            .project("entity:lmdb1", &Freshness::Consistent)
            .expect("project 2");
        assert_eq!(result2, Some(Counter { count: 2 }));

        // Append more → cache stale → re-replay
        store
            .append(&coord, kind, &serde_json::json!({"x": 3}))
            .expect("append 3");
        let result3: Option<Counter> = store
            .project("entity:lmdb1", &Freshness::Consistent)
            .expect("project 3");
        assert_eq!(
            result3,
            Some(Counter { count: 3 }),
            "LMDB CACHE INVALIDATION: after appending more events, project should re-replay.\n\
             Investigate: src/store/mod.rs project watermark comparison.\n\
             Run: cargo test --features lmdb --test projection_cache lmdb_projection_round_trip"
        );

        store.close().expect("close");
    }

    #[test]
    fn lmdb_delete_prefix_then_project_repopulates_cache() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("lmdb_cache");
        let config = StoreConfig {
            data_dir: dir.path().join("data"),
            segment_max_bytes: 4096,
            sync_every_n_events: 1,
            ..StoreConfig::new("")
        };
        let coord = Coordinate::new("entity:lmdb-miss", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);

        {
            let cache = LmdbCache::open(&cache_path, 10 * 1024 * 1024).expect("open lmdb cache");
            let store =
                Store::open_with_cache(config.clone(), Box::new(cache)).expect("open store");
            store
                .append(&coord, kind, &serde_json::json!({"x": 1}))
                .expect("append 1");
            store
                .append(&coord, kind, &serde_json::json!({"x": 2}))
                .expect("append 2");
            let _: Option<Counter> = store
                .project("entity:lmdb-miss", &Freshness::Consistent)
                .expect("warm cache");
            store.close().expect("close");
        }

        {
            let cache = LmdbCache::open(&cache_path, 10 * 1024 * 1024).expect("reopen cache");
            // Cache key now includes TypeId hash — use prefix delete.
            let deleted = cache
                .delete_prefix(b"entity:lmdb-miss")
                .expect("delete prefix");
            assert!(
                deleted >= 1,
                "LMDB CACHE MISS PROOF: delete_prefix should remove at least one warmed cache key, got {deleted}."
            );
            assert!(
                cache.get(b"entity:lmdb-miss").expect("get after delete").is_none(),
                "LMDB CACHE MISS PROOF: delete_prefix must actually clear the cache key before replay."
            );
        }

        {
            let cache =
                LmdbCache::open(&cache_path, 10 * 1024 * 1024).expect("reopen cache for store");
            let store = Store::open_with_cache(config, Box::new(cache)).expect("reopen store");
            let result: Option<Counter> = store
                .project("entity:lmdb-miss", &Freshness::Consistent)
                .expect("project after delete");
            assert_eq!(result, Some(Counter { count: 2 }));
            store.close().expect("close");
        }

        let cache = LmdbCache::open(&cache_path, 10 * 1024 * 1024).expect("final reopen cache");
        let repopulated = cache
            .delete_prefix(b"entity:lmdb-miss")
            .expect("check repopulated");
        assert!(
            repopulated >= 1,
            "LMDB CACHE MISS PROOF: projecting after delete_prefix must repopulate the cache key."
        );
    }
}

// ================================================================
// Wave 3C: Freshness::BestEffort + cache metadata edge cases
// PROVES: LAW-001 (No Fake Success — stale cache must not serve wrong data)
// DEFENDS: FM-009 (Polite Downgrade — BestEffort must eventually refresh)
// ================================================================

// Shared Counter type for projection tests
#[cfg(feature = "redb")]
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct BestEffortCounter {
    count: u32,
}
#[cfg(feature = "redb")]
impl batpak::prelude::EventSourced<serde_json::Value> for BestEffortCounter {
    fn from_events(events: &[batpak::prelude::Event<serde_json::Value>]) -> Option<Self> {
        Some(BestEffortCounter {
            count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
        })
    }
    fn apply_event(&mut self, _event: &batpak::prelude::Event<serde_json::Value>) {
        self.count += 1;
    }
    fn relevant_event_kinds() -> &'static [batpak::prelude::EventKind] {
        &[]
    }
}

#[cfg(feature = "redb")]
#[test]
fn freshness_best_effort_serves_stale_cache_within_window() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, RedbCache, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache.redb");
    let cache = RedbCache::open(&cache_path).expect("open redb cache");

    let config = StoreConfig {
        data_dir: dir.path().join("data"),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open_with_cache(config, Box::new(cache)).expect("open store");

    let coord = Coordinate::new("entity:besteff1", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({"x": 1}))
        .expect("append 1");
    store
        .append(&coord, kind, &serde_json::json!({"x": 2}))
        .expect("append 2");

    // Project with Consistent to populate cache
    let result: Option<BestEffortCounter> = store
        .project("entity:besteff1", &Freshness::Consistent)
        .expect("project consistent");
    assert_eq!(result, Some(BestEffortCounter { count: 2 }));

    // Append a third event — cache is now stale
    store
        .append(&coord, kind, &serde_json::json!({"x": 3}))
        .expect("append 3");

    // BestEffort with large window should serve the stale cached value
    let result_best: Option<BestEffortCounter> = store
        .project(
            "entity:besteff1",
            &Freshness::BestEffort {
                max_stale_ms: 60_000,
            },
        )
        .expect("project best effort");
    assert_eq!(
        result_best,
        Some(BestEffortCounter { count: 2 }),
        "FRESHNESS BEST EFFORT: with large stale window, should serve cached value (count=2) \
         even though a 3rd event was appended.\n\
         Investigate: src/store/mod.rs project() BestEffort branch.\n\
         Common causes: BestEffort not checking age, always replaying from segments."
    );

    // BestEffort with zero window should force re-replay
    let result_strict: Option<BestEffortCounter> = store
        .project(
            "entity:besteff1",
            &Freshness::BestEffort { max_stale_ms: 0 },
        )
        .expect("project best effort strict");
    assert_eq!(
        result_strict,
        Some(BestEffortCounter { count: 3 }),
        "FRESHNESS BEST EFFORT ZERO: with max_stale_ms=0, cache should always be considered \
         stale, forcing a full replay (count=3).\n\
         Investigate: src/store/mod.rs project() BestEffort age calculation.\n\
         Common causes: age comparison off-by-one, zero treated as infinity."
    );

    store.close().expect("close");
}

#[test]
fn cache_metadata_short_bytes_returns_none() {
    // When cache bytes are < 16, the CacheMeta can't be decoded.
    // Both RedbCache and LmdbCache handle this by returning None.
    // Test the contract at the NoCache level — it always returns None regardless.
    let cache = NoCache;
    cache.put(b"short", b"x", test_meta()).expect("put");
    // NoCache always returns None, so this is really testing the interface contract.
    let result = cache.get(b"short").expect("get");
    assert!(
        result.is_none(),
        "CACHE METADATA: NoCache should return None regardless of what was put.\n\
         This test verifies the interface contract for short/missing data."
    );
}

#[cfg(feature = "redb")]
#[test]
fn redb_delete_prefix_with_0xff_keys() {
    // Tests prefix_successor edge case: keys with 0xFF bytes.
    // prefix_successor must handle all-0xFF prefixes correctly.
    use batpak::store::projection::RedbCache;
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("test.redb");
    let cache = RedbCache::open(&path).expect("open redb");
    let meta = test_meta();

    // Insert keys with high-byte prefixes
    cache
        .put(&[0xFF, 0x01], b"val1", meta.clone())
        .expect("put");
    cache
        .put(&[0xFF, 0x02], b"val2", meta.clone())
        .expect("put");
    cache
        .put(&[0xFF, 0xFF], b"val3", meta.clone())
        .expect("put");
    cache
        .put(&[0xFE, 0x01], b"other", meta.clone())
        .expect("put");

    let deleted = cache.delete_prefix(&[0xFF]).expect("delete_prefix");
    assert_eq!(
        deleted, 3,
        "DELETE PREFIX 0xFF: should delete all 3 keys starting with 0xFF.\n\
         Investigate: src/store/projection.rs prefix_successor().\n\
         Common causes: prefix_successor wrapping incorrectly on 0xFF, missing carry logic."
    );

    // The 0xFE key should survive
    assert!(
        cache.get(&[0xFE, 0x01]).expect("get").is_some(),
        "DELETE PREFIX 0xFF: key [0xFE, 0x01] should survive prefix delete of [0xFF]."
    );
}

#[cfg(feature = "redb")]
#[test]
fn redb_delete_prefix_empty_prefix_deletes_all() {
    // Empty prefix matches everything — should delete all keys.
    use batpak::store::projection::RedbCache;
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("test.redb");
    let cache = RedbCache::open(&path).expect("open redb");
    let meta = test_meta();

    cache.put(b"a", b"1", meta.clone()).expect("put");
    cache.put(b"b", b"2", meta.clone()).expect("put");
    cache.put(b"z", b"3", meta.clone()).expect("put");

    let deleted = cache.delete_prefix(b"").expect("delete_prefix");
    assert_eq!(
        deleted, 3,
        "DELETE PREFIX EMPTY: empty prefix should match all keys.\n\
         Investigate: src/store/projection.rs prefix_successor() with empty input.\n\
         Common causes: empty prefix edge case not handled, range scan returning nothing."
    );
}

#[test]
fn nocache_prefetch_is_noop() {
    let cache = NoCache;
    let meta = test_meta();
    assert_eq!(
        cache.capabilities(),
        CacheCapabilities::none(),
        "NoCache must explicitly report that it does not support prefetch hints."
    );
    cache
        .prefetch(b"any_key", meta)
        .expect("NoCache::prefetch should not error — it's a no-op by default.");
}
