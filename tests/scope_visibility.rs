// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/scope_visibility.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Scope query visibility + filter composition across index overlays.
//!
//! [INV-SCOPE-QUERY] `Region::scope(...)` returns only events whose
//! `coord.scope()` matches exactly. Combining scope with kind / clock_range
//! composes correctly (B1: filters run inside the overlay, not as a
//! post-filter). Exercised across every public overlay topology so any
//! overlay that forgets the scope gate surfaces as a fan-out delta.

use batpak::coordinate::{KindFilter, Region};
use batpak::event::EventKind;
use batpak::prelude::Coordinate;
use batpak::store::{Cursor, IndexTopology, Store, StoreConfig, StoreError};
use tempfile::TempDir;

fn topologies() -> Vec<(&'static str, IndexTopology)> {
    vec![
        ("aos", IndexTopology::aos()),
        ("scan", IndexTopology::scan()),
        ("entity-local", IndexTopology::entity_local()),
        ("tiled", IndexTopology::tiled()),
        ("tiled_simd", IndexTopology::tiled_simd()),
        ("all", IndexTopology::all()),
    ]
}

fn open_store(dir: &TempDir, topology: IndexTopology) -> Store {
    let config = StoreConfig::new(dir.path())
        .with_index_topology(topology)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1);
    Store::open(config).expect("open store")
}

const KIND_A: EventKind = EventKind::custom(0xC, 1);
const KIND_B: EventKind = EventKind::custom(0xC, 2);

fn seed(store: &Store, entity: &str, scope: &str, kind: EventKind, count: u32) {
    let coord = Coordinate::new(entity, scope).expect("valid coord");
    for i in 0..count {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
}

fn populate(store: &Store) {
    // 5 of KIND_A at (alpha, scope:A) with clocks 0..=4
    seed(store, "entity:alpha", "scope:A", KIND_A, 5);
    // 5 of KIND_B at (alpha, scope:B) with clocks 0..=4
    seed(store, "entity:alpha", "scope:B", KIND_B, 5);
    // 5 of KIND_A at (beta, scope:A) with clocks 0..=4
    seed(store, "entity:beta", "scope:A", KIND_A, 5);
}

fn strip_open_completed(entries: Vec<batpak::store::IndexEntry>) -> Vec<batpak::store::IndexEntry> {
    entries
        .into_iter()
        .filter(|entry| entry.kind != EventKind::SYSTEM_OPEN_COMPLETED)
        .collect()
}

fn run_matrix(label: &str, store: &Store) {
    // Scope-only query: must return exactly the events whose scope matches.
    let scope_a = store.query(&Region::scope("scope:A"));
    assert_eq!(
        scope_a.len(),
        10,
        "topology `{label}`: Region::scope(A) must surface all 10 scope:A events, got {}",
        scope_a.len()
    );
    for entry in &scope_a {
        assert_eq!(
            entry.coord.scope(),
            "scope:A",
            "topology `{label}`: scope query returned an event with scope={:?}; \
             the overlay's pre-filter is leaking.",
            entry.coord.scope()
        );
    }

    let scope_b = store.query(&Region::scope("scope:B"));
    assert_eq!(
        scope_b.len(),
        5,
        "topology `{label}`: Region::scope(B) must surface all 5 scope:B events, got {}",
        scope_b.len()
    );
    for entry in &scope_b {
        assert_eq!(
            entry.coord.scope(),
            "scope:B",
            "topology `{label}`: scope:B query returned scope={:?}",
            entry.coord.scope()
        );
    }

    // Scope + kind composition.
    let scope_a_kind_a =
        store.query(&Region::scope("scope:A").with_fact(KindFilter::Exact(KIND_A)));
    assert_eq!(
        scope_a_kind_a.len(),
        10,
        "topology `{label}`: scope:A + KIND_A matches every scope:A event (all KIND_A in this setup), got {}",
        scope_a_kind_a.len()
    );
    for entry in &scope_a_kind_a {
        assert_eq!(entry.kind, KIND_A);
        assert_eq!(entry.coord.scope(), "scope:A");
    }

    let scope_a_kind_b =
        store.query(&Region::scope("scope:A").with_fact(KindFilter::Exact(KIND_B)));
    assert!(
        scope_a_kind_b.is_empty(),
        "topology `{label}`: no scope:A event has KIND_B, got {} entries; filter composition leaking",
        scope_a_kind_b.len()
    );

    let scope_b_kind_b =
        store.query(&Region::scope("scope:B").with_fact(KindFilter::Exact(KIND_B)));
    assert_eq!(
        scope_b_kind_b.len(),
        5,
        "topology `{label}`: scope:B + KIND_B must match all 5 scope:B events, got {}",
        scope_b_kind_b.len()
    );

    // Scope + clock_range: (0..=2) is 3 clocks per entity-scope stream.
    // scope:A contains two streams (alpha, beta), so expect 6 entries.
    let scope_a_clocked = store.query(&Region::scope("scope:A").with_clock_range((0, 2)));
    assert_eq!(
        scope_a_clocked.len(),
        6,
        "topology `{label}`: scope:A + clocks 0..=2 across 2 streams must yield 6, got {}",
        scope_a_clocked.len()
    );
    for entry in &scope_a_clocked {
        assert_eq!(entry.coord.scope(), "scope:A");
        assert!(
            entry.clock <= 2,
            "topology `{label}`: clock_range violation — found clock={}",
            entry.clock
        );
    }

    // KindFilter::Any is a degenerate composition that must return every event
    // when combined with an "all" region. Ensures the B4 path (limit applied
    // during collection for Any) doesn't drop entries.
    let any_kind = strip_open_completed(store.query(&Region::all().with_fact(KindFilter::Any)));
    assert_eq!(
        any_kind.len(),
        15,
        "topology `{label}`: KindFilter::Any must surface every event (15), got {}",
        any_kind.len()
    );
}

#[test]
fn scope_queries_compose_across_all_topologies() {
    for (label, topology) in topologies() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_store(&dir, topology);
        populate(&store);
        run_matrix(label, &store);
        store.close().expect("close store");
    }
}

#[test]
fn bounded_scope_cursor_skips_hidden_gap_and_reaches_later_visible_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_store(&dir, IndexTopology::all());
    let coord = Coordinate::new("entity:scope-gap", "scope:gap").expect("valid coord");

    store
        .append(&coord, KIND_A, &serde_json::json!({"baseline": true}))
        .expect("append baseline");

    let fence = store
        .begin_visibility_fence()
        .expect("begin visibility fence");
    let hidden_tickets: Vec<_> = (0..3)
        .map(|i| {
            fence
                .submit(&coord, KIND_A, &serde_json::json!({"hidden": i}))
                .expect("submit hidden fenced event")
        })
        .collect();
    fence.cancel().expect("cancel visibility fence");
    for ticket in hidden_tickets {
        let err = match ticket.wait() {
            Ok(_) => panic!("PROPERTY: cancelled fence ticket must not resolve as visible success"),
            Err(err) => err,
        };
        assert!(
            matches!(err, StoreError::VisibilityFenceCancelled),
            "PROPERTY: cancelled fence work must surface VisibilityFenceCancelled, got {err:?}"
        );
    }

    let visible_after_gap = store
        .append(&coord, KIND_A, &serde_json::json!({"after_gap": true}))
        .expect("append visible event after hidden gap");

    let mut cursor: Cursor = store.cursor_guaranteed(&Region::scope("scope:gap"));

    let first = cursor.poll_batch(1);
    assert_eq!(
        first.len(),
        1,
        "PROPERTY: first bounded scope poll must return the baseline visible event"
    );
    assert_eq!(first[0].global_sequence, 1);

    let second = cursor.poll_batch(1);
    assert_eq!(
        second.len(),
        1,
        "PROPERTY: second bounded scope poll must skip the cancelled hidden gap and surface the later visible event"
    );
    assert_eq!(
        second[0].event_id,
        visible_after_gap.event_id,
        "PROPERTY: bounded scope cursor must advance past hidden entries instead of stalling on an empty batch"
    );

    store.close().expect("close store");
}
