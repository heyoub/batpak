#![allow(
    clippy::disallowed_methods,    // concurrent tests use thread::spawn
    clippy::needless_borrows_for_generic_args
)]
//! Integration tests for Store lifecycle.
//! Append/get/query, segment rotation, cold start index rebuild, concurrent r/w.
//! [SPEC:tests/store_integration.rs]
//!
//! PROVES: LAW-002 (No Local State — uses real Store), LAW-003 (No Orphan Infrastructure)
//! DEFENDS: FM-007 (Island Syndrome — full production path), FM-008 (Shadow Test — imports real types)
//! INVARIANTS: INV-TEMP (cold start rebuild), INV-CONC (concurrent r/w)
//!
//! Anti-almost-correctness: These tests exercise the real DashMap index query()
//! method (Phase 1.5 fix), the dead logic branch (Phase 1.6 fix), and the
//! Arc<str> serialization path (Phase 1.1 fix) through round-trip persistence.

use batpak::prelude::*;
use batpak::store::{Freshness, Store, StoreConfig};
use tempfile::TempDir;

fn test_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096, // small segments to force rotation
        sync_every_n_events: 1,
        ..StoreConfig::new("")
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

    assert_eq!(
        stored.coordinate, coord,
        "ROUND-TRIP FAILED: coordinate mismatch after append+get. \
         Investigate: src/store/mod.rs append/get and Arc<str> serialization."
    );
    assert_eq!(
        stored.event.event_kind(),
        kind,
        "ROUND-TRIP FAILED: event_kind mismatch after append+get — expected {:?}, got {:?}.\n\
         Check: src/store/mod.rs append(), src/store/reader.rs decode path.\n\
         Common causes: EventKind serialization truncation, wrong field ordering in wire format.\n\
         Run: cargo test append_and_get_single_event -- --nocapture",
        kind,
        stored.event.event_kind()
    );

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
    assert_eq!(
        stats.event_count, 10,
        "EVENT COUNT MISMATCH: expected 10 events, got {}.\n\
         Check: src/store/index.rs insert(), src/store/writer.rs append logic.\n\
         Common causes: double-counting, off-by-one in loop, index not updated on each write.\n\
         Run: cargo test append_multiple_events_same_entity -- --nocapture",
        stats.event_count
    );

    // Verify content: all 10 events should be retrievable and have correct payloads
    let results = store.stream("entity:1");
    assert_eq!(
        results.len(),
        10,
        "CONTENT VERIFICATION FAILED: stream('entity:1') returned {} events, expected 10.\n\
         Check: src/store/index.rs query() entity lookup, src/store/reader.rs decode.\n\
         Common causes: index not keyed by entity, stream() filters incorrectly.\n\
         Run: cargo test append_multiple_events_same_entity -- --nocapture",
        results.len()
    );
    for (idx, entry) in results.iter().enumerate() {
        assert_eq!(
            entry.coord.entity(),
            "entity:1",
            "CONTENT VERIFICATION FAILED: event[{idx}] has wrong entity '{}', expected 'entity:1'.\n\
             Check: src/store/mod.rs append(), Arc<str> entity serialization path.\n\
             Common causes: entity string interning bug, coordinate not preserved through write.\n\
             Run: cargo test append_multiple_events_same_entity -- --nocapture",
            entry.coord.entity()
        );
    }

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
        let coord =
            Coordinate::new(format!("user:{i}").as_str(), "scope:test").expect("valid coord");
        store.append(&coord, kind, &payload).expect("append");
    }
    // And some non-matching entities
    let coord = Coordinate::new("order:1", "scope:test").expect("valid coord");
    store.append(&coord, kind, &payload).expect("append");

    let region = Region::entity("user:");
    let results = store.query(&region);
    assert_eq!(
        results.len(),
        5,
        "ENTITY PREFIX QUERY FAILED: expected 5 'user:*' events, got {}.\n\
         Check: src/store/index.rs query() entity_prefix path, BTreeMap range scan.\n\
         Common causes: prefix range bounds wrong (start_bound/end_bound), Arc<str> key comparison mismatch.\n\
         Run: cargo test query_by_entity_prefix -- --nocapture",
        results.len()
    );

    // Verify content: every returned entry must have an entity starting with "user:"
    for (idx, entry) in results.iter().enumerate() {
        let entity = entry.coord.entity();
        assert!(
            entity.starts_with("user:"),
            "ENTITY PREFIX QUERY CONTAMINATION: entry[{idx}] has entity '{}' which does not match prefix 'user:'.\n\
             Check: src/store/index.rs query() entity_prefix range, BTreeMap range end bound.\n\
             Common causes: range upper bound too loose, prefix check skipped for last entry.\n\
             Run: cargo test query_by_entity_prefix -- --nocapture",
            entity
        );
        assert_eq!(
            entry.kind, kind,
            "ENTITY PREFIX QUERY WRONG KIND: entry[{idx}] has kind {:?}, expected {:?}.\n\
             Check: src/store/index.rs insert() kind assignment.\n\
             Common causes: EventKind not propagated to IndexEntry.\n\
             Run: cargo test query_by_entity_prefix -- --nocapture",
            entry.kind, kind
        );
    }

    // Verify non-matching entity is excluded
    let order_results: Vec<_> = results
        .iter()
        .filter(|e| e.coord.entity().starts_with("order:"))
        .collect();
    assert!(
        order_results.is_empty(),
        "ENTITY PREFIX QUERY LEAKAGE: 'order:' entity leaked into 'user:' prefix query ({} events).\n\
         Check: src/store/index.rs query() entity_prefix range end bound computation.\n\
         Common causes: BTreeMap range end bound not exclusive, prefix increment overflow.\n\
         Run: cargo test query_by_entity_prefix -- --nocapture",
        order_results.len()
    );

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
    assert_eq!(
        results.len(),
        2,
        "SCOPE QUERY FAILED: expected 2 scope:a events, got {}.\n\
         Check: src/store/index.rs query() scope path (DashMap Ref lifetime fix — Phase 1.5).\n\
         Common causes: DashMap guard dropped too early, scope key normalization mismatch.\n\
         Run: cargo test query_by_scope -- --nocapture",
        results.len()
    );

    // Verify content: all returned entries must belong to scope:a
    for (idx, entry) in results.iter().enumerate() {
        let scope = entry.coord.scope();
        assert_eq!(
            scope, "scope:a",
            "SCOPE QUERY CONTAMINATION: entry[{idx}] has scope '{}', expected 'scope:a'.\n\
             Check: src/store/index.rs query() scope filter, DashMap iteration guard lifetime.\n\
             Common causes: scope filter predicate inverted, wrong DashMap shard iterated.\n\
             Run: cargo test query_by_scope -- --nocapture",
            scope
        );
        assert_eq!(
            entry.coord.entity(),
            "entity:1",
            "SCOPE QUERY WRONG ENTITY: entry[{idx}] has entity '{}', expected 'entity:1'.\n\
             Check: src/store/index.rs scope index structure, coordinate stored correctly.\n\
             Common causes: coordinate fields swapped during index insertion.\n\
             Run: cargo test query_by_scope -- --nocapture",
            entry.coord.entity()
        );
        assert_eq!(
            entry.kind, kind,
            "SCOPE QUERY WRONG KIND: entry[{idx}] has kind {:?}, expected {:?}.\n\
             Check: src/store/index.rs insert() kind assignment.\n\
             Common causes: EventKind not propagated to IndexEntry.\n\
             Run: cargo test query_by_scope -- --nocapture",
            entry.kind, kind
        );
    }

    // Verify scope:b event is excluded
    let scope_b_in_results: Vec<_> = results
        .iter()
        .filter(|e| e.coord.scope() == "scope:b")
        .collect();
    assert!(
        scope_b_in_results.is_empty(),
        "SCOPE QUERY LEAKAGE: scope:b event leaked into scope:a query ({} events).\n\
         Check: src/store/index.rs query() scope filter predicate.\n\
         Common causes: scope equality check uses prefix instead of exact match.\n\
         Run: cargo test query_by_scope -- --nocapture",
        scope_b_in_results.len()
    );

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
    assert_eq!(
        results.len(),
        2,
        "FACT QUERY FAILED: expected 2 events of kind_a ({:?}), got {}.\n\
         Check: src/store/index.rs by_fact() path, EventKind index key.\n\
         Common causes: EventKind hash/eq mismatch, by_fact index not updated on insert.\n\
         Run: cargo test query_by_fact -- --nocapture",
        kind_a,
        results.len()
    );

    // Verify content: all returned entries must match kind_a, not kind_b
    for (idx, entry) in results.iter().enumerate() {
        assert_eq!(
            entry.kind,
            kind_a,
            "FACT QUERY WRONG KIND: entry[{idx}] has kind {:?}, expected kind_a {:?}.\n\
             Check: src/store/index.rs by_fact() filter, EventKind comparison in index.\n\
             Common causes: index bucket collision between kind_a and kind_b, wrong EventKind key.\n\
             Run: cargo test query_by_fact -- --nocapture",
            entry.kind,
            kind_a
        );
    }

    // Verify kind_b is excluded
    let kind_b_in_results: Vec<_> = results.iter().filter(|e| e.kind == kind_b).collect();
    assert!(
        kind_b_in_results.is_empty(),
        "FACT QUERY LEAKAGE: kind_b ({:?}) leaked into kind_a ({:?}) query ({} events).\n\
         Check: src/store/index.rs by_fact() bucket lookup, EventKind Hash impl.\n\
         Common causes: EventKind Hash collision, index uses wrong discriminant.\n\
         Run: cargo test query_by_fact -- --nocapture",
        kind_b,
        kind_a,
        kind_b_in_results.len()
    );

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
            ..StoreConfig::new("")
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
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("cold start open");
        let stats = store.stats();
        assert_eq!(
            stats.event_count, 20,
            "COLD START FAILED: index should have 20 events after rebuild, got {}. \
             Investigate: src/store/mod.rs Store::open cold start scan.",
            stats.event_count
        );

        // Verify query still works after cold start
        let results = store.stream("entity:1");
        assert_eq!(
            results.len(),
            20,
            "COLD START STREAM FAILED: stream('entity:1') returned {} events after cold start, expected 20.\n\
             Check: src/store/mod.rs Store::open() cold start scan, src/store/index.rs rebuild logic.\n\
             Common causes: stream() skips events from non-active segments, index rebuild stops early.\n\
             Run: cargo test cold_start_rebuilds_index -- --nocapture",
            results.len()
        );

        // Verify ordering: clock values must be monotonically increasing
        // within a single entity stream, proving index rebuild preserved order.
        let mut prev_clock: Option<u32> = None;
        for (idx, entry) in results.iter().enumerate() {
            let clk = entry.clock;
            if let Some(prev) = prev_clock {
                assert!(
                    clk >= prev,
                    "COLD START ORDER BROKEN: entry[{idx}] clock {clk} < previous {prev}.\n\
                     Check: src/store/mod.rs Store::open() cold start scan order.\n\
                     Common causes: segment files scanned out of order, clock not recovered.\n\
                     Run: cargo test cold_start_rebuilds_index -- --nocapture",
                );
            }
            prev_clock = Some(clk);

            // Verify coordinate integrity survived cold start
            assert_eq!(
                entry.coord.entity(),
                "entity:1",
                "COLD START COORDINATE CORRUPTION: entry[{idx}] has entity '{}' after cold start, expected 'entity:1'.\n\
                 Check: src/store/reader.rs coordinate deserialization, Arc<str> round-trip (Phase 1.1 fix).\n\
                 Common causes: Arc<str> serialized as pointer, entity string not persisted correctly.\n\
                 Run: cargo test cold_start_rebuilds_index -- --nocapture",
                entry.coord.entity()
            );
            assert_eq!(
                entry.coord.scope(),
                "scope:test",
                "COLD START COORDINATE CORRUPTION: entry[{idx}] has scope '{}' after cold start, expected 'scope:test'.\n\
                 Check: src/store/reader.rs coordinate deserialization, scope field offset in wire format.\n\
                 Common causes: entity/scope fields swapped in codec, scope not written to segment.\n\
                 Run: cargo test cold_start_rebuilds_index -- --nocapture",
                entry.coord.scope()
            );
        }

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
        ..StoreConfig::new("")
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
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();

    assert!(
        segment_count > 1,
        "SEGMENT ROTATION FAILED: expected multiple .fbat segments with 512-byte limit, got {}.\n\
         Check: src/store/writer.rs rotation check (STEP 7), segment_max_bytes threshold comparison.\n\
         Common causes: rotation check uses > instead of >=, byte count measured before not after write.\n\
         Run: cargo test segment_rotation_on_size -- --nocapture",
        segment_count
    );

    store.close().expect("close");
}

// --- Concurrent read/write ---

#[test]
fn concurrent_append_and_query() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = std::sync::Arc::new(Store::open(config).expect("open store"));
    let coord = Coordinate::new("entity:1", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Writer thread
    let store_w = std::sync::Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::Builder::new()
        .name("store-writer".to_string())
        .spawn(move || {
            for i in 0..100 {
                let payload = serde_json::json!({"i": i});
                store_w.append(&coord_w, kind, &payload).expect("append");
            }
        })
        .expect("spawn thread");

    // Reader thread (queries while writes happen)
    // Verifies: concurrent reads don't crash, and event counts never decrease.
    // Yields between iterations so the writer thread has time to make progress.
    let store_r = std::sync::Arc::clone(&store);
    let reader = std::thread::Builder::new()
        .name("store-reader".to_string())
        .spawn(move || {
            let mut max_seen = 0usize;
            for _ in 0..200 {
                let results = store_r.stream("entity:1");
                let count = results.len();
                assert!(
                    count >= max_seen,
                    "CONCURRENT READ REGRESSION: event count went from {max_seen} to {count}. \
                 Events should never disappear during concurrent writes."
                );
                max_seen = count;
                if max_seen >= 100 {
                    break;
                }
                std::thread::yield_now();
            }
            max_seen
        })
        .expect("spawn thread");

    writer.join().expect("writer thread");
    let _max_seen = reader.join().expect("reader thread");
    // After writer finishes, verify all events are visible.
    // Note: the reader may or may not have seen intermediate states depending
    // on thread scheduling. What matters is final consistency.

    let stats = store.stats();
    assert_eq!(
        stats.event_count, 100,
        "CONCURRENT R/W FAILED: expected 100 events after concurrent writes, got {}.\n\
         Check: src/store/index.rs insert() under concurrent load, writer.rs flush ordering.\n\
         Common causes: lost write under contention, event_count stat not atomically updated.\n\
         Run: cargo test concurrent_append_and_query -- --nocapture",
        stats.event_count
    );

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
    let opts = batpak::store::AppendOptions {
        expected_sequence: Some(0), // after first event, latest clock is 0
        ..Default::default()
    };
    let result = store.append_with_options(&coord, kind, &payload, opts);
    assert!(
        result.is_ok(),
        "CAS FAILED: append_with_options rejected correct expected_sequence=0, error: {:?}.\n\
         Check: src/store/mod.rs append_with_options() CAS check, sequence clock read path.\n\
         Common causes: sequence read returns 1-based instead of 0-based clock, CAS fence off-by-one.\n\
         Run: cargo test append_with_cas_success -- --nocapture",
        result.err()
    );

    // Verify the CAS-appended event is actually stored and retrievable
    let receipt = result.expect("CAS succeeded");
    let stored = store.get(receipt.event_id).expect(
        "CAS-appended event must be retrievable by event_id — \
         Check: src/store/mod.rs get(), index updated after append_with_options.",
    );
    assert_eq!(
        stored.coordinate.entity(),
        "entity:cas",
        "CAS ROUND-TRIP FAILED: retrieved event has entity '{}', expected 'entity:cas'.\n\
         Check: src/store/mod.rs append_with_options() coordinate propagation.\n\
         Common causes: coordinate not passed through to underlying append, index entry wrong.\n\
         Run: cargo test append_with_cas_success -- --nocapture",
        stored.coordinate.entity()
    );

    store.close().expect("close");
}

// --- EventSourced projection ---

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
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
        .project("entity:proj", &Freshness::Consistent)
        .expect("project");

    assert!(
        counter.is_some(),
        "PROJECTION REPLAY FAILED: project() returned None after 5 events were appended.\n\
         Check: src/store/mod.rs project(), EventSourced::from_events() called with non-empty slice.\n\
         Common causes: stream('entity:proj') returns empty, relevant_event_kinds() filter too strict.\n\
         Run: cargo test projection_replays_events -- --nocapture"
    );
    let counter = counter.expect("checked is_some above");
    assert_eq!(
        counter.count,
        5,
        "PROJECTION REPLAY FAILED: Counter counted {} events, expected 5.\n\
         Check: src/store/mod.rs project() event slice construction, EventSourced::apply_event() called per event.\n\
         Common causes: events filtered by relevant_event_kinds() mismatch, apply_event() skipped, stream returns subset.\n\
         Run: cargo test projection_replays_events -- --nocapture",
        counter.count
    );

    store.close().expect("close");
}
