//! PROVES: S12-SUBSCRIPTION-RUNTIME-OPERATION-STATUS syncbat runtime engine.
//! CATCHES: catch-up/live/watermark/ACK/backpressure/cursor/checkout regressions.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use batpak::prelude::*;
use batpak::store::Freshness;
use batpak::store::{Store, StoreConfig};
use flume::bounded;
use syncbat::operation_status::{
    OperationStatusFactV1, OperationStatusLifecycle, OperationStatusView,
};
use syncbat::operation_status_sink::{operation_status_entity, StoreOperationStatusSink};
use syncbat::subscription_runtime::error::stream_code;
use syncbat::{
    CompositeSubscriptionRuntime, Core, Ctx, EffectClass, Handler, HandlerResult, OperationName,
    OperationStatusSink, OperationStatusSinkError, OperationStatusStreamCursorV1,
    OperationStatusStreamEnvelopeV1, ReceiptHashPolicy, RuntimeCursor, SessionControl,
    SessionDelivery, SessionPoll, SubscriptionId, SubscriptionRegistry, SubscriptionRoute,
    SubscriptionRuntimeConfig, SubscriptionRuntimeError, SubscriptionSession,
    SubscriptionSessionFactory, SubscriptionStore, SYNCBAT_RECEIPT_EVENT_KIND,
};

const SUBSCRIPTION_ID: &str = "echo.status.v1";
const OPERATION: &str = "mod.a.echo";
const WIRE_SCHEMA: &str = "batpak.operation-status-stream-envelope.v1";

struct EchoHandler;

impl Handler for EchoHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        Ok(input.to_vec())
    }
}

struct FailHandler;

impl Handler for FailHandler {
    fn handle(&mut self, _input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        Err(syncbat::HandlerError::failed("handler failed"))
    }
}

struct FailingStatusSink;

impl OperationStatusSink for FailingStatusSink {
    fn record_fact(&self, _fact: &OperationStatusFactV1) -> Result<(), OperationStatusSinkError> {
        Err(OperationStatusSinkError::new("status sink offline"))
    }
}

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

fn status_entity() -> Result<String, Box<dyn std::error::Error>> {
    Ok(operation_status_entity(OPERATION)?)
}

fn test_registry() -> Result<SubscriptionRegistry, Box<dyn std::error::Error>> {
    let operation = OperationName::new(OPERATION).map_err(|error| {
        std::io::Error::other(format!("PROPERTY: operation name invalid: {error}"))
    })?;
    let entity = status_entity()?;
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::OperationStatus {
            operation,
            entity,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_status_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: None,
        },
    )?;
    Ok(registry)
}

fn append_status_fact(
    store: &Store,
    fact: &OperationStatusFactV1,
) -> Result<(), Box<dyn std::error::Error>> {
    let entity = status_entity()?;
    let coord = Coordinate::new(&entity, "scope:operation-status")?;
    let _receipt = store
        .append_typed(&coord, fact)
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

fn status_generations(
    deliveries: &[SessionDelivery],
) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
    let mut generations = Vec::new();
    for delivery in deliveries {
        if let SessionDelivery::Event(event) = delivery {
            let envelope: OperationStatusStreamEnvelopeV1 =
                batpak::canonical::from_bytes(&event.envelope_bytes)?;
            generations.push(envelope.entity_generation);
        }
    }
    Ok(generations)
}

fn echo_descriptor() -> syncbat::OperationDescriptor {
    syncbat::OperationDescriptor::new(
        OPERATION,
        EffectClass::Inspect,
        "schema.echo.input.v1",
        "schema.echo.output.v1",
        "receipt.echo.v1",
    )
}

#[test]
fn subscription_runtime_operation_status_cursor_v1_roundtrip_and_resume_rules(
) -> Result<(), Box<dyn std::error::Error>> {
    let entity = status_entity()?;
    let beginning = OperationStatusStreamCursorV1::beginning(SUBSCRIPTION_ID, OPERATION, &entity);
    let decoded = OperationStatusStreamCursorV1::decode(&beginning.encode())?;
    assert_eq!(decoded, beginning);
    assert_eq!(decoded.resume_after_entity_generation(), None);

    let after_one = OperationStatusStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        OPERATION,
        &entity,
        1,
    );
    assert_eq!(after_one.resume_after_entity_generation(), Some(1));

    let mismatched = OperationStatusStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        "other.operation",
        &entity,
        1,
    );
    let err = mismatched.validate_route(SUBSCRIPTION_ID, OPERATION, &entity);
    assert!(matches!(
        err,
        Err(SubscriptionRuntimeError::CursorMismatch { .. })
    ));
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_registry_rejects_duplicate_subscription_id(
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = test_registry()?;
    let mut registry = registry;
    let error = match registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::OperationStatus {
            operation: OperationName::new(OPERATION)?,
            entity: status_entity()?,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_status_schema_ref: None,
            freshness: Freshness::Consistent,
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
fn subscription_runtime_operation_status_registry_rejects_invalid_entity_route(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = SubscriptionRegistry::new();
    let error = match registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::OperationStatus {
            operation: OperationName::new(OPERATION)?,
            entity: "wrong:entity".to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_status_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: None,
        },
    ) {
        Ok(()) => {
            return Err(std::io::Error::other(
                "PROPERTY: invalid operation-status entity route must be rejected",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        error,
        SubscriptionRuntimeError::InvalidRoute { .. }
    ));
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_open_rejects_invalid_cursor(
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
fn subscription_runtime_operation_status_open_rejects_cursor_mismatch(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let _entity = status_entity()?;
    let mismatched = OperationStatusStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        OPERATION,
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
fn subscription_runtime_operation_status_checkout_writes_started_and_completed_facts(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let status_sink = StoreOperationStatusSink::new(Arc::clone(&store));
    let mut builder = Core::builder();
    builder.register(echo_descriptor(), EchoHandler)?;
    builder.status_sink(status_sink);
    let mut core = builder.build()?;
    core.invoke(OPERATION, b"hello".to_vec())
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;

    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let envelope_bytes = deliveries
        .iter()
        .find_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.envelope_bytes.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected catch-up SUB_EVENT"))?;
    let envelope: OperationStatusStreamEnvelopeV1 = batpak::canonical::from_bytes(&envelope_bytes)?;
    let view: OperationStatusView = batpak::canonical::from_bytes(&envelope.status)?;
    assert_eq!(
        view.started_count, 1,
        "PROPERTY: checkout must write started fact"
    );
    assert_eq!(
        view.completed_count, 1,
        "PROPERTY: checkout must write completed fact"
    );
    assert_eq!(
        view.lifecycle,
        OperationStatusLifecycle::Completed,
        "PROPERTY: latest lifecycle must be completed"
    );
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_catch_up_snapshot_and_live_update(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Completed,
            "receipt.echo.v1",
            None,
            None,
            None,
            None,
        ),
    )?;

    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;
    let catch_up = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        catch_up
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: catch-up must deliver a materialized status snapshot"
    );

    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;
    let live = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        live.iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: live wake must deliver newly materialized status updates"
    );
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_denied_and_failed_outcome_updates_view(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Denied,
            "receipt.echo.v1",
            Some("admission.denied".to_owned()),
            Some("denied by guard".to_owned()),
            None,
            None,
        ),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Failed,
            "receipt.echo.v1",
            Some("fail.test".to_owned()),
            Some("handler failed".to_owned()),
            None,
            None,
        ),
    )?;

    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let last_event = deliveries
        .iter()
        .filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.envelope_bytes.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .next_back()
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected status SUB_EVENT"))?;
    let envelope: OperationStatusStreamEnvelopeV1 = batpak::canonical::from_bytes(&last_event)?;
    let view: OperationStatusView = batpak::canonical::from_bytes(&envelope.status)?;
    assert_eq!(view.denied_count, 1, "PROPERTY: denied fact must fold");
    assert_eq!(view.failed_count, 1, "PROPERTY: failed fact must fold");
    assert_eq!(
        view.lifecycle,
        OperationStatusLifecycle::Failed,
        "PROPERTY: latest lifecycle must reflect failed terminal fact"
    );
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_denied_after_started_counts_one_attempt(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Denied,
            "receipt.echo.v1",
            Some("effect.violation".to_owned()),
            Some("observed append was not declared".to_owned()),
            None,
            None,
        ),
    )?;

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let last_event = deliveries
        .iter()
        .filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.envelope_bytes.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .next_back()
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected status SUB_EVENT"))?;
    let envelope: OperationStatusStreamEnvelopeV1 = batpak::canonical::from_bytes(&last_event)?;
    let view: OperationStatusView = batpak::canonical::from_bytes(&envelope.status)?;
    assert_eq!(
        view.attempts_seen, 1,
        "PROPERTY: started + denied terminal facts describe one checkout attempt"
    );
    assert_eq!(view.started_count, 1, "PROPERTY: started fact must fold");
    assert_eq!(view.denied_count, 1, "PROPERTY: denied fact must fold");
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_resume_honors_cursor_after_entity_generation(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Completed,
            "receipt.echo.v1",
            None,
            None,
            None,
            None,
        ),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;

    let entity = status_entity()?;
    let resume = OperationStatusStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        OPERATION,
        &entity,
        1,
    );
    let mut session = open_session(
        Arc::clone(&store),
        &test_registry()?,
        Some(&resume.encode()),
        128,
    )?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let generations = status_generations(&deliveries)?;
    assert!(
        !generations.is_empty(),
        "PROPERTY: resume test requires at least one delivered event"
    );
    assert!(
        generations.iter().all(|gen| *gen > 1),
        "PROPERTY: resume must skip entity_generation <= 1, got {generations:?}"
    );
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_watermark_for_empty_fold(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let entity = status_entity()?;
    let coord = Coordinate::new(&entity, "scope:operation-status")?;
    let payload = serde_json::json!({"ignored": true});
    let _receipt = store.append(&coord, SYNCBAT_RECEIPT_EVENT_KIND, &payload)?;

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
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
fn subscription_runtime_operation_status_slow_consumer_closes_with_err(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;

    let operation = OperationName::new(OPERATION)?;
    let entity = status_entity()?;
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::OperationStatus {
            operation,
            entity,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_status_schema_ref: None,
            freshness: Freshness::Consistent,
            backpressure_capacity: Some(1),
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
    append_status_fact(
        &store,
        &OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Completed,
            "receipt.echo.v1",
            None,
            None,
            None,
            None,
        ),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
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
fn subscription_runtime_operation_status_cancel_emits_client_cancelled_end(
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
fn subscription_runtime_operation_status_cumulative_ack_frees_delivery_window(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;
    append_status_fact(
        &store,
        &OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Completed,
            "receipt.echo.v1",
            None,
            None,
            None,
            None,
        ),
    )?;

    let operation = OperationName::new(OPERATION)?;
    let entity = status_entity()?;
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::OperationStatus {
            operation,
            entity,
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_status_schema_ref: None,
            freshness: Freshness::Consistent,
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
    append_status_fact(
        &store,
        &OperationStatusFactV1::started(OPERATION, "receipt.echo.v1"),
    )?;
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
fn subscription_runtime_operation_status_unknown_route_rejected_at_open(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let (_control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(Arc::clone(&store)),
        SubscriptionRegistry::new(),
        SubscriptionRuntimeConfig::default(),
    );
    let error = match runtime.open_session("missing.status.v1", None, 128, control_rx) {
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
fn subscription_runtime_operation_status_status_sink_failure_before_handler_fails_closed(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = Core::builder();
    builder.register(echo_descriptor(), EchoHandler)?;
    builder.status_sink(FailingStatusSink);
    let mut core = builder.build()?;
    let error =
        match core.invoke(OPERATION, b"hello".to_vec()) {
            Ok(_) => return Err(std::io::Error::other(
                "PROPERTY: status sink failure must fail closed before handler side effects matter",
            )
            .into()),
            Err(error) => error,
        };
    assert!(matches!(error, syncbat::RuntimeError::StatusSink { .. }));
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_missing_handler_records_failed_terminal_status(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let mut descriptors = BTreeMap::new();
    descriptors.insert(OPERATION.to_owned(), echo_descriptor());
    let mut core = crate::core::Core {
        descriptors,
        handlers: BTreeMap::new(),
        admission_guard: None,
        receipt_sink: None,
        status_sink: Some(Arc::new(StoreOperationStatusSink::new(Arc::clone(&store)))),
        receipt_hash_policy: ReceiptHashPolicy::default(),
        effect_backend: None,
    };

    let error = match core.invoke(OPERATION, b"hello".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: descriptor without handler must not complete",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        error,
        syncbat::RuntimeError::MissingHandler { .. }
    ));

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let last_event = deliveries
        .iter()
        .filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.envelope_bytes.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .next_back()
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected missing-handler status"))?;
    let envelope: OperationStatusStreamEnvelopeV1 = batpak::canonical::from_bytes(&last_event)?;
    let view: OperationStatusView = batpak::canonical::from_bytes(&envelope.status)?;
    assert_eq!(
        view.started_count, 1,
        "PROPERTY: missing-handler branch starts attempt"
    );
    assert_eq!(
        view.failed_count, 1,
        "PROPERTY: missing-handler branch records terminal failure"
    );
    assert_eq!(
        view.attempts_seen, 1,
        "PROPERTY: missing-handler branch is one attempt"
    );
    assert_eq!(
        view.last_code.as_deref(),
        Some("missing_handler"),
        "PROPERTY: missing-handler status must carry stable class"
    );
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_checkout_failed_handler_updates_view(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let status_sink = StoreOperationStatusSink::new(Arc::clone(&store));
    let mut builder = Core::builder();
    builder.register(echo_descriptor(), FailHandler)?;
    builder.status_sink(status_sink);
    let mut core = builder.build()?;
    let _ = core.invoke(OPERATION, b"bad".to_vec());

    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let envelope_bytes = deliveries
        .iter()
        .find_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.envelope_bytes.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected failed checkout status"))?;
    let envelope: OperationStatusStreamEnvelopeV1 = batpak::canonical::from_bytes(&envelope_bytes)?;
    let view: OperationStatusView = batpak::canonical::from_bytes(&envelope.status)?;
    assert_eq!(view.failed_count, 1, "PROPERTY: failed checkout must fold");
    assert_eq!(
        view.lifecycle,
        OperationStatusLifecycle::Failed,
        "PROPERTY: failed checkout must surface failed lifecycle"
    );
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_runtime_cursor_is_opaque_on_wire_path(
) -> Result<(), Box<dyn std::error::Error>> {
    let entity = status_entity()?;
    let cursor = OperationStatusStreamCursorV1::after_entity_generation(
        SUBSCRIPTION_ID,
        OPERATION,
        &entity,
        4,
    );
    let runtime = RuntimeCursor::from_bytes(cursor.encode().to_vec());
    let decoded = OperationStatusStreamCursorV1::decode(runtime.as_bytes())?;
    assert_eq!(decoded, cursor);
    Ok(())
}

#[test]
fn subscription_runtime_operation_status_client_disconnect_without_cancel_ends_session(
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
        .map_err(|_| std::io::Error::other("PROPERTY: disconnect send failed"))?;
    let poll = session.poll(Duration::from_millis(250))?;
    assert!(
        matches!(poll, SessionPoll::Ended),
        "PROPERTY: disconnect without cancel must end session"
    );
    Ok(())
}
