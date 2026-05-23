// justifies: INV-TEST-PANIC-AS-ASSERTION; this integration harness uses panic!/expect to surface contract regressions with explicit messages.
#![allow(clippy::panic, clippy::unwrap_used)]

//! PROVES: INV-BIDIRECTIONAL-SUBSTRATE-LANE, INV-EXTERNAL-REPLAY-NO-SIDECAR-TRUTH.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::{Store, StoreConfig};
use tempfile::TempDir;

fn open_store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    (dir, store)
}

fn coord(entity: &str, scope: &str) -> Coordinate {
    Coordinate::new(entity, scope).expect("valid coordinate")
}

fn sequences(entries: &[batpak::store::index::IndexEntry]) -> Vec<u64> {
    entries
        .iter()
        .map(|entry| entry.global_sequence())
        .collect()
}

#[test]
fn query_entries_after_returns_ascending_global_sequence_order() {
    let (_dir, store) = open_store();
    let kind = EventKind::custom(0xA, 41);
    let a = coord("page:order:a", "scope:page");
    let b = coord("page:order:b", "scope:page");

    let first = store
        .append(&a, kind, &serde_json::json!({"n": 1}))
        .expect("append first");
    let second = store
        .append(&b, kind, &serde_json::json!({"n": 2}))
        .expect("append second");
    let third = store
        .append(&a, kind, &serde_json::json!({"n": 3}))
        .expect("append third");

    let entries = store.query_entries_after(&Region::scope("scope:page"), None, 8);

    assert_eq!(
        sequences(&entries),
        vec![first.sequence, second.sequence, third.sequence],
        "query_entries_after must expose pages in commit-order global_sequence order"
    );
}

#[test]
fn query_entries_after_resumes_strictly_after_global_sequence() {
    let (_dir, store) = open_store();
    let kind = EventKind::custom(0xA, 42);
    let entity = coord("page:resume", "scope:page");

    let first = store
        .append(&entity, kind, &serde_json::json!({"n": 1}))
        .expect("append first");
    let second = store
        .append(&entity, kind, &serde_json::json!({"n": 2}))
        .expect("append second");
    let third = store
        .append(&entity, kind, &serde_json::json!({"n": 3}))
        .expect("append third");

    let first_page = store.query_entries_after(&Region::entity("page:resume"), None, 2);
    assert_eq!(
        sequences(&first_page),
        vec![first.sequence, second.sequence]
    );

    let second_page = store.query_entries_after(
        &Region::entity("page:resume"),
        Some(first_page[1].global_sequence()),
        2,
    );
    assert_eq!(sequences(&second_page), vec![third.sequence]);
}

#[test]
fn query_entries_after_applies_region_filtering() {
    let (_dir, store) = open_store();
    let kind = EventKind::custom(0xA, 43);
    let target_a = coord("page:filter:a", "scope:target");
    let target_child = coord("page:filter:a:child", "scope:target");
    let wrong_scope = coord("page:filter:a", "scope:other");
    let wrong_entity = coord("page:filter:b", "scope:target");

    let first = store
        .append(&target_a, kind, &serde_json::json!({"n": 1}))
        .expect("append target");
    store
        .append(&wrong_scope, kind, &serde_json::json!({"n": 2}))
        .expect("append wrong scope");
    let second = store
        .append(&target_child, kind, &serde_json::json!({"n": 3}))
        .expect("append target child");
    store
        .append(&wrong_entity, kind, &serde_json::json!({"n": 4}))
        .expect("append wrong entity");

    let entries = store.query_entries_after(
        &Region::entity("page:filter:a").with_scope("scope:target"),
        None,
        8,
    );

    assert_eq!(sequences(&entries), vec![first.sequence, second.sequence]);
    assert!(
        entries
            .iter()
            .all(|entry| entry.coord().entity().starts_with("page:filter:a")
                && entry.coord().scope() == "scope:target"),
        "query_entries_after must preserve Region filtering while paging"
    );
}

#[test]
fn query_entries_after_keeps_cancelled_fence_writes_invisible() {
    let (_dir, store) = open_store();
    let kind = EventKind::custom(0xA, 44);
    let entity = coord("page:visibility", "scope:page");

    let visible_before = store
        .append(&entity, kind, &serde_json::json!({"n": 0}))
        .expect("append visible before fence");
    let fence = store.begin_visibility_fence().expect("begin fence");
    let _hidden = fence
        .submit(&entity, kind, &serde_json::json!({"n": 1}))
        .expect("submit hidden event");
    fence.cancel().expect("cancel fence");
    let visible_after = store
        .append(&entity, kind, &serde_json::json!({"n": 2}))
        .expect("append visible after fence");

    let entries = store.query_entries_after(&Region::entity("page:visibility"), None, 8);

    assert_eq!(
        sequences(&entries),
        vec![visible_before.sequence, visible_after.sequence],
        "query_entries_after must share the index visibility rules used by regular queries"
    );
}
