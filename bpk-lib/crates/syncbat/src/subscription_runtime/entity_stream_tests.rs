//! PROVES: S12-SUBSCRIPTION-RUNTIME-ENTITY syncbat runtime engine.
//! CATCHES: coordinate replay/live/resume/watermark/ACK/backpressure regressions.

use std::sync::Arc;
use std::time::Duration;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use flume::bounded;
use syncbat::subscription_runtime::error::stream_code;
use syncbat::{
    CompositeSubscriptionRuntime, EntityStreamCursorV1, EntityStreamEnvelopeV1, SessionControl,
    SessionDelivery, SessionPoll, SubscriptionId, SubscriptionRegistry, SubscriptionRoute,
    SubscriptionRuntimeConfig, SubscriptionRuntimeError, SubscriptionSession,
    SubscriptionSessionFactory, SubscriptionStore,
};

const SUBSCRIPTION_ID: &str = "orders.entity.v1";
const ENTITY: &str = "entity:orders";
const DESCENDANT_ENTITY: &str = "entity:orders:child";
const SCOPE: &str = "scope:open";
const OTHER_SCOPE: &str = "scope:other";
const WIRE_SCHEMA: &str = "batpak.entity-stream-envelope.v1";
const EVENT_KIND: EventKind = EventKind::custom(0x0A, 0x01);

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
        SubscriptionRoute::EntityStream {
            entity: ENTITY.to_owned(),
            scope: SCOPE.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: None,
        },
    )?;
    Ok(registry)
}

fn append_event(
    store: &Store,
    entity: &str,
    scope: &str,
    payload: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let coord = Coordinate::new(entity, scope)?;
    let _receipt = store
        .append(&coord, EVENT_KIND, payload)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    Ok(())
}

fn open_session(
    store: Arc<Store>,
    registry: &SubscriptionRegistry,
    resume_cursor: Option<&[u8]>,
    client_window: u32,
) -> Result<Box<dyn SubscriptionSession>, Box<dyn std::error::Error>> {
    open_session_with_config(
        store,
        registry,
        resume_cursor,
        client_window,
        SubscriptionRuntimeConfig::default(),
    )
}

fn open_session_with_config(
    store: Arc<Store>,
    registry: &SubscriptionRegistry,
    resume_cursor: Option<&[u8]>,
    client_window: u32,
    config: SubscriptionRuntimeConfig,
) -> Result<Box<dyn SubscriptionSession>, Box<dyn std::error::Error>> {
    let (_control_tx, control_rx) = bounded(4);
    let runtime =
        CompositeSubscriptionRuntime::new(SubscriptionStore::new(store), registry.clone(), config);
    runtime
        .open_session(SUBSCRIPTION_ID, resume_cursor, client_window, control_rx)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })
}

fn collect_deliveries(
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
            let envelope: EntityStreamEnvelopeV1 =
                batpak::canonical::from_bytes(&event.envelope_bytes)?;
            sequences.push(envelope.global_sequence);
        }
    }
    Ok(sequences)
}

#[test]
fn subscription_runtime_entity_cursor_v1_roundtrip_and_resume_rules(
) -> Result<(), Box<dyn std::error::Error>> {
    let beginning = EntityStreamCursorV1::beginning(SUBSCRIPTION_ID, ENTITY, SCOPE);
    let decoded = EntityStreamCursorV1::decode(&beginning.encode())?;
    assert_eq!(decoded, beginning);
    assert_eq!(decoded.resume_after_global_sequence(), None);

    let after_one =
        EntityStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, ENTITY, SCOPE, 1, 1);
    assert_eq!(after_one.resume_after_global_sequence(), Some(1));

    let mismatched =
        EntityStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, "other:entity", SCOPE, 1, 1);
    let err = mismatched.validate_route(SUBSCRIPTION_ID, ENTITY, SCOPE);
    assert!(matches!(
        err,
        Err(SubscriptionRuntimeError::CursorMismatch { .. })
    ));
    Ok(())
}

#[test]
fn subscription_runtime_entity_replay_delivers_exact_coordinate_events(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 1}))?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 2}))?;

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    assert_eq!(
        event_global_sequences(&deliveries)?.len(),
        2,
        "PROPERTY: replay must deliver exact coordinate events"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_live_delivery_after_append() -> Result<(), Box<dyn std::error::Error>>
{
    let (store, _dir) = test_store()?;
    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;

    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 1}))?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    assert!(
        deliveries
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: live wake must deliver newly appended entity events"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_descendant_entity_skipped_exact_entity_later_delivered(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_event(
        &store,
        DESCENDANT_ENTITY,
        SCOPE,
        &serde_json::json!({"seq": "child"}),
    )?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": "exact"}))?;

    let config = SubscriptionRuntimeConfig::new(256, 1);
    let mut session =
        open_session_with_config(Arc::clone(&store), &test_registry()?, None, 128, config)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let entities: Vec<String> = deliveries
        .iter()
        .filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => {
                let envelope: EntityStreamEnvelopeV1 =
                    batpak::canonical::from_bytes(&event.envelope_bytes).ok()?;
                Some(envelope.entity)
            }
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .collect();
    assert_eq!(
        entities.len(),
        1,
        "PROPERTY: only exact entity must deliver"
    );
    assert_eq!(
        entities[0], ENTITY,
        "PROPERTY: descendant entity events must be skipped"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_wrong_scope_skipped() -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_event(&store, ENTITY, OTHER_SCOPE, &serde_json::json!({"seq": 1}))?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 2}))?;

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    assert_eq!(
        event_global_sequences(&deliveries)?.len(),
        1,
        "PROPERTY: wrong scope events must not be delivered"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_open_rejects_invalid_cursor(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let error = match open_session(Arc::clone(&store), &test_registry()?, Some(&[0xFF; 8]), 128) {
        Ok(_) => {
            return Err(std::io::Error::other("PROPERTY: invalid cursor must be rejected").into())
        }
        Err(error) => error,
    };
    assert!(
        format!("{error}").contains("cursor"),
        "PROPERTY: invalid cursor must surface cursor failure"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_open_rejects_cursor_mismatch(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let mismatched =
        EntityStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, "other:entity", SCOPE, 1, 1);
    let error = match open_session(
        Arc::clone(&store),
        &test_registry()?,
        Some(&mismatched.encode()),
        128,
    ) {
        Ok(_) => {
            return Err(std::io::Error::other("PROPERTY: cursor mismatch must be rejected").into())
        }
        Err(error) => error,
    };
    assert!(
        format!("{error}").contains("mismatch"),
        "PROPERTY: cursor mismatch must surface mismatch failure"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_watermark_only_after_source_exhaustion(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_event(
        &store,
        DESCENDANT_ENTITY,
        SCOPE,
        &serde_json::json!({"seq": "child"}),
    )?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": "exact"}))?;

    let config = SubscriptionRuntimeConfig::new(256, 1);
    let mut session =
        open_session_with_config(Arc::clone(&store), &test_registry()?, None, 128, config)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let first_event = deliveries
        .iter()
        .position(|d| matches!(d, SessionDelivery::Event(_)));
    let first_watermark = deliveries
        .iter()
        .position(|d| matches!(d, SessionDelivery::Watermark(_)));
    let event_index = first_event
        .ok_or_else(|| std::io::Error::other("PROPERTY: exact entity event must be delivered"))?;
    let watermark_index = first_watermark.ok_or_else(|| {
        std::io::Error::other("PROPERTY: watermark must follow source exhaustion")
    })?;
    assert!(
        event_index < watermark_index,
        "PROPERTY: watermark must not precede a later matching entity event"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_cumulative_ack_frees_delivery_window(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 1}))?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 2}))?;

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::EntityStream {
            entity: ENTITY.to_owned(),
            scope: SCOPE.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: Some(1),
        },
    )?;
    let (control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(Arc::clone(&store)),
        registry,
        SubscriptionRuntimeConfig::default(),
    );
    let mut session = runtime.open_session(SUBSCRIPTION_ID, None, 128, control_rx)?;
    let first = collect_deliveries(session.as_mut(), 1)?;
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
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 3}))?;
    let second = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        second
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: cumulative ACK must free delivery window for follow-up events"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_resume_after_cursor_does_not_redeliver_acknowledged_event(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 1}))?;
    append_event(&store, ENTITY, SCOPE, &serde_json::json!({"seq": 2}))?;
    let resume = EntityStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, ENTITY, SCOPE, 1, 1);

    let mut session = open_session(
        Arc::clone(&store),
        &test_registry()?,
        Some(&resume.encode()),
        128,
    )?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    assert_eq!(
        event_global_sequences(&deliveries)?,
        vec![2],
        "PROPERTY: resume must skip acknowledged global_sequence <= 1"
    );
    Ok(())
}

#[test]
fn subscription_runtime_entity_slow_consumer_closes_with_err(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    for index in 0..3 {
        append_event(&store, ENTITY, SCOPE, &serde_json::json!({ "seq": index }))?;
    }

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::EntityStream {
            entity: ENTITY.to_owned(),
            scope: SCOPE.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_event_payload_schema_ref: None,
            backpressure_capacity: Some(1),
        },
    )?;
    let mut session = open_session_with_config(
        Arc::clone(&store),
        &registry,
        None,
        128,
        SubscriptionRuntimeConfig::new(256, 1),
    )?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
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
fn subscription_runtime_entity_cancel_emits_client_cancelled(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let (control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(Arc::clone(&store)),
        test_registry()?,
        SubscriptionRuntimeConfig::default(),
    );
    let mut session = runtime.open_session(SUBSCRIPTION_ID, None, 128, control_rx)?;
    control_tx
        .send(SessionControl::Cancel)
        .map_err(|_| std::io::Error::other("PROPERTY: control channel send failed"))?;
    let deliveries = collect_deliveries(session.as_mut(), 4)?;
    assert!(deliveries.iter().any(|delivery| matches!(
        delivery,
        SessionDelivery::End(end) if end.reason_code == stream_code::CLIENT_CANCELLED
    )));
    Ok(())
}

#[test]
fn subscription_runtime_entity_disconnect_without_cancel_ends_session(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let (control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(Arc::clone(&store)),
        test_registry()?,
        SubscriptionRuntimeConfig::default(),
    );
    let mut session = runtime.open_session(SUBSCRIPTION_ID, None, 128, control_rx)?;
    control_tx
        .send(SessionControl::Disconnected)
        .map_err(|_| std::io::Error::other("PROPERTY: control channel send failed"))?;
    let deliveries = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        deliveries.is_empty(),
        "PROPERTY: disconnect without cancel must end session without semantic terminal frame"
    );
    Ok(())
}
