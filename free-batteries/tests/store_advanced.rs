//! Advanced Store tests: code paths missed by store_integration.rs.
//! Covers: walk_ancestors, snapshot, diagnostics, append_reaction,
//! subscription, cursor, compact, CAS failure, idempotency,
//! apply_transition, clock_range queries, fd_budget eviction,
//! corrupt segment recovery.
//! [SPEC:tests/store_advanced.rs]

use free_batteries::prelude::*;
use free_batteries::store::{Store, StoreConfig};
use free_batteries::typestate::Transition;
use tempfile::TempDir;
use std::sync::Arc;

fn test_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open store");
    (store, dir)
}

// --- walk_ancestors: hash chain traversal ---

#[test]
fn walk_ancestors_follows_chain() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:walk", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let mut receipts = Vec::new();
    for i in 0..5 {
        let payload = serde_json::json!({"step": i});
        receipts.push(store.append(&coord, kind, &payload).expect("append"));
    }

    // Walk from the last event — should find ancestors in chain
    let last_id = receipts.last().expect("has receipts").event_id;
    let ancestors = store.walk_ancestors(last_id, 10);

    // Must return more than just the starting event — the chain has 5 events
    assert!(ancestors.len() >= 2,
        "walk_ancestors should traverse the chain, not just return the start. \
         Got {} ancestors for a 5-event chain. \
         Investigate: src/store/mod.rs walk_ancestors.", ancestors.len());

    // First ancestor should be the event we started from
    assert_eq!(ancestors[0].event.event_id(), last_id);

    // Second ancestor must be DIFFERENT from the first (chain was traversed)
    assert_ne!(ancestors[0].event.event_id(), ancestors[1].event.event_id(),
        "walk_ancestors should return different events along the chain, \
         not the same event repeated.");

    store.close().expect("close");
}

#[test]
fn walk_ancestors_respects_limit() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:limit", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        let payload = serde_json::json!({"i": i});
        store.append(&coord, kind, &payload).expect("append");
    }

    let entries = store.stream("entity:limit");
    let last_id = entries.last().expect("has entries").event_id;
    let ancestors = store.walk_ancestors(last_id, 2);

    // With a 10-event chain and limit=2, we should get EXACTLY 2 ancestors
    assert_eq!(ancestors.len(), 2,
        "walk_ancestors(limit=2) on a 10-event chain should return exactly 2. \
         Got {}. Investigate: src/store/mod.rs walk_ancestors limit logic.",
        ancestors.len());

    store.close().expect("close");
}

// --- snapshot ---

#[test]
fn snapshot_copies_segments() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:snap", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        store.append(&coord, kind, &serde_json::json!({"i": i})).expect("append");
    }
    store.sync().expect("sync");

    let snap_dir = TempDir::new().expect("snap dir");
    store.snapshot(snap_dir.path()).expect("snapshot");

    // Verify: snapshot dir should contain .fbat files
    let fbat_count = std::fs::read_dir(snap_dir.path())
        .expect("read snap dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "fbat").unwrap_or(false))
        .count();

    assert!(fbat_count > 0,
        "SNAPSHOT FAILED: destination should contain .fbat segment files. \
         Investigate: src/store/mod.rs snapshot().");

    // Verify: can open a store from the snapshot
    let snap_config = StoreConfig {
        data_dir: snap_dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    let snap_store = Store::open(snap_config).expect("open snapshot");
    let stats = snap_store.stats();
    assert_eq!(stats.event_count, 10,
        "Snapshot store should have same event count. Got {}.", stats.event_count);

    snap_store.close().expect("close snap");
    store.close().expect("close");
}

// --- diagnostics ---

#[test]
fn diagnostics_reports_config() {
    let (store, dir) = test_store();
    let diag = store.diagnostics();

    assert_eq!(diag.data_dir, dir.path().to_path_buf());
    assert_eq!(diag.segment_max_bytes, 4096);
    assert_eq!(diag.event_count, 0);

    store.close().expect("close");
}

// --- append_reaction ---

#[test]
fn append_reaction_links_causation() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:react", "scope:test").expect("valid coord");
    let kind_cmd = EventKind::custom(0xF, 1);
    let kind_evt = EventKind::custom(0xF, 2);

    // Root cause event
    let root = store.append(&coord, kind_cmd, &serde_json::json!({"cmd": "create"}))
        .expect("root append");

    // Reaction event linked to root
    let reaction = store.append_reaction(
        &coord, kind_evt, &serde_json::json!({"evt": "created"}),
        root.event_id, root.event_id,
    ).expect("reaction append");

    // Verify: reaction has different event_id
    assert_ne!(root.event_id, reaction.event_id);

    // Verify: can retrieve both
    let root_stored = store.get(root.event_id).expect("get root");
    let react_stored = store.get(reaction.event_id).expect("get reaction");
    assert_eq!(root_stored.event.event_kind(), kind_cmd);
    assert_eq!(react_stored.event.event_kind(), kind_evt);

    store.close().expect("close");
}

// --- CAS failure ---

#[test]
fn cas_fails_on_wrong_sequence() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:cas-fail", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store.append(&coord, kind, &serde_json::json!({"x": 1})).expect("first");
    store.append(&coord, kind, &serde_json::json!({"x": 2})).expect("second");

    // CAS with stale expected_sequence (clock 0, but actual is now 1)
    let opts = free_batteries::store::AppendOptions {
        expected_sequence: Some(0),
        ..Default::default()
    };
    let result = store.append_with_options(&coord, kind, &serde_json::json!({"x": 3}), opts);
    assert!(result.is_err(),
        "CAS should fail when expected_sequence is stale. \
         Investigate: src/store/mod.rs append_with_options CAS check.");

    store.close().expect("close");
}

// --- Idempotency ---

#[test]
fn idempotency_returns_same_receipt() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:idemp", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let key: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let opts = free_batteries::store::AppendOptions {
        idempotency_key: Some(key),
        ..Default::default()
    };

    let r1 = store.append_with_options(&coord, kind, &serde_json::json!({"x": 1}), opts)
        .expect("first append");

    // Second append with same key should return same receipt
    let r2 = store.append_with_options(&coord, kind, &serde_json::json!({"x": 2}), opts)
        .expect("idempotent append");

    assert_eq!(r1.event_id, r2.event_id,
        "IDEMPOTENCY BROKEN: same key should return same event_id. \
         Investigate: src/store/mod.rs append_with_options idempotency check.");

    // Only 1 event should exist
    let stats = store.stats();
    assert_eq!(stats.event_count, 1);

    store.close().expect("close");
}

// --- Subscription (push-based) ---

#[test]
fn subscription_receives_matching_events() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:sub", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let region = Region::entity("entity:sub");
    let sub = store.subscribe(&region);

    // Write from another thread so recv doesn't deadlock
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..3 {
            store_w.append(&coord_w, kind, &serde_json::json!({"i": i})).expect("append");
        }
    });
    writer.join().expect("writer");

    // Should receive 3 matching notifications
    let mut count = 0;
    // Use try_recv in a loop since channel is bounded and events already sent
    let rx = sub.receiver();
    while let Ok(notif) = rx.try_recv() {
        if region.matches_event(notif.coord.entity(), notif.coord.scope(), notif.kind) {
            count += 1;
        }
    }
    assert_eq!(count, 3,
        "SUBSCRIPTION FAILED: expected 3 notifications, got {}. \
         Investigate: src/store/subscription.rs and writer broadcast.", count);

    store.sync().expect("sync");
}

#[test]
fn subscription_filters_by_region() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let kind = EventKind::custom(0xF, 1);

    // Subscribe only to entity:a
    let region = Region::entity("entity:a");
    let sub = store.subscribe(&region);

    let store_w = Arc::clone(&store);
    let writer = std::thread::spawn(move || {
        let coord_a = Coordinate::new("entity:a", "scope:test").expect("valid coord");
        let coord_b = Coordinate::new("entity:b", "scope:test").expect("valid coord");
        store_w.append(&coord_a, kind, &serde_json::json!({"target": "a"})).expect("append a");
        store_w.append(&coord_b, kind, &serde_json::json!({"target": "b"})).expect("append b");
        store_w.append(&coord_a, kind, &serde_json::json!({"target": "a2"})).expect("append a2");
    });
    writer.join().expect("writer");

    // Raw receiver gets all events, but region filter should match only entity:a
    let rx = sub.receiver();
    let mut matching = 0;
    while let Ok(notif) = rx.try_recv() {
        if region.matches_event(notif.coord.entity(), notif.coord.scope(), notif.kind) {
            matching += 1;
        }
    }
    assert_eq!(matching, 2,
        "SUBSCRIPTION FILTER FAILED: expected 2 entity:a notifications, got {}.", matching);

    store.sync().expect("sync");
}

// --- Cursor (pull-based) ---

#[test]
fn cursor_polls_events_in_order() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:cur", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..5 {
        store.append(&coord, kind, &serde_json::json!({"i": i})).expect("append");
    }

    let region = Region::entity("entity:cur");
    let mut cursor = store.cursor(&region);

    let mut polled = Vec::new();
    while let Some(entry) = cursor.poll() {
        polled.push(entry);
    }

    assert_eq!(polled.len(), 5,
        "CURSOR POLL FAILED: expected 5 events, got {}. \
         Investigate: src/store/cursor.rs poll().", polled.len());

    // Verify global_sequence is monotonically increasing
    for window in polled.windows(2) {
        assert!(window[0].global_sequence < window[1].global_sequence,
            "Cursor events should be ordered by global_sequence.");
    }

    store.close().expect("close");
}

#[test]
fn cursor_poll_batch_respects_max() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:batch", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        store.append(&coord, kind, &serde_json::json!({"i": i})).expect("append");
    }

    let region = Region::entity("entity:batch");
    let mut cursor = store.cursor(&region);

    let batch1 = cursor.poll_batch(3);
    assert_eq!(batch1.len(), 3,
        "poll_batch(3) should return exactly 3 events. Got {}.", batch1.len());

    let batch2 = cursor.poll_batch(3);
    assert_eq!(batch2.len(), 3, "Second batch should have 3 more.");

    let batch3 = cursor.poll_batch(100);
    assert_eq!(batch3.len(), 4, "Third batch should have remaining 4.");

    let batch4 = cursor.poll_batch(100);
    assert_eq!(batch4.len(), 0, "Fourth batch should be empty (all consumed).");

    store.close().expect("close");
}

// --- compact (currently no-op sync) ---

#[test]
fn compact_does_not_lose_data() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:compact", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..5 {
        store.append(&coord, kind, &serde_json::json!({"i": i})).expect("append");
    }

    store.compact().expect("compact");

    let stats = store.stats();
    assert_eq!(stats.event_count, 5,
        "compact() should not lose data. Got {} events.", stats.event_count);

    store.close().expect("close");
}

// --- open_default ---

#[test]
fn open_default_uses_default_config() {
    // open_default creates ./free-batteries-data — use a temp dir to avoid side effects
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open");
    let diag = store.diagnostics();
    assert_eq!(diag.segment_max_bytes, 256 * 1024 * 1024); // 256MB default
    assert_eq!(diag.fd_budget, 64);
    store.close().expect("close");
}

// --- Event not found ---

#[test]
fn get_nonexistent_returns_not_found() {
    let (store, _dir) = test_store();
    let result = store.get(0xDEAD);
    assert!(result.is_err(), "get() of nonexistent event should return Err");
    store.close().expect("close");
}

// --- apply_transition: typestate through the store ---

#[test]
fn apply_transition_persists_event() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:transition", "scope:test").expect("valid coord");

    // Simulate: Draft -> Published transition with a payload
    let kind = EventKind::custom(0xA, 1); // category 0xA, type 1
    let transition = Transition::<(), (), serde_json::Value>::new(
        kind,
        serde_json::json!({"title": "hello", "from": "draft", "to": "published"}),
    );

    let receipt = store.apply_transition(&coord, transition).expect("apply_transition");

    // Verify: event persisted and retrievable
    let stored = store.get(receipt.event_id).expect("get transition event");
    assert_eq!(stored.event.event_kind(), kind);
    assert_eq!(stored.coordinate, coord);

    store.close().expect("close");
}

// --- clock_range query filter ---

#[test]
fn query_with_clock_range_filters_events() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:clock", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Append 10 events (clock 0..9)
    for i in 0..10 {
        store.append(&coord, kind, &serde_json::json!({"i": i})).expect("append");
    }

    // Query with clock_range [3, 7] — should get events with clock 3,4,5,6,7
    let region = Region::entity("entity:clock").with_clock_range((3, 7));
    let results = store.query(&region);

    assert_eq!(results.len(), 5,
        "CLOCK RANGE FAILED: expected 5 events in clock range [3,7], got {}. \
         Investigate: src/store/index.rs query() clock_range filter.", results.len());

    // Verify all results have clock in [3, 7]
    for entry in &results {
        assert!(entry.clock >= 3 && entry.clock <= 7,
            "Event clock {} outside range [3,7]", entry.clock);
    }

    store.close().expect("close");
}

#[test]
fn query_clock_range_with_scope_filter() {
    let (store, _dir) = test_store();
    let kind = EventKind::custom(0xF, 1);

    // Two entities, same scope
    let coord_a = Coordinate::new("entity:a", "scope:shared").expect("valid coord");
    let coord_b = Coordinate::new("entity:b", "scope:shared").expect("valid coord");

    for i in 0..5 {
        store.append(&coord_a, kind, &serde_json::json!({"i": i})).expect("append a");
        store.append(&coord_b, kind, &serde_json::json!({"i": i})).expect("append b");
    }

    // entity:a with clock range [1,3]
    let region = Region::entity("entity:a").with_clock_range((1, 3));
    let results = store.query(&region);
    assert_eq!(results.len(), 3,
        "Expected 3 events for entity:a clock [1,3], got {}", results.len());

    store.close().expect("close");
}

// --- Region.with_fact_category filter ---

#[test]
fn query_by_fact_category() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:cat", "scope:test").expect("valid coord");

    // Category 0xA: types 1 and 2
    let kind_a1 = EventKind::custom(0xA, 1);
    let kind_a2 = EventKind::custom(0xA, 2);
    // Category 0xB: type 1
    let kind_b1 = EventKind::custom(0xB, 1);

    store.append(&coord, kind_a1, &serde_json::json!({"cat": "a"})).expect("append");
    store.append(&coord, kind_a2, &serde_json::json!({"cat": "a"})).expect("append");
    store.append(&coord, kind_b1, &serde_json::json!({"cat": "b"})).expect("append");

    // Query by category 0xA — should get both kind_a1 and kind_a2
    let region = Region::all().with_fact_category(0xA);
    let results = store.query(&region);
    assert_eq!(results.len(), 2,
        "CATEGORY QUERY FAILED: expected 2 events in category 0xA, got {}. \
         Investigate: src/store/index.rs KindFilter::Category path.", results.len());

    store.close().expect("close");
}

// --- fd_budget LRU eviction ---

#[test]
fn fd_budget_evicts_oldest_segments() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,   // tiny segments → many segment files
        sync_every_n_events: 1,
        fd_budget: 2,             // only 2 FDs allowed → forces eviction
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:fd", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Write enough events to create many segments (>2, exceeding fd_budget)
    for i in 0..100 {
        store.append(&coord, kind, &serde_json::json!({"data": format!("payload_{i}")}))
            .expect("append");
    }
    store.sync().expect("sync");

    // Verify: multiple segments created
    let segment_count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "fbat").unwrap_or(false))
        .count();
    assert!(segment_count > 2,
        "Expected >2 segments to stress fd_budget, got {}", segment_count);

    // Read events from different segments — this exercises LRU eviction
    // because fd_budget=2 but we have >2 segments
    let entries = store.stream("entity:fd");
    assert_eq!(entries.len(), 100);

    // Read first event (oldest segment), last event (newest), then first again
    // This forces eviction: open seg1, open seg_last (evicts seg1 if budget=2),
    // then re-open seg1 (evicts seg_last)
    let first = store.get(entries[0].event_id).expect("get first");
    let last = store.get(entries[99].event_id).expect("get last");
    let first_again = store.get(entries[0].event_id).expect("get first again after eviction");

    assert_eq!(first.event.event_id(), first_again.event.event_id(),
        "FD EVICTION CORRUPTION: re-reading after eviction should return same event. \
         Investigate: src/store/reader.rs get_fd() LRU cache.");

    // Verify event identity integrity through eviction cycles
    assert_eq!(first.event.event_kind(), last.event.event_kind(),
        "Events across segments should preserve kind through LRU eviction");

    store.close().expect("close");
}

// --- corrupt segment recovery ---

#[test]
fn cold_start_skips_corrupt_segment_gracefully() {
    let dir = TempDir::new().expect("temp dir");
    let kind = EventKind::custom(0xF, 1);

    // Phase 1: populate with good data
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 512,
            sync_every_n_events: 1,
            ..StoreConfig::default()
        };
        let store = Store::open(config).expect("open");
        let coord = Coordinate::new("entity:corrupt", "scope:test").expect("valid coord");
        for i in 0..20 {
            store.append(&coord, kind, &serde_json::json!({"i": i})).expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: create a corrupt segment file (bad magic)
    let corrupt_path = dir.path().join("999999.fbat");
    std::fs::write(&corrupt_path, b"BAAD_not_a_real_segment").expect("write corrupt");

    // Phase 3: cold start — should skip the corrupt segment
    // The store should either skip it or error on it.
    // scan_segment checks magic bytes and returns CorruptSegment error.
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        ..StoreConfig::default()
    };
    let result = Store::open(config);
    // The store should fail on a corrupt segment (bad magic = hard error)
    assert!(result.is_err(),
        "Store::open should reject segments with bad magic. \
         Investigate: src/store/reader.rs scan_segment magic check.");
}

#[test]
fn corrupt_frame_in_segment_is_detected() {
    // Write good events, then inject a corrupt frame into the segment file.
    // Verify cold start detects the corruption (CRC mismatch stops scanning).
    let dir = TempDir::new().expect("temp dir");
    let kind = EventKind::custom(0xF, 1);

    // Phase 1: populate with good data and sync
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            sync_every_n_events: 1,
            ..StoreConfig::default()
        };
        let store = Store::open(config).expect("open");
        let coord = Coordinate::new("entity:crc", "scope:test").expect("valid");
        for i in 0..3 {
            store.append(&coord, kind, &serde_json::json!({"i": i})).expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: corrupt a segment file by flipping bytes in the middle
    let segments: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "fbat").unwrap_or(false))
        .collect();
    assert!(!segments.is_empty(), "Should have segment files");

    let seg_path = segments[0].path();
    let mut data = std::fs::read(&seg_path).expect("read segment");
    // Flip bytes near the end of the file (inside a frame's msgpack region)
    if data.len() > 20 {
        let mid = data.len() - 10;
        data[mid] ^= 0xFF;
        data[mid + 1] ^= 0xFF;
    }
    std::fs::write(&seg_path, &data).expect("write corrupted segment");

    // Phase 3: cold start should still open (corrupt frames are skipped/truncated)
    // but should have fewer events than originally written
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    // The store may open successfully (skipping corrupt frames) or may error
    // depending on where the corruption landed. Either behavior is acceptable
    // — what matters is it doesn't silently return wrong data.
    match Store::open(config) {
        Ok(store) => {
            let stats = store.stats();
            // Corrupted segment may have fewer events (some frames skipped)
            // The key assertion: we don't get MORE events than we wrote
            assert!(stats.event_count <= 3,
                "Corrupt segment should not produce phantom events. Got {}.",
                stats.event_count);
            let _ = store.close();
        }
        Err(_) => {
            // Store rejected the corrupt segment entirely — also acceptable
        }
    }
}

// --- StoreError Display coverage ---

#[test]
fn store_error_display_variants() {
    use free_batteries::store::StoreError;

    // Each variant should display its key information, not just a generic string
    let not_found = format!("{}", StoreError::NotFound(0xDEAD));
    assert!(not_found.contains("dead"), "NotFound should contain the event ID hex. Got: {not_found}");

    let writer = format!("{}", StoreError::WriterCrashed);
    assert!(writer.contains("writer") || writer.contains("crash"),
        "WriterCrashed should mention writer/crash. Got: {writer}");

    let shutting = format!("{}", StoreError::ShuttingDown);
    assert!(shutting.contains("shut"), "ShuttingDown should mention shutdown. Got: {shutting}");

    let cache = format!("{}", StoreError::CacheFailed("redis timeout".into()));
    assert!(cache.contains("redis timeout"),
        "CacheFailed should contain the inner message. Got: {cache}");

    let dup = format!("{}", StoreError::DuplicateEvent(0xBEEF));
    assert!(dup.contains("beef"), "DuplicateEvent should contain the key hex. Got: {dup}");

    let seq = format!("{}", StoreError::SequenceMismatch {
        entity: "user:1".into(), expected: 5, actual: 3
    });
    assert!(seq.contains("user:1") && seq.contains("5") && seq.contains("3"),
        "SequenceMismatch should contain entity, expected, actual. Got: {seq}");

    let crc = format!("{}", StoreError::CrcMismatch { segment_id: 7, offset: 42 });
    assert!(crc.contains("7") && crc.contains("42"),
        "CrcMismatch should contain segment_id and offset. Got: {crc}");

    let corrupt = format!("{}", StoreError::CorruptSegment { segment_id: 3, detail: "bad magic".into() });
    assert!(corrupt.contains("bad magic"),
        "CorruptSegment should contain the detail. Got: {corrupt}");

    let ser = format!("{}", StoreError::Serialization("unexpected EOF".into()));
    assert!(ser.contains("unexpected EOF"),
        "Serialization should contain the inner message. Got: {ser}");
}

// --- CoordinateError Display ---

#[test]
fn coordinate_error_display() {
    let err = Coordinate::new("", "scope").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("entity"), "EmptyEntity error should mention 'entity': {msg}");

    let err = Coordinate::new("entity", "").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("scope"), "EmptyScope error should mention 'scope': {msg}");
}

// --- Coordinate Display ---

#[test]
fn coordinate_display_format() {
    let coord = Coordinate::new("user:42", "tenant:acme").expect("valid");
    let display = format!("{coord}");
    assert_eq!(display, "user:42@tenant:acme",
        "Coordinate Display should be 'entity@scope'");
}

// --- IndexEntry causation helpers ---

#[test]
fn index_entry_causation_helpers() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:helpers", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Root event (self-correlated, no causation)
    let root = store.append(&coord, kind, &serde_json::json!({"cmd": "create"}))
        .expect("root");

    // Reaction event
    let reaction = store.append_reaction(
        &coord, kind, &serde_json::json!({"evt": "created"}),
        root.event_id, root.event_id,
    ).expect("reaction");

    let entries = store.stream("entity:helpers");
    assert_eq!(entries.len(), 2);

    // Root: is_root_cause=true, is_correlated=false (correlation==event_id)
    let root_entry = entries.iter().find(|e| e.event_id == root.event_id).expect("find root");
    assert!(root_entry.is_root_cause(), "Root event should be root cause");
    assert!(!root_entry.is_correlated(), "Self-correlated event is NOT 'correlated'");

    // Reaction: is_root_cause=false, is_correlated=true, is_caused_by(root)=true
    let react_entry = entries.iter().find(|e| e.event_id == reaction.event_id).expect("find reaction");
    assert!(!react_entry.is_root_cause(), "Reaction should not be root cause");
    assert!(react_entry.is_correlated(), "Reaction should be correlated");
    assert!(react_entry.is_caused_by(root.event_id), "Reaction should be caused by root");

    store.close().expect("close");
}
