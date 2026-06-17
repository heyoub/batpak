// justifies: INV-TEST-PANIC-AS-ASSERTION, ADR-0007; this visibility-fence harness treats invariant violations as test failures; panic! is the assertion style throughout this file.
#![allow(clippy::panic)]
//! PROVES: the `VisibilityFence` lifecycle -- root/batch/reaction submissions stay
//! hidden until commit, commit preserves reaction correlation/causation metadata,
//! and cancel (explicit, on drop, or on shutdown) discards pending work and keeps
//! it invisible across reopen (INV-FENCE-CANCELLED-STAYS-HIDDEN).
//! CATCHES: drift where fenced writes leak before commit, cancelled/dropped fence
//! work survives a reopen, or reaction metadata is lost across a fenced commit.
//! SEEDED: a deterministic per-test store driven through each fence terminal path.

use batpak::coordinate::Coordinate;
use batpak::store::{
    AppendOptions, AppendReceipt, AppendTicket, BatchAppendTicket, Store, StoreError,
};
use std::time::Duration;
use tempfile::TempDir;

#[path = "support/control_plane_surface.rs"]
mod cps_support;
use cps_support::{test_config, KIND_COUNTER};

#[path = "support/bounded_writer_reply.rs"]
mod bounded_writer_reply;
use bounded_writer_reply::writer_reply;

fn wait_append_ticket(ticket: &AppendTicket, label: &str) -> Result<AppendReceipt, StoreError> {
    writer_reply(ticket.receiver(), label)
}

fn wait_batch_ticket(
    ticket: &BatchAppendTicket,
    label: &str,
) -> Result<Vec<AppendReceipt>, StoreError> {
    writer_reply(ticket.receiver(), label)
}

#[test]
fn fence_drop_without_commit_auto_cancels() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-drop", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let fenced_ticket = {
        let fence = store.begin_visibility_fence().expect("begin fence");
        // Drop the fence without calling commit() or cancel().
        // The Drop impl sends CancelVisibilityFence to the writer.
        fence
            .submit(&coord, kind, &serde_json::json!({"fenced": true}))
            .expect("fence submit")
    };

    // The ticket should resolve with VisibilityFenceCancelled because the
    // fence was implicitly cancelled on drop.
    let fenced_result = fenced_ticket
        .receiver()
        .recv_timeout(Duration::from_secs(2))
        .expect("PROPERTY: dropped VisibilityFence must auto-cancel outstanding tickets promptly");
    assert!(
        matches!(fenced_result, Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: dropping a VisibilityFence without commit or cancel must auto-cancel, \
         and any outstanding tickets must surface VisibilityFenceCancelled."
    );

    // The fenced event must NOT be visible.
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: events submitted through a dropped (auto-cancelled) fence must not be visible."
    );

    // The store must remain usable after a fence auto-cancel.
    let receipt = store
        .append(&coord, kind, &serde_json::json!({"after_drop": true}))
        .expect("append after fence drop");
    assert!(
        receipt.sequence >= 1,
        "PROPERTY: store must be usable after an auto-cancelled fence drop. \
         Got sequence {}, expected >= 1.",
        receipt.sequence
    );

    store.close().expect("close store");
}

#[test]
fn fenced_root_submit_stays_hidden_until_commit_and_cancel_discards_it() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-root", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit(&coord, kind, &serde_json::json!({"root": true}))
        .expect("submit fenced root");

    assert!(
        ticket.receiver().is_empty(),
        "PROPERTY: a root submission under a live fence must not resolve before commit."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: a root submission under a live fence must remain invisible before commit."
    );
    assert_eq!(
        store.by_entity("entity:fence-root").len(),
        0,
        "PROPERTY: the entity stream must also keep fenced root submissions hidden before commit."
    );

    fence.cancel().expect("cancel fence");
    assert!(
        matches!(wait_append_ticket(&ticket, "cancelled fence ticket"), Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: cancelling a fence after a root submission must surface VisibilityFenceCancelled."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: cancelling a fence must discard the pending root submission."
    );

    store.close().expect("close store");
    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.by_fact(kind).len(),
        0,
        "PROPERTY: a cancelled root submission under a fence must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn fenced_batch_submit_stays_hidden_until_commit_and_cancel_discards_it() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-batch", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let fence = store.begin_visibility_fence().expect("begin fence");
    let mut outbox = fence.outbox();
    outbox
        .stage_with_options(
            coord.clone(),
            kind,
            &serde_json::json!({"batch": "a"}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xAAA1)),
        )
        .expect("stage item a");
    outbox
        .stage_with_options(
            coord,
            kind,
            &serde_json::json!({"batch": "b"}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xAAA2)),
        )
        .expect("stage item b");
    let ticket = outbox.submit_flush().expect("submit fenced batch");

    assert!(
        ticket.receiver().is_empty(),
        "PROPERTY: a batch submission under a live fence must not resolve before commit."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: a batch submission under a live fence must remain invisible before commit."
    );
    assert_eq!(
        store.by_entity("entity:fence-batch").len(),
        0,
        "PROPERTY: the entity stream must also keep fenced batch submissions hidden before commit."
    );

    fence.cancel().expect("cancel fence");
    assert!(
        matches!(
            wait_batch_ticket(&ticket, "cancelled fence batch ticket"),
            Err(StoreError::VisibilityFenceCancelled)
        ),
        "PROPERTY: cancelling a fence after batch submit_flush must surface VisibilityFenceCancelled."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        0,
        "PROPERTY: cancelling a fence must discard the pending batch submission."
    );

    store.close().expect("close store");
    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.by_fact(kind).len(),
        0,
        "PROPERTY: a cancelled batch submission under a fence must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn fenced_reaction_submit_stays_hidden_until_commit_and_cancel_discards_it() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let root_coord = Coordinate::new("entity:fence-reaction-root", "scope:test").expect("coord");
    let reaction_coord =
        Coordinate::new("entity:fence-reaction-child", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let root = store
        .append(&root_coord, kind, &serde_json::json!({"root": true}))
        .expect("append root");

    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit_reaction(
            &reaction_coord,
            kind,
            &serde_json::json!({"reaction": true}),
            batpak::id::CorrelationId::from(u128::from(root.event_id)),
            batpak::id::CausationId::from(u128::from(root.event_id)),
        )
        .expect("submit fenced reaction");

    assert!(
        ticket.receiver().is_empty(),
        "PROPERTY: a reaction submission under a live fence must not resolve before commit."
    );
    assert_eq!(
        store.by_entity("entity:fence-reaction-child").len(),
        0,
        "PROPERTY: a reaction submission under a live fence must remain invisible before commit."
    );
    assert_eq!(
        store.by_fact(kind).len(),
        1,
        "PROPERTY: the unfenced root event must remain visible while the fenced reaction stays hidden."
    );

    fence.cancel().expect("cancel fence");
    assert!(
        matches!(wait_append_ticket(&ticket, "cancelled fence ticket"), Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: cancelling a fence after a reaction submission must surface VisibilityFenceCancelled."
    );
    assert_eq!(
        store.by_entity("entity:fence-reaction-child").len(),
        0,
        "PROPERTY: cancelling a fence must discard the pending reaction submission."
    );

    store.close().expect("close store");
    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.by_entity("entity:fence-reaction-child").len(),
        0,
        "PROPERTY: a cancelled reaction submission under a fence must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn fenced_reaction_commit_preserves_reaction_metadata() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let root_coord =
        Coordinate::new("entity:fence-reaction-commit-root", "scope:test").expect("coord");
    let reaction_coord =
        Coordinate::new("entity:fence-reaction-commit-child", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let root = store
        .append(&root_coord, kind, &serde_json::json!({"root": true}))
        .expect("append root");

    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit_reaction(
            &reaction_coord,
            kind,
            &serde_json::json!({"reaction": "commit"}),
            batpak::id::CorrelationId::from(u128::from(root.event_id)),
            batpak::id::CausationId::from(u128::from(root.event_id)),
        )
        .expect("submit fenced reaction");
    assert_eq!(
        store.by_entity("entity:fence-reaction-commit-child").len(),
        0,
        "PROPERTY: a fenced reaction must stay hidden until the fence commits."
    );

    fence.commit().expect("commit fence");
    let reaction =
        wait_append_ticket(&ticket, "committed fenced reaction").expect("wait committed reaction");
    let entries = store.by_entity("entity:fence-reaction-commit-child");
    assert_eq!(
        entries.len(),
        1,
        "PROPERTY: committing a fenced reaction must publish exactly one reaction entry."
    );
    let reaction_entry = &entries[0];
    assert_eq!(
        reaction_entry.event_id(),
        u128::from(reaction.event_id),
        "PROPERTY: the committed reaction receipt must identify the stored reaction event."
    );
    assert_eq!(
        reaction_entry.correlation_id(),
        u128::from(root.event_id),
        "PROPERTY: a committed fenced reaction must preserve the triggering correlation id."
    );
    assert_eq!(
        reaction_entry.causation_id(),
        Some(u128::from(root.event_id)),
        "PROPERTY: a committed fenced reaction must preserve the triggering causation id."
    );

    store.close().expect("close store");
}

#[test]
fn shutdown_with_live_fence_cancels_pending_fence_work() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");
    let coord = Coordinate::new("entity:fence-shutdown", "scope:test").expect("coord");
    let kind = KIND_COUNTER;

    let ticket = {
        let fence = store.begin_visibility_fence().expect("begin fence");
        let ticket = fence
            .submit(&coord, kind, &serde_json::json!({"fenced": "shutdown"}))
            .expect("submit fenced work");
        let _fence = std::mem::ManuallyDrop::new(fence);
        ticket
    };

    store.close().expect("close store");

    assert!(
        matches!(
            wait_append_ticket(&ticket, "cancelled fence ticket"),
            Err(StoreError::VisibilityFenceCancelled)
        ),
        "PROPERTY: shutting down with a still-live visibility fence must cancel its pending work \
         rather than silently committing or hanging."
    );

    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    assert_eq!(
        reopened.by_fact(kind).len(),
        0,
        "PROPERTY: shutdown-cancelled fence writes must stay invisible after reopen."
    );
    reopened.close().expect("close reopened");
}
