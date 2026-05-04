// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; unified red-path watch tests in tests/unified_watch_red.rs use unwrap/panic as assertion style and narrow bounded test counters that fit within u32.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::panic)]

#[path = "support/bounded_blocking.rs"]
mod bounded_blocking;
#[path = "support/red_counters.rs"]
mod red_counters;
#[path = "support/red_kind_b.rs"]
mod red_kind_b;
#[path = "support/red_kinds.rs"]
mod red_kinds;
use bounded_blocking::blocking;

use red_counters::*;
use red_kind_b::*;
use red_kinds::*;

use batpak::prelude::*;
use std::sync::Arc;
use tempfile::TempDir;

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

    handle.join().expect("writer thread");

    let (_gen, state) =
        blocking("unified-watch-recv", move || watcher.recv()).expect("recv should not error");
    let counter = state.expect("should have projection");
    assert_eq!(
        counter.count, 8,
        "PROPERTY: watch_projection must catch up to the fully visible state after new writes.\n\
         Investigate: src/store/mod.rs watch_projection + ProjectionWatcher::recv.\n\
         Common causes: watcher returns before replay catches up, generation/state mismatch."
    );
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

    let (_gen, state) =
        blocking("unified-watch-recv", move || watcher.recv()).expect("recv should not error");
    let counter = state.expect("projection should exist");
    assert_eq!(
        counter.count, 10,
        "PROPERTY: watch_projection must catch up by watermark even when the lossy subscription \
         collapses multiple notifications into one.\n\
         Investigate: src/store/mod.rs ProjectionWatcher::recv + StoreIndex::stream_since."
    );
}

#[test]
fn subscription_returns_none_on_store_close() {
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

    let result = blocking("unified-watch-subscription-close-recv", move || sub.recv());
    assert!(
        result.is_none(),
        "PROPERTY: subscription must return None when store shuts down."
    );

    handle.join().expect("closer thread");
}

#[test]
fn watch_projection_returns_store_closed_when_slow_watcher_is_pruned() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_broadcast_capacity(1);
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("watch:pruned", "watch:scope").expect("coord");

    store
        .append(&coord, kind_a(), &payload(0))
        .expect("seed append");

    let mut watcher = store.watch_projection::<AllCounter>("watch:pruned", Freshness::Consistent);

    for i in 1u32..6 {
        store
            .append(&coord, kind_a(), &payload(i))
            .expect("burst append");
    }

    // Intentional: the two blocking watcher receives are bounded by
    // `bounded_blocking::blocking` around this closure.
    let (first, second) = blocking("unified-watch-pruned-recv-sequence", move || {
        let first = watcher.recv();
        let second = watcher.recv();
        (first, second)
    });
    let (_gen, state) = first.expect("first recv should drain buffered notification");
    let state = state.expect("projection should exist");
    assert_eq!(
        state.count, 6,
        "PROPERTY: even a pruned watcher must catch up to the latest visible state before the \
         channel closes.\n\
         Investigate: src/store/projection/watch.rs recv + src/store/projection/flow.rs."
    );

    let err: batpak::store::WatcherError = match second {
        Ok(_) => panic!("PROPERTY: pruned watcher should terminate with WatcherError::StoreClosed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, batpak::store::WatcherError::StoreClosed),
        "PROPERTY: a pruned watcher must surface WatcherError::StoreClosed, got {err:?}"
    );
}

#[test]
fn project_if_changed_reports_honest_generation_for_empty_filtered_state() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open"));
    let coord = Coordinate::new("watch:filtered-empty", "watch:scope").expect("coord");

    store
        .append(&coord, kind_b(), &payload(99))
        .expect("append irrelevant event");

    let changed = store
        .project_if_changed::<KindFilteredCounter>(
            "watch:filtered-empty",
            0,
            &Freshness::Consistent,
        )
        .expect("project_if_changed")
        .expect("changed projection");

    let current_generation = store
        .entity_generation("watch:filtered-empty")
        .expect("entity generation");

    assert_eq!(
        changed.1, None,
        "PROPERTY: a filtered projection with no relevant events must still report an empty fold."
    );
    assert_eq!(
        changed.0, current_generation,
        "PROPERTY: project_if_changed must return the honest advanced generation even when the projection fold is empty."
    );
}
