// justifies: pins cache-corruption fall-back shapes, anchors INV-CACHE-CAPABILITIES-EXPLICIT in src/store/projection/flow.rs
#![allow(clippy::too_many_lines)]
//! Cache-CORRUPTION shapes for projection reads: garbage files, single-bit
//! flips, metadata-only/truncated payloads, and a failing cache backend. Every
//! shape must degrade to an honest replay (or a clean cache miss) instead of
//! serving garbage or a stale success.
//!
//! Integration tests: `cargo test --test projection_cache_corruption`
//!
//! PROVES: LAW-001 (No Fake Success — a corrupt cache row must never be served
//!   as a successful projection).
//! CATCHES: drift where NativeCache decode failures fail open, where a
//!   stale-but-young corrupt row is trusted because its age window says "fresh
//!   enough", or where a cache-get error aborts instead of falling back.
//! SEEDED: deterministic corruption fixtures — overwritten garbage bytes,
//!   targeted byte flips on the first/last byte, and legacy metadata-bearing
//!   rows with empty/truncated payloads.
//! INVARIANTS: INV-CACHE-CAPABILITIES-EXPLICIT (decode failure = miss),
//!   INV-CLOCK-NOW-US-LIVE (freshness window cannot rescue corruption),
//!   INV-REPLAY-LANE-SELECTION (replay path selection).

#[path = "support/projection_cache.rs"]
mod pc_support;

use batpak::store::projection::{CacheMeta, ProjectionCache};
use batpak::store::StoreError;
use pc_support::*;

fn test_meta() -> CacheMeta {
    CacheMeta {
        watermark: 42,
        cached_at_us: 1_000_000,
        cached_at_mono_ns: None,
        process_boot_ns: None,
    }
}

struct GetErrorCache;

impl ProjectionCache for GetErrorCache {
    fn capabilities(&self) -> batpak::store::projection::CacheCapabilities {
        batpak::store::projection::CacheCapabilities::none()
    }

    fn get(&self, _key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        Err(StoreError::CacheFailed(
            "simulated cache get failure".into(),
        ))
    }

    fn put(&self, _key: &[u8], _value: &[u8], _meta: CacheMeta) -> Result<(), StoreError> {
        Ok(())
    }

    fn delete_prefix(&self, _prefix: &[u8]) -> Result<u64, StoreError> {
        Ok(0)
    }

    fn sync(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

fn legacy_cache_bytes(value: &[u8], watermark: u64, cached_at_us: i64) -> Vec<u8> {
    let mut bytes = value.to_vec();
    bytes.extend_from_slice(&watermark.to_le_bytes());
    bytes.extend_from_slice(&cached_at_us.to_le_bytes());
    bytes
}

fn find_only_native_cache_entry(cache_path: &std::path::Path) -> std::path::PathBuf {
    let mut stack = vec![cache_path.to_path_buf()];
    let mut entries = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("bin") {
                entries.push(path);
            }
        }
    }

    assert_eq!(
        entries.len(),
        1,
        "PROJECTION CACHE TEST SETUP: expected exactly one native cache entry, found {}",
        entries.len()
    );
    entries.pop().expect("single cache entry")
}

#[test]
fn native_corruption_falls_back_to_cache_miss() {
    use batpak::store::projection::NativeCache;
    use tempfile::TempDir;

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
         Investigate: src/store/projection/mod.rs NativeCache::get decode error path."
    );

    // Corrupt file should be deleted (self-healing)
    assert!(
        !corrupt_path.exists(),
        "NATIVE SELF-HEAL: corrupt cache file should be deleted after failed decode."
    );
}

#[test]
fn freshness_maybe_stale_replays_when_stale_cache_bytes_are_corrupt() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");

    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let coord = Coordinate::new("entity:maybe-stale-corrupt", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);

    {
        let store = Store::open_with_native_cache(config.clone(), &cache_path).expect("open store");
        store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");

        let seeded: Option<MaybeStaleCounter> = store
            .project("entity:maybe-stale-corrupt", &Freshness::Consistent)
            .expect("seed cache");
        assert_eq!(seeded, Some(MaybeStaleCounter { count: 2 }));

        // Advance the entity so the warmed cache row is stale by watermark,
        // but keep the row young enough that MaybeStale would otherwise try
        // to serve it from the external cache.
        store
            .append(&coord, kind, &serde_json::json!({"x": 3}))
            .expect("append 3");
        store.close().expect("close seeded store");
    }

    let cache_entry = find_only_native_cache_entry(&cache_path);
    let mut corrupted = std::fs::read(&cache_entry).expect("read cache entry");
    let last = corrupted
        .len()
        .checked_sub(1)
        .expect("non-empty cache entry");
    corrupted[last] ^= 0x5A;
    std::fs::write(&cache_entry, corrupted).expect("corrupt cache entry");

    let store = Store::open_with_native_cache(config, &cache_path).expect("reopen store");
    let result: Option<MaybeStaleCounter> = store
        .project(
            "entity:maybe-stale-corrupt",
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("project maybe stale after corruption");
    assert_eq!(
        result,
        Some(MaybeStaleCounter { count: 3 }),
        "MAYBE STALE CORRUPTION HONESTY: a stale-but-young corrupt cache row must fall back to replay and return the current folded state.\n\
         It must not serve garbage and must not preserve the stale count=2 row just because the age window still says 'fresh enough'."
    );
    store.close().expect("close");
}

#[test]
fn freshness_maybe_stale_replays_when_fresh_cache_bytes_are_corrupt() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");

    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let coord = Coordinate::new("entity:maybe-stale-fresh-corrupt", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);

    {
        let store = Store::open_with_native_cache(config.clone(), &cache_path).expect("open store");
        store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");

        let seeded: Option<MaybeStaleCounter> = store
            .project("entity:maybe-stale-fresh-corrupt", &Freshness::Consistent)
            .expect("seed cache");
        assert_eq!(seeded, Some(MaybeStaleCounter { count: 2 }));
        store.close().expect("close seeded store");
    }

    let cache_entry = find_only_native_cache_entry(&cache_path);
    let mut corrupted = std::fs::read(&cache_entry).expect("read cache entry");
    let first = corrupted.first_mut().expect("non-empty cache entry");
    *first ^= 0xA5;
    std::fs::write(&cache_entry, corrupted).expect("corrupt cache entry");

    let store = Store::open_with_native_cache(config, &cache_path).expect("reopen store");
    let result: Option<MaybeStaleCounter> = store
        .project(
            "entity:maybe-stale-fresh-corrupt",
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("project maybe stale after fresh corruption");
    assert_eq!(
        result,
        Some(MaybeStaleCounter { count: 2 }),
        "MAYBE STALE FRESH CORRUPTION HONESTY: a fresh-but-corrupt cache row must fall back to replay and return the current folded state.\n\
         It must not fail open just because the age window still says 'fresh enough'."
    );
    store.close().expect("close");
}

#[test]
fn maybe_stale_replays_when_cache_row_has_valid_metadata_but_empty_payload() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let coord = Coordinate::new("entity:metadata-only-stale", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);

    {
        let store = Store::open_with_native_cache(config.clone(), &cache_path).expect("open store");
        store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");
        let seeded: Option<MaybeStaleCounter> = store
            .project("entity:metadata-only-stale", &Freshness::Consistent)
            .expect("seed cache");
        assert_eq!(seeded, Some(MaybeStaleCounter { count: 2 }));
        store
            .append(&coord, kind, &serde_json::json!({"x": 3}))
            .expect("append 3");
        store.close().expect("close seeded store");
    }

    let cache_entry = find_only_native_cache_entry(&cache_path);
    std::fs::write(&cache_entry, legacy_cache_bytes(&[], 2, 1_000_000))
        .expect("write metadata-only legacy cache entry");

    let store = Store::open_with_native_cache(config, &cache_path).expect("reopen store");
    let result: Option<MaybeStaleCounter> = store
        .project(
            "entity:metadata-only-stale",
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("project maybe stale after metadata-only cache row");
    assert_eq!(
        result,
        Some(MaybeStaleCounter { count: 3 }),
        "METADATA-ONLY CACHE HONESTY: a cache file with valid metadata but undecodable payload must replay rather than serve a stale MaybeStale success."
    );
    store.close().expect("close");
}

#[test]
fn consistent_replays_when_cache_row_has_valid_metadata_but_truncated_payload() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let coord = Coordinate::new("entity:metadata-valid-truncated", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);

    {
        let store = Store::open_with_native_cache(config.clone(), &cache_path).expect("open store");
        store
            .append(&coord, kind, &serde_json::json!({"x": 1}))
            .expect("append 1");
        store
            .append(&coord, kind, &serde_json::json!({"x": 2}))
            .expect("append 2");
        let seeded: Option<MaybeStaleCounter> = store
            .project("entity:metadata-valid-truncated", &Freshness::Consistent)
            .expect("seed cache");
        assert_eq!(seeded, Some(MaybeStaleCounter { count: 2 }));
        store.close().expect("close seeded store");
    }

    let cache_entry = find_only_native_cache_entry(&cache_path);
    std::fs::write(&cache_entry, legacy_cache_bytes(b"\x92", 2, 1_000_000))
        .expect("write truncated-payload legacy cache entry");

    let store = Store::open_with_native_cache(config, &cache_path).expect("reopen store");
    let result: Option<MaybeStaleCounter> = store
        .project("entity:metadata-valid-truncated", &Freshness::Consistent)
        .expect("project after truncated cache payload");
    assert_eq!(
        result,
        Some(MaybeStaleCounter { count: 2 }),
        "TRUNCATED PAYLOAD CACHE HONESTY: valid cache metadata with an undecodable payload must fall back to replay under Consistent freshness."
    );
    store.close().expect("close");
}

#[test]
fn projection_replays_when_cache_get_errors() {
    use batpak::prelude::*;
    use batpak::store::{Freshness, Store, StoreConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Store::open_with_cache(config, Box::new(GetErrorCache)).expect("open store");
    let coord = Coordinate::new("entity:cache-get-error", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({"x": 1}))
        .expect("append 1");
    store
        .append(&coord, kind, &serde_json::json!({"x": 2}))
        .expect("append 2");

    let result: Option<MaybeStaleCounter> = store
        .project("entity:cache-get-error", &Freshness::Consistent)
        .expect("project after cache get error");
    assert_eq!(
        result,
        Some(MaybeStaleCounter { count: 2 }),
        "CACHE GET ERROR HONESTY: a cache backend get failure must fall back to replay and return the honest folded state."
    );

    store.close().expect("close");
}
