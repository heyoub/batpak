// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-FENCE-CANCELLED-STAYS-HIDDEN; tests in tests/fence_cancellation.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Visibility-fence cancellation is durable across close+reopen.
//!
//! [INV-FENCE-CANCEL-DURABLE] When a batch submitted under a visibility fence
//! is cancelled, the batch is permanently hidden: subsequent reads — even
//! after the store is closed and reopened — never observe those events. The
//! cancelled range is persisted via `hidden_ranges::write_cancelled_ranges`
//! so the reopen path reinstates the hidden-sequence set before any reader
//! can see an entry.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::{Store, StoreConfig, StoreError};
use std::time::Duration;
use tempfile::TempDir;

#[path = "support/bounded_writer_reply.rs"]
mod bounded_writer_reply;
use bounded_writer_reply::writer_reply;

const FENCE_KIND: EventKind = EventKind::custom(0xC, 1);

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1)
}

fn user_visible_entries(store: &Store) -> Vec<batpak::store::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.kind,
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect()
}

#[test]
fn cancelled_fence_hides_batch_before_and_after_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let coord = Coordinate::new("entity:fence", "scope:cancel").expect("valid coord");

    // Phase 1: open store, drop a baseline event, then open a fence and push
    // several writes under it. Cancel the fence before committing.
    {
        let store = Store::open(config(&dir)).expect("open store");
        store
            .append(&coord, FENCE_KIND, &serde_json::json!({"baseline": true}))
            .expect("append baseline");

        let fence = store
            .begin_visibility_fence()
            .expect("begin visibility fence");

        let tickets: Vec<_> = (0..3)
            .map(|i| {
                fence
                    .submit(&coord, FENCE_KIND, &serde_json::json!({"fenced": i}))
                    .expect("submit under fence")
            })
            .collect();

        fence.cancel().expect("cancel fence");

        // Every ticket submitted under a cancelled fence must resolve to
        // VisibilityFenceCancelled so the caller knows it was discarded.
        for (i, ticket) in tickets.into_iter().enumerate() {
            let outcome = writer_reply(ticket.receiver(), "writer ticket");
            assert!(
                matches!(outcome, Err(StoreError::VisibilityFenceCancelled)),
                "PROPERTY: cancelled-fence ticket #{i} must surface VisibilityFenceCancelled, got {outcome:?}"
            );
        }

        // Query while still open: only the baseline event is visible.
        let entries_open = user_visible_entries(&store);
        assert_eq!(
            entries_open.len(),
            1,
            "PROPERTY: under a cancelled fence, only the pre-fence baseline must be visible; got {}",
            entries_open.len()
        );
        assert_eq!(
            entries_open[0].coord.scope(),
            "scope:cancel",
            "baseline event must survive with its original scope"
        );

        store.close().expect("close store");
    }

    // Phase 2: reopen. The cancelled range must have been persisted to disk
    // and reinstated during cold-start, so the fenced frames that are still
    // durable on disk are never surfaced by a reader.
    {
        let store = Store::open(config(&dir)).expect("reopen store after fence cancellation");
        let entries_after_reopen = user_visible_entries(&store);
        assert_eq!(
            entries_after_reopen.len(),
            1,
            "PROPERTY: cancelled-fence writes must remain hidden across close+reopen; got {} \
             visible entries after reopen (expected exactly 1 baseline).",
            entries_after_reopen.len()
        );
        assert_eq!(
            entries_after_reopen[0].coord.scope(),
            "scope:cancel",
            "post-reopen baseline must preserve its scope"
        );

        // A subsequent append must land at the next sequence — the cancelled
        // range does not swallow new writes.
        let receipt = store
            .append(
                &coord,
                FENCE_KIND,
                &serde_json::json!({"post_reopen": true}),
            )
            .expect("append after reopen");
        let final_entries = user_visible_entries(&store);
        assert_eq!(
            final_entries.len(),
            2,
            "PROPERTY: new appends after reopen must be visible alongside the baseline; got {}",
            final_entries.len()
        );
        assert!(
            final_entries.iter().any(|e| e.event_id == receipt.event_id),
            "PROPERTY: the post-reopen append must surface by its event id"
        );

        store.close().expect("close store");
    }
}

#[test]
fn dropped_fence_auto_cancels_pending_work_and_releases_active_fence() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-drop", "scope:cancel").expect("valid coord");

    let ticket = {
        let fence = store
            .begin_visibility_fence()
            .expect("begin visibility fence");
        fence
            .submit(
                &coord,
                FENCE_KIND,
                &serde_json::json!({"drop_cancel": true}),
            )
            .expect("submit under fence")
    };

    let dropped_result = ticket
        .receiver()
        .recv_timeout(Duration::from_secs(2))
        .expect("PROPERTY: dropped visibility fence must cancel pending tickets promptly");
    assert!(
        matches!(dropped_result, Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: dropped visibility fence must resolve pending work as VisibilityFenceCancelled, got {dropped_result:?}"
    );

    let receipt = store
        .append(
            &coord,
            FENCE_KIND,
            &serde_json::json!({"after_drop_cancel": true}),
        )
        .expect("append after dropped fence");
    let visible = user_visible_entries(&store);
    assert!(
        visible.iter().any(|entry| entry.event_id == receipt.event_id),
        "PROPERTY: dropping a visibility fence must release the active fence so subsequent unfenced appends become visible"
    );

    store.close().expect("close store");
}
