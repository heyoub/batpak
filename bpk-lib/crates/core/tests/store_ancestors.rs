//! Ancestry and DAG-position contract tests extracted from `store_advanced.rs`.
//!
//! PROVES: hash-chain ancestry traversal remains correct across anchor, limit,
//! middle-of-chain, zero-limit, and genesis cases.
//! DEFENDS: ancestor traversal truncation drift, descendant leakage, and
//! strict-ordering regressions in `DagPosition::is_ancestor_of`.

mod support;
use support::prelude::*;

#[path = "support/small_store.rs"]
mod small_store_support;

fn test_store() -> (tempfile::TempDir, Store) {
    small_store_support::small_segment_store().expect("small segment store")
}

#[test]
fn walk_ancestors_follows_chain() {
    let (_dir, store) = test_store();
    let coord = Coordinate::new("entity:walk", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let mut receipts = Vec::new();
    for i in 0..5 {
        let payload = serde_json::json!({"step": i});
        receipts.push(store.append(&coord, kind, &payload).expect("append"));
    }

    let last_id = receipts.last().expect("has receipts").event_id;
    let ancestors = store.walk_ancestors(last_id, 10);
    let actual_ids: Vec<_> = ancestors
        .iter()
        .map(|stored| stored.event.event_id())
        .collect();
    let expected_ids: Vec<_> = receipts
        .iter()
        .rev()
        .map(|receipt| receipt.event_id)
        .collect();

    assert!(
        ancestors.len() >= 2,
        "PROPERTY: walk_ancestors must traverse the hash chain and return at least 2 entries for a 5-event chain.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: walk stops after the anchor event, parent pointer not followed past first entry.\n\
         Run: cargo test --test store_ancestors walk_ancestors_follows_chain"
    );
    assert_eq!(
        ancestors[0].event.event_id(),
        last_id,
        "PROPERTY: walk_ancestors first result must be the starting event.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: off-by-one in initial anchor insertion, wrong field returned.\n\
         Run: cargo test --test store_ancestors walk_ancestors_follows_chain"
    );
    assert_ne!(
        ancestors[0].event.event_id(),
        ancestors[1].event.event_id(),
        "PROPERTY: walk_ancestors must return distinct events along the hash chain.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: parent-pointer not followed, same entry re-inserted in loop.\n\
         Run: cargo test --test store_ancestors walk_ancestors_follows_chain"
    );
    assert_eq!(
        actual_ids,
        expected_ids,
        "PROPERTY: walk_ancestors must return the exact ancestor chain in reverse append order.\n\
         Investigate: src/store/mod.rs walk_ancestors parent lookup.\n\
         Common causes: matching the wrong prev_hash, skipping an ancestor, or traversing descendants instead of ancestors.\n\
         Run: cargo test --test store_ancestors walk_ancestors_follows_chain"
    );

    store.close().expect("close");
}

#[test]
fn walk_ancestors_respects_limit() {
    let (_dir, store) = test_store();
    let coord = Coordinate::new("entity:limit", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        let payload = serde_json::json!({"i": i});
        store.append(&coord, kind, &payload).expect("append");
    }

    let entries = store.by_entity("entity:limit");
    let last_id = entries.last().expect("has entries").event_id();
    let ancestors = store.walk_ancestors(batpak::id::EventId::from(last_id), 2);

    assert_eq!(
        ancestors.len(),
        2,
        "PROPERTY: walk_ancestors(limit=2) on a 10-event chain must return exactly 2 entries.\n\
         Investigate: src/store/mod.rs walk_ancestors limit logic.\n\
         Common causes: limit parameter ignored, off-by-one in loop termination condition.\n\
         Run: cargo test --test store_ancestors walk_ancestors_respects_limit"
    );

    store.close().expect("close");
}

#[test]
fn walk_ancestors_from_middle_excludes_descendants() {
    let (_dir, store) = test_store();
    let coord = Coordinate::new("entity:middle", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let receipts: Vec<_> = (0..5)
        .map(|i| {
            let payload = serde_json::json!({"step": i});
            store.append(&coord, kind, &payload).expect("append")
        })
        .collect();

    let anchor = receipts[2].event_id;
    let ancestors = store.walk_ancestors(anchor, 10);
    let actual_ids: Vec<_> = ancestors
        .iter()
        .map(|stored| stored.event.event_id())
        .collect();
    let expected_ids: Vec<_> = receipts[..=2]
        .iter()
        .rev()
        .map(|receipt| receipt.event_id)
        .collect();

    assert_eq!(
        actual_ids,
        expected_ids,
        "PROPERTY: walk_ancestors from a middle event must exclude later descendants and only return the anchor plus its true ancestors.\n\
         Investigate: src/store/mod.rs walk_ancestors fallback clock filter and hash-chain traversal.\n\
         Common causes: including entries with greater clock than the anchor or following the wrong chain link.\n\
         Run: cargo test --test store_ancestors walk_ancestors_from_middle_excludes_descendants"
    );

    store.close().expect("close");
}

#[test]
fn walk_ancestors_zero_limit_returns_empty() {
    let (_dir, store) = test_store();
    let coord = Coordinate::new("entity:zero-limit", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let receipt = store
        .append(&coord, kind, &serde_json::json!({"step": 0}))
        .expect("append");
    let ancestors = store.walk_ancestors(receipt.event_id, 0);

    assert!(
        ancestors.is_empty(),
        "PROPERTY: walk_ancestors(limit=0) must return no events.\n\
         Investigate: src/store/mod.rs walk_ancestors limit guard.\n\
         Common causes: off-by-one in loop termination or ignoring the limit before reading the first ancestor.\n\
         Run: cargo test --test store_ancestors walk_ancestors_zero_limit_returns_empty"
    );

    store.close().expect("close");
}

#[test]
fn walk_ancestors_genesis_returns_single_event() {
    let (_dir, store) = test_store();
    let coord = Coordinate::new("entity:gen", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let receipt = store
        .append(&coord, kind, &serde_json::json!({"genesis": true}))
        .expect("append");
    let ancestors = store.walk_ancestors(receipt.event_id, 10);

    assert_eq!(
        ancestors.len(),
        1,
        "PROPERTY: walk_ancestors on a genesis event (first in chain) must return exactly 1 event.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Common causes: walk doesn't stop at genesis (prev_hash == [0;32]), off-by-one.\n\
         Run: cargo test --test store_ancestors walk_ancestors_genesis_returns_single_event"
    );
    assert_eq!(
        ancestors[0].event.event_id(),
        receipt.event_id,
        "PROPERTY: the single ancestor returned must be the genesis event itself.\n\
         Investigate: src/store/mod.rs walk_ancestors.\n\
         Run: cargo test --test store_ancestors walk_ancestors_genesis_returns_single_event"
    );

    store.close().expect("close");
}

#[test]
fn dag_position_different_depth_not_ancestor() {
    let pos_a = DagPosition::child_at(5, 1000, 0);
    let pos_b = DagPosition::child_at(10, 2000, 0);

    assert!(
        pos_a.is_ancestor_of(&pos_b),
        "PROPERTY: same-depth, same-lane, lower-sequence must be ancestor.\n\
         Investigate: src/coordinate/position.rs is_ancestor_of.\n\
         Run: cargo test --test store_ancestors dag_position_different_depth_not_ancestor"
    );
    assert!(
        !pos_a.is_ancestor_of(&pos_a),
        "PROPERTY: a position must NOT be its own ancestor (strict ordering).\n\
         Investigate: src/coordinate/position.rs is_ancestor_of.\n\
         Run: cargo test --test store_ancestors dag_position_different_depth_not_ancestor"
    );
}
