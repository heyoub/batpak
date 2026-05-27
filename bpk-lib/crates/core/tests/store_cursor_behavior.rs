//! Advanced Store cursor delivery integration tests.

mod support;
use batpak::store::Store;
use std::sync::Arc;
use support::prelude::*;
use tempfile::TempDir;

#[path = "support/small_store.rs"]
mod small_store_support;

fn test_store() -> (Store, TempDir) {
    small_store_support::small_segment_store().expect("small segment store")
}

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
                .map(|entry| entry.global_sequence())
                .collect()
        })
        .collect()
}

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
            window[0].global_sequence() < window[1].global_sequence(),
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
        first.global_sequence(),
        0,
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
    let first_seqs: Vec<u64> = first_three.iter().map(|e| e.global_sequence()).collect();
    for entry in &remaining {
        assert!(
            !first_seqs.contains(&entry.global_sequence()),
            "PROPERTY: Cursor must not return duplicate events. Sequence {} appeared twice.\n\
             Investigate: src/store/delivery/cursor.rs started flag and position comparison.",
            entry.global_sequence()
        );
    }
}
