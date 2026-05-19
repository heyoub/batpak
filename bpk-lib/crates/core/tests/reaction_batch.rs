// justifies: INV-TEST-PANIC-AS-ASSERTION; test body in tests/reaction_batch.rs exercises precondition-holds invariants; .unwrap is acceptable in test code where a panic is a test failure.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration tests for [`ReactionBatch`]'s public push surface
//! (Dispatch Chapter T2).
//!
//! `flush` is `pub(crate)` — the typed-reactor loop (T4b) is its only legal
//! caller. Internal flush coverage (atomic multi-item commit, `PriorItem`
//! resolution, empty-batch semantics) lives in `src/store/reaction.rs::tests`
//! where crate-private access is available. These integration tests
//! exercise the public push-side contract and the drop-on-error-is-structural
//! guarantee from a downstream consumer's point of view.

use batpak::prelude::*;

#[path = "support/small_store.rs"]
mod small_store_support;
use small_store_support::small_segment_store;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, EventPayload)]
#[batpak(category = 5, type_id = 1)]
struct Reaction {
    note: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, EventPayload)]
#[batpak(category = 5, type_id = 2)]
struct FollowUp {
    after: String,
}

fn source_coord() -> Coordinate {
    Coordinate::new("entity:reaction-source", "scope:test").unwrap()
}

fn reaction_coord() -> Coordinate {
    Coordinate::new("entity:reaction-target", "scope:test").unwrap()
}

#[test]
fn push_typed_stamps_kind_and_advances_len() {
    let mut batch = ReactionBatch::default();
    assert!(batch.is_empty());

    batch
        .push_typed(
            reaction_coord(),
            &Reaction {
                note: "first".into(),
            },
            CausationRef::None,
        )
        .unwrap();
    batch
        .push_typed(
            reaction_coord(),
            &FollowUp {
                after: "first".into(),
            },
            CausationRef::PriorItem(0),
        )
        .unwrap();

    assert_eq!(
        batch.len(),
        2,
        "PROPERTY: push_typed must advance len by one per push"
    );
    assert!(!batch.is_empty());
}

#[test]
fn push_typed_with_options_accepts_append_options() {
    let mut batch = ReactionBatch::default();
    let opts = AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xFEED_BEEF));
    batch
        .push_typed_with_options(
            reaction_coord(),
            &Reaction {
                note: "opts".into(),
            },
            opts,
            CausationRef::None,
        )
        .unwrap();
    assert_eq!(batch.len(), 1);
}

#[test]
fn drop_without_flush_leaves_store_unchanged() {
    let (store, _dir) = small_segment_store().unwrap();

    // Write a root event so the store has non-zero sequence state.
    let root = store
        .append_typed(
            &source_coord(),
            &Reaction {
                note: "root".into(),
            },
        )
        .unwrap();

    let seq_before_drop = store.stats().global_sequence;

    // Stage reactions but never flush — batch drops at end of scope.
    {
        let mut batch = ReactionBatch::default();
        batch
            .push_typed(
                reaction_coord(),
                &Reaction {
                    note: "never-flushed".into(),
                },
                CausationRef::Absolute(u128::from(root.event_id)),
            )
            .unwrap();
        batch
            .push_typed(
                reaction_coord(),
                &FollowUp {
                    after: "never-flushed".into(),
                },
                CausationRef::PriorItem(0),
            )
            .unwrap();
        assert_eq!(batch.len(), 2);
        // `batch` drops here — `flush` was never called.
    }

    let seq_after_drop = store.stats().global_sequence;
    assert_eq!(
        seq_before_drop, seq_after_drop,
        "PROPERTY: dropping a ReactionBatch without flushing must not mutate the store"
    );

    // And the store's query surface sees only the root event, no reactions.
    assert_eq!(store.by_fact_typed::<Reaction>().len(), 1);
    assert_eq!(store.by_fact_typed::<FollowUp>().len(), 0);
}

#[test]
fn len_is_empty_reflect_item_count() {
    let batch = ReactionBatch::default();
    assert_eq!(batch.len(), 0);
    assert!(batch.is_empty());
}
