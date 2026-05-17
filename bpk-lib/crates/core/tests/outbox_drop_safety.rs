// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/outbox_drop_safety.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Outbox drop safety.
//!
//! [INV-OUTBOX-DROP] Dropping an `Outbox` without calling `.flush()` leaves the
//! store untouched: nothing commits, no frames reach disk, no subscriber
//! notification fires. Flushing an outbox after staging is atomic — all items
//! land together.

use batpak::prelude::*;
use batpak::store::Outbox;

#[path = "support/small_store.rs"]
mod small_store_support;
use small_store_support::small_segment_store;

fn test_coord() -> Coordinate {
    Coordinate::new("entity:outbox", "scope:drop").expect("valid coord")
}

fn user_visible_events(store: &Store) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect()
}

#[test]
fn dropping_outbox_without_flush_commits_nothing() {
    let (store, _dir) = small_segment_store().unwrap();
    let coord = test_coord();
    let kind = EventKind::custom(0xF, 1);

    {
        let mut outbox: Outbox<'_> = store.outbox();
        outbox
            .stage(coord.clone(), kind, &serde_json::json!({"step": 0}))
            .expect("stage first item");
        outbox
            .stage(coord.clone(), kind, &serde_json::json!({"step": 1}))
            .expect("stage second item");
        let outbox_len = outbox.len();
        assert_eq!(
            outbox_len, 2,
            "PROPERTY: staged items must be visible to the local outbox view"
        );
        // Drop the outbox implicitly at scope exit without calling flush.
    }

    let events = user_visible_events(&store);
    assert!(
        events.is_empty(),
        "PROPERTY: dropping an Outbox without flush must leave the store untouched; \
         found {} visible events, expected 0",
        events.len()
    );

    // The store must remain usable — the drop did not poison the writer.
    let receipt = store
        .append(&coord, kind, &serde_json::json!({"post_drop": true}))
        .expect("append after outbox drop must succeed");
    assert_ne!(
        receipt.event_id, 0,
        "PROPERTY: append after Outbox drop must produce a non-zero event id"
    );
}

#[test]
fn flushed_outbox_lands_all_items_atomically() {
    let (store, _dir) = small_segment_store().unwrap();
    let coord = test_coord();
    let kind = EventKind::custom(0xF, 1);

    let receipts = {
        let mut outbox: Outbox<'_> = store.outbox();
        for i in 0..4 {
            outbox
                .stage(coord.clone(), kind, &serde_json::json!({"step": i}))
                .expect("stage item");
        }
        outbox.flush().expect("flush outbox")
    };

    assert_eq!(receipts.len(), 4, "flush must return one receipt per stage");

    let events = user_visible_events(&store);
    assert_eq!(
        events.len(),
        4,
        "PROPERTY: flushing a multi-item outbox must surface every staged item",
    );
}

#[test]
fn re_staging_after_flush_stays_coherent() {
    // Ensures that draining the outbox on flush leaves it clean for reuse,
    // and a subsequent drop after re-stage still commits nothing.
    let (store, _dir) = small_segment_store().unwrap();
    let coord = test_coord();
    let kind = EventKind::custom(0xF, 1);

    let mut outbox: Outbox<'_> = store.outbox();
    outbox
        .stage(coord.clone(), kind, &serde_json::json!({"initial": true}))
        .expect("stage initial");
    let first = outbox.flush().expect("flush initial");
    assert_eq!(first.len(), 1, "flush must yield one receipt");
    let outbox_len_after_flush = outbox.len();
    assert_eq!(
        outbox_len_after_flush, 0,
        "PROPERTY: flush must fully drain the outbox — len after flush was {}",
        outbox_len_after_flush
    );

    // Stage more items but do NOT flush. Drop the outbox at scope end.
    outbox
        .stage(coord.clone(), kind, &serde_json::json!({"discarded": 1}))
        .expect("re-stage after flush");
    outbox
        .stage(coord.clone(), kind, &serde_json::json!({"discarded": 2}))
        .expect("re-stage after flush");
    drop(outbox);

    // Only the initially flushed item is visible.
    let events = user_visible_events(&store);
    assert_eq!(
        events.len(),
        1,
        "PROPERTY: re-staged items dropped without flush must not commit; found {}",
        events.len()
    );
}
