//! Focused projection-cache edge pins split out of `projection_cache.rs`.
//!
//! PROVES: LAW-001 (cache failures and generation-aware replay stay honest)
//! CATCHES: Cache I/O downgrades and project_if_changed stale-generation shortcuts
//! SEEDED: mutation-smoke gaps in NativeCache::get and project_inner generation checks

use batpak::store::projection::{CacheMeta, NativeCache, ProjectionCache};
use batpak::store::{Freshness, IndexTopology, Store, StoreConfig, StoreError};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

const GENERATION_KIND: EventKind = EventKind::custom(0xF, 0x51);

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct GenerationCounter {
    count: u32,
}

impl EventSourced for GenerationCounter {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        Some(Self {
            count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[GENERATION_KIND]
    }
}

fn cache_meta() -> CacheMeta {
    CacheMeta {
        watermark: 42,
        cached_at_us: 1_000_000,
        cached_at_mono_ns: None,
        process_boot_ns: None,
    }
}

#[test]
fn native_get_surfaces_non_not_found_io_errors() {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");
    let shard_path = cache_path.join("ab");
    std::fs::write(&shard_path, b"not a directory").expect("create shard path as a file");

    let result = cache.get(&[0xAB]);
    assert!(
        matches!(result, Err(StoreError::CacheFailed(_))),
        "PROPERTY: a cache shard path that is not a directory must propagate as CacheFailed, not degrade to a cache miss.\n\
         Investigate: src/store/projection/mod.rs NativeCache::get.\n\
         Common causes: treating every IO error as ErrorKind::NotFound."
    );
}

#[test]
fn native_open_rejects_regular_file_cache_root() {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    std::fs::write(&cache_path, b"not a directory").expect("create cache root as file");

    let result = NativeCache::open(&cache_path);
    assert!(
        matches!(result, Err(StoreError::CacheFailed(_))),
        "PROPERTY: NativeCache::open must reject an existing regular file at the cache root instead of treating it as a usable directory.\n\
         Investigate: src/store/projection/mod.rs NativeCache::open.\n\
         Common causes: swallowing create_dir_all failures or failing open after root path validation."
    );
}

#[test]
fn native_get_surfaces_cache_entry_path_that_is_directory() {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");
    let shard_path = cache_path.join("61");
    let entry_path = shard_path.join("61.bin");
    std::fs::create_dir_all(&entry_path).expect("create entry path as directory");

    let result = cache.get(b"a");
    assert!(
        matches!(result, Err(StoreError::CacheFailed(_))),
        "PROPERTY: a cache entry path that is a directory must surface as CacheFailed, not as a cache miss or decoded value.\n\
         Investigate: src/store/projection/mod.rs NativeCache::get.\n\
         Common causes: treating directories as readable cache files."
    );
}

#[test]
fn native_put_surfaces_shard_path_that_is_file() {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");
    let shard_path = cache_path.join("61");
    std::fs::write(&shard_path, b"not a directory").expect("create shard path as file");

    let result = cache.put(b"a", b"value", cache_meta());
    assert!(
        matches!(result, Err(StoreError::CacheFailed(_))),
        "PROPERTY: NativeCache::put must reject a shard path that is a regular file instead of overwriting or pretending to cache.\n\
         Investigate: src/store/projection/mod.rs NativeCache::put.\n\
         Common causes: ignoring create_dir_all failures on the shard directory."
    );
    assert_eq!(
        std::fs::read(&shard_path).expect("read shard file"),
        b"not a directory",
        "PROPERTY: failed NativeCache::put must leave the obstructing shard file intact."
    );
}

#[test]
fn native_delete_prefix_visits_matching_overlong_shard_directory() {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");
    let overlong_shard = cache_path.join("616");
    std::fs::create_dir_all(&overlong_shard).expect("create overlong shard");
    std::fs::write(overlong_shard.join("6162.bin"), b"legacy").expect("write cache file");

    let removed = cache.delete_prefix(b"a").expect("delete prefix");
    assert_eq!(
        removed, 1,
        "PROPERTY: delete_prefix must scan shard directories that extend a matching prefix.\n\
         Investigate: src/store/projection/mod.rs NativeCache::delete_prefix shard filter.\n\
         Common causes: changing the bidirectional prefix condition from && to ||."
    );
}

#[test]
fn native_delete_prefix_matches_full_key_prefix_inside_shared_shard() {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let cache = NativeCache::open(&cache_path).expect("open native cache");

    cache
        .put(b"ab", b"match", cache_meta())
        .expect("put matching key");
    cache
        .put(b"ac", b"same shard sibling", cache_meta())
        .expect("put same-shard sibling");
    cache
        .put(b"b", b"other shard", cache_meta())
        .expect("put other shard");

    let removed = cache.delete_prefix(b"ab").expect("delete prefix");
    assert_eq!(
        removed, 1,
        "PROPERTY: delete_prefix must match the full hex prefix, not just the shard directory."
    );
    assert!(
        cache.get(b"ab").expect("get deleted key").is_none(),
        "PROPERTY: the exact matching key must be removed."
    );
    assert!(
        cache.get(b"ac").expect("get same-shard sibling").is_some(),
        "PROPERTY: a key in the same shard with a different full prefix must survive."
    );
    assert!(
        cache.get(b"b").expect("get other shard key").is_some(),
        "PROPERTY: a key in another shard must survive."
    );
}

#[test]
fn project_if_changed_replays_when_watermark_matches_but_generation_advances() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1)
        .with_index_topology(IndexTopology::entity_local());
    let store = Store::open(config).expect("open store");

    let coord = Coordinate::new("entity:generation-watermark", "scope:test").expect("coord");
    store
        .append(&coord, GENERATION_KIND, &serde_json::json!({"x": 1}))
        .expect("append relevant 1");
    store
        .append(&coord, GENERATION_KIND, &serde_json::json!({"x": 2}))
        .expect("append relevant 2");

    let seeded: Option<GenerationCounter> = store
        .project("entity:generation-watermark", &Freshness::Consistent)
        .expect("seed group-local cache");
    assert_eq!(seeded, Some(GenerationCounter { count: 2 }));
    let baseline_generation = store
        .entity_generation("entity:generation-watermark")
        .expect("baseline generation");

    store
        .append(
            &coord,
            EventKind::custom(0xF, 9),
            &serde_json::json!({"irrelevant": true}),
        )
        .expect("append irrelevant");

    let changed = store
        .project_if_changed::<GenerationCounter>(
            "entity:generation-watermark",
            baseline_generation,
            &Freshness::Consistent,
        )
        .expect("project_if_changed")
        .expect("entity generation changed");

    assert!(
        changed.0 > baseline_generation,
        "PROPERTY: when an irrelevant append advances entity generation but leaves the relevant watermark unchanged, project_if_changed must replay and return the newer materialization generation"
    );
    assert_eq!(changed.1, Some(GenerationCounter { count: 2 }));

    store.close().expect("close");
}

#[test]
fn native_get_put_smoke_keeps_test_meta_used() {
    let dir = TempDir::new().expect("temp dir");
    let cache = NativeCache::open(dir.path().join("cache")).expect("open native cache");
    cache
        .put(b"key", b"value", cache_meta())
        .expect("put cache value");

    let result = cache.get(b"key").expect("get cache value");
    assert!(
        result.is_some(),
        "PROPERTY: the focused edge file still exercises a successful NativeCache get path"
    );
}

#[test]
fn native_get_returns_none_for_missing_cache_key() {
    let dir = TempDir::new().expect("temp dir");
    let cache = NativeCache::open(dir.path().join("cache")).expect("open native cache");

    let result = cache.get(b"missing-key").expect("get missing key");
    assert!(
        result.is_none(),
        "PROPERTY: NativeCache::get must return None for absent keys"
    );
}
