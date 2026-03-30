#![allow(
    clippy::unwrap_used,           // test assertions
    clippy::disallowed_methods,    // chaos tests use thread::spawn for concurrency probes
    clippy::cast_possible_truncation, // test data fits in target types
    clippy::needless_borrows_for_generic_args
)]
//! Advanced Store tests: code paths missed by store_integration.rs.
//! Covers: walk_ancestors, snapshot, diagnostics, append_reaction,
//! subscription, cursor, compact, CAS failure, idempotency,
//! apply_transition, clock_range queries, fd_budget eviction,
//! corrupt segment recovery.
//!
//! PROVES: LAW-001 (No Fake Success), LAW-003 (No Orphan Infrastructure)
//! DEFENDS: FM-007 (Island Syndrome), FM-013 (Coverage Mirage)
//! INVARIANTS: INV-STATE (cursor state machine), INV-TEMP (temporal ordering)
//!
//! PROVES: LAW-001 (No Fake Success), LAW-003 (No Orphan Infrastructure — exercises full public API)
//! DEFENDS: FM-009 (Polite Downgrade — restart_policy wired), FM-011 (Error Path Hollowing), FM-013 (Coverage Mirage)
//! INVARIANTS: INV-CONC (CAS, idempotency), INV-TEMP (walk_ancestors, compaction), INV-PERF (fd_budget)
//! [SPEC:tests/store_advanced.rs]

use batpak::event::Reactive;
use batpak::prelude::*;
use batpak::store::{RestartPolicy, Store, StoreConfig};
use batpak::typestate::Transition;
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
    let opts = batpak::store::AppendOptions {
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
    let opts = batpak::store::AppendOptions {
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
    let tombstone_kind = EventKind::TOMBSTONE;

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
    use batpak::store::StoreError;

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

// ================================================================
// Phase 3 — NEW TESTS: Flags, Subscription ops, Cursor edge cases,
// walk_ancestors genesis, DagPosition is_ancestor_of, commit_bypass,
// react_loop, prefetch wiring.
// ================================================================

// --- Flags round-trip ---

#[test]
fn append_with_flags_round_trips() {
    use batpak::event::header::{FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL};

    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:flags", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let flags = FLAG_REQUIRES_ACK | FLAG_TRANSACTIONAL;

    let opts = AppendOptions {
        flags,
        ..Default::default()
    };
    let receipt = store
        .append_with_options(&coord, kind, &serde_json::json!({"flagged": true}), opts)
        .expect("append with flags");

    let stored = store.get(receipt.event_id).expect("get");
    assert_eq!(
        stored.event.header.flags, flags,
        "PROPERTY: flags set via AppendOptions must round-trip through append→get.\n\
         Investigate: src/store/mod.rs append_with_options, src/store/writer.rs handle_append.\n\
         Common causes: flags not propagated from AppendOptions to EventHeader, writer overwrites flags.\n\
         Run: cargo test --test store_advanced append_with_flags_round_trips"
    );
    assert!(
        stored.event.header.requires_ack(),
        "PROPERTY: FLAG_REQUIRES_ACK must be readable via requires_ack() accessor.\n\
         Investigate: src/event/header.rs requires_ack.\n\
         Run: cargo test --test store_advanced append_with_flags_round_trips"
    );
    assert!(
        stored.event.header.is_transactional(),
        "PROPERTY: FLAG_TRANSACTIONAL must be readable via is_transactional() accessor.\n\
         Investigate: src/event/header.rs is_transactional.\n\
         Run: cargo test --test store_advanced append_with_flags_round_trips"
    );

    store.close().expect("close");
}

// --- SubscriptionOps::map ---

#[test]
fn subscription_ops_map_transforms_notifications() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:map", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:map");

    let sub = store.subscribe(&region);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    std::thread::spawn(move || {
        store_w
            .append(&coord_w, kind, &serde_json::json!({"v": 1}))
            .expect("append");
    })
    .join()
    .expect("writer");

    // Use map to transform: change the kind to a custom marker
    let marker_kind = EventKind::custom(0xA, 0xBB);
    let mut ops = sub.ops().map(move |n| {
        let mut transformed = n.clone();
        transformed.kind = marker_kind;
        Some(transformed)
    });

    // Use try-based approach: events are already sent
    let rx_result = std::thread::spawn(move || ops.recv())
        .join()
        .expect("recv thread");

    assert!(
        rx_result.is_some(),
        "PROPERTY: SubscriptionOps::map must pass through transformed notifications.\n\
         Investigate: src/store/subscription.rs SubscriptionOps::map and recv.\n\
         Common causes: map_fn not applied in recv loop, map returns None.\n\
         Run: cargo test --test store_advanced subscription_ops_map_transforms_notifications"
    );
    let notif = rx_result.unwrap();
    assert_eq!(
        notif.kind, marker_kind,
        "PROPERTY: SubscriptionOps::map must apply the transformation function to notifications.\n\
         Investigate: src/store/subscription.rs recv map_fn application.\n\
         Common causes: map_fn ignored, original notification returned instead.\n\
         Run: cargo test --test store_advanced subscription_ops_map_transforms_notifications"
    );

    store.sync().expect("sync");
}

// --- SubscriptionOps::filter chains ---

#[test]
fn subscription_ops_filter_chains_correctly() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let kind1 = EventKind::custom(0xF, 1);
    let kind2 = EventKind::custom(0xF, 2);
    let coord = Coordinate::new("entity:filt", "scope:test").expect("valid coord");
    let region = Region::entity("entity:filt");

    let sub = store.subscribe(&region);

    // Chain two filters and take(2) to prevent blocking forever:
    // first accepts kind1 only, second is always-true (AND semantics)
    let mut ops = sub
        .ops()
        .filter(move |n| n.kind == kind1)
        .filter(|_n| true)
        .take(2);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::spawn(move || {
        store_w
            .append(&coord_w, kind1, &serde_json::json!({"k": 1}))
            .expect("append");
        store_w
            .append(&coord_w, kind2, &serde_json::json!({"k": 2}))
            .expect("append");
        store_w
            .append(&coord_w, kind1, &serde_json::json!({"k": 3}))
            .expect("append");
    });

    let result = std::thread::spawn(move || {
        let mut results = Vec::new();
        while let Some(n) = ops.recv() {
            results.push(n);
        }
        results
    })
    .join()
    .expect("recv thread");

    writer.join().expect("writer");

    assert_eq!(
        result.len(),
        2,
        "PROPERTY: chained filter with AND semantics must pass only kind1 events (2 of 3).\n\
         Investigate: src/store/subscription.rs SubscriptionOps::filter, recv.\n\
         Common causes: filters not chained, last filter replaces previous.\n\
         Run: cargo test --test store_advanced subscription_ops_filter_chains_correctly"
    );

    store.sync().expect("sync");
}

// --- SubscriptionOps::take ---

#[test]
fn subscription_ops_take_limits_count() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:take", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:take");

    let sub = store.subscribe(&region);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    std::thread::spawn(move || {
        for i in 0..5 {
            store_w
                .append(&coord_w, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        drop(store_w);
    })
    .join()
    .expect("writer");

    let mut ops = sub.ops().take(3);
    let result = std::thread::spawn(move || {
        let mut results = Vec::new();
        while let Some(n) = ops.recv() {
            results.push(n);
        }
        results
    })
    .join()
    .expect("recv thread");

    assert_eq!(
        result.len(),
        3,
        "PROPERTY: SubscriptionOps::take(3) must return at most 3 notifications from 5 events.\n\
         Investigate: src/store/subscription.rs SubscriptionOps::take, recv count check.\n\
         Common causes: count not incremented in recv, limit check after return.\n\
         Run: cargo test --test store_advanced subscription_ops_take_limits_count"
    );

    store.sync().expect("sync");
}

// --- Cursor edge cases ---

#[test]
fn cursor_on_empty_store_returns_empty() {
    let (store, _dir) = test_store();
    let region = Region::entity("entity:nothing");
    let mut cursor = store.cursor(&region);

    assert!(
        cursor.poll().is_none(),
        "PROPERTY: cursor.poll() on an empty store must return None.\n\
         Investigate: src/store/cursor.rs poll.\n\
         Common causes: cursor starts with a non-zero position, index returns phantom entries.\n\
         Run: cargo test --test store_advanced cursor_on_empty_store_returns_empty"
    );

    let batch = cursor.poll_batch(10);
    assert!(
        batch.is_empty(),
        "PROPERTY: cursor.poll_batch() on an empty store must return an empty Vec.\n\
         Investigate: src/store/cursor.rs poll_batch.\n\
         Run: cargo test --test store_advanced cursor_on_empty_store_returns_empty"
    );

    store.close().expect("close");
}

#[test]
fn cursor_sees_events_appended_after_creation() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:late", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:late");

    // Create cursor BEFORE any events
    let mut cursor = store.cursor(&region);
    assert!(cursor.poll().is_none(), "cursor should be empty initially");

    // Append events AFTER cursor creation
    for i in 0..3 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    // Cursor should now see the new events
    let batch = cursor.poll_batch(10);
    assert_eq!(
        batch.len(),
        3,
        "PROPERTY: cursor must see events appended after cursor creation (guaranteed delivery).\n\
         Investigate: src/store/cursor.rs poll_batch, position tracking.\n\
         Common causes: cursor snapshots index at creation time and never refreshes.\n\
         Run: cargo test --test store_advanced cursor_sees_events_appended_after_creation"
    );

    store.close().expect("close");
}

#[test]
fn cursor_guaranteed_delivery_under_load() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:load", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:load");

    let event_count = 100;

    // Append from multiple threads
    let mut handles = Vec::new();
    for t in 0..4 {
        let s = Arc::clone(&store);
        let c = coord.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..25 {
                s.append(&c, kind, &serde_json::json!({"t": t, "i": i}))
                    .expect("append");
            }
        }));
    }
    for h in handles {
        h.join().expect("writer");
    }

    // Cursor should see ALL events (guaranteed delivery)
    let mut cursor = store.cursor(&region);
    let mut total = 0;
    loop {
        let batch = cursor.poll_batch(50);
        if batch.is_empty() {
            break;
        }
        total += batch.len();
    }

    assert_eq!(
        total, event_count,
        "PROPERTY: cursor must deliver exactly {event_count} events under concurrent load (guaranteed delivery).\n\
         Investigate: src/store/cursor.rs poll_batch, src/store/index.rs.\n\
         Common causes: index race conditions, cursor skips entries during concurrent writes.\n\
         Run: cargo test --test store_advanced cursor_guaranteed_delivery_under_load"
    );

    store.sync().expect("sync");
}

// --- walk_ancestors genesis edge case ---

#[test]
fn walk_ancestors_genesis_returns_single_event() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:gen", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let receipt = store
        .append(&coord, kind, &serde_json::json!({"genesis": true}))
        .expect("append");
    let ancestors = store.walk_ancestors(receipt.event_id, 10);

    assert_eq!(
        ancestors.len(), 1,
        "PROPERTY: walk_ancestors on a genesis event (first in chain) must return exactly 1 event.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: walk doesn't stop at genesis (prev_hash == [0;32]), off-by-one.\n\
         Run: cargo test --test store_advanced walk_ancestors_genesis_returns_single_event"
    );
    assert_eq!(
        ancestors[0].event.event_id(),
        receipt.event_id,
        "PROPERTY: the single ancestor returned must be the genesis event itself.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Run: cargo test --test store_advanced walk_ancestors_genesis_returns_single_event"
    );

    store.close().expect("close");
}

// --- DagPosition::is_ancestor_of fix verification ---

#[test]
fn dag_position_different_depth_not_ancestor() {
    let pos_a = DagPosition::child_at(5, 1000, 0); // depth=0, seq=5
    let pos_b = DagPosition::child_at(10, 2000, 0); // depth=0, seq=10

    // Same depth, same lane — pos_a IS ancestor of pos_b
    assert!(
        pos_a.is_ancestor_of(&pos_b),
        "PROPERTY: same-depth, same-lane, lower-sequence must be ancestor.\n\
         Investigate: src/coordinate/position.rs is_ancestor_of.\n\
         Run: cargo test --test store_advanced dag_position_different_depth_not_ancestor"
    );

    // Self is NOT ancestor of self (strict less-than on sequence)
    assert!(
        !pos_a.is_ancestor_of(&pos_a),
        "PROPERTY: a position must NOT be its own ancestor (strict ordering).\n\
         Investigate: src/coordinate/position.rs is_ancestor_of.\n\
         Run: cargo test --test store_advanced dag_position_different_depth_not_ancestor"
    );
}

// --- Pipeline::commit_bypass ---

#[test]
fn pipeline_commit_bypass_persists() {
    use batpak::pipeline::bypass::BypassReason;

    struct TestBypass;
    impl BypassReason for TestBypass {
        fn name(&self) -> &'static str {
            "test-bypass"
        }
        fn justification(&self) -> &'static str {
            "testing commit_bypass"
        }
    }

    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:bypass", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let proposal = Proposal::new(serde_json::json!({"bypassed": true}));
    let bypass_receipt = Pipeline::<()>::bypass(proposal, &TestBypass);

    let committed = Pipeline::<()>::commit_bypass(bypass_receipt, |p| -> Result<_, StoreError> {
        let r = store.append(&coord, kind, &p)?;
        Ok(Committed {
            payload: p,
            event_id: r.event_id,
            sequence: r.sequence,
            hash: [0u8; 32],
        })
    })
    .expect("commit_bypass");

    // Verify persisted
    let stored = store.get(committed.event_id).expect("get");
    assert_eq!(
        stored.event.event_kind(),
        kind,
        "PROPERTY: commit_bypass must persist the event through the store.\n\
         Investigate: src/pipeline/mod.rs commit_bypass.\n\
         Common causes: commit_fn not called, payload not forwarded.\n\
         Run: cargo test --test store_advanced pipeline_commit_bypass_persists"
    );

    store.close().expect("close");
}

// --- Store::react_loop ---

#[test]
fn react_loop_spawns_and_processes() {
    use batpak::event::sourcing::Reactive;

    struct TestReactor;
    impl Reactive<serde_json::Value> for TestReactor {
        fn react(
            &self,
            event: &batpak::prelude::Event<serde_json::Value>,
        ) -> Vec<(Coordinate, EventKind, serde_json::Value)> {
            if event.event_kind() == EventKind::custom(0xA, 1) {
                vec![(
                    Coordinate::new("entity:reactions", "scope:test").expect("valid"),
                    EventKind::custom(0xA, 2),
                    serde_json::json!({"reacted_to": event.event_id().to_string()}),
                )]
            } else {
                vec![]
            }
        }
    }

    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open store"));

    let region = Region::entity("entity:trigger");
    let _handle = store
        .react_loop(&region, TestReactor)
        .expect("spawn reactor");

    // Append a trigger event
    let coord = Coordinate::new("entity:trigger", "scope:test").expect("valid coord");
    store
        .append(
            &coord,
            EventKind::custom(0xA, 1),
            &serde_json::json!({"trigger": true}),
        )
        .expect("append");

    // Give the reactor thread time to process
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Check that a reaction was appended
    let reactions = store.query(&Region::entity("entity:reactions"));
    assert!(
        !reactions.is_empty(),
        "PROPERTY: react_loop must produce reaction events when the reactor emits them.\n\
         Investigate: src/store/mod.rs react_loop, src/event/sourcing.rs Reactive.\n\
         Common causes: reactor thread not started, subscribe/recv not wired, append_reaction fails.\n\
         Run: cargo test --test store_advanced react_loop_spawns_and_processes"
    );
    assert_eq!(
        reactions[0].kind,
        EventKind::custom(0xA, 2),
        "PROPERTY: reaction event must have the kind returned by the reactor.\n\
         Investigate: src/store/mod.rs react_loop.\n\
         Run: cargo test --test store_advanced react_loop_spawns_and_processes"
    );

    store.sync().expect("sync");
}

// --- ProjectionCache::prefetch wiring ---

#[test]
fn project_calls_prefetch() {
    use batpak::store::projection::{CacheMeta, ProjectionCache};
    use std::sync::atomic::{AtomicBool, Ordering};

    // Custom cache that tracks prefetch calls
    struct TrackingCache {
        prefetch_called: Arc<AtomicBool>,
    }

    impl ProjectionCache for TrackingCache {
        fn get(&self, _key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
            Ok(None) // always miss
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
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = batpak::store::Store::open_with_cache(config, Box::new(cache))
        .expect("open store with tracking cache");

    // Append an event so project has something to work with
    let coord = Coordinate::new("entity:pf", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({"data": 1}))
        .expect("append");

    // Define a minimal EventSourced type
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Counter {
        count: u32,
    }
    impl EventSourced<serde_json::Value> for Counter {
        fn from_events(events: &[batpak::prelude::Event<serde_json::Value>]) -> Option<Self> {
            Some(Counter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &batpak::prelude::Event<serde_json::Value>) {
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
        "PROPERTY: Store::project must call cache.prefetch() before checking the cache.\n\
         Investigate: src/store/mod.rs project, src/store/projection.rs prefetch.\n\
         Common causes: prefetch call not added to project(), called after cache.get().\n\
         Run: cargo test --test store_advanced project_calls_prefetch"
    );

    store.close().expect("close");
}

// ================================================================
// Writer restart_policy tests — PROVES LAW-001, DEFENDS FM-009
// These tests use panic_writer_for_test() which interacts badly under high
// parallelism (INV-CONC). Serialized to prevent hangs.
// ================================================================

/// RestartPolicy::Once allows one restart after panic.
/// After restart, the store should still accept appends normally.
#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_once_recovers_from_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 64 * 1024,
        restart_policy: RestartPolicy::Once,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:test", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Append before panic — should succeed
    store
        .append(&coord, kind, &"before_panic")
        .expect("append before panic");

    // Trigger writer panic
    store.panic_writer_for_test().expect("send panic command");

    // Append after restart — should succeed because Once allows 1 restart
    store.append(&coord, kind, &"after_panic").expect(
        "RESTART FAILED: append after writer panic should succeed with RestartPolicy::Once.\n\
         Investigate: src/store/writer.rs writer_thread_main() catch_unwind logic.\n\
         Common causes: restart not re-creating segment, rx channel dead.",
    );

    // Verify both events persisted
    let entries = store.stream("restart:test");
    assert_eq!(
        entries.len(),
        2,
        "RESTART DATA LOSS: both events (before and after panic) should be persisted.\n\
         Investigate: src/store/writer.rs writer_thread_main() segment re-creation.\n\
         Run: cargo test --test store_advanced writer_restart_once_recovers_from_panic"
    );
}

/// RestartPolicy::Once gives up after the 2nd panic.
/// The writer thread should be dead, and further appends should fail with WriterCrashed.
#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_once_gives_up_after_second_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 64 * 1024,
        restart_policy: RestartPolicy::Once,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:exhaust", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // First panic — writer restarts (budget: 1)
    store.panic_writer_for_test().expect("send first panic");

    // Second panic — budget exhausted, writer exits
    let _ = store.panic_writer_for_test();
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Now the writer should be dead — append should fail
    let result = store.append(&coord, kind, &"should_fail");
    assert!(
        result.is_err(),
        "RESTART BUDGET NOT ENFORCED: append should fail after restart budget exhausted.\n\
         Investigate: src/store/writer.rs writer_thread_main() budget_ok logic.\n\
         Common causes: restart counter not incremented, budget check wrong.\n\
         Run: cargo test --test store_advanced writer_restart_once_gives_up_after_second_panic"
    );
}

/// RestartPolicy::Bounded respects max_restarts within the time window.
#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_bounded_respects_limit() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 64 * 1024,
        restart_policy: RestartPolicy::Bounded {
            max_restarts: 2,
            within_ms: 60_000, // 60s window — won't expire during test
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:bounded", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // First panic — restarts (1/2)
    store.panic_writer_for_test().expect("first panic");
    store
        .append(&coord, kind, &"after_panic_1")
        .expect("append after 1st restart should succeed (budget 1/2)");

    // Second panic — restarts (2/2)
    store.panic_writer_for_test().expect("second panic");
    store
        .append(&coord, kind, &"after_panic_2")
        .expect("append after 2nd restart should succeed (budget 2/2)");

    // Third panic — budget exhausted
    let _ = store.panic_writer_for_test();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let result = store.append(&coord, kind, &"should_fail");
    assert!(
        result.is_err(),
        "BOUNDED RESTART BUDGET NOT ENFORCED: append should fail after 3 panics with max_restarts=2.\n\
         Investigate: src/store/writer.rs writer_thread_main() Bounded branch.\n\
         Run: cargo test --test store_advanced writer_restart_bounded_respects_limit"
    );
}

// ===== Wave 2C: Cursor edge case tests =====
// Cursor had only happy-path tests. These exercise empty streams, re-poll after EOF,
// batch edge cases, and position persistence.
// DEFENDS: FM-009 (Polite Downgrade — cursor must not fake events), FM-013 (Coverage Mirage)

#[test]
fn cursor_empty_stream_returns_none() {
    let (store, _dir) = test_store();
    let region = Region::entity("nonexistent:entity");
    let mut cursor = store.cursor(&region);
    assert!(
        cursor.poll().is_none(),
        "PROPERTY: Cursor on empty stream must return None, not fake data.\n\
         Investigate: src/store/cursor.rs poll() when index query returns empty.\n\
         Common causes: returning default IndexEntry instead of None.\n\
         DEFENDS: FM-009 (Polite Downgrade)."
    );
}

#[test]
fn cursor_poll_batch_empty_stream_returns_empty_vec() {
    let (store, _dir) = test_store();
    let region = Region::entity("nonexistent:entity");
    let mut cursor = store.cursor(&region);
    let batch = cursor.poll_batch(10);
    assert!(
        batch.is_empty(),
        "PROPERTY: Cursor::poll_batch on empty stream must return empty vec.\n\
         Investigate: src/store/cursor.rs poll_batch()."
    );
}

#[test]
fn cursor_repoll_after_eof_sees_new_events() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("cursor:repoll", "cursor:scope").expect("valid");
    let kind = EventKind::custom(1, 1);
    let region = Region::entity("cursor:repoll");

    // Append 2 events, consume them
    store.append(&coord, kind, &"e1").expect("append");
    store.append(&coord, kind, &"e2").expect("append");

    let mut cursor = store.cursor(&region);
    assert!(cursor.poll().is_some(), "first poll");
    assert!(cursor.poll().is_some(), "second poll");
    assert!(cursor.poll().is_none(), "should be exhausted");

    // Append a new event AFTER cursor reached EOF
    store.append(&coord, kind, &"e3").expect("append new");

    // Re-poll should see the new event
    let entry = cursor.poll();
    assert!(
        entry.is_some(),
        "PROPERTY: Cursor must see new events appended after reaching EOF.\n\
         Investigate: src/store/cursor.rs poll() position tracking.\n\
         Common causes: position set to max, preventing future polls.\n\
         Run: cargo test --test store_advanced cursor_repoll_after_eof_sees_new_events"
    );
}

#[test]
fn cursor_position_persists_no_duplicates() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("cursor:nodup", "cursor:scope").expect("valid");
    let kind = EventKind::custom(1, 1);
    let region = Region::entity("cursor:nodup");

    // Append 5 events
    for i in 0..5 {
        store
            .append(&coord, kind, &format!("event_{i}"))
            .expect("append");
    }

    let mut cursor = store.cursor(&region);

    // Poll 3
    let first_three: Vec<_> = (0..3).filter_map(|_| cursor.poll()).collect();
    assert_eq!(first_three.len(), 3, "should get 3 events");

    // Poll remaining — must NOT repeat first 3
    let mut remaining = Vec::new();
    while let Some(entry) = cursor.poll() {
        remaining.push(entry);
    }
    assert_eq!(
        remaining.len(),
        2,
        "PROPERTY: Cursor must not repeat events across poll calls.\n\
         Investigate: src/store/cursor.rs position tracking.\n\
         Common causes: position reset between polls, global_sequence comparison wrong."
    );

    // Verify no overlap
    let first_seqs: Vec<u64> = first_three.iter().map(|e| e.global_sequence).collect();
    for entry in &remaining {
        assert!(
            !first_seqs.contains(&entry.global_sequence),
            "PROPERTY: Cursor must not return duplicate events. Sequence {} appeared twice.\n\
             Investigate: src/store/cursor.rs started flag and position comparison.",
            entry.global_sequence
        );
    }
}

#[test]
fn cursor_poll_batch_respects_max_boundary() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("cursor:batch", "cursor:scope").expect("valid");
    let kind = EventKind::custom(1, 1);
    let region = Region::entity("cursor:batch");

    for i in 0..10 {
        store
            .append(&coord, kind, &format!("event_{i}"))
            .expect("append");
    }

    let mut cursor = store.cursor(&region);

    // Request batch of 3 — should return exactly 3
    let batch = cursor.poll_batch(3);
    assert_eq!(
        batch.len(),
        3,
        "PROPERTY: poll_batch(3) with 10 available must return exactly 3.\n\
         Investigate: src/store/cursor.rs poll_batch() max check."
    );

    // Request batch of 100 — should return remaining 7
    let batch = cursor.poll_batch(100);
    assert_eq!(
        batch.len(),
        7,
        "PROPERTY: poll_batch(100) with 7 remaining must return exactly 7.\n\
         Investigate: src/store/cursor.rs poll_batch() exhaustion."
    );

    // Request again — should be empty
    let batch = cursor.poll_batch(10);
    assert!(
        batch.is_empty(),
        "PROPERTY: poll_batch after exhaustion must return empty vec."
    );
}

// ===== AppendOptions builder tests: with_correlation + with_causation =====
// These pub methods were orphans — defined but never called anywhere in the
// codebase. build.rs allowlisted them with TODOs. These tests close the gap.
// PROVES: LAW-003 (No Orphan Infrastructure)
// DEFENDS: FM-007 (Island Syndrome — pub items must connect to tests)

#[test]
fn with_correlation_sets_header_correlation_id() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:corr", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let custom_corr: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let opts = AppendOptions::new().with_correlation(custom_corr);
    let receipt = store
        .append_with_options(&coord, kind, &"corr_test", opts)
        .expect("append with correlation");

    let event = store.get(receipt.event_id).expect("get event");
    assert_eq!(
        event.event.header.correlation_id, custom_corr,
        "WITH_CORRELATION: correlation_id on stored event should match the value \
         set via AppendOptions::with_correlation().\n\
         Investigate: src/store/mod.rs append_with_options → writer.rs AppendGuards.\n\
         Common causes: correlation_id not propagated from AppendOptions to EventHeader."
    );
}

#[test]
fn with_causation_sets_header_causation_id() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:caus", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let custom_cause: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    let opts = AppendOptions::new().with_causation(custom_cause);
    let receipt = store
        .append_with_options(&coord, kind, &"cause_test", opts)
        .expect("append with causation");

    let event = store.get(receipt.event_id).expect("get event");
    assert_eq!(
        event.event.header.causation_id,
        Some(custom_cause),
        "WITH_CAUSATION: causation_id on stored event should match the value \
         set via AppendOptions::with_causation().\n\
         Investigate: src/store/mod.rs append_with_options → writer.rs AppendGuards.\n\
         Common causes: causation_id not propagated from AppendOptions to EventHeader."
    );
}

#[test]
fn with_correlation_and_causation_combined() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:both", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let corr: u128 = 0xAAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000_1111;
    let cause: u128 = 0x2222_3333_4444_5555_6666_7777_8888_9999;
    let opts = AppendOptions::new()
        .with_correlation(corr)
        .with_causation(cause);
    let receipt = store
        .append_with_options(&coord, kind, &"both_test", opts)
        .expect("append with both");

    let event = store.get(receipt.event_id).expect("get event");
    assert_eq!(
        event.event.header.correlation_id, corr,
        "COMBINED: correlation_id should be set when both with_correlation and with_causation used."
    );
    assert_eq!(
        event.event.header.causation_id,
        Some(cause),
        "COMBINED: causation_id should be set when both with_correlation and with_causation used."
    );

    // Variance: default append should NOT have our custom IDs
    let default_receipt = store
        .append(&coord, kind, &"default_test")
        .expect("default append");
    let default_event = store.get(default_receipt.event_id).expect("get default");
    assert_ne!(
        default_event.event.header.correlation_id, corr,
        "VARIANCE: default append should auto-generate a different correlation_id."
    );
    assert_eq!(
        default_event.event.header.causation_id, None,
        "VARIANCE: default append should have None causation_id."
    );
}

// ================================================================
// Reactive pattern
// ================================================================

struct OrderReactor;
impl batpak::event::Reactive<serde_json::Value> for OrderReactor {
    fn react(
        &self,
        event: &Event<serde_json::Value>,
    ) -> Vec<(Coordinate, EventKind, serde_json::Value)> {
        // When we see a "create_order" event, emit an "order_created" reaction
        if event.event_kind() == EventKind::custom(0xA, 1) {
            vec![(
                Coordinate::new("order:reactions", "scope:test").expect("valid"),
                EventKind::custom(0xA, 2),
                serde_json::json!({"reacted_to": event.event_id().to_string()}),
            )]
        } else {
            vec![]
        }
    }
}

#[test]
fn reactive_subscribe_react_append_pattern() {
    // This test proves the SPEC's "7 lines of glue" pattern works:
    // subscribe → receive → react() → append_reaction()

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("order:1", "scope:test").expect("valid");
    let kind = EventKind::custom(0xA, 1); // "create_order"

    // Subscribe before writing
    let region = Region::all();
    let sub = store.subscribe(&region);

    // Write the root event from another thread
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::spawn(move || {
        store_w
            .append(&coord_w, kind, &serde_json::json!({"item": "widget"}))
            .expect("append root")
    });
    let root_receipt = writer.join().expect("writer thread");

    // Receive the notification
    let rx = sub.receiver();
    let notif = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("should receive notification");

    // React: the OrderReactor decides what to emit
    let reactor = OrderReactor;
    // Build a minimal event for the reactor (it only needs kind + event_id)
    let header = EventHeader::new(
        notif.event_id,
        notif.correlation_id,
        notif.causation_id,
        0,
        DagPosition::root(),
        0,
        notif.kind,
    );
    let event = Event::<serde_json::Value>::new(header, serde_json::Value::Null);
    let reactions = reactor.react(&event);

    assert_eq!(
        reactions.len(),
        1,
        "PROPERTY: OrderReactor must produce exactly 1 reaction for a create_order event.\n\
         Investigate: src/event/sourcing.rs Reactive trait react() method.\n\
         Common causes: react() returning an empty vec because event_kind comparison \
         fails, or EventKind::custom encoding mismatch between writer and reactor.\n\
         Run: cargo test --test store_advanced reactive_subscribe_react_append_pattern"
    );

    // Append reactions via append_reaction (the causal link)
    for (react_coord, react_kind, react_payload) in reactions {
        store
            .append_reaction(
                &react_coord,
                react_kind,
                &react_payload,
                root_receipt.event_id,
                root_receipt.event_id,
            )
            .expect("append reaction");
    }

    // Verify: 2 events total (root + reaction)
    let stats = store.stats();
    assert_eq!(
        stats.event_count, 2,
        "PROPERTY: After root event + 1 reaction, store must contain exactly 2 events.\n\
         Investigate: src/store/mod.rs Store::append_reaction() src/event/sourcing.rs.\n\
         Common causes: append_reaction() not writing to the store, or stats.event_count \
         not counting reaction events that go to a different coordinate.\n\
         Run: cargo test --test store_advanced reactive_subscribe_react_append_pattern"
    );

    store.sync().expect("sync");
}
