//! Outbox drop safety.
//!
//! [INV-OUTBOX-DROP-SAFETY] Dropping an `Outbox` without calling `.flush()` leaves the
//! store untouched: nothing commits, no frames reach disk, no subscriber
//! notification fires. Flushing an outbox after staging is atomic — all items
//! land together.

use batpak::store::Outbox;
use batpak_testkit::prelude::*;

use batpak_testkit::small_store as small_store_support;
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
    let (_dir, store) = small_segment_store().expect("open small-segment store");
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
        assert!(
            !outbox.is_empty(),
            "PROPERTY: Outbox::is_empty must reflect staged items before flush/drop"
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
        receipt.event_id,
        batpak::id::EventId::from(0u128),
        "PROPERTY: append after Outbox drop must produce a non-zero event id"
    );
}

#[test]
fn flushing_empty_outbox_is_a_noop_and_stays_empty() {
    let (_dir, store) = small_segment_store().expect("open small-segment store");
    let coord = test_coord();
    let kind = EventKind::custom(0xF, 1);

    let mut outbox: Outbox<'_> = store.outbox();
    assert_eq!(
        outbox.len(),
        0,
        "PROPERTY: a new Outbox starts with an empty staged buffer"
    );
    assert!(
        outbox.is_empty(),
        "PROPERTY: Outbox::is_empty must report true before any staging"
    );

    let receipts = outbox.flush().expect("flush empty outbox");
    assert!(
        receipts.is_empty(),
        "PROPERTY: flushing an empty Outbox must return no receipts"
    );
    assert_eq!(
        outbox.len(),
        0,
        "PROPERTY: empty flush must leave the Outbox staged buffer empty"
    );
    assert!(
        outbox.is_empty(),
        "PROPERTY: empty flush must preserve Outbox::is_empty"
    );
    assert!(
        user_visible_events(&store).is_empty(),
        "PROPERTY: flushing an empty Outbox must not publish any user event"
    );

    store
        .append(&coord, kind, &serde_json::json!({"post_empty_flush": true}))
        .expect("append after empty outbox flush");
}

#[test]
fn submit_flushing_empty_outbox_resolves_empty_without_visibility() {
    let (_dir, store) = small_segment_store().expect("open small-segment store");
    let mut outbox: Outbox<'_> = store.outbox();

    let ticket = outbox.submit_flush().expect("submit empty outbox");
    assert!(
        outbox.is_empty(),
        "PROPERTY: submit_flush on an empty Outbox must leave the staged buffer empty"
    );
    let receipts = ticket.wait().expect("wait for empty outbox ticket");
    assert!(
        receipts.is_empty(),
        "PROPERTY: submitted empty Outbox must resolve with no receipts"
    );
    assert!(
        user_visible_events(&store).is_empty(),
        "PROPERTY: submitted empty Outbox must not publish any user event"
    );
}

#[test]
fn flushed_outbox_lands_all_items_atomically() {
    let (_dir, store) = small_segment_store().expect("open small-segment store");
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
    let (_dir, store) = small_segment_store().expect("open small-segment store");
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
    assert!(
        outbox.is_empty(),
        "PROPERTY: Outbox::is_empty must return to true after flush drains staged items"
    );

    // Stage more items but do NOT flush. Drop the outbox at scope end.
    outbox
        .stage(coord.clone(), kind, &serde_json::json!({"discarded": 1}))
        .expect("re-stage after flush");
    outbox
        .stage(coord.clone(), kind, &serde_json::json!({"discarded": 2}))
        .expect("re-stage after flush");
    assert!(
        !outbox.is_empty(),
        "PROPERTY: Outbox::is_empty must flip back to false after re-staging"
    );
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
