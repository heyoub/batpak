//! PROVES: S12-SUBSCRIPTION-RUNTIME-EVENTS syncbat runtime engine.
//! CATCHES: replay/resume/ACK/backpressure/watermark regressions.

use std::sync::Arc;
use std::time::Duration;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use flume::bounded;
use syncbat::subscription_runtime::error::stream_code;
use syncbat::{
    EventStreamCursorV1, SessionControl, SessionDelivery, SessionPoll, SubscriptionId,
    SubscriptionRegistry, SubscriptionRoute, SubscriptionRuntimeConfig, SubscriptionRuntimeError,
    SubscriptionStore,
};

const SUBSCRIPTION_ID: &str = "orders.open.v1";
const CATEGORY: u8 = 0x0A;
const WIRE_SCHEMA: &str = "batpak.event-stream-envelope.v1";

fn test_store() -> Result<(Arc<Store>, tempfile::TempDir), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
            .with_sync_every_n_events(1),
    )?;
    Ok((Arc::new(store), dir))
}

fn test_registry() -> Result<SubscriptionRegistry, Box<dyn std::error::Error>> {
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID).map_err(|error| {
            std::io::Error::other(format!("PROPERTY: subscription id invalid: {error}"))
        })?,
        SubscriptionRoute::EventCategory {
            category: CATEGORY,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: None,
        },
    )?;
    Ok(registry)
}

fn append_category_event(
    store: &Store,
    coord: &Coordinate,
    kind: EventKind,
    payload: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let _receipt = store
        .append(coord, kind, payload)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    Ok(())
}

fn collect_deliveries(
    session: &mut syncbat::EventStreamSession,
    max_steps: usize,
) -> Result<Vec<SessionDelivery>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for _ in 0..max_steps {
        match session.poll(Duration::from_millis(10))? {
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
            SessionPoll::Blocked => break,
            SessionPoll::Ended => break,
        }
    }
    Ok(out)
}

fn event_global_sequences(
    deliveries: &[SessionDelivery],
) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
    let mut sequences = Vec::new();
    for delivery in deliveries {
        if let SessionDelivery::Event(event) = delivery {
            let envelope: syncbat::EventStreamEnvelopeV1 =
                batpak::canonical::from_bytes(&event.envelope_bytes)?;
            sequences.push(envelope.global_sequence);
        }
    }
    Ok(sequences)
}

#[test]
fn subscription_runtime_event_cursor_v1_roundtrip_and_resume_rules(
) -> Result<(), Box<dyn std::error::Error>> {
    let beginning = EventStreamCursorV1::beginning(SUBSCRIPTION_ID, CATEGORY);
    let decoded = EventStreamCursorV1::decode(&beginning.encode())?;
    assert_eq!(decoded, beginning);
    assert_eq!(decoded.resume_after_global_sequence(), None);

    let after_zero = EventStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, CATEGORY, 0, 1);
    assert_eq!(after_zero.resume_after_global_sequence(), Some(0));

    let mismatched = EventStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, 0x0B, 1, 1);
    let err = mismatched.validate_route(SUBSCRIPTION_ID, CATEGORY);
    assert!(matches!(
        err,
        Err(syncbat::SubscriptionRuntimeError::CursorMismatch { .. })
    ));
    Ok(())
}

#[test]
fn subscription_runtime_event_registry_rejects_duplicate_subscription_id(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::EventCategory {
            category: CATEGORY,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: None,
        },
    )?;
    let error = match registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::EventCategory {
            category: CATEGORY,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: None,
        },
    ) {
        Ok(()) => {
            return Err(std::io::Error::other(
                "PROPERTY: duplicate subscription id must be rejected",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SubscriptionRuntimeError::DuplicateSubscription { .. }
    ));
    Ok(())
}

#[test]
fn subscription_runtime_event_registry_rejects_invalid_event_route(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = SubscriptionRegistry::new();
    let error = match registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::EventCategory {
            category: 0x10,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: None,
        },
    ) {
        Ok(()) => {
            return Err(std::io::Error::other(
                "PROPERTY: out-of-range event category must be rejected",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SubscriptionRuntimeError::InvalidRoute {
            reason: "event category out of range"
        }
    ));
    Ok(())
}

#[test]
fn subscription_runtime_event_open_rejects_zero_runtime_limits(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let (_control_tx, control_rx) = bounded(4);
    let error = match syncbat::EventStreamSession::open(
        SubscriptionStore::new(Arc::clone(&store)),
        &test_registry()?,
        SubscriptionRuntimeConfig::new(0, 64),
        SUBSCRIPTION_ID,
        None,
        128,
        control_rx,
    ) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: zero server window must be rejected").into(),
            )
        }
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SubscriptionRuntimeError::InvalidConfig {
            reason: "server max window is zero"
        }
    ));

    let (_control_tx, control_rx) = bounded(4);
    let error = match syncbat::EventStreamSession::open(
        SubscriptionStore::new(Arc::clone(&store)),
        &test_registry()?,
        SubscriptionRuntimeConfig::new(256, 0),
        SUBSCRIPTION_ID,
        None,
        128,
        control_rx,
    ) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: zero query page size must be rejected").into(),
            )
        }
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SubscriptionRuntimeError::InvalidConfig {
            reason: "query page size is zero"
        }
    ));
    Ok(())
}

#[test]
fn subscription_runtime_event_replay_live_ack_and_watermark(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let coord = Coordinate::new("entity:orders", "scope:open")?;
    let kind = EventKind::custom(CATEGORY, 1);
    append_category_event(&store, &coord, kind, &serde_json::json!({"seq": 1}))?;
    append_category_event(&store, &coord, kind, &serde_json::json!({"seq": 2}))?;

    let (_control_tx, control_rx) = bounded(4);
    let config = SubscriptionRuntimeConfig::new(256, 64);
    let mut session = syncbat::EventStreamSession::open(
        SubscriptionStore::new(Arc::clone(&store)),
        &test_registry()?,
        config,
        SUBSCRIPTION_ID,
        None,
        128,
        control_rx,
    )?;

    let first_pass = collect_deliveries(&mut session, 8)?;
    let events: Vec<_> = first_pass
        .iter()
        .filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.delivery_index),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .collect();
    assert_eq!(events, vec![1, 2]);
    assert_eq!(
        event_global_sequences(&first_pass)?,
        vec![1, 2],
        "PROPERTY: replay deliveries must carry commit-order global sequences"
    );
    assert!(
        first_pass
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Watermark(_))),
        "PROPERTY: catch-up must emit a coalesced watermark witness"
    );

    append_category_event(&store, &coord, kind, &serde_json::json!({"seq": 3}))?;
    let second_pass = collect_deliveries(&mut session, 8)?;
    assert!(
        second_pass
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: live wake must deliver newly committed events"
    );
    Ok(())
}

#[test]
fn subscription_runtime_event_resume_honors_cursor_after_global_sequence(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let coord = Coordinate::new("entity:orders", "scope:open")?;
    let kind = EventKind::custom(CATEGORY, 1);
    append_category_event(&store, &coord, kind, &serde_json::json!({"seq": 1}))?;
    append_category_event(&store, &coord, kind, &serde_json::json!({"seq": 2}))?;
    let resume = EventStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, CATEGORY, 1, 1);

    let (_control_tx, control_rx) = bounded(4);
    let mut session = syncbat::EventStreamSession::open(
        SubscriptionStore::new(Arc::clone(&store)),
        &test_registry()?,
        SubscriptionRuntimeConfig::default(),
        SUBSCRIPTION_ID,
        Some(&resume.encode()),
        128,
        control_rx,
    )?;
    let deliveries = collect_deliveries(&mut session, 8)?;
    assert_eq!(
        event_global_sequences(&deliveries)?,
        vec![2],
        "PROPERTY: resume must skip committed global_sequence <= 1"
    );
    Ok(())
}

#[test]
fn subscription_runtime_event_slow_consumer_closes_with_err(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let coord = Coordinate::new("entity:orders", "scope:open")?;
    let kind = EventKind::custom(CATEGORY, 1);
    for index in 0..3 {
        append_category_event(&store, &coord, kind, &serde_json::json!({ "seq": index }))?;
    }

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::EventCategory {
            category: CATEGORY,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: Some(1),
        },
    )?;
    let (_control_tx, control_rx) = bounded(4);
    let mut session = syncbat::EventStreamSession::open(
        SubscriptionStore::new(Arc::clone(&store)),
        &registry,
        SubscriptionRuntimeConfig::new(256, 1),
        SUBSCRIPTION_ID,
        None,
        128,
        control_rx,
    )?;
    let deliveries = collect_deliveries(&mut session, 8)?;
    assert!(
        deliveries.iter().any(|delivery| matches!(
            delivery,
            SessionDelivery::Error(error) if error.code == stream_code::SLOW_CONSUMER
        )),
        "PROPERTY: bounded queue must fail closed with slow_consumer"
    );
    Ok(())
}

#[test]
fn subscription_runtime_event_cancel_emits_client_cancelled_end(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let (control_tx, control_rx) = bounded(4);
    let mut session = syncbat::EventStreamSession::open(
        SubscriptionStore::new(Arc::clone(&store)),
        &test_registry()?,
        SubscriptionRuntimeConfig::default(),
        SUBSCRIPTION_ID,
        None,
        128,
        control_rx,
    )?;
    control_tx
        .send(SessionControl::Cancel)
        .map_err(|_| std::io::Error::other("PROPERTY: control channel send failed"))?;
    let deliveries = collect_deliveries(&mut session, 4)?;
    assert!(deliveries.iter().any(|delivery| matches!(
        delivery,
        SessionDelivery::End(end) if end.reason_code == stream_code::CLIENT_CANCELLED
    )));
    Ok(())
}

#[test]
fn subscription_runtime_event_cumulative_ack_frees_delivery_window(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let coord = Coordinate::new("entity:orders", "scope:open")?;
    let kind = EventKind::custom(CATEGORY, 1);
    append_category_event(&store, &coord, kind, &serde_json::json!({"seq": 1}))?;
    append_category_event(&store, &coord, kind, &serde_json::json!({"seq": 2}))?;

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::EventCategory {
            category: CATEGORY,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: Some(1),
        },
    )?;
    let (control_tx, control_rx) = bounded(4);
    let mut session = syncbat::EventStreamSession::open(
        SubscriptionStore::new(Arc::clone(&store)),
        &registry,
        SubscriptionRuntimeConfig::new(256, 1),
        SUBSCRIPTION_ID,
        None,
        128,
        control_rx,
    )?;
    let first = collect_deliveries(&mut session, 1)?;
    let first_event = first
        .iter()
        .find_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected first SUB_EVENT delivery"))?;
    control_tx
        .send(SessionControl::Ack {
            delivery_index: first_event.delivery_index,
            cursor: first_event.cursor_after.clone(),
        })
        .map_err(|_| std::io::Error::other("PROPERTY: ack send failed"))?;
    let second = collect_deliveries(&mut session, 4)?;
    assert!(
        second
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: cumulative ACK must free the bounded delivery window"
    );
    Ok(())
}
