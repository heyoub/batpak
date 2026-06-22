//! Integration tests for the EventPayload typed API surface (ADR-0010).
//!
//! Covers every new public item introduced by the payload-binding layer:
//! EventPayload, append_typed, append_typed_with_options, submit_typed,
//! try_submit_typed, append_reaction_typed, submit_reaction_typed,
//! try_submit_reaction_typed, by_fact_typed, BatchAppendItem::typed,
//! Transition::from_payload.
//!
//! PROVES: LAW-003 (No Orphan Infrastructure), INV-TRACEABILITY-COMPLETE (every pub API has witness).
//! CATCHES: typed payload public surface drift and clean-registry validator regressions.
//! SEEDED: deterministic / no randomness.

use batpak::store::{AppendOptions, BatchAppendItem, CausationRef, Store};
use batpak::typestate::transition::{StateMarker, Transition};
use batpak_testkit::prelude::*;

use batpak_testkit::bounded_writer_reply;
use batpak_testkit::small_store as small_store_support;
use bounded_writer_reply::writer_reply;
use small_store_support::small_segment_store;

// ─── test payload type ────────────────────────────────────────────────────────
//
// Uses `#[derive(EventPayload)]` from `batpak-macros` (ADR-0010).
// This file doubles as the in-workspace path-hygiene check: the derive
// expands `::batpak::...` paths while compiling inside the batpak
// workspace itself.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct ThingHappened {
    value: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, EventPayload)]
#[batpak(category = 1, type_id = 2)]
struct OtherThingHappened {
    label: String,
}

mod left_payload_module {
    #[derive(
        Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, batpak::EventPayload,
    )]
    #[batpak(category = 1, type_id = 3)]
    pub(super) struct SharedPayloadName {
        pub value: u64,
    }
}

mod right_payload_module {
    #[derive(
        Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, batpak::EventPayload,
    )]
    #[batpak(category = 1, type_id = 4)]
    pub(super) struct SharedPayloadName {
        pub value: u64,
    }
}

// ─── typestate helpers (minimal) ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Open;
#[derive(Debug, Clone, Copy)]
struct Closed;

impl batpak::typestate::transition::sealed::Sealed for Open {}
impl batpak::typestate::transition::sealed::Sealed for Closed {}
impl StateMarker for Open {}
impl StateMarker for Closed {}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn test_store() -> (tempfile::TempDir, Store) {
    small_segment_store().expect("EventPayload surface test precondition holds")
}

fn coord() -> Coordinate {
    Coordinate::new("entity:payload-test", "scope:test")
        .expect("EventPayload surface test precondition holds")
}

#[test]
fn derive_private_registry_surface_is_available_to_test_binaries() {
    let mut seen = Vec::new();
    for item in batpak::__private::inventory::iter::<batpak::__private::EventPayloadRegistration> {
        seen.push((item.kind_bits, item.type_name));
    }

    assert!(
        seen.iter().any(|(_, name)| name.contains("ThingHappened")),
        "PROPERTY: derive-generated EventPayload registrations must be visible through batpak::__private::inventory in test binaries"
    );
    assert!(
        seen.iter()
            .any(|(_, name)| name.contains("OtherThingHappened")),
        "PROPERTY: multiple derived payload types must each contribute a registration item"
    );

    // With the two payloads in this file using distinct kind bits, the
    // shared collision scanner must be callable and must not panic.
    batpak::__private::scan_for_kind_collisions();
    batpak::__private::assert_no_kind_collisions();
}

#[test]
fn derive_registry_type_names_are_fully_qualified() {
    let expected_left = std::any::type_name::<left_payload_module::SharedPayloadName>();
    let expected_right = std::any::type_name::<right_payload_module::SharedPayloadName>();

    let seen = batpak::__private::inventory::iter::<batpak::__private::EventPayloadRegistration>
        .into_iter()
        .map(|item| item.type_name)
        .collect::<Vec<_>>();

    assert!(
        seen.contains(&expected_left),
        "PROPERTY: registry type names must match std::any::type_name for module-qualified payloads, missing {expected_left}; got {seen:?}"
    );
    assert!(
        seen.contains(&expected_right),
        "PROPERTY: registry type names must distinguish same-ident payloads in different modules, missing {expected_right}; got {seen:?}"
    );
}

// ─── append_typed ─────────────────────────────────────────────────────────────

#[test]
fn append_typed_round_trip() {
    let (_dir, store) = test_store();
    let payload = ThingHappened { value: 42 };
    let receipt = store
        .append_typed(&coord(), &payload)
        .expect("append_typed");
    assert_ne!(
        receipt.event_id,
        batpak::id::EventId::from(0u128),
        "PROPERTY: append_typed must return a non-zero event_id"
    );

    let hits = store.by_fact_typed::<ThingHappened>();
    assert_eq!(
        hits.len(),
        1,
        "PROPERTY: by_fact_typed::<ThingHappened>() must return exactly the one appended event"
    );
    assert_eq!(
        hits[0].event_id(),
        u128::from(receipt.event_id),
        "PROPERTY: by_fact_typed must return the correct event_id"
    );
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── append_typed_with_options ────────────────────────────────────────────────

#[test]
fn append_typed_with_options_idempotency() {
    let (_dir, store) = test_store();
    let payload = ThingHappened { value: 7 };
    let opts = AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xDEAD_BEEF));

    let r1 = store
        .append_typed_with_options(&coord(), &payload, opts.clone())
        .expect("first append_typed_with_options");
    let r2 = store
        .append_typed_with_options(&coord(), &payload, opts)
        .expect("idempotent second append_typed_with_options");
    assert_eq!(
        r1.event_id, r2.event_id,
        "PROPERTY: append_typed_with_options with the same idempotency key must return the same event_id"
    );
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── submit_typed ─────────────────────────────────────────────────────────────

#[test]
fn submit_typed_wait_returns_receipt() {
    let (_dir, store) = test_store();
    let payload = ThingHappened { value: 99 };
    let ticket = store
        .submit_typed(&coord(), &payload)
        .expect("submit_typed");
    let receipt = writer_reply(ticket.receiver(), "typed writer ticket").expect("ticket.wait");
    assert_ne!(
        receipt.event_id,
        batpak::id::EventId::from(0u128),
        "PROPERTY: submit_typed ticket must resolve to a non-zero event_id"
    );
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── try_submit_typed ─────────────────────────────────────────────────────────

#[test]
fn try_submit_typed_ok_path() {
    let (_dir, store) = test_store();
    let payload = ThingHappened { value: 1 };
    let outcome = store
        .try_submit_typed(&coord(), &payload)
        .expect("try_submit_typed");
    let ticket = outcome.into_result().expect("outcome is Ok");
    writer_reply(ticket.receiver(), "typed writer ticket").expect("ticket.wait");
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── append_reaction_typed ────────────────────────────────────────────────────

#[test]
fn append_reaction_typed_links_causation() {
    let (_dir, store) = test_store();
    let root = store
        .append_typed(&coord(), &ThingHappened { value: 0 })
        .expect("root append_typed");

    let reaction_coord = Coordinate::new("entity:payload-reaction", "scope:test")
        .expect("EventPayload surface test precondition holds");
    let receipt = store
        .append_reaction_typed(
            &reaction_coord,
            &OtherThingHappened {
                label: "caused".into(),
            },
            batpak::id::CorrelationId::from(u128::from(root.event_id)),
            batpak::id::CausationId::from(u128::from(root.event_id)),
        )
        .expect("append_reaction_typed");

    assert_ne!(
        receipt.event_id,
        batpak::id::EventId::from(0u128),
        "PROPERTY: append_reaction_typed must return a non-zero event_id"
    );
    let hits = store.by_fact_typed::<OtherThingHappened>();
    assert_eq!(
        hits.len(),
        1,
        "PROPERTY: by_fact_typed must find the reaction event"
    );
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── submit_reaction_typed ────────────────────────────────────────────────────

#[test]
fn submit_reaction_typed_ticket_resolves() {
    let (_dir, store) = test_store();
    let root = store
        .append_typed(&coord(), &ThingHappened { value: 0 })
        .expect("root");

    let reaction_coord = Coordinate::new("entity:payload-submit-reaction", "scope:test")
        .expect("EventPayload surface test precondition holds");
    let ticket = store
        .submit_reaction_typed(
            &reaction_coord,
            &OtherThingHappened {
                label: "submitted".into(),
            },
            batpak::id::CorrelationId::from(u128::from(root.event_id)),
            batpak::id::CausationId::from(u128::from(root.event_id)),
        )
        .expect("submit_reaction_typed");
    writer_reply(ticket.receiver(), "typed writer ticket").expect("ticket.wait");
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── try_submit_reaction_typed ────────────────────────────────────────────────

#[test]
fn try_submit_reaction_typed_ok_path() {
    let (_dir, store) = test_store();
    let root = store
        .append_typed(&coord(), &ThingHappened { value: 0 })
        .expect("root");

    let reaction_coord = Coordinate::new("entity:payload-try-reaction", "scope:test")
        .expect("EventPayload surface test precondition holds");
    let outcome = store
        .try_submit_reaction_typed(
            &reaction_coord,
            &OtherThingHappened {
                label: "try-reaction".into(),
            },
            batpak::id::CorrelationId::from(u128::from(root.event_id)),
            batpak::id::CausationId::from(u128::from(root.event_id)),
        )
        .expect("try_submit_reaction_typed");
    let ticket = outcome.into_result().expect("outcome is Ok");
    writer_reply(ticket.receiver(), "typed writer ticket").expect("ticket.wait");
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── by_fact_typed ────────────────────────────────────────────────────────────

#[test]
fn by_fact_typed_filters_by_kind() {
    let (_dir, store) = test_store();
    store
        .append_typed(&coord(), &ThingHappened { value: 1 })
        .expect("EventPayload surface test precondition holds");
    store
        .append_typed(&coord(), &ThingHappened { value: 2 })
        .expect("EventPayload surface test precondition holds");

    let other_coord = Coordinate::new("entity:other", "scope:test")
        .expect("EventPayload surface test precondition holds");
    store
        .append_typed(
            &other_coord,
            &OtherThingHappened {
                label: "noise".into(),
            },
        )
        .expect("EventPayload surface test precondition holds");

    let thing_hits = store.by_fact_typed::<ThingHappened>();
    let other_hits = store.by_fact_typed::<OtherThingHappened>();

    assert_eq!(
        thing_hits.len(),
        2,
        "PROPERTY: by_fact_typed must return only ThingHappened events"
    );
    assert_eq!(
        other_hits.len(),
        1,
        "PROPERTY: by_fact_typed must return only OtherThingHappened events"
    );
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── BatchAppendItem::typed ───────────────────────────────────────────────────

#[test]
fn batch_append_item_typed_constructor() {
    let (_dir, store) = test_store();
    let item = BatchAppendItem::typed(
        coord(),
        &ThingHappened { value: 55 },
        AppendOptions::new(),
        CausationRef::None,
    )
    .expect("BatchAppendItem::typed");

    let receipts = store.append_batch(vec![item]).expect("append_batch");
    assert_eq!(
        receipts.len(),
        1,
        "PROPERTY: batch of one typed item must produce one receipt"
    );

    let hits = store.by_fact_typed::<ThingHappened>();
    assert_eq!(
        hits.len(),
        1,
        "PROPERTY: typed batch item must produce a queryable event"
    );
    assert_eq!(
        hits[0].event_id(),
        u128::from(receipts[0].event_id),
        "PROPERTY: batch receipt event_id must match by_fact_typed result"
    );
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── Transition::from_payload ─────────────────────────────────────────────────

#[test]
fn transition_from_payload_uses_kind_constant() {
    let payload = ThingHappened { value: 77 };
    let transition: Transition<Open, Closed, ThingHappened> = Transition::from_payload(payload);
    assert_eq!(
        transition.kind(),
        ThingHappened::KIND,
        "PROPERTY: Transition::from_payload must set kind to T::KIND"
    );
    assert_eq!(
        transition.payload().value,
        77,
        "PROPERTY: Transition::from_payload must preserve the payload"
    );
}

#[test]
fn transition_from_payload_store_round_trip() {
    let (_dir, store) = test_store();
    let payload = ThingHappened { value: 13 };
    let transition: Transition<Open, Closed, ThingHappened> = Transition::from_payload(payload);

    let receipt = store
        .apply_transition(&coord(), transition)
        .expect("apply_transition with from_payload");
    assert_ne!(receipt.event_id, batpak::id::EventId::from(0u128));

    let hits = store.by_fact_typed::<ThingHappened>();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id(), u128::from(receipt.event_id));
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── Outbox::stage_typed family (Dispatch Chapter T5) ────────────────────────

#[test]
fn outbox_stage_typed_smoke() {
    let (_dir, store) = test_store();
    let mut outbox = store.outbox();
    outbox
        .stage_typed(coord(), &ThingHappened { value: 1 })
        .expect("stage_typed");
    let receipts = outbox.flush().expect("flush");
    assert_eq!(
        receipts.len(),
        1,
        "PROPERTY: stage_typed produces one receipt per staged item"
    );
    let hits = store.by_fact_typed::<ThingHappened>();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id(), u128::from(receipts[0].event_id));
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

#[test]
fn outbox_stage_typed_with_options_smoke() {
    let (_dir, store) = test_store();
    let opts = AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xDEAD_BEEF));
    let mut outbox = store.outbox();
    outbox
        .stage_typed_with_options(coord(), &ThingHappened { value: 2 }, opts)
        .expect("stage_typed_with_options");
    let receipts = outbox.flush().expect("flush");
    assert_eq!(receipts.len(), 1);
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

#[test]
fn outbox_stage_typed_with_causation_smoke() {
    let (_dir, store) = test_store();
    let root = store
        .append_typed(&coord(), &ThingHappened { value: 3 })
        .expect("root");
    let mut outbox = store.outbox();
    outbox
        .stage_typed_with_causation(
            coord(),
            &OtherThingHappened {
                label: "caused".into(),
            },
            CausationRef::Absolute(u128::from(root.event_id)),
        )
        .expect("stage_typed_with_causation");
    let receipts = outbox.flush().expect("flush");
    assert_eq!(receipts.len(), 1);
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

#[test]
fn outbox_stage_typed_with_options_and_causation_smoke() {
    let (_dir, store) = test_store();
    let root = store
        .append_typed(&coord(), &ThingHappened { value: 4 })
        .expect("root");
    let opts = AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(0xCAFE_F00D));
    let mut outbox = store.outbox();
    outbox
        .stage_typed_with_options_and_causation(
            coord(),
            &OtherThingHappened {
                label: "caused+opts".into(),
            },
            opts,
            CausationRef::Absolute(u128::from(root.event_id)),
        )
        .expect("stage_typed_with_options_and_causation");
    let receipts = outbox.flush().expect("flush");
    assert_eq!(receipts.len(), 1);
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

// ─── VisibilityFence typed submit family (Dispatch Chapter T5) ───────────────

#[test]
fn fence_submit_typed_smoke() {
    let (_dir, store) = test_store();
    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit_typed(&coord(), &ThingHappened { value: 5 })
        .expect("submit_typed");
    fence.commit().expect("commit fence");
    let receipt = writer_reply(ticket.receiver(), "typed writer ticket").expect("ticket.wait");
    assert_ne!(receipt.event_id, batpak::id::EventId::from(0u128));
    let hits = store.by_fact_typed::<ThingHappened>();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id(), u128::from(receipt.event_id));
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}

#[test]
fn fence_submit_reaction_typed_smoke() {
    let (_dir, store) = test_store();
    let root = store
        .append_typed(&coord(), &ThingHappened { value: 6 })
        .expect("root");
    let reaction_coord = Coordinate::new("entity:payload-fence-reaction", "scope:test")
        .expect("EventPayload surface test precondition holds");
    let fence = store.begin_visibility_fence().expect("begin fence");
    let ticket = fence
        .submit_reaction_typed(
            &reaction_coord,
            &OtherThingHappened {
                label: "fenced-reaction".into(),
            },
            batpak::id::CorrelationId::from(u128::from(root.event_id)),
            batpak::id::CausationId::from(u128::from(root.event_id)),
        )
        .expect("submit_reaction_typed");
    fence.commit().expect("commit fence");
    let receipt = writer_reply(ticket.receiver(), "typed writer ticket").expect("ticket.wait");
    assert_ne!(receipt.event_id, batpak::id::EventId::from(0u128));
    store
        .close()
        .expect("EventPayload surface test precondition holds");
}
