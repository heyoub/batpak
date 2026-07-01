//! Stage E2 coverage for crypto-shred KEY-AWARE SUBSCRIPTION DELIVERY.
//!
//! Before E2 a syncbat subscription envelope carried the STORED payload bytes;
//! for an encrypted event those are ciphertext, so a subscriber received
//! undecryptable bytes (silent data loss). E2 decrypts at the CORE boundary
//! (keys never cross into syncbat) so the delivered envelope carries PLAINTEXT,
//! and a crypto-shredded event is SKIPPED loudly (never shipped as ciphertext)
//! while the cursor still advances — delivery ordering stays coherent.
//!
//! Gated behind `payload-encryption`; the plaintext path is covered
//! byte-identically by the ungated subscription-runtime suites.
#![cfg(feature = "payload-encryption")]

use std::sync::Arc;
use std::time::Duration;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{KeyScopeGranularity, ShredScope, Store, StoreConfig};
use flume::bounded;
use syncbat::{
    CompositeSubscriptionRuntime, EntityStreamEnvelopeV1, EventStreamEnvelopeV1, SessionDelivery,
    SessionPoll, SubscriptionId, SubscriptionRegistry, SubscriptionRoute,
    SubscriptionRuntimeConfig, SubscriptionSession, SubscriptionSessionFactory, SubscriptionStore,
};

const GRAN: KeyScopeGranularity = KeyScopeGranularity::PerEntity;
// Category 0x0A is a user (non-reserved) category, so its events ARE encrypted.
const EVENT_KIND: EventKind = EventKind::custom(0x0A, 0x01);
const CATEGORY: u8 = 0x0A;

const ENTITY_SUB: &str = "secrets.entity.v1";
const EVENT_SUB: &str = "secrets.category.v1";
const ENTITY: &str = "entity:secrets";
const SCOPE: &str = "scope:vault";
const WIRE_SCHEMA: &str = "batpak.stream-envelope.v1";

fn open_encrypted() -> Result<(Arc<Store>, tempfile::TempDir), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_payload_encryption(GRAN)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
            .with_sync_every_n_events(1),
    )?;
    Ok((Arc::new(store), dir))
}

fn append(
    store: &Store,
    entity: &str,
    payload: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let coord = Coordinate::new(entity, SCOPE)?;
    let _receipt = store
        .append(&coord, EVENT_KIND, payload)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    Ok(())
}

fn registry() -> Result<SubscriptionRegistry, Box<dyn std::error::Error>> {
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(ENTITY_SUB)
            .map_err(|e| std::io::Error::other(format!("entity sub id: {e}")))?,
        SubscriptionRoute::EntityStream {
            entity: ENTITY.to_owned(),
            scope: SCOPE.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: None,
        },
    )?;
    registry.insert(
        SubscriptionId::new(EVENT_SUB)
            .map_err(|e| std::io::Error::other(format!("event sub id: {e}")))?,
        SubscriptionRoute::EventCategory {
            category: CATEGORY,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: None,
        },
    )?;
    Ok(registry)
}

fn open_session(
    store: Arc<Store>,
    registry: &SubscriptionRegistry,
    subscription_id: &str,
) -> Result<Box<dyn SubscriptionSession>, Box<dyn std::error::Error>> {
    let (_control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(store),
        registry.clone(),
        SubscriptionRuntimeConfig::default(),
    );
    runtime
        .open_session(subscription_id, None, 128, control_rx)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })
}

fn collect(
    session: &mut dyn SubscriptionSession,
    max_steps: usize,
) -> Result<Vec<SessionDelivery>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for _ in 0..max_steps {
        match session.poll(Duration::from_millis(250))? {
            SessionPoll::Delivery(delivery) => {
                let done = matches!(
                    delivery,
                    SessionDelivery::Error(_) | SessionDelivery::End(_)
                );
                out.push(delivery);
                if done {
                    break;
                }
            }
            SessionPoll::Blocked | SessionPoll::Ended => break,
        }
    }
    Ok(out)
}

/// Decode the plaintext `note` from each delivered entity-stream envelope,
/// asserting no delivery is an error.
fn entity_notes(deliveries: &[SessionDelivery]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut notes = Vec::new();
    for delivery in deliveries {
        match delivery {
            SessionDelivery::Event(event) => {
                let envelope: EntityStreamEnvelopeV1 =
                    batpak::canonical::from_bytes(&event.envelope_bytes)?;
                let payload: serde_json::Value = batpak::encoding::from_bytes(&envelope.payload)?;
                let note = payload
                    .get("note")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("delivered entity payload missing plaintext note")?;
                notes.push(note.to_owned());
            }
            SessionDelivery::Error(_) => return Err("entity delivery faulted".into()),
            SessionDelivery::Watermark(_) | SessionDelivery::End(_) => {}
        }
    }
    Ok(notes)
}

/// Decode the plaintext `note` from each delivered event-stream envelope.
fn event_notes(deliveries: &[SessionDelivery]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut notes = Vec::new();
    for delivery in deliveries {
        match delivery {
            SessionDelivery::Event(event) => {
                let envelope: EventStreamEnvelopeV1 =
                    batpak::canonical::from_bytes(&event.envelope_bytes)?;
                let payload: serde_json::Value = batpak::encoding::from_bytes(&envelope.payload)?;
                let note = payload
                    .get("note")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("delivered event payload missing plaintext note")?;
                notes.push(note.to_owned());
            }
            SessionDelivery::Error(_) => return Err("event delivery faulted".into()),
            SessionDelivery::Watermark(_) | SessionDelivery::End(_) => {}
        }
    }
    Ok(notes)
}

#[test]
fn entity_subscription_over_encrypted_entity_delivers_decrypted(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = open_encrypted()?;
    append(&store, ENTITY, &serde_json::json!({ "note": "one" }))?;
    append(&store, ENTITY, &serde_json::json!({ "note": "two" }))?;

    let registry = registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, ENTITY_SUB)?;
    let deliveries = collect(session.as_mut(), 8)?;
    let notes = entity_notes(&deliveries)?;

    assert_eq!(
        notes,
        vec!["one".to_owned(), "two".to_owned()],
        "the entity subscription must deliver the DECRYPTED plaintext, not ciphertext"
    );
    Ok(())
}

#[test]
fn event_subscription_skips_shredded_and_stays_coherent() -> Result<(), Box<dyn std::error::Error>>
{
    let (store, _dir) = open_encrypted()?;

    // A DOOMED entity's event lands FIRST in commit order, then two LIVE events.
    append(
        &store,
        "entity:doomed",
        &serde_json::json!({ "note": "must-never-deliver" }),
    )?;
    append(
        &store,
        "entity:live",
        &serde_json::json!({ "note": "alpha" }),
    )?;
    append(
        &store,
        "entity:live",
        &serde_json::json!({ "note": "beta" }),
    )?;

    // Destroy the doomed entity's key: its event is now unrecoverable.
    let doomed = Coordinate::new("entity:doomed", SCOPE)?;
    assert!(
        store
            .shred_scope(ShredScope::Entity(&doomed))
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?,
        "shredding the doomed entity destroys its key"
    );

    let registry = registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, EVENT_SUB)?;
    let deliveries = collect(session.as_mut(), 12)?;
    let notes = event_notes(&deliveries)?;

    assert_eq!(
        notes,
        vec!["alpha".to_owned(), "beta".to_owned()],
        "the shredded head is skipped LOUDLY yet both later live events deliver decrypted, in \
         commit order — delivery stays coherent and never stalls"
    );
    assert!(
        !notes.iter().any(|n| n == "must-never-deliver"),
        "the shredded event's plaintext is NEVER delivered (and never as ciphertext)"
    );
    Ok(())
}
