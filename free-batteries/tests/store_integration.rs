//! Integration tests for Store lifecycle.
//! Append/get/query, segment rotation, cold start index rebuild, concurrent r/w.
//! [SPEC:tests/store_integration.rs]
//!
//! Anti-almost-correctness: These tests exercise the real DashMap index query()
//! method (Phase 1.5 fix), the dead logic branch (Phase 1.6 fix), and the
//! Arc<str> serialization path (Phase 1.1 fix) through round-trip persistence.

use free_batteries::prelude::*;
use free_batteries::store::{Store, StoreConfig, Freshness};
use tempfile::TempDir;

fn test_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096, // small segments to force rotation
        sync_every_n_events: 1,
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open store");
    (store, dir)
}

// --- Basic append/get round-trip ---

#[test]
fn append_and_get_single_event() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"key": "value"});

    let receipt = store.append(&coord, kind, &payload).expect("append");
    let stored = store.get(receipt.event_id).expect("get");

    assert_eq!(stored.coordinate, coord,
        "ROUND-TRIP FAILED: coordinate mismatch after append+get. \
         Investigate: src/store/mod.rs append/get and Arc<str> serialization.");
    assert_eq!(stored.event.event_kind(), kind);

    store.close().expect("close");
}

#[test]
fn append_multiple_events_same_entity() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        let payload = serde_json::json!({"i": i});
        store.append(&coord, kind, &payload).expect("append");
    }

    let stats = store.stats();
    assert_eq!(stats.event_count, 10,
        "EVENT COUNT MISMATCH: expected 10 events, got {}. \
         Investigate: src/store/index.rs insert.", stats.event_count);

    store.close().expect("close");
}

// --- Query by Region ---

#[test]
fn query_by_entity_prefix() {
    let (store, _dir) = test_store();
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});

    // Create events across different entities
    for i in 0..5 {
        let coord = Coordinate::new(
            &format!("user:{i}"), "scope:test"
        ).expect("valid coord");
        store.append(&coord, kind, &payload).expect("append");
    }
    // And some non-matching entities
    let coord = Coordinate::new("order:1", "scope:test").expect("valid coord");
    store.append(&coord, kind, &payload).expect("append");

    let region = Region::entity("user:");
    let results = store.query(&region);
    assert_eq!(results.len(), 5,
        "ENTITY PREFIX QUERY FAILED: expected 5 'user:*' events, got {}. \
         Investigate: src/store/index.rs query() entity_prefix path.", results.len());

    store.close().expect("close");
}

#[test]
fn query_by_scope() {
    let (store, _dir) = test_store();
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});

    let coord_a = Coordinate::new("entity:1", "scope:a").expect("valid coord");
    let coord_b = Coordinate::new("entity:2", "scope:b").expect("valid coord");

    store.append(&coord_a, kind, &payload).expect("append");
    store.append(&coord_a, kind, &payload).expect("append");
    store.append(&coord_b, kind, &payload).expect("append");

    let region = Region::scope("scope:a");
    let results = store.query(&region);
    assert_eq!(results.len(), 2,
        "SCOPE QUERY FAILED: expected 2 scope:a events, got {}. \
         Investigate: src/store/index.rs query() scope path. \
         This exercises the DashMap Ref lifetime fix (Phase 1.5).", results.len());

    store.close().expect("close");
}

#[test]
fn query_by_fact() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind_a = EventKind::custom(0xF, 1);
    let kind_b = EventKind::custom(0xF, 2);
    let payload = serde_json::json!({"x": 1});

    store.append(&coord, kind_a, &payload).expect("append");
    store.append(&coord, kind_a, &payload).expect("append");
    store.append(&coord, kind_b, &payload).expect("append");

    let results = store.by_fact(kind_a);
    assert_eq!(results.len(), 2,
        "FACT QUERY FAILED: expected 2 events of kind_a, got {}. \
         Investigate: src/store/index.rs by_fact path.", results.len());

    store.close().expect("close");
}

// --- Cold start index rebuild ---

#[test]
fn cold_start_rebuilds_index() {
    let dir = TempDir::new().expect("create temp dir");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});

    // Phase 1: populate
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            ..StoreConfig::default()
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
        for _ in 0..20 {
            store.append(&coord, kind, &payload).expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: cold start — reopen and verify index
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            ..StoreConfig::default()
        };
        let store = Store::open(config).expect("cold start open");
        let stats = store.stats();
        assert_eq!(stats.event_count, 20,
            "COLD START FAILED: index should have 20 events after rebuild, got {}. \
             Investigate: src/store/mod.rs Store::open cold start scan.", stats.event_count);

        // Verify query still works
        let results = store.stream("entity:1");
        assert_eq!(results.len(), 20);

        store.close().expect("close");
    }
}

// --- Segment rotation ---

#[test]
fn segment_rotation_on_size() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512, // tiny segments
        sync_every_n_events: 1,
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"data": "some payload to fill segments quickly"});

    for _ in 0..50 {
        store.append(&coord, kind, &payload).expect("append");
    }
    store.sync().expect("sync");

    // Count segment files
    let segment_count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().map(|ext| ext == "fbat").unwrap_or(false)
        })
        .count();

    assert!(segment_count > 1,
        "SEGMENT ROTATION FAILED: expected multiple segments with 512-byte max, got {}. \
         Investigate: src/store/writer.rs STEP 7 rotation check.", segment_count);

    store.close().expect("close");
}

// --- Concurrent read/write ---

#[test]
fn concurrent_append_and_query() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    let store = std::sync::Arc::new(Store::open(config).expect("open store"));
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Writer thread
    let store_w = std::sync::Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..100 {
            let payload = serde_json::json!({"i": i});
            store_w.append(&coord_w, kind, &payload).expect("append");
        }
    });

    // Reader thread (queries while writes happen)
    // Verifies: concurrent reads don't crash, and event counts never decrease
    let store_r = std::sync::Arc::clone(&store);
    let reader = std::thread::spawn(move || {
        let mut max_seen = 0usize;
        for _ in 0..50 {
            let results = store_r.stream("entity:1");
            let count = results.len();
            assert!(count >= max_seen,
                "CONCURRENT READ REGRESSION: event count went from {max_seen} to {count}. \
                 Events should never disappear during concurrent writes.");
            max_seen = count;
        }
        max_seen
    });

    writer.join().expect("writer thread");
    let max_seen = reader.join().expect("reader thread");
    // Reader should have seen SOME events (not always 0)
    assert!(max_seen > 0,
        "CONCURRENT READ: reader never saw any events during writing. \
         This suggests reader queries aren't seeing writer commits.");

    let stats = store.stats();
    assert_eq!(stats.event_count, 100,
        "CONCURRENT R/W FAILED: expected 100 events after concurrent writes, got {}.",
        stats.event_count);

    // store is in Arc, close via sync
    store.sync().expect("sync");
}

// --- Append with options: CAS ---

#[test]
fn append_with_cas_success() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:cas", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});

    // First append — no CAS needed (sequence starts at 0)
    store.append(&coord, kind, &payload).expect("first append");

    // CAS with correct expected sequence (clock starts at 0, first event gets clock=0)
    let opts = free_batteries::store::AppendOptions {
        expected_sequence: Some(0), // after first event, latest clock is 0
        ..Default::default()
    };
    let result = store.append_with_options(&coord, kind, &payload, opts);
    assert!(result.is_ok(), "CAS with correct sequence should succeed");

    store.close().expect("close");
}

// --- EventSourced projection ---

#[derive(Default, Debug)]
struct Counter {
    count: u64,
}

impl EventSourced<serde_json::Value> for Counter {
    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
}

#[test]
fn projection_replays_events() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:proj", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..5 {
        let payload = serde_json::json!({"i": i});
        store.append(&coord, kind, &payload).expect("append");
    }

    let counter: Option<Counter> = store
        .project("entity:proj", Freshness::Consistent)
        .expect("project");

    assert!(counter.is_some(), "Projection should return Some after events");
    assert_eq!(counter.expect("checked").count, 5,
        "PROJECTION REPLAY FAILED: Counter should have counted 5 events. \
         Investigate: src/store/mod.rs project().");

    store.close().expect("close");
}
