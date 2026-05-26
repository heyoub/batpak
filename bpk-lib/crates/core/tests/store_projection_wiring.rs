//! Projection wiring tests split out of store_advanced.rs.

mod support;
use batpak::store::projection::{CacheCapabilities, CacheMeta, ProjectionCache};
use batpak::store::{Freshness, Store, StoreConfig, StoreError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use support::prelude::*;
use tempfile::TempDir;

#[cfg(feature = "dangerous-test-hooks")]
fn entry_point(entry: &batpak::store::index::IndexEntry) -> batpak::store::HlcPoint {
    batpak::store::HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    }
}

#[test]
fn project_calls_prefetch_only_when_supported() {
    struct TrackingCache {
        prefetch_called: Arc<AtomicBool>,
    }

    impl ProjectionCache for TrackingCache {
        fn capabilities(&self) -> CacheCapabilities {
            CacheCapabilities::prefetch_hints()
        }

        fn get(&self, _key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
            Ok(None)
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

        fn prefetch(&self, _key: &[u8], _predicted_meta: CacheMeta) -> Result<(), StoreError> {
            self.prefetch_called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    let prefetch_called = Arc::new(AtomicBool::new(false));
    let cache = TrackingCache {
        prefetch_called: Arc::clone(&prefetch_called),
    };

    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Store::open_with_cache(config, Box::new(cache)).expect("open store with cache");
    let coord = Coordinate::new("entity:pf", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({"data": 1}))
        .expect("append");

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Counter {
        count: u32,
    }

    impl EventSourced for Counter {
        type Input = batpak::prelude::JsonValueInput;

        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            Some(Counter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }

        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
            self.count += 1;
        }

        fn relevant_event_kinds() -> &'static [EventKind] {
            &[]
        }
    }

    let _result: Option<Counter> = store
        .project("entity:pf", &Freshness::Consistent)
        .expect("project");

    assert!(
        prefetch_called.load(Ordering::SeqCst),
        "PROPERTY: Store::project must call cache.prefetch() when the cache advertises prefetch support.\n\
         Investigate: src/store/mod.rs project(), src/store/projection/mod.rs CacheCapabilities."
    );

    store.close().expect("close");
}

#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn first_projection_report_can_lower_overstated_applied_frontier() {
    let dir = TempDir::new().expect("create temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord =
        Coordinate::new("entity:projection-first-report-lag", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 2);

    for n in 1..=5 {
        store
            .append(&coord, kind, &serde_json::json!({ "n": n }))
            .expect("append projection progress event");
    }

    let entries = store.query(&Region::entity(coord.entity()));
    assert_eq!(entries.len(), 5);
    let slow = entry_point(&entries[0]);
    let fast = entry_point(&entries[4]);

    store.dangerous_notify_projection_applied("frontier:first-fast", fast);
    assert_eq!(store.dangerous_watermark_snapshot().applied_hlc, fast);

    store.dangerous_notify_projection_applied("frontier:first-slow", slow);
    assert_eq!(
        store.dangerous_watermark_snapshot().applied_hlc,
        slow,
        "PROPERTY: first report from a newly observed lagging projection must lower applied_hlc to the true min-across-projections value"
    );

    store.close().expect("close");
}
