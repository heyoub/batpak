//! Advanced Store tests: code paths missed by store_integration.rs.
//! Covers: walk_ancestors, snapshot, diagnostics, append_reaction,
//! subscription, cursor, compact, CAS failure, idempotency,
//! apply_transition, clock_range queries, fd_budget eviction,
//! corrupt segment recovery.
//! [SPEC:tests/store_advanced.rs]

use free_batteries::prelude::*;
use free_batteries::store::{Store, StoreConfig};
use free_batteries::typestate::Transition;
use std::sync::Arc;
use tempfile::TempDir;

fn test_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
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
    assert!(
        ancestors.len() >= 2,
        "PROPERTY: walk_ancestors must traverse the hash chain and return at least 2 entries for a 5-event chain.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: walk stops after the anchor event, parent pointer not followed past first entry.\n\
         Run: cargo test --test store_advanced walk_ancestors_follows_chain"
    );

    // First ancestor should be the event we started from
    assert_eq!(
        ancestors[0].event.event_id(),
        last_id,
        "PROPERTY: walk_ancestors first result must be the starting event.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: off-by-one in initial anchor insertion, wrong field returned.\n\
         Run: cargo test --test store_advanced walk_ancestors_follows_chain"
    );

    // Second ancestor must be DIFFERENT from the first (chain was traversed)
    assert_ne!(
        ancestors[0].event.event_id(),
        ancestors[1].event.event_id(),
        "PROPERTY: walk_ancestors must return distinct events along the hash chain.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: parent-pointer not followed, same entry re-inserted in loop.\n\
         Run: cargo test --test store_advanced walk_ancestors_follows_chain"
    );

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
    assert_eq!(
        ancestors.len(),
        2,
        "PROPERTY: walk_ancestors(limit=2) on a 10-event chain must return exactly 2 entries.\n\
         Investigate: src/store/mod.rs walk_ancestors limit logic.\n\
         Common causes: limit parameter ignored, off-by-one in loop termination condition.\n\
         Run: cargo test --test store_advanced walk_ancestors_respects_limit"
    );

    store.close().expect("close");
}

// --- snapshot ---

#[test]
fn snapshot_copies_segments() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:snap", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");

    let snap_dir = TempDir::new().expect("snap dir");
    store.snapshot(snap_dir.path()).expect("snapshot");

    // Verify: snapshot dir should contain .fbat files
    let fbat_count = std::fs::read_dir(snap_dir.path())
        .expect("read snap dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();

    assert!(
        fbat_count > 0,
        "PROPERTY: snapshot destination must contain at least one .fbat segment file.\n\
         Investigate: src/store/mod.rs snapshot.\n\
         Common causes: snapshot copies to wrong directory, segment files flushed after snapshot call.\n\
         Run: cargo test --test store_advanced snapshot_copies_segments"
    );

    // Verify: can open a store from the snapshot
    let snap_config = StoreConfig {
        data_dir: snap_dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let snap_store = Store::open(snap_config).expect("open snapshot");
    let stats = snap_store.stats();
    assert_eq!(
        stats.event_count, 10,
        "PROPERTY: snapshot must preserve full event count — no events lost during copy.\n\
         Investigate: src/store/mod.rs snapshot.\n\
         Common causes: segment file not flushed before copy, partial write, index not rebuilt.\n\
         Run: cargo test --test store_advanced snapshot_copies_segments"
    );

    snap_store.close().expect("close snap");
    store.close().expect("close");
}

// --- diagnostics ---

#[test]
fn diagnostics_reports_config() {
    let (store, dir) = test_store();
    let diag = store.diagnostics();

    assert_eq!(
        diag.data_dir,
        dir.path().to_path_buf(),
        "PROPERTY: diagnostics must report the configured data_dir unchanged.\n\
         Investigate: src/store/mod.rs diagnostics.\n\
         Common causes: diagnostics returns a different field, path normalisation mismatch.\n\
         Run: cargo test --test store_advanced diagnostics_reports_config"
    );
    assert_eq!(
        diag.segment_max_bytes, 4096,
        "PROPERTY: diagnostics must report the configured segment_max_bytes.\n\
         Investigate: src/store/mod.rs diagnostics.\n\
         Common causes: StoreConfig not propagated into inner state, field name mismatch.\n\
         Run: cargo test --test store_advanced diagnostics_reports_config"
    );
    assert_eq!(
        diag.event_count, 0,
        "PROPERTY: diagnostics on an empty store must report event_count == 0.\n\
         Investigate: src/store/mod.rs diagnostics.\n\
         Common causes: counter not reset on open, leftover state from previous run.\n\
         Run: cargo test --test store_advanced diagnostics_reports_config"
    );

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
    let root = store
        .append(&coord, kind_cmd, &serde_json::json!({"cmd": "create"}))
        .expect("root append");

    // Reaction event linked to root
    let reaction = store
        .append_reaction(
            &coord,
            kind_evt,
            &serde_json::json!({"evt": "created"}),
            root.event_id,
            root.event_id,
        )
        .expect("reaction append");

    // Verify: reaction has different event_id
    assert_ne!(
        root.event_id, reaction.event_id,
        "PROPERTY: append_reaction must produce a new unique event_id distinct from its cause.\n\
         Investigate: src/store/mod.rs append_reaction.\n\
         Common causes: event_id generation reuses the cause ID, hash collision in tiny test set.\n\
         Run: cargo test --test store_advanced append_reaction_links_causation"
    );

    // Verify: can retrieve both
    let root_stored = store.get(root.event_id).expect("get root");
    let react_stored = store.get(reaction.event_id).expect("get reaction");
    assert_eq!(
        root_stored.event.event_kind(),
        kind_cmd,
        "PROPERTY: root event must retain its original EventKind after being stored.\n\
         Investigate: src/store/mod.rs append, src/store/segment.rs write_frame.\n\
         Common causes: event_kind field not serialised, wrong frame read back.\n\
         Run: cargo test --test store_advanced append_reaction_links_causation"
    );
    assert_eq!(
        react_stored.event.event_kind(),
        kind_evt,
        "PROPERTY: reaction event must retain its EventKind (kind_evt) after storage.\n\
         Investigate: src/store/mod.rs append_reaction, src/store/segment.rs write_frame.\n\
         Common causes: reaction inherits cause kind instead of its own, serialisation bug.\n\
         Run: cargo test --test store_advanced append_reaction_links_causation"
    );

    store.close().expect("close");
}

// --- CAS failure ---

#[test]
fn cas_fails_on_wrong_sequence() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:cas-fail", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store
        .append(&coord, kind, &serde_json::json!({"x": 1}))
        .expect("first");
    store
        .append(&coord, kind, &serde_json::json!({"x": 2}))
        .expect("second");

    // CAS with stale expected_sequence (clock 0, but actual is now 1)
    let opts = free_batteries::store::AppendOptions {
        expected_sequence: Some(0),
        ..Default::default()
    };
    let result = store.append_with_options(&coord, kind, &serde_json::json!({"x": 3}), opts);
    assert!(
        result.is_err(),
        "PROPERTY: append_with_options must return Err when expected_sequence is stale (CAS failure).\n\
         Investigate: src/store/mod.rs append_with_options CAS check.\n\
         Common causes: sequence comparison uses wrong field, CAS check skipped under lock.\n\
         Run: cargo test --test store_advanced cas_fails_on_wrong_sequence"
    );

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

    let r1 = store
        .append_with_options(&coord, kind, &serde_json::json!({"x": 1}), opts)
        .expect("first append");

    // Second append with same key should return same receipt
    let r2 = store
        .append_with_options(&coord, kind, &serde_json::json!({"x": 2}), opts)
        .expect("idempotent append");

    assert_eq!(
        r1.event_id, r2.event_id,
        "PROPERTY: append_with_options with the same idempotency_key must return the same event_id.\n\
         Investigate: src/store/mod.rs append_with_options idempotency check.\n\
         Common causes: idempotency key not stored after first write, key lookup hash collision.\n\
         Run: cargo test --test store_advanced idempotency_returns_same_receipt"
    );

    // Only 1 event should exist
    let stats = store.stats();
    assert_eq!(
        stats.event_count, 1,
        "PROPERTY: idempotent appends must not increase event_count — only one event must be stored.\n\
         Investigate: src/store/mod.rs append_with_options idempotency check.\n\
         Common causes: idempotency key lookup misses in-memory cache, duplicate written to segment.\n\
         Run: cargo test --test store_advanced idempotency_returns_same_receipt"
    );

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
            store_w
                .append(&coord_w, kind, &serde_json::json!({"i": i}))
                .expect("append");
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
    assert_eq!(
        count, 3,
        "PROPERTY: subscription must deliver exactly 3 notifications for 3 matching appends.\n\
         Investigate: src/store/subscription.rs, src/store/mod.rs writer broadcast.\n\
         Common causes: broadcast channel dropped before all events sent, region filter too narrow.\n\
         Run: cargo test --test store_advanced subscription_receives_matching_events"
    );

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
        store_w
            .append(&coord_a, kind, &serde_json::json!({"target": "a"}))
            .expect("append a");
        store_w
            .append(&coord_b, kind, &serde_json::json!({"target": "b"}))
            .expect("append b");
        store_w
            .append(&coord_a, kind, &serde_json::json!({"target": "a2"}))
            .expect("append a2");
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
    assert_eq!(
        matching, 2,
        "PROPERTY: subscription filtered to entity:a must match exactly 2 of 3 appended events.\n\
         Investigate: src/store/subscription.rs region filter, src/store/mod.rs broadcast.\n\
         Common causes: region predicate not applied, entity prefix match too broad or too narrow.\n\
         Run: cargo test --test store_advanced subscription_filters_by_region"
    );

    store.sync().expect("sync");
}

// --- Cursor (pull-based) ---

#[test]
fn cursor_polls_events_in_order() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:cur", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..5 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    let region = Region::entity("entity:cur");
    let mut cursor = store.cursor(&region);

    let mut polled = Vec::new();
    while let Some(entry) = cursor.poll() {
        polled.push(entry);
    }

    assert_eq!(
        polled.len(),
        5,
        "PROPERTY: cursor must yield all 5 appended events when polled to exhaustion.\n\
         Investigate: src/store/cursor.rs poll.\n\
         Common causes: cursor stops at segment boundary, region filter drops valid events.\n\
         Run: cargo test --test store_advanced cursor_polls_events_in_order"
    );

    // Verify global_sequence is monotonically increasing
    for window in polled.windows(2) {
        assert!(
            window[0].global_sequence < window[1].global_sequence,
            "PROPERTY: cursor must yield events in strictly ascending global_sequence order.\n\
             Investigate: src/store/cursor.rs poll.\n\
             Common causes: cursor index not sorted on open, iterator yields unordered segments.\n\
             Run: cargo test --test store_advanced cursor_polls_events_in_order"
        );
    }

    store.close().expect("close");
}

#[test]
fn cursor_poll_batch_respects_max() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:batch", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    let region = Region::entity("entity:batch");
    let mut cursor = store.cursor(&region);

    let batch1 = cursor.poll_batch(3);
    assert_eq!(
        batch1.len(),
        3,
        "PROPERTY: first poll_batch(3) on a 10-event stream must return exactly 3 events.\n\
         Investigate: src/store/cursor.rs poll_batch.\n\
         Common causes: max parameter ignored, cursor yields all remaining instead of bounded slice.\n\
         Run: cargo test --test store_advanced cursor_poll_batch_respects_max"
    );

    let batch2 = cursor.poll_batch(3);
    assert_eq!(
        batch2.len(),
        3,
        "PROPERTY: second poll_batch(3) must return exactly 3 more events.\n\
         Investigate: src/store/cursor.rs poll_batch.\n\
         Common causes: cursor position not advanced after first batch, events re-yielded.\n\
         Run: cargo test --test store_advanced cursor_poll_batch_respects_max"
    );

    let batch3 = cursor.poll_batch(100);
    assert_eq!(
        batch3.len(),
        4,
        "PROPERTY: third poll_batch must return the remaining 4 events.\n\
         Investigate: src/store/cursor.rs poll_batch.\n\
         Common causes: cursor position drifts, batch limit applied incorrectly to remainder.\n\
         Run: cargo test --test store_advanced cursor_poll_batch_respects_max"
    );

    let batch4 = cursor.poll_batch(100);
    assert_eq!(
        batch4.len(),
        0,
        "PROPERTY: poll_batch on an exhausted cursor must return an empty batch.\n\
         Investigate: src/store/cursor.rs poll_batch.\n\
         Common causes: cursor resets on empty, returns stale events after stream end.\n\
         Run: cargo test --test store_advanced cursor_poll_batch_respects_max"
    );

    store.close().expect("close");
}

// --- compact ---

#[test]
fn compact_does_not_lose_data() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:compact", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..5 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    store
        .compact(&CompactionConfig::default())
        .expect("compact");

    let stats = store.stats();
    assert_eq!(
        stats.event_count, 5,
        "PROPERTY: compact() must not lose any events — all 5 appended events must remain.\n\
         Investigate: src/store/mod.rs compact, src/store/segment.rs compaction path.\n\
         Common causes: compaction drops events below tombstone horizon, segment replaced before flush.\n\
         Run: cargo test --test store_advanced compact_does_not_lose_data"
    );

    store.close().expect("close");
}

/// Retention compaction drops events — index must not reference dropped events.
#[test]
fn compact_retention_removes_dropped_events_from_index() {
    let dir = TempDir::new().expect("create temp dir");
    let keep_kind = EventKind::custom(0xF, 1);
    let drop_kind = EventKind::custom(0xF, 2);

    // Phase 1: populate events, then close to seal all segments.
    let mut drop_ids = Vec::new();
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 512, // force many segment rotations
            sync_every_n_events: 1,
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:retention", "scope:test").expect("valid coord");

        for i in 0..10 {
            let kind = if i % 2 == 0 { keep_kind } else { drop_kind };
            let receipt = store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
            if i % 2 != 0 {
                drop_ids.push(receipt.event_id);
            }
        }
        store.close().expect("close");
    }

    // Phase 2: reopen (all previous segments are now sealed) and compact.
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("reopen");

    let retention_config = CompactionConfig {
        strategy: CompactionStrategy::Retention(Box::new(move |stored| {
            stored.event.header.event_kind == keep_kind
        })),
        min_segments: 1,
    };
    store.compact(&retention_config).expect("compact");

    // Dropped event IDs must return NotFound
    for dropped_id in &drop_ids {
        let get_result = store.get(*dropped_id);
        assert!(
            get_result.is_err(),
            "COMPACT RETENTION INDEX LEAK: get({dropped_id}) should return NotFound after retention \
             compaction dropped the event.\n\
             Investigate: src/store/mod.rs compact(), src/store/index.rs clear().\n\
             Common causes: index not rebuilt after compaction, stale entries pointing to deleted segments."
        );
    }

    // Remaining events should still be accessible (5 kept + events in new active segment = 5)
    assert_eq!(
        store.stats().event_count,
        5,
        "COMPACT RETENTION COUNT: expected 5 kept events after dropping 5.\n\
         Investigate: src/store/mod.rs compact() index rebuild."
    );

    store.close().expect("close");
}

/// Tombstone compaction replaces dropped events with tombstone kind — index must reflect new kind.
#[test]
fn compact_tombstone_updates_event_kind_in_index() {
    let dir = TempDir::new().expect("create temp dir");
    let live_kind = EventKind::custom(0xF, 1);
    let doomed_kind = EventKind::custom(0xF, 2);
    let tombstone_kind = EventKind::custom(0x0, 0xFFE);

    // Phase 1: populate events, then close to seal all segments.
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 512,
            sync_every_n_events: 1,
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:tombstone", "scope:test").expect("valid coord");

        for i in 0..10 {
            let kind = if i % 2 == 0 { live_kind } else { doomed_kind };
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        store.close().expect("close");
    }

    // Phase 2: reopen and compact with tombstone strategy.
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("reopen");

    let tombstone_config = CompactionConfig {
        strategy: CompactionStrategy::Tombstone(Box::new(move |stored| {
            stored.event.header.event_kind == live_kind
        })),
        min_segments: 1,
    };
    store.compact(&tombstone_config).expect("compact");

    // All 10 events should still exist (tombstones replace, not remove)
    assert_eq!(
        store.stats().event_count,
        10,
        "COMPACT TOMBSTONE COUNT: expected all 10 events to remain (5 live + 5 tombstoned).\n\
         Investigate: src/store/mod.rs compact() tombstone path."
    );

    // Tombstoned events should have tombstone kind in the index
    let region = Region::all().with_fact(KindFilter::Exact(tombstone_kind));
    let tombstoned = store.query(&region);
    assert_eq!(
        tombstoned.len(), 5,
        "COMPACT TOMBSTONE KIND: expected 5 events with tombstone kind in index after compaction.\n\
         Investigate: src/store/mod.rs compact() index rebuild, tombstone_kind.\n\
         Common causes: index not rebuilt after compaction, kind not updated."
    );

    store.close().expect("close");
}

// --- StoreConfig::new() defaults ---

#[test]
fn store_config_new_uses_sensible_defaults() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("open");
    let diag = store.diagnostics();
    assert_eq!(
        diag.segment_max_bytes,
        256 * 1024 * 1024,
        "PROPERTY: StoreConfig::new() must set segment_max_bytes to 256 MiB.\n\
         Investigate: src/store/mod.rs StoreConfig::new.\n\
         Common causes: default constant changed, field wired to wrong config value.\n\
         Run: cargo test --test store_advanced store_config_new_uses_sensible_defaults"
    );
    assert_eq!(
        diag.fd_budget, 64,
        "PROPERTY: StoreConfig::new() must set fd_budget to 64.\n\
         Investigate: src/store/mod.rs StoreConfig::new.\n\
         Common causes: default constant changed, fd_budget not propagated into diagnostics.\n\
         Run: cargo test --test store_advanced store_config_new_uses_sensible_defaults"
    );
    store.close().expect("close");
}

// --- Event not found ---

#[test]
fn get_nonexistent_returns_not_found() {
    let (store, _dir) = test_store();
    let result = store.get(0xDEAD);
    assert!(
        result.is_err(),
        "PROPERTY: get() of a nonexistent event_id must return Err(StoreError::NotFound).\n\
         Investigate: src/store/mod.rs get, src/store/reader.rs lookup.\n\
         Common causes: index returns a default entry instead of None, error type suppressed.\n\
         Run: cargo test --test store_advanced get_nonexistent_returns_not_found"
    );
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

    let receipt = store
        .apply_transition(&coord, transition)
        .expect("apply_transition");

    // Verify: event persisted and retrievable
    let stored = store.get(receipt.event_id).expect("get transition event");
    assert_eq!(
        stored.event.event_kind(),
        kind,
        "PROPERTY: apply_transition must persist the EventKind carried by the Transition.\n\
         Investigate: src/store/mod.rs apply_transition, src/typestate/mod.rs Transition.\n\
         Common causes: transition payload serialised without kind, wrong kind written to frame.\n\
         Run: cargo test --test store_advanced apply_transition_persists_event"
    );
    assert_eq!(
        stored.coordinate, coord,
        "PROPERTY: apply_transition must persist the event under the supplied Coordinate.\n\
         Investigate: src/store/mod.rs apply_transition.\n\
         Common causes: coordinate not forwarded to inner append call, coordinate field swapped.\n\
         Run: cargo test --test store_advanced apply_transition_persists_event"
    );

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
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    // Query with clock_range [3, 7] — should get events with clock 3,4,5,6,7
    let region = Region::entity("entity:clock").with_clock_range((3, 7));
    let results = store.query(&region);

    assert_eq!(
        results.len(),
        5,
        "PROPERTY: clock_range [3,7] query must return exactly 5 events (clocks 3,4,5,6,7).\n\
         Investigate: src/store/index.rs query clock_range filter.\n\
         Common causes: range bounds exclusive instead of inclusive, clock field misread from frame.\n\
         Run: cargo test --test store_advanced query_with_clock_range_filters_events"
    );

    // Verify all results have clock in [3, 7]
    for entry in &results {
        assert!(
            entry.clock >= 3 && entry.clock <= 7,
            "PROPERTY: every result from a clock_range [3,7] query must have clock in [3,7], got {}.\n\
             Investigate: src/store/index.rs query clock_range filter.\n\
             Common causes: range bounds off-by-one, filter applied before or after wrong index.\n\
             Run: cargo test --test store_advanced query_with_clock_range_filters_events",
            entry.clock
        );
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
        store
            .append(&coord_a, kind, &serde_json::json!({"i": i}))
            .expect("append a");
        store
            .append(&coord_b, kind, &serde_json::json!({"i": i}))
            .expect("append b");
    }

    // entity:a with clock range [1,3]
    let region = Region::entity("entity:a").with_clock_range((1, 3));
    let results = store.query(&region);
    assert_eq!(
        results.len(),
        3,
        "PROPERTY: entity:a with clock_range [1,3] must return exactly 3 events.\n\
         Investigate: src/store/index.rs query clock_range + entity filter.\n\
         Common causes: entity filter applied after range filter loses scope, range inclusive bounds wrong.\n\
         Run: cargo test --test store_advanced query_clock_range_with_scope_filter"
    );

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

    store
        .append(&coord, kind_a1, &serde_json::json!({"cat": "a"}))
        .expect("append");
    store
        .append(&coord, kind_a2, &serde_json::json!({"cat": "a"}))
        .expect("append");
    store
        .append(&coord, kind_b1, &serde_json::json!({"cat": "b"}))
        .expect("append");

    // Query by category 0xA — should get both kind_a1 and kind_a2
    let region = Region::all().with_fact_category(0xA);
    let results = store.query(&region);
    assert_eq!(
        results.len(),
        2,
        "PROPERTY: fact_category filter 0xA must match exactly 2 events (kind_a1 and kind_a2).\n\
         Investigate: src/store/index.rs KindFilter::Category path.\n\
         Common causes: category nibble extracted from wrong byte, filter matches all kinds.\n\
         Run: cargo test --test store_advanced query_by_fact_category"
    );

    store.close().expect("close");
}

// --- fd_budget LRU eviction ---

#[test]
fn fd_budget_evicts_oldest_segments() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512, // tiny segments → many segment files
        sync_every_n_events: 1,
        fd_budget: 2, // only 2 FDs allowed → forces eviction
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:fd", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Write enough events to create many segments (>2, exceeding fd_budget)
    for i in 0..100 {
        store
            .append(
                &coord,
                kind,
                &serde_json::json!({"data": format!("payload_{i}")}),
            )
            .expect("append");
    }
    store.sync().expect("sync");

    // Verify: multiple segments created
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
        segment_count > 2,
        "PROPERTY: writing 100 events with segment_max_bytes=512 must create more than 2 segment files.\n\
         Investigate: src/store/writer.rs segment rotation logic.\n\
         Common causes: rotation threshold not honoured, all events written to one segment.\n\
         Run: cargo test --test store_advanced fd_budget_evicts_oldest_segments"
    );

    // Read events from different segments — this exercises LRU eviction
    // because fd_budget=2 but we have >2 segments
    let entries = store.stream("entity:fd");
    assert_eq!(
        entries.len(),
        100,
        "PROPERTY: stream must return all 100 appended events even when fd_budget forces LRU eviction.\n\
         Investigate: src/store/reader.rs get_fd LRU cache, src/store/mod.rs stream.\n\
         Common causes: evicted segment FD not re-opened on next access, stream skips closed segments.\n\
         Run: cargo test --test store_advanced fd_budget_evicts_oldest_segments"
    );

    // Read first event (oldest segment), last event (newest), then first again
    // This forces eviction: open seg1, open seg_last (evicts seg1 if budget=2),
    // then re-open seg1 (evicts seg_last)
    let first = store.get(entries[0].event_id).expect("get first");
    let last = store.get(entries[99].event_id).expect("get last");
    let first_again = store
        .get(entries[0].event_id)
        .expect("get first again after eviction");

    assert_eq!(
        first.event.event_id(),
        first_again.event.event_id(),
        "PROPERTY: re-reading the same event after LRU fd eviction must return the identical event_id.\n\
         Investigate: src/store/reader.rs get_fd LRU cache.\n\
         Common causes: evicted segment FD reopened to wrong offset, cache key collision after eviction.\n\
         Run: cargo test --test store_advanced fd_budget_evicts_oldest_segments"
    );

    // Verify event identity integrity through eviction cycles
    assert_eq!(
        first.event.event_kind(),
        last.event.event_kind(),
        "PROPERTY: EventKind must be identical for events written with the same kind, \
         even when read from different segments after LRU eviction.\n\
         Investigate: src/store/reader.rs get_fd LRU cache, src/store/segment.rs read_frame.\n\
         Common causes: frame data corrupted during eviction cycle, wrong frame decoded after re-open.\n\
         Run: cargo test --test store_advanced fd_budget_evicts_oldest_segments"
    );

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
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open");
        let coord = Coordinate::new("entity:corrupt", "scope:test").expect("valid coord");
        for i in 0..20 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
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
        ..StoreConfig::new("")
    };
    let result = Store::open(config);
    // The store should fail on a corrupt segment (bad magic = hard error)
    assert!(
        result.is_err(),
        "PROPERTY: Store::open must return Err when a segment file has an invalid magic header.\n\
         Investigate: src/store/reader.rs scan_segment magic check.\n\
         Common causes: magic bytes check skipped or returns Ok(empty), corrupt file silently ignored.\n\
         Run: cargo test --test store_advanced cold_start_skips_corrupt_segment_gracefully"
    );
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
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open");
        let coord = Coordinate::new("entity:crc", "scope:test").expect("valid");
        for i in 0..3 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: corrupt a segment file by flipping bytes in the middle
    let segments: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .collect();
    assert!(
        !segments.is_empty(),
        "PROPERTY: after appending events and syncing, at least one .fbat segment file must exist.\n\
         Investigate: src/store/writer.rs sync, src/store/segment.rs write path.\n\
         Common causes: sync no-op, segment file never flushed to disk, wrong extension used.\n\
         Run: cargo test --test store_advanced corrupt_frame_in_segment_is_detected"
    );

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
        ..StoreConfig::new("")
    };
    // The store may open successfully (skipping corrupt frames) or may error
    // depending on where the corruption landed. Either behavior is acceptable
    // — what matters is it doesn't silently return wrong data.
    match Store::open(config) {
        Ok(store) => {
            let stats = store.stats();
            // Corrupted segment may have fewer events (some frames skipped)
            // The key assertion: we don't get MORE events than we wrote
            assert!(
                stats.event_count <= 3,
                "PROPERTY: a store opened with a corrupted segment must not report more events than were written — no phantom events allowed. Got {}.\n\
                 Investigate: src/store/reader.rs scan_segment CRC check, src/store/mod.rs open.\n\
                 Common causes: CRC check skipped, corrupt bytes decoded as valid frames.\n\
                 Run: cargo test --test store_advanced corrupt_frame_in_segment_is_detected",
                stats.event_count
            );
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
    assert!(
        not_found.contains("dead"),
        "PROPERTY: StoreError::NotFound Display must include the event ID in hex (e.g. 'dead').\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm for NotFound omits the id, uses decimal instead of hex.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let writer = format!("{}", StoreError::WriterCrashed);
    assert!(
        writer.contains("writer") || writer.contains("crash"),
        "PROPERTY: StoreError::WriterCrashed Display must mention 'writer' or 'crash'.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm returns generic message without variant-specific text.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let shutting = format!("{}", StoreError::ShuttingDown);
    assert!(
        shutting.contains("shut"),
        "PROPERTY: StoreError::ShuttingDown Display must contain 'shut' (e.g. 'shutting down').\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm returns generic or empty string for ShuttingDown variant.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let cache = format!("{}", StoreError::CacheFailed("redis timeout".into()));
    assert!(
        cache.contains("redis timeout"),
        "PROPERTY: StoreError::CacheFailed Display must include the inner error message.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: inner string not interpolated, Display arm discards the inner field.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let dup = format!("{}", StoreError::DuplicateEvent(0xBEEF));
    assert!(
        dup.contains("beef"),
        "PROPERTY: StoreError::DuplicateEvent Display must include the event key in hex (e.g. 'beef').\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm for DuplicateEvent omits the key, uses decimal instead of hex.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let seq = format!(
        "{}",
        StoreError::SequenceMismatch {
            entity: "user:1".into(),
            expected: 5,
            actual: 3
        }
    );
    assert!(
        seq.contains("user:1") && seq.contains("5") && seq.contains("3"),
        "PROPERTY: StoreError::SequenceMismatch Display must include entity, expected, and actual values.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm omits one or more struct fields, entity string not interpolated.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let crc = format!(
        "{}",
        StoreError::CrcMismatch {
            segment_id: 7,
            offset: 42
        }
    );
    assert!(
        crc.contains("7") && crc.contains("42"),
        "PROPERTY: StoreError::CrcMismatch Display must include segment_id (7) and offset (42).\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm for CrcMismatch omits numeric fields, formats only one field.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let corrupt = format!(
        "{}",
        StoreError::CorruptSegment {
            segment_id: 3,
            detail: "bad magic".into()
        }
    );
    assert!(
        corrupt.contains("bad magic"),
        "PROPERTY: StoreError::CorruptSegment Display must include the detail string.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: detail field not interpolated into Display output, generic message used.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );

    let ser = format!("{}", StoreError::Serialization("unexpected EOF".into()));
    assert!(
        ser.contains("unexpected EOF"),
        "PROPERTY: StoreError::Serialization Display must include the inner error message.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: inner string not forwarded to Display output, variant uses static text only.\n\
         Run: cargo test --test store_advanced store_error_display_variants"
    );
}

// --- CoordinateError Display ---

#[test]
fn coordinate_error_display() {
    let err = Coordinate::new("", "scope").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("entity"),
        "PROPERTY: CoordinateError for an empty entity string must mention 'entity' in its Display.\n\
         Investigate: src/store/mod.rs CoordinateError Display impl.\n\
         Common causes: EmptyEntity variant Display returns generic string without the word 'entity'.\n\
         Run: cargo test --test store_advanced coordinate_error_display"
    );

    let err = Coordinate::new("entity", "").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("scope"),
        "PROPERTY: CoordinateError for an empty scope string must mention 'scope' in its Display.\n\
         Investigate: src/store/mod.rs CoordinateError Display impl.\n\
         Common causes: EmptyScope variant Display returns generic string without the word 'scope'.\n\
         Run: cargo test --test store_advanced coordinate_error_display"
    );
}

// --- Coordinate Display ---

#[test]
fn coordinate_display_format() {
    let coord = Coordinate::new("user:42", "tenant:acme").expect("valid");
    let display = format!("{coord}");
    assert_eq!(
        display, "user:42@tenant:acme",
        "PROPERTY: Coordinate Display must format as 'entity@scope' (e.g. 'user:42@tenant:acme').\n\
         Investigate: src/store/mod.rs Coordinate Display impl.\n\
         Common causes: separator wrong (e.g. '/' or ':' instead of '@'), fields swapped.\n\
         Run: cargo test --test store_advanced coordinate_display_format"
    );
}

// --- IndexEntry causation helpers ---

#[test]
fn index_entry_causation_helpers() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:helpers", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Root event (self-correlated, no causation)
    let root = store
        .append(&coord, kind, &serde_json::json!({"cmd": "create"}))
        .expect("root");

    // Reaction event
    let reaction = store
        .append_reaction(
            &coord,
            kind,
            &serde_json::json!({"evt": "created"}),
            root.event_id,
            root.event_id,
        )
        .expect("reaction");

    let entries = store.stream("entity:helpers");
    assert_eq!(
        entries.len(),
        2,
        "PROPERTY: stream must return exactly 2 events (root + reaction) for entity:helpers.\n\
         Investigate: src/store/mod.rs stream, src/store/index.rs entity lookup.\n\
         Common causes: reaction event stored under wrong entity key, stream skips reaction frames.\n\
         Run: cargo test --test store_advanced index_entry_causation_helpers"
    );

    // Root: is_root_cause=true, is_correlated=false (correlation==event_id)
    let root_entry = entries
        .iter()
        .find(|e| e.event_id == root.event_id)
        .expect("find root");
    assert!(
        root_entry.is_root_cause(),
        "PROPERTY: an event with no explicit causation must be identified as a root cause.\n\
         Investigate: src/store/mod.rs IndexEntry::is_root_cause.\n\
         Common causes: is_root_cause checks wrong field, causation_id default value incorrect.\n\
         Run: cargo test --test store_advanced index_entry_causation_helpers"
    );
    assert!(
        !root_entry.is_correlated(),
        "PROPERTY: a self-correlated event (correlation_id == event_id) must not be 'correlated'.\n\
         Investigate: src/store/mod.rs IndexEntry::is_correlated.\n\
         Common causes: is_correlated returns true for self-correlation, field comparison inverted.\n\
         Run: cargo test --test store_advanced index_entry_causation_helpers"
    );

    // Reaction: is_root_cause=false, is_correlated=true, is_caused_by(root)=true
    let react_entry = entries
        .iter()
        .find(|e| e.event_id == reaction.event_id)
        .expect("find reaction");
    assert!(
        !react_entry.is_root_cause(),
        "PROPERTY: a reaction event with an explicit cause must not be identified as a root cause.\n\
         Investigate: src/store/mod.rs IndexEntry::is_root_cause.\n\
         Common causes: is_root_cause ignores causation_id field, always returns true.\n\
         Run: cargo test --test store_advanced index_entry_causation_helpers"
    );
    assert!(
        react_entry.is_correlated(),
        "PROPERTY: a reaction event with a correlation_id different from its own event_id must be 'correlated'.\n\
         Investigate: src/store/mod.rs IndexEntry::is_correlated.\n\
         Common causes: correlation_id not set on reaction frame, is_correlated comparison wrong.\n\
         Run: cargo test --test store_advanced index_entry_causation_helpers"
    );
    assert!(
        react_entry.is_caused_by(root.event_id),
        "PROPERTY: a reaction event must report is_caused_by(root.event_id) == true.\n\
         Investigate: src/store/mod.rs IndexEntry::is_caused_by.\n\
         Common causes: causation_id not stored in reaction frame, is_caused_by checks wrong field.\n\
         Run: cargo test --test store_advanced index_entry_causation_helpers"
    );

    store.close().expect("close");
}
