// justifies: unified red-path watch tests use unwrap/panic as assertion style and narrow bounded test counters that fit within u32.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::panic)]

#[path = "support/unified_red.rs"]
mod unified_red_support;

use unified_red_support::*;

#[test]
fn watch_projection_emits_on_new_events() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open"));
    let coord = Coordinate::new("watch:entity", "watch:scope").expect("coord");

    for i in 0u32..5 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }

    let mut watcher = store.watch_projection::<AllCounter>("watch:entity", Freshness::Consistent);

    let store2 = Arc::clone(&store);
    let handle = std::thread::Builder::new()
        .name("watch-writer".into())
        .spawn(move || {
            let coord = Coordinate::new("watch:entity", "watch:scope").expect("coord");
            for i in 5u32..8 {
                store2
                    .append(&coord, kind_a(), &payload(i))
                    .expect("append");
            }
        })
        .expect("spawn");

    let result = watcher.recv().expect("recv should not error");
    let counter = result.expect("should have projection");
    assert!(
        counter.count >= 6,
        "PROPERTY: watch_projection must re-project with new events.\n\
         Got count={}, expected >= 6.\n\
         Investigate: src/store/mod.rs watch_projection + ProjectionWatcher::recv.",
        counter.count
    );

    handle.join().expect("writer thread");
}

#[test]
fn watch_projection_catches_up_after_lossy_notifications() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_broadcast_capacity(1);
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("watch:lossy", "watch:scope").expect("coord");

    for i in 0u32..3 {
        store
            .append(&coord, kind_a(), &payload(i))
            .expect("seed append");
    }

    let mut watcher = store.watch_projection::<AllCounter>("watch:lossy", Freshness::Consistent);

    for i in 3u32..10 {
        store
            .append(&coord, kind_a(), &payload(i))
            .expect("append burst");
    }

    let result = watcher.recv().expect("recv should not error");
    let counter = result.expect("projection should exist");
    assert_eq!(
        counter.count, 10,
        "PROPERTY: watch_projection must catch up by watermark even when the lossy subscription \
         collapses multiple notifications into one.\n\
         Investigate: src/store/mod.rs ProjectionWatcher::recv + StoreIndex::stream_since."
    );
}

#[test]
fn watch_projection_returns_none_on_store_close() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open"));
    let coord = Coordinate::new("drop:entity", "drop:scope").expect("coord");
    store.append(&coord, kind_a(), &payload(0)).expect("append");

    let sub = store.subscribe_lossy(&Region::entity("drop:entity"));

    let handle = std::thread::Builder::new()
        .name("store-closer".into())
        .spawn(move || match Arc::try_unwrap(store) {
            Ok(store) => {
                let _ = store.close();
            }
            Err(store) => {
                drop(store);
            }
        })
        .expect("spawn");

    let result = sub.recv();
    assert!(
        result.is_none(),
        "PROPERTY: subscription must return None when store shuts down."
    );

    handle.join().expect("closer thread");
}
