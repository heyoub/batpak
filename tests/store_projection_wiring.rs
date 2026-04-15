//! Projection wiring tests split out of store_advanced.rs.

use batpak::prelude::*;
use batpak::store::projection::{CacheCapabilities, CacheMeta, ProjectionCache};
use batpak::store::{Freshness, Store, StoreConfig, StoreError, SyncConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

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
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
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
         Investigate: src/store/mod.rs project(), src/store/projection.rs CacheCapabilities."
    );

    store.close().expect("close");
}
