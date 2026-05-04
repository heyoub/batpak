// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; advanced store tests in tests/store_advanced.rs rely on unwrap/panic as assertion style, spawn threads for chaos concurrency probes, and narrow bounded test data into target types that the fixture guarantees fit.
#![allow(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::needless_borrows_for_generic_args,
    clippy::panic
)]
//! Advanced Store tests: code paths missed by store_integration.rs.
//! Covers: diagnostics, append_reaction, subscription, cursor,
//! CAS failure, idempotency, apply_transition, clock_range queries,
//! fd_budget eviction, corrupt segment recovery.
//!
//! PROVES: LAW-001 (No Fake Success), LAW-003 (No Orphan Infrastructure)
//! DEFENDS: FM-007 (Island Syndrome), FM-013 (Coverage Mirage)
//! INVARIANTS: INV-STATE (cursor state machine), INV-TEMP (temporal ordering)
//!
//! PROVES: LAW-001 (No Fake Success), LAW-003 (No Orphan Infrastructure — exercises full public API)
//! DEFENDS: FM-009 (Polite Downgrade — restart_policy wired), FM-011 (Error Path Hollowing), FM-013 (Coverage Mirage)
//! INVARIANTS: INV-CONC (CAS, idempotency), INV-TEMP (walk_ancestors, compaction), INV-PERF (fd_budget)

use batpak::event::Reactive;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, StoreDiagnostics, StoreError, StoreStats, SyncConfig};
use batpak::typestate::Transition;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tempfile::TempDir;

// Test-local EventPayload used by the apply_transition test. FREEZE-7 removed
// `Transition::new(kind, payload)`, so transitions can no longer be built from
// a raw `serde_json::Value`; the payload type must impl `EventPayload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, batpak::EventPayload)]
#[batpak(category = 0x0A, type_id = 1)]
struct PublishedDoc {
    title: String,
    from: String,
    to: String,
}

#[path = "support/small_store.rs"]
mod small_store_support;
use small_store_support::small_segment_store as test_store;

fn append_cursor_json_events(store: &Store, coord: &Coordinate, kind: EventKind, count: usize) {
    for i in 0..count {
        store
            .append(coord, kind, &serde_json::json!({ "i": i }))
            .expect("append");
    }
}

fn cursor_batch_sequences(cursor: &mut batpak::store::Cursor, requests: &[usize]) -> Vec<Vec<u64>> {
    requests
        .iter()
        .map(|max| {
            cursor
                .poll_batch(*max)
                .into_iter()
                .map(|entry| entry.global_sequence)
                .collect()
        })
        .collect()
}

// --- diagnostics ---

#[test]
fn diagnostics_reports_config() {
    let (store, dir) = test_store();
    let diag: StoreDiagnostics = store.diagnostics();
    let expected_data_dir = std::fs::canonicalize(dir.path()).expect("canonical temp dir");

    assert_eq!(
        diag.data_dir, expected_data_dir,
        "PROPERTY: diagnostics must report the opened data_dir path.\n\
         Investigate: src/store/mod.rs diagnostics.\n\
         Common causes: diagnostics returns a different field, path canonicalisation mismatch.\n\
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
        diag.event_count, 1,
        "PROPERTY: diagnostics on a freshly opened mutable store must include the SYSTEM_OPEN_COMPLETED lifecycle event.\n\
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
         Investigate: src/store/mod.rs append, src/store/segment/mod.rs write_frame.\n\
         Common causes: event_kind field not serialised, wrong frame read back.\n\
         Run: cargo test --test store_advanced append_reaction_links_causation"
    );
    assert_eq!(
        react_stored.event.event_kind(),
        kind_evt,
        "PROPERTY: reaction event must retain its EventKind (kind_evt) after storage.\n\
         Investigate: src/store/mod.rs append_reaction, src/store/segment/mod.rs write_frame.\n\
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
    let err = result.expect_err(
        "PROPERTY: append_with_options must return Err when expected_sequence is stale (CAS failure).\
         Investigate: src/store/mod.rs append_with_options CAS check.\
         Common causes: sequence comparison uses wrong field, CAS check skipped under lock."
    );
    assert!(
        matches!(err, StoreError::SequenceMismatch { .. }),
        "PROPERTY: CAS failure must surface as StoreError::SequenceMismatch, got {err:?}"
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
    let stats: StoreStats = store.stats();
    assert_eq!(
        stats.event_count, 2,
        "PROPERTY: idempotent appends must not increase event_count beyond the lifecycle event plus one stored user event.\n\
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
    let sub = store.subscribe_lossy(&region);

    // Write from another thread so recv doesn't deadlock
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::Builder::new()
        .name("store-advanced-sub-recv-writer".into())
        .spawn(move || {
            for i in 0..3 {
                store_w
                    .append(&coord_w, kind, &serde_json::json!({"i": i}))
                    .expect("append");
            }
        })
        .expect("spawn subscription recv writer thread");
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
         Investigate: src/store/delivery/subscription.rs, src/store/mod.rs writer broadcast.\n\
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
    let sub = store.subscribe_lossy(&region);

    let store_w = Arc::clone(&store);
    let writer = std::thread::Builder::new()
        .name("store-advanced-sub-filter-writer".into())
        .spawn(move || {
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
        })
        .expect("spawn subscription filter writer thread");
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
         Investigate: src/store/delivery/subscription.rs region filter, src/store/mod.rs broadcast.\n\
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

    let large_payload = "x".repeat(2_048);
    for i in 0..5 {
        store
            .append(
                &coord,
                kind,
                &serde_json::json!({"i": i, "blob": large_payload}),
            )
            .expect("append");
    }

    let region = Region::entity("entity:cur");
    let mut cursor = store.cursor_guaranteed(&region);

    let mut polled = Vec::new();
    while let Some(entry) = cursor.poll() {
        polled.push(entry);
    }

    assert_eq!(
        polled.len(),
        5,
        "PROPERTY: cursor must yield all 5 appended events when polled to exhaustion.\n\
         Investigate: src/store/delivery/cursor.rs poll.\n\
         Common causes: cursor stops at segment boundary, region filter drops valid events.\n\
         Run: cargo test --test store_advanced cursor_polls_events_in_order"
    );

    // Verify global_sequence is monotonically increasing
    for window in polled.windows(2) {
        assert!(
            window[0].global_sequence < window[1].global_sequence,
            "PROPERTY: cursor must yield events in strictly ascending global_sequence order.\n\
             Investigate: src/store/delivery/cursor.rs poll.\n\
             Common causes: cursor index not sorted on open, iterator yields unordered segments.\n\
             Run: cargo test --test store_advanced cursor_polls_events_in_order"
        );
    }

    store.close().expect("close");
}

#[test]
fn cursor_poll_batch_respects_boundaries_without_duplicates() {
    let (store, _dir) = test_store();
    let kind = EventKind::custom(0xF, 1);
    let plans: &[(&str, &[usize], &[usize])] = &[
        ("entity:batch:stepped", &[3, 3, 100, 100], &[3, 3, 4, 0]),
        ("entity:batch:boundary", &[3, 100, 10], &[3, 7, 0]),
    ];

    for (entity, requests, expected_counts) in plans {
        let coord = Coordinate::new(entity, "scope:test").expect("valid coord");
        append_cursor_json_events(&store, &coord, kind, 10);

        let mut cursor = store.cursor_guaranteed(&Region::entity(entity));
        let batch_sequences = cursor_batch_sequences(&mut cursor, requests);
        let actual_counts: Vec<usize> = batch_sequences.iter().map(Vec::len).collect();

        assert_eq!(
            actual_counts,
            *expected_counts,
            "PROPERTY: poll_batch must honor exact batch boundaries across stepped and oversized requests.\n\
             Entity: {entity}\n\
             Requests: {requests:?}\n\
             Got counts: {actual_counts:?}\n\
             Expected counts: {expected_counts:?}\n\
             Investigate: src/store/delivery/cursor.rs poll_batch.\n\
             Common causes: max parameter ignored, exhaustion not sticky, or cursor position drifts between batch calls.\n\
             Run: cargo test --test store_advanced cursor_poll_batch_respects_boundaries_without_duplicates"
        );

        let flattened: Vec<u64> = batch_sequences.into_iter().flatten().collect();
        assert_eq!(
            flattened.len(),
            10,
            "PROPERTY: poll_batch plans must drain each 10-event stream exactly once.\n\
             Entity: {entity}\n\
             Requests: {requests:?}\n\
             Drained sequences: {flattened:?}\n\
             Investigate: src/store/delivery/cursor.rs poll_batch advancement.\n\
             Run: cargo test --test store_advanced cursor_poll_batch_respects_boundaries_without_duplicates"
        );

        let unique: std::collections::HashSet<u64> = flattened.iter().copied().collect();
        assert_eq!(
            unique.len(),
            flattened.len(),
            "PROPERTY: poll_batch must never duplicate events while satisfying mixed batch plans.\n\
             Entity: {entity}\n\
             Requests: {requests:?}\n\
             Drained sequences: {flattened:?}\n\
             Investigate: src/store/delivery/cursor.rs position tracking.\n\
             Run: cargo test --test store_advanced cursor_poll_batch_respects_boundaries_without_duplicates"
        );

        for pair in flattened.windows(2) {
            assert!(
                pair[0] < pair[1],
                "PROPERTY: poll_batch must preserve strictly increasing global_sequence across batch boundaries.\n\
                 Entity: {entity}\n\
                 Requests: {requests:?}\n\
                 Drained sequences: {flattened:?}\n\
                 Investigate: src/store/delivery/cursor.rs and src/store/index/mod.rs ordering.\n\
                 Run: cargo test --test store_advanced cursor_poll_batch_respects_boundaries_without_duplicates"
            );
        }
    }

    store.close().expect("close");
}

// --- StoreConfig::new() defaults ---

#[test]
fn store_config_new_uses_sensible_defaults() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("open");
    let diag: StoreDiagnostics = store.diagnostics();
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
    let err = result.expect_err(
        "PROPERTY: get() of a nonexistent event_id must return Err(StoreError::NotFound).\
         Investigate: src/store/mod.rs get, src/store/segment/scan.rs lookup.",
    );
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "PROPERTY: get() on a nonexistent event_id must surface as StoreError::NotFound, got {err:?}"
    );
    store.close().expect("close");
}

// --- apply_transition: typestate through the store ---

batpak::define_state_machine!(document_state_seal, DocumentState { Draft, Published });

#[test]
fn apply_transition_persists_event() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:transition", "scope:test").expect("valid coord");

    // Simulate: Draft -> Published transition with a payload. FREEZE-7:
    // `Transition::from_payload` derives the event kind from `P::KIND`, so
    // the kind tested here comes from `PublishedDoc` rather than a separate
    // argument.
    let kind = <PublishedDoc as batpak::EventPayload>::KIND;
    let transition = Transition::<Draft, Published, PublishedDoc>::from_payload(PublishedDoc {
        title: "hello".into(),
        from: "draft".into(),
        to: "published".into(),
    });

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
         Investigate: src/store/index/mod.rs query clock_range filter.\n\
         Common causes: range bounds exclusive instead of inclusive, clock field misread from frame.\n\
         Run: cargo test --test store_advanced query_with_clock_range_filters_events"
    );

    // Verify all results have clock in [3, 7]
    for entry in &results {
        assert!(
            entry.clock >= 3 && entry.clock <= 7,
            "PROPERTY: every result from a clock_range [3,7] query must have clock in [3,7], got {}.\n\
             Investigate: src/store/index/mod.rs query clock_range filter.\n\
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
         Investigate: src/store/index/mod.rs query clock_range + entity filter.\n\
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
         Investigate: src/store/index/mod.rs KindFilter::Category path.\n\
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
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
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
         Investigate: src/store/write/writer.rs segment rotation logic.\n\
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
         Investigate: src/store/segment/scan.rs get_fd LRU cache, src/store/mod.rs stream.\n\
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
         Investigate: src/store/segment/scan.rs get_fd LRU cache.\n\
         Common causes: evicted segment FD reopened to wrong offset, cache key collision after eviction.\n\
         Run: cargo test --test store_advanced fd_budget_evicts_oldest_segments"
    );

    // Verify event identity integrity through eviction cycles
    assert_eq!(
        first.event.event_kind(),
        last.event.event_kind(),
        "PROPERTY: EventKind must be identical for events written with the same kind, \
         even when read from different segments after LRU eviction.\n\
         Investigate: src/store/segment/scan.rs get_fd LRU cache, src/store/segment/mod.rs read_frame.\n\
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
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
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
    // Note: `Store` doesn't implement Debug (it owns Arc'd internal state),
    // so `Result::expect_err` doesn't compile here. Match instead.
    let err = match Store::open(config) {
        Ok(_) => panic!(
            "PROPERTY: Store::open must return Err when a segment file has an \
             invalid magic header. Investigate: src/store/segment/scan.rs scan_segment \
             magic check. Common causes: magic bytes check skipped or returns \
             Ok(empty), corrupt file silently ignored."
        ),
        Err(e) => e,
    };
    assert!(
        matches!(err, StoreError::CorruptSegment { .. }),
        "PROPERTY: invalid magic header must surface as StoreError::CorruptSegment, got {err:?}"
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
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
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

    // Phase 2: corrupt a segment file by flipping bytes in the middle.
    // Sort by file_name so the chosen segment is deterministic across
    // filesystems (POSIX `readdir` makes no order guarantee).
    let mut segments: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .collect();
    segments.sort_by_key(|e| e.file_name());
    assert!(
        !segments.is_empty(),
        "PROPERTY: after appending events and syncing, at least one .fbat segment file must exist.\n\
         Investigate: src/store/write/writer.rs sync, src/store/segment/mod.rs write path.\n\
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
                stats.event_count <= 6,
                "PROPERTY: a store opened with a corrupted segment must not report more events than the original data plus lifecycle rows — no phantom events allowed. Got {}.\n\
                 Investigate: src/store/segment/scan.rs scan_segment CRC check, src/store/mod.rs open.\n\
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

// --- SubscriptionOps::map ---

#[test]
fn subscription_ops_map_transforms_notifications() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:map", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:map");

    let sub = store.subscribe_lossy(&region);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-map-writer".into())
        .spawn(move || {
            store_w
                .append(&coord_w, kind, &serde_json::json!({"v": 1}))
                .expect("append");
        })
        .expect("spawn subscription ops map writer thread")
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
    let rx_result = std::thread::Builder::new()
        .name("store-advanced-sub-ops-map-recv".into())
        .spawn(move || ops.recv())
        .expect("spawn subscription ops map recv thread")
        .join()
        .expect("join subscription ops map recv thread");

    assert!(
        rx_result.is_some(),
        "PROPERTY: SubscriptionOps::map must pass through transformed notifications.\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::map and recv.\n\
         Common causes: map_fn not applied in recv loop, map returns None.\n\
         Run: cargo test --test store_advanced subscription_ops_map_transforms_notifications"
    );
    let notif = rx_result.expect("mapped notification should be Some per preceding assert");
    assert_eq!(
        notif.kind, marker_kind,
        "PROPERTY: SubscriptionOps::map must apply the transformation function to notifications.\n\
         Investigate: src/store/delivery/subscription.rs recv map_fn application.\n\
         Common causes: map_fn ignored, original notification returned instead.\n\
         Run: cargo test --test store_advanced subscription_ops_map_transforms_notifications"
    );

    store.sync().expect("sync");
}

// --- SubscriptionOps::filter chains ---
// Intentional: inner `ops.recv()` exhaustion probes are bounded by the outer
// mpsc `recv_timeout` assertions below.

#[test]
fn subscription_ops_filter_chains_correctly() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let kind1 = EventKind::custom(0xF, 1);
    let kind2 = EventKind::custom(0xF, 2);
    let coord = Coordinate::new("entity:filt", "scope:test").expect("valid coord");
    let region = Region::entity("entity:filt");

    let sub = store.subscribe_lossy(&region);

    // Chain two filters and take(2) to prevent blocking forever:
    // first accepts kind1 only, second is always-true (AND semantics)
    let mut ops = sub
        .ops()
        .filter(move |n| n.kind == kind1)
        .filter(|_n| true)
        .take(2);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::Builder::new()
        .name("store-advanced-sub-ops-filter-writer".into())
        .spawn(move || {
            store_w
                .append(&coord_w, kind1, &serde_json::json!({"k": 1}))
                .expect("append");
            store_w
                .append(&coord_w, kind2, &serde_json::json!({"k": 2}))
                .expect("append");
            store_w
                .append(&coord_w, kind1, &serde_json::json!({"k": 3}))
                .expect("append");
        })
        .expect("spawn subscription ops filter writer thread");

    let result = [ops.recv(), ops.recv()];

    writer.join().expect("writer");

    assert_eq!(
        result.iter().flatten().count(),
        2,
        "PROPERTY: chained filter with AND semantics must pass only kind1 events (2 of 3).\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::filter, recv.\n\
         Common causes: filters not chained, last filter replaces previous.\n\
         Run: cargo test --test store_advanced subscription_ops_filter_chains_correctly"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-filter-exhausted-recv".into())
        .spawn(move || {
            let exhausted = ops.recv().is_none();
            let _ = tx.send(exhausted);
        })
        .expect("spawn exhausted subscription ops filter recv thread");
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(100))
            .expect(
                "PROPERTY: exhausted filtered SubscriptionOps::take recv must return immediately while store is open"
            ),
        "PROPERTY: exhausted filtered SubscriptionOps::take recv must return None"
    );

    store.sync().expect("sync");
}

// --- SubscriptionOps::take ---
// Intentional: inner `ops.recv()` exhaustion probes are bounded by the outer
// mpsc `recv_timeout` assertions below.

#[test]
fn subscription_ops_take_limits_count() {
    let (store, _dir) = test_store();
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:take", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:take");

    let sub = store.subscribe_lossy(&region);

    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-take-writer".into())
        .spawn(move || {
            for i in 0..5 {
                store_w
                    .append(&coord_w, kind, &serde_json::json!({"i": i}))
                    .expect("append");
            }
            drop(store_w);
        })
        .expect("spawn subscription ops take writer thread")
        .join()
        .expect("writer");

    let mut ops = sub.ops().take(3);
    let result = [ops.recv(), ops.recv(), ops.recv()];

    assert_eq!(
        result.iter().flatten().count(),
        3,
        "PROPERTY: SubscriptionOps::take(3) must return at most 3 notifications from 5 events.\n\
         Investigate: src/store/delivery/subscription.rs SubscriptionOps::take, recv count check.\n\
         Common causes: count not incremented in recv, limit check after return.\n\
         Run: cargo test --test store_advanced subscription_ops_take_limits_count"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("store-advanced-sub-ops-take-exhausted-recv".into())
        .spawn(move || {
            let exhausted = ops.recv().is_none();
            let _ = tx.send(exhausted);
        })
        .expect("spawn exhausted subscription ops take recv thread");
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(100))
            .expect(
                "PROPERTY: exhausted SubscriptionOps::take recv must return immediately while store is open"
            ),
        "PROPERTY: exhausted SubscriptionOps::take recv must return None"
    );

    store.sync().expect("sync");
}

// --- Cursor edge cases ---

#[test]
fn cursor_empty_stream_stays_empty_across_poll_and_batch_calls() {
    let (store, _dir) = test_store();
    let region = Region::entity("entity:nothing");
    let mut cursor = store.cursor_guaranteed(&region);

    assert!(
        cursor.poll().is_none(),
        "PROPERTY: cursor.poll() on an empty store must return None.\n\
         Investigate: src/store/delivery/cursor.rs poll.\n\
         Common causes: cursor starts with a non-zero position, index returns phantom entries.\n\
         Run: cargo test --test store_advanced cursor_empty_stream_stays_empty_across_poll_and_batch_calls"
    );

    let batch = cursor.poll_batch(10);
    assert!(
        batch.is_empty(),
        "PROPERTY: cursor.poll_batch() on an empty stream must return an empty Vec even after a prior empty poll().\n\
         Investigate: src/store/delivery/cursor.rs poll_batch.\n\
         Common causes: empty poll mutates cursor state, or poll_batch fabricates a stale entry.\n\
         Run: cargo test --test store_advanced cursor_empty_stream_stays_empty_across_poll_and_batch_calls"
    );

    assert!(
        cursor.poll().is_none(),
        "PROPERTY: an empty cursor must stay empty across repeated poll() calls.\n\
         Investigate: src/store/delivery/cursor.rs poll.\n\
         Common causes: empty-path state machine mutates `started`/position and fabricates later entries.\n\
         Run: cargo test --test store_advanced cursor_empty_stream_stays_empty_across_poll_and_batch_calls"
    );

    assert!(
        cursor.poll_batch(1).is_empty(),
        "PROPERTY: an empty cursor must stay empty across repeated poll_batch() calls after prior empty reads.\n\
         Investigate: src/store/delivery/cursor.rs poll_batch.\n\
         Common causes: exhaustion is not sticky, or repeated empty reads reset internal state.\n\
         Run: cargo test --test store_advanced cursor_empty_stream_stays_empty_across_poll_and_batch_calls"
    );

    store.close().expect("close");
}

#[test]
fn cursor_all_region_first_poll_includes_global_sequence_zero() {
    let (store, _dir) = test_store();
    let mut cursor = store.cursor_guaranteed(&Region::all());
    let first = cursor
        .poll()
        .expect("fresh all-region cursor must see the lifecycle open event");
    assert_eq!(
        first.global_sequence, 0,
        "PROPERTY: a fresh cursor must not skip global_sequence 0 when started=false"
    );
}

#[test]
fn cursor_sees_events_appended_after_creation() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:late", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let region = Region::entity("entity:late");

    // Create cursor BEFORE any events
    let mut cursor = store.cursor_guaranteed(&region);
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
        "PROPERTY: cursor must see events appended after cursor creation.\n\
         Investigate: src/store/delivery/cursor.rs poll_batch, position tracking.\n\
         Common causes: cursor snapshots index at creation time and never refreshes.\n\
         Run: cargo test --test store_advanced cursor_sees_events_appended_after_creation"
    );

    store.close().expect("close");
}

#[test]
fn cursor_ordered_delivery_under_load() {
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
        handles.push(
            std::thread::Builder::new()
                .name(format!("store-advanced-cursor-load-{t}"))
                .spawn(move || {
                    for i in 0..25 {
                        s.append(&c, kind, &serde_json::json!({"t": t, "i": i}))
                            .expect("append");
                    }
                })
                .expect("spawn cursor load thread"),
        );
    }
    for h in handles {
        h.join().expect("writer");
    }

    // Cursor should see all committed events in order from the index.
    let mut cursor = store.cursor_guaranteed(&region);
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
        "PROPERTY: cursor must deliver exactly {event_count} indexed events under concurrent load.\n\
         Investigate: src/store/delivery/cursor.rs poll_batch, src/store/index/mod.rs.\n\
         Common causes: index race conditions, cursor skips entries during concurrent writes.\n\
         Run: cargo test --test store_advanced cursor_ordered_delivery_under_load"
    );

    store.sync().expect("sync");
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

    let committed: Committed<serde_json::Value> =
        Pipeline::<()>::commit_bypass(bypass_receipt, |p| -> Result<_, StoreError> {
            let r = store.append(&coord, kind, &p)?;
            CommitMetadata::from_append_receipt(&r)
        })
        .expect("commit_bypass");
    let committed_event_id = committed.event_id();
    let committed_audit = committed
        .bypass_audit()
        .expect("commit_bypass should retain bypass audit");

    // Verify persisted
    let stored = store.get(committed_event_id).expect("get");
    assert_eq!(
        stored.event.event_kind(),
        kind,
        "PROPERTY: commit_bypass must persist the event through the store.\n\
         Investigate: src/pipeline/mod.rs commit_bypass.\n\
         Common causes: commit_fn not called, payload not forwarded.\n\
         Run: cargo test --test store_advanced pipeline_commit_bypass_persists"
    );
    assert_eq!(
        committed_audit.reason,
        "test-bypass",
        "PROPERTY: commit_bypass must retain the bypass audit reason alongside the persisted event."
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
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
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

    // Poll for the reactor to produce a reaction instead of sleeping a fixed duration.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let reactions = loop {
        let r = store.query(&Region::entity("entity:reactions"));
        if !r.is_empty() {
            break r;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "PROPERTY: react_loop must produce reaction events when the reactor emits them. \
                 Got nothing after 5s deadline. \
                 Investigate: src/store/mod.rs react_loop, src/event/sourcing.rs Reactive."
            );
        }
        std::thread::yield_now();
    };
    assert_eq!(
        reactions[0].kind,
        EventKind::custom(0xA, 2),
        "PROPERTY: reaction event must have the kind returned by the reactor.\n\
         Investigate: src/store/mod.rs react_loop.\n\
         Run: cargo test --test store_advanced react_loop_spawns_and_processes"
    );

    store.sync().expect("sync");
}

// ===== Wave 2C: Cursor edge case tests =====
// Cursor had only happy-path tests. These exercise empty streams, re-poll after EOF,
// batch edge cases, and position persistence.
// DEFENDS: FM-009 (Polite Downgrade — cursor must not fake events), FM-013 (Coverage Mirage)

#[test]
fn cursor_repoll_after_eof_sees_new_events() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("cursor:repoll", "cursor:scope").expect("valid");
    let kind = EventKind::custom(1, 1);
    let region = Region::entity("cursor:repoll");

    // Append 2 events, consume them
    store.append(&coord, kind, &"e1").expect("append");
    store.append(&coord, kind, &"e2").expect("append");

    let mut cursor = store.cursor_guaranteed(&region);
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
         Investigate: src/store/delivery/cursor.rs poll() position tracking.\n\
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

    let mut cursor = store.cursor_guaranteed(&region);

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
         Investigate: src/store/delivery/cursor.rs position tracking.\n\
         Common causes: position reset between polls, global_sequence comparison wrong."
    );

    // Verify no overlap
    let first_seqs: Vec<u64> = first_three.iter().map(|e| e.global_sequence).collect();
    for entry in &remaining {
        assert!(
            !first_seqs.contains(&entry.global_sequence),
            "PROPERTY: Cursor must not return duplicate events. Sequence {} appeared twice.\n\
             Investigate: src/store/delivery/cursor.rs started flag and position comparison.",
            entry.global_sequence
        );
    }
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
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
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
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
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
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
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
    // This test proves the minimal reactive wiring pattern works:
    // subscribe → receive → react() → append_reaction()

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("order:1", "scope:test").expect("valid");
    let kind = EventKind::custom(0xA, 1); // "create_order"

    // Subscribe before writing
    let region = Region::all();
    let sub = store.subscribe_lossy(&region);

    // Write the root event from another thread
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::Builder::new()
        .name("store-advanced-reactive-writer".into())
        .spawn(move || {
            store_w
                .append(&coord_w, kind, &serde_json::json!({"item": "widget"}))
                .expect("append root")
        })
        .expect("spawn reactive writer thread");
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
        stats.event_count, 3,
        "PROPERTY: After root event + 1 reaction, store must contain the lifecycle event plus those 2 user-visible events.\n\
         Investigate: src/store/mod.rs Store::append_reaction() src/event/sourcing.rs.\n\
         Common causes: append_reaction() not writing to the store, or stats.event_count \
         not counting reaction events that go to a different coordinate.\n\
         Run: cargo test --test store_advanced reactive_subscribe_react_append_pattern"
    );

    store.sync().expect("sync");
}
