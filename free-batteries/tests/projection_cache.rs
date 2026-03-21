//! Direct tests of ProjectionCache trait methods per backend.
//! Fulfills SPEC promise: "Every trait method on ProjectionCache is exercised
//! against every backend (NoCache, RedbCache, LmdbCache)."
//! [SPEC:tests/projection_cache.rs]

use free_batteries::store::projection::{CacheMeta, NoCache, ProjectionCache};

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
    use free_batteries::store::projection::RedbCache;
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
}

// ================================================================
// LmdbCache — backed by LMDB via heed.
// ================================================================

#[cfg(feature = "lmdb")]
mod lmdb_tests {
    use super::*;
    use free_batteries::store::projection::LmdbCache;
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
    fn lmdb_sync() {
        let (cache, _dir) = lmdb_cache();
        cache.sync().expect("LmdbCache::sync should not error.");
    }
}
