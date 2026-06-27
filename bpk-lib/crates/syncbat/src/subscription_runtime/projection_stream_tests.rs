//! PROVES: S12-SUBSCRIPTION-RUNTIME-PROJECTIONS syncbat runtime engine.
//! CATCHES: catch-up/live/watermark/ACK/backpressure/cursor regressions.

use std::sync::Arc;
use std::time::Duration;

use batpak::prelude::*;
use batpak::store::Freshness;
use batpak::store::ReplayInput;
use batpak::store::{Store, StoreConfig};
use batpak_testkit::red_counters::AllCounter;
use batpak_testkit::red_kind_b::kind_b;
use batpak_testkit::red_kinds::{kind_a, payload};
use flume::bounded;
use syncbat::subscription_runtime::error::stream_code;
use syncbat::{
    CompositeSubscriptionRuntime, ProjectionStreamCursorV1, RuntimeCursor, SessionControl,
    SessionDelivery, SessionPoll, SubscriptionId, SubscriptionRegistry, SubscriptionRoute,
    SubscriptionRuntimeConfig, SubscriptionRuntimeError, SubscriptionSession,
    SubscriptionSessionFactory, SubscriptionStore, TypedProjectionProjector,
};

const SUBSCRIPTION_ID: &str = "counter.projection.v1";
const PROJECTION_ID: &str = "testkit-all-counter";
const ENTITY: &str = "entity:projection";
const WIRE_SCHEMA: &str = "batpak.projection-stream-envelope.v1";

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
        SubscriptionRoute::Projection {
            projection_id: PROJECTION_ID.to_owned(),
            entity: ENTITY.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_projection_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: None,
            projector: Arc::new(TypedProjectionProjector::<AllCounter>::new()),
        },
    )?;
    Ok(registry)
}

fn append_entity_event(
    store: &Store,
    kind: EventKind,
    payload: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let coord = Coordinate::new(ENTITY, "scope:projection")?;
    let _receipt = store
        .append(&coord, kind, payload)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    Ok(())
}

fn open_session(
    store: Arc<Store>,
    registry: &SubscriptionRegistry,
    resume_cursor: Option<&[u8]>,
    client_window: u32,
) -> Result<Box<dyn SubscriptionSession>, Box<dyn std::error::Error>> {
    let (_control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(store),
        registry.clone(),
        SubscriptionRuntimeConfig::default(),
    );
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

fn projection_generations(
    deliveries: &[SessionDelivery],
) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
    let mut generations = Vec::new();
    for delivery in deliveries {
        if let SessionDelivery::Event(event) = delivery {
            let envelope: syncbat::ProjectionStreamEnvelopeV1 =
                batpak::canonical::from_bytes(&event.envelope_bytes)?;
            generations.push(envelope.entity_generation);
        }
    }
    Ok(generations)
}

#[test]
fn subscription_runtime_projection_cursor_v1_roundtrip_and_resume_rules(
) -> Result<(), Box<dyn std::error::Error>> {
    let beginning = ProjectionStreamCursorV1::beginning(SUBSCRIPTION_ID, PROJECTION_ID, ENTITY);
    let decoded = ProjectionStreamCursorV1::decode(&beginning.encode())?;
    assert_eq!(decoded, beginning);
    assert_eq!(decoded.resume_after_entity_generation(), None);

    let after_one = ProjectionStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        PROJECTION_ID,
        ENTITY,
        1,
    );
    assert_eq!(after_one.resume_after_entity_generation(), Some(1));

    let mismatched = ProjectionStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        "other-projection",
        ENTITY,
        1,
    );
    let err = mismatched.validate_route(SUBSCRIPTION_ID, PROJECTION_ID, ENTITY);
    assert!(matches!(
        err,
        Err(SubscriptionRuntimeError::CursorMismatch { .. })
    ));
    Ok(())
}

#[test]
fn subscription_runtime_projection_registry_rejects_duplicate_subscription_id(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::Projection {
            projection_id: PROJECTION_ID.to_owned(),
            entity: ENTITY.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_projection_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: None,
            projector: Arc::new(TypedProjectionProjector::<AllCounter>::new()),
        },
    )?;
    let error = match registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::Projection {
            projection_id: PROJECTION_ID.to_owned(),
            entity: ENTITY.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_projection_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: None,
            projector: Arc::new(TypedProjectionProjector::<AllCounter>::new()),
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
fn subscription_runtime_projection_open_rejects_invalid_cursor(
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
fn subscription_runtime_projection_open_rejects_cursor_mismatch(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let mismatched = ProjectionStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        PROJECTION_ID,
        "other:entity",
        1,
    );
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
fn subscription_runtime_projection_catch_up_snapshot_and_live_update(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    for index in 0..3 {
        append_entity_event(&store, kind_a(), &payload(index))?;
    }

    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;
    let catch_up = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        catch_up
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: catch-up must deliver a materialized projection snapshot"
    );
    let envelope_bytes = catch_up
        .iter()
        .find_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.envelope_bytes.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected catch-up SUB_EVENT"))?;
    let envelope: syncbat::ProjectionStreamEnvelopeV1 =
        batpak::canonical::from_bytes(&envelope_bytes)?;
    let state: AllCounter = batpak::canonical::from_bytes(&envelope.state)?;
    assert_eq!(
        state.count, 3,
        "PROPERTY: catch-up snapshot must reflect committed entity events"
    );

    append_entity_event(&store, kind_a(), &payload(3))?;
    let live = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        live.iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: live wake must deliver newly materialized projection updates"
    );
    Ok(())
}

#[test]
fn subscription_runtime_projection_resume_honors_cursor_after_entity_generation(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    for index in 0..3 {
        append_entity_event(&store, kind_a(), &payload(index))?;
    }
    let resume = ProjectionStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        PROJECTION_ID,
        ENTITY,
        1,
    );
    let mut session = open_session(
        Arc::clone(&store),
        &test_registry()?,
        Some(&resume.encode()),
        128,
    )?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let generations = projection_generations(&deliveries)?;
    assert!(
        generations.iter().all(|gen| *gen > 1),
        "PROPERTY: resume must skip entity_generation <= 1, got {generations:?}"
    );
    Ok(())
}

#[test]
fn subscription_runtime_projection_watermark_for_empty_fold(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_entity_event(&store, kind_b(), &payload(0))?;

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new("filtered.projection.v1")?,
        SubscriptionRoute::Projection {
            projection_id: "testkit-kind-filtered-counter".to_owned(),
            entity: ENTITY.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_projection_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: None,
            projector: Arc::new(TypedProjectionProjector::<
                batpak_testkit::red_counters::KindFilteredCounter,
            >::new()),
        },
    )?;

    let (_control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(Arc::clone(&store)),
        registry,
        SubscriptionRuntimeConfig::default(),
    );
    let mut session = runtime.open_session("filtered.projection.v1", None, 128, control_rx)?;
    let deliveries = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        deliveries
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Watermark(_))),
        "PROPERTY: generation advance with empty fold must emit SUB_WATERMARK"
    );
    Ok(())
}

#[test]
fn subscription_runtime_projection_slow_consumer_closes_with_err(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_entity_event(&store, kind_a(), &payload(0))?;

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::Projection {
            projection_id: PROJECTION_ID.to_owned(),
            entity: ENTITY.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_projection_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: Some(1),
            projector: Arc::new(TypedProjectionProjector::<AllCounter>::new()),
        },
    )?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;
    let first = collect_deliveries(session.as_mut(), 1)?;
    assert!(
        first
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: slow-consumer test requires one in-flight delivery"
    );
    for index in 1..3 {
        append_entity_event(&store, kind_a(), &payload(index))?;
    }
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
fn subscription_runtime_projection_cancel_emits_client_cancelled_end(
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
fn subscription_runtime_projection_cumulative_ack_frees_delivery_window(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_entity_event(&store, kind_a(), &payload(0))?;
    append_entity_event(&store, kind_a(), &payload(1))?;

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::Projection {
            projection_id: PROJECTION_ID.to_owned(),
            entity: ENTITY.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_projection_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: Some(1),
            projector: Arc::new(TypedProjectionProjector::<AllCounter>::new()),
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
    append_entity_event(&store, kind_a(), &payload(2))?;
    let second = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        second
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: cumulative ACK must free the bounded delivery window"
    );
    Ok(())
}

#[test]
fn subscription_runtime_projection_unknown_route_rejected_at_open(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let (_control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(Arc::clone(&store)),
        SubscriptionRegistry::new(),
        SubscriptionRuntimeConfig::default(),
    );
    let error = match runtime.open_session("missing.projection.v1", None, 128, control_rx) {
        Ok(_) => {
            return Err(std::io::Error::other("PROPERTY: unknown route must be rejected").into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SubscriptionRuntimeError::UnknownSubscription { .. }
    ));
    Ok(())
}

#[test]
fn subscription_runtime_projection_replay_input_bound_is_public(
) -> Result<(), Box<dyn std::error::Error>> {
    fn assert_replay_input<T: ReplayInput>() {}
    assert_replay_input::<batpak::prelude::JsonValueInput>();
    let _ = Freshness::Consistent;
    Ok(())
}

#[test]
fn subscription_runtime_projection_runtime_cursor_is_opaque_on_wire_path(
) -> Result<(), Box<dyn std::error::Error>> {
    let cursor = ProjectionStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        PROJECTION_ID,
        ENTITY,
        4,
    );
    let runtime = RuntimeCursor::from_bytes(cursor.encode().to_vec());
    let decoded = ProjectionStreamCursorV1::decode(runtime.as_bytes())?;
    assert_eq!(decoded, cursor);
    Ok(())
}
