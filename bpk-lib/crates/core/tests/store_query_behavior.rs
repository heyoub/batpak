// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; advanced store tests rely on unwrap/panic as assertion style, spawn threads for concurrency probes, and narrow bounded test data into target types that the fixture guarantees fit.
#![allow(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::needless_borrows_for_generic_args,
    clippy::panic
)]
//! Advanced Store config, lookup, and query integration tests.

mod support;
use batpak::store::{Store, StoreConfig, StoreDiagnostics, StoreError};
use support::prelude::*;
use tempfile::TempDir;

#[path = "support/small_store.rs"]
mod small_store_support;

fn test_store() -> (TempDir, Store) {
    small_store_support::small_segment_store().expect("small segment store")
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
         Run: cargo test --test store_query_behavior store_config_new_uses_sensible_defaults"
    );
    assert_eq!(
        diag.fd_budget, 64,
        "PROPERTY: StoreConfig::new() must set fd_budget to 64.\n\
         Investigate: src/store/mod.rs StoreConfig::new.\n\
         Common causes: default constant changed, fd_budget not propagated into diagnostics.\n\
         Run: cargo test --test store_query_behavior store_config_new_uses_sensible_defaults"
    );
    store.close().expect("close");
}

// --- Event not found ---

#[test]
fn get_nonexistent_returns_not_found() {
    let (_dir, store) = test_store();
    let result = store.get(batpak::id::EventId::from(0xDEADu128));
    let err = match result {
        Ok(_) => panic!(
            "PROPERTY: get() of a nonexistent event_id must return Err(StoreError::NotFound).\
             Investigate: src/store/mod.rs get, src/store/segment/scan.rs lookup."
        ),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "PROPERTY: get() on a nonexistent event_id must surface as StoreError::NotFound, got {err:?}"
    );
    store.close().expect("close");
}
// --- clock_range query filter ---

#[test]
fn query_with_clock_range_filters_events() {
    let (_dir, store) = test_store();
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
         Run: cargo test --test store_query_behavior query_with_clock_range_filters_events"
    );

    // Verify all results have clock in [3, 7]
    for entry in &results {
        assert!(
            entry.clock() >= 3 && entry.clock() <= 7,
            "PROPERTY: every result from a clock_range [3,7] query must have clock in [3,7], got {}.\n\
             Investigate: src/store/index/mod.rs query clock_range filter.\n\
             Common causes: range bounds off-by-one, filter applied before or after wrong index.\n\
             Run: cargo test --test store_query_behavior query_with_clock_range_filters_events",
            entry.clock()
        );
    }

    store.close().expect("close");
}

#[test]
fn query_clock_range_with_scope_filter() {
    let (_dir, store) = test_store();
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
         Run: cargo test --test store_query_behavior query_clock_range_with_scope_filter"
    );

    store.close().expect("close");
}

// --- Region.with_fact_category filter ---

#[test]
fn query_by_fact_category() {
    let (_dir, store) = test_store();
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
         Run: cargo test --test store_query_behavior query_by_fact_category"
    );

    store.close().expect("close");
}
