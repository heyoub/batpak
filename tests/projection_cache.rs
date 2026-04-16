//! Direct tests of ProjectionCache trait methods per backend.
//! Plus: integration tests with real Store operations (append → project → cache hit).
//! Every trait method on ProjectionCache is exercised against every live backend
//! surface (NoCache, NativeCache).
//!
//! Integration tests: `cargo test --test projection_cache`
//!
//! PROVES: LAW-001 (No Fake Success — cached projections must be correct)
//! DEFENDS: FM-009 (Polite Downgrade — MaybeStale must eventually refresh)
//! INVARIANTS: INV-TYPE (cache round-trip fidelity), INV-TEMP (freshness semantics)

use batpak::store::projection::{CacheMeta, NoCache, ProjectionCache};

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
// NativeCache — built-in file-backed cache.
// ================================================================

mod native_tests {
    use super::*;
    use batpak::store::projection::NativeCache;
    use tempfile::TempDir;

    fn native_cache() -> (NativeCache, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("cache");
        let cache = NativeCache::open(&path).expect("open native cache");
        (cache, dir)
    }

    #[test]
    fn native_get_put_round_trip() {
        let (cache, _dir) = native_cache();
        let meta = test_meta();

        // Put creates shard dir and file, then get retrieves
        cache.put(b"key1", b"hello", meta.clone()).expect("put");
        let (value, returned_meta) = cache.get(b"key1").expect("get").expect("should be Some");
        assert_eq!(
            value, b"hello",
            "NativeCache round-trip failed. Investigate: src/store/projection.rs NativeCache."
        );
        assert_eq!(
            returned_meta.watermark, 42,
            "NATIVE ROUND-TRIP META WATERMARK: watermark should be preserved across put/get.\n\
             Investigate: src/store/projection.rs NativeCache::put and NativeCache::get.\n\
             Common causes: CacheMeta serialization losing watermark field."
        );
        assert_eq!(
            returned_meta.cached_at_us, 1_000_000,
            "NATIVE ROUND-TRIP META CACHED_AT: cached_at_us should be preserved across put/get.\n\
             Investigate: src/store/projection.rs NativeCache::put and NativeCache::get.\n\
             Common causes: CacheMeta serialization losing cached_at_us field."
        );

        // Non-existent key returns None
        assert!(
            cache.get(b"nonexistent").expect("get").is_none(),
            "NATIVE ROUND-TRIP: get for a key that was never inserted should return None.\n\
             Investigate: src/store/projection.rs NativeCache::get."
        );
    }

    #[test]
    fn native_delete_prefix() {
        let (cache, _dir) = native_cache();
        let meta = test_meta();

        cache.put(b"user:1", b"alice", meta.clone()).expect("put");
        cache.put(b"user:2", b"bob", meta.clone()).expect("put");
        cache.put(b"order:1", b"widget", meta.clone()).expect("put");

        let deleted = cache.delete_prefix(b"user:").expect("delete_prefix");
        assert_eq!(deleted, 2, "Should delete 2 keys with prefix 'user:'.");

        // user keys gone
        assert!(
            cache.get(b"user:1").expect("get").is_none(),
            "NATIVE DELETE PREFIX: key 'user:1' should be gone after delete_prefix('user:').\n\
             Investigate: src/store/projection.rs NativeCache::delete_prefix."
        );
        assert!(
            cache.get(b"user:2").expect("get").is_none(),
            "NATIVE DELETE PREFIX: key 'user:2' should be gone after delete_prefix('user:')."
        );
        // order key remains
        assert!(
            cache.get(b"order:1").expect("get").is_some(),
            "NATIVE DELETE PREFIX: key 'order:1' should survive delete_prefix('user:')."
        );
    }

    #[test]
    fn native_delete_prefix_is_idempotent() {
        let (cache, _dir) = native_cache();
        let meta = test_meta();

        cache.put(b"user:1", b"alice", meta.clone()).expect("put");
        cache.put(b"user:2", b"bob", meta).expect("put");

        let first = cache.delete_prefix(b"user:").expect("delete prefix");
        let second = cache.delete_prefix(b"user:").expect("delete prefix again");
        cache.sync().expect("sync");

        assert_eq!(
            first, 2,
            "NATIVE DELETE PREFIX IDEMPOTENCE: first delete should remove both matching entries."
        );
        assert_eq!(
            second, 0,
            "NATIVE DELETE PREFIX IDEMPOTENCE: repeating the delete must be a clean no-op."
        );
    }

    #[test]
    fn native_sync_is_safe() {
        let (cache, _dir) = native_cache();
        cache.sync().expect("NativeCache::sync should not error.");
    }

    #[test]
    fn native_reopen_preserves_cache() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("cache");
        let meta = test_meta();

        // Write with first instance
        {
            let cache = NativeCache::open(&cache_path).expect("open");
            cache
                .put(b"persistent_key", b"durable_value", meta)
                .expect("put");
        }

        // Reopen and verify
        {
            let cache = NativeCache::open(&cache_path).expect("reopen");
            let (value, returned_meta) = cache
                .get(b"persistent_key")
                .expect("get")
                .expect("should be Some after reopen");
            assert_eq!(
                value, b"durable_value",
                "NativeCache must survive process restart."
            );
            assert_eq!(returned_meta.watermark, 42);
        }
    }

    #[test]
    fn native_corruption_falls_back_to_cache_miss() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("cache");
        let meta = test_meta();

        let cache = NativeCache::open(&cache_path).expect("open");
        cache.put(b"corrupt_me", b"valid_data", meta).expect("put");

        // Corrupt the file by writing garbage
        let hex_key: String = b"corrupt_me".iter().map(|b| format!("{b:02x}")).collect();
        let shard = &hex_key[..2];
        let corrupt_path = cache_path.join(shard).join(format!("{hex_key}.bin"));
        std::fs::write(&corrupt_path, b"garbage").expect("write garbage");

        // Get should return None (cache miss), not error
        let result = cache.get(b"corrupt_me").expect("get should not error");
        assert!(
            result.is_none(),
            "NATIVE CORRUPTION: corrupt cache file should degrade to cache miss, not error.\n\
             Investigate: src/store/projection.rs NativeCache::get decode error path."
        );

        // Corrupt file should be deleted (self-healing)
        assert!(
            !corrupt_path.exists(),
            "NATIVE SELF-HEAL: corrupt cache file should be deleted after failed decode."
        );
    }

    #[test]
    fn native_delete_prefix_with_0xff_keys() {
        let (cache, _dir) = native_cache();
        let meta = test_meta();

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
            "DELETE PREFIX 0xFF: should delete all 3 keys starting with 0xFF."
        );

        assert!(
            cache.get(&[0xFE, 0x01]).expect("get").is_some(),
            "DELETE PREFIX 0xFF: key [0xFE, 0x01] should survive prefix delete of [0xFF]."
        );
    }

    #[test]
    fn native_delete_prefix_empty_prefix_deletes_all() {
        let (cache, _dir) = native_cache();
        let meta = test_meta();

        cache.put(b"a", b"1", meta.clone()).expect("put");
        cache.put(b"b", b"2", meta.clone()).expect("put");
        cache.put(b"z", b"3", meta.clone()).expect("put");

        let deleted = cache.delete_prefix(b"").expect("delete_prefix");
        assert_eq!(
            deleted, 3,
            "DELETE PREFIX EMPTY: empty prefix should match all keys."
        );
    }

    // -- Integration: Store + NativeCache --

    use batpak::prelude::*;
    use batpak::store::SyncConfig;

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Counter {
        count: u32,
    }
    impl EventSourced for Counter {
        type Input = batpak::prelude::JsonValueInput;

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
    fn native_projection_round_trip() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("cache");

        let config = StoreConfig {
            data_dir: dir.path().join("data"),
            segment_max_bytes: 4096,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let store = Store::open_with_native_cache(config, &cache_path)
            .expect("open store with native cache");

        let coord = Coordinate::new("entity:native1", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");

        // First project: cache miss, replays from segments
        let result: Option<Counter> = store
            .project("entity:native1", &Freshness::Consistent)
            .expect("project");
        assert_eq!(
            result,
            Some(Counter { count: 2 }),
            "NATIVE PROJECTION ROUND-TRIP: first project should replay 2 events."
        );

        // Second project: should hit cache (same watermark)
        let result2: Option<Counter> = store
            .project("entity:native1", &Freshness::Consistent)
            .expect("project 2");
        assert_eq!(result2, Some(Counter { count: 2 }));

        // Append more → cache should be stale → re-replay
        store
            .append(&coord, kind, &serde_json::json!({"x": 3}))
            .expect("append 3");
        let result3: Option<Counter> = store
            .project("entity:native1", &Freshness::Consistent)
            .expect("project 3");
        assert_eq!(
            result3,
            Some(Counter { count: 3 }),
            "NATIVE CACHE INVALIDATION: after appending more events, project should re-replay."
        );

        store.close().expect("close");
    }

    #[test]
    fn native_delete_prefix_then_project_repopulates_cache() {
        let dir = TempDir::new().expect("temp dir");
        let cache_path = dir.path().join("cache");
        let config = StoreConfig {
            data_dir: dir.path().join("data"),
            segment_max_bytes: 4096,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let coord = Coordinate::new("entity:native-miss", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);

        {
            let store =
                Store::open_with_native_cache(config.clone(), &cache_path).expect("open store");
            store
                .append(&coord, kind, &serde_json::json!({"x": 1}))
                .expect("append 1");
            store
                .append(&coord, kind, &serde_json::json!({"x": 2}))
                .expect("append 2");
            let _: Option<Counter> = store
                .project("entity:native-miss", &Freshness::Consistent)
                .expect("warm cache");
            store.close().expect("close");
        }

        {
            let cache = NativeCache::open(&cache_path).expect("reopen cache");
            let deleted = cache
                .delete_prefix(b"entity:native-miss")
                .expect("delete prefix");
            assert!(
                deleted >= 1,
                "NATIVE CACHE MISS PROOF: delete_prefix should remove at least one warmed cache key, got {deleted}."
            );
        }

        {
            let store = Store::open_with_native_cache(config, &cache_path).expect("reopen store");
            let result: Option<Counter> = store
                .project("entity:native-miss", &Freshness::Consistent)
                .expect("project after delete");
            assert_eq!(result, Some(Counter { count: 2 }));
            store.close().expect("close");
        }

        let cache = NativeCache::open(&cache_path).expect("final reopen cache");
        let repopulated = cache
            .delete_prefix(b"entity:native-miss")
            .expect("check repopulated");
        assert!(
            repopulated >= 1,
            "NATIVE CACHE MISS PROOF: projecting after delete_prefix must repopulate the cache key."
        );
    }
}

// ================================================================
// Freshness::MaybeStale + cache metadata edge cases
// PROVES: LAW-001 (No Fake Success — stale cache must not serve wrong data)
// DEFENDS: FM-009 (Polite Downgrade — MaybeStale must eventually refresh)
// ================================================================

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct MaybeStaleCounter {
    count: u32,
}
impl batpak::prelude::EventSourced for MaybeStaleCounter {
    type Input = batpak::prelude::JsonValueInput;

    fn from_events(events: &[batpak::prelude::Event<serde_json::Value>]) -> Option<Self> {
        Some(MaybeStaleCounter {
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

#[test]
fn freshness_maybe_stale_serves_stale_cache_within_window() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, NativeCache, Store, StoreConfig, SyncConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");

    let config = StoreConfig {
        data_dir: dir.path().join("data"),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
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
    let result: Option<MaybeStaleCounter> = store
        .project("entity:besteff1", &Freshness::Consistent)
        .expect("project consistent");
    assert_eq!(result, Some(MaybeStaleCounter { count: 2 }));

    // Append a third event — cache is now stale
    store
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
fn cache_metadata_short_bytes_returns_none() {
    let cache = NoCache;
    cache.put(b"short", b"x", test_meta()).expect("put");
    let result = cache.get(b"short").expect("get");
    assert!(
        result.is_none(),
        "CACHE METADATA: NoCache should return None regardless of what was put."
    );
}

#[test]
fn nocache_prefetch_is_noop() {
    let cache = NoCache;
    let meta = test_meta();
    let caps = cache.capabilities();
    assert!(
        !caps.supports_prefetch,
        "NoCache must explicitly report that it does not support prefetch hints."
    );
    assert!(
        caps.is_noop,
        "NoCache must report itself as a no-op cache backend."
    );
    cache
        .prefetch(b"any_key", meta)
        .expect("NoCache::prefetch should not error — it's a no-op by default.");
}
