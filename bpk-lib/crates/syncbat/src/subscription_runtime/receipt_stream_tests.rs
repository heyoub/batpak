//! PROVES: S12-SUBSCRIPTION-RUNTIME-RECEIPTS syncbat runtime engine.
//! CATCHES: replay/live/resume/watermark/ACK/backpressure/decode regressions.

use std::sync::Arc;
use std::time::Duration;

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use flume::bounded;
use syncbat::subscription_runtime::error::stream_code;
use syncbat::{
    CompositeSubscriptionRuntime, Core, Ctx, EffectClass, Handler, HandlerResult,
    OperationDescriptor, ReceiptEnvelope, ReceiptOutcome, ReceiptSink, ReceiptStreamCursorV1,
    ReceiptStreamEnvelopeV1, SessionControl, SessionDelivery, SessionPoll, StoreReceiptSink,
    SubscriptionId, SubscriptionRegistry, SubscriptionRoute, SubscriptionRuntimeConfig,
    SubscriptionRuntimeError, SubscriptionSession, SubscriptionSessionFactory, SubscriptionStore,
    SYNCBAT_RECEIPT_EVENT_KIND,
};

const SUBSCRIPTION_ID: &str = "receipts.echo.v1";
const RECEIPT_KIND: &str = "receipt.echo.v1";
const OTHER_RECEIPT_KIND: &str = "receipt.ping.v1";
const WIRE_SCHEMA: &str = "batpak.receipt-stream-envelope.v1";
const OPERATION: &str = "mod.a.echo";

struct EchoHandler;

impl Handler for EchoHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        Ok(input.to_vec())
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

fn receipt_coord() -> Result<Coordinate, Box<dyn std::error::Error>> {
    Coordinate::new("syncbat:receipt", "scope:test").map_err(|error| {
        std::io::Error::other(format!("PROPERTY: receipt coordinate invalid: {error}")).into()
    })
}

fn test_registry() -> Result<SubscriptionRegistry, Box<dyn std::error::Error>> {
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID).map_err(|error| {
            std::io::Error::other(format!("PROPERTY: subscription id invalid: {error}"))
        })?,
        SubscriptionRoute::ReceiptStream {
            receipt_kind: RECEIPT_KIND.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_receipt_schema_ref: None,
            backpressure_capacity: None,
        },
    )?;
    Ok(registry)
}

fn append_receipt(
    store: Arc<Store>,
    receipt_kind: &str,
    descriptor_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let sink = StoreReceiptSink::new(store, receipt_coord()?);
    let envelope =
        ReceiptEnvelope::from_descriptor(descriptor_name, receipt_kind, ReceiptOutcome::Completed);
    sink.record_receipt(&envelope)
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
    Ok(())
}

fn append_malformed_receipt_event(store: &Store) -> Result<(), Box<dyn std::error::Error>> {
    let _receipt = store
        .append(
            &receipt_coord()?,
            SYNCBAT_RECEIPT_EVENT_KIND,
            &serde_json::json!({"not_a_receipt_envelope": true}),
        )
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

fn receipt_global_sequences(
    deliveries: &[SessionDelivery],
) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
    let mut sequences = Vec::new();
    for delivery in deliveries {
        if let SessionDelivery::Event(event) = delivery {
            let envelope: ReceiptStreamEnvelopeV1 =
                batpak::canonical::from_bytes(&event.envelope_bytes)?;
            sequences.push(envelope.global_sequence);
        }
    }
    Ok(sequences)
}

fn echo_descriptor() -> OperationDescriptor {
    OperationDescriptor::new(
        OPERATION,
        EffectClass::Inspect,
        "schema.echo.input.v1",
        "schema.echo.output.v1",
        RECEIPT_KIND,
    )
}

#[test]
fn subscription_runtime_receipt_cursor_v1_roundtrip_and_resume_rules(
) -> Result<(), Box<dyn std::error::Error>> {
    let beginning = ReceiptStreamCursorV1::beginning(SUBSCRIPTION_ID, RECEIPT_KIND);
    let decoded = ReceiptStreamCursorV1::decode(&beginning.encode())?;
    assert_eq!(decoded, beginning);
    assert_eq!(decoded.resume_after_global_sequence(), None);

    let after_zero =
        ReceiptStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, RECEIPT_KIND, 0, 1);
    assert_eq!(after_zero.resume_after_global_sequence(), Some(0));

    let mismatched =
        ReceiptStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, OTHER_RECEIPT_KIND, 1, 1);
    let err = mismatched.validate_route(SUBSCRIPTION_ID, RECEIPT_KIND);
    assert!(matches!(
        err,
        Err(SubscriptionRuntimeError::CursorMismatch { .. })
    ));
    Ok(())
}

#[test]
fn subscription_runtime_receipt_unknown_route_rejected_at_open(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let (_control_tx, control_rx) = bounded(4);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(Arc::clone(&store)),
        SubscriptionRegistry::new(),
        SubscriptionRuntimeConfig::default(),
    );
    let error = match runtime.open_session("missing.receipts.v1", None, 128, control_rx) {
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
fn subscription_runtime_receipt_open_rejects_invalid_cursor(
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
fn subscription_runtime_receipt_open_rejects_cursor_mismatch(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let mismatched =
        ReceiptStreamCursorV1::after_global_sequence(SUBSCRIPTION_ID, OTHER_RECEIPT_KIND, 1, 1);
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
fn subscription_runtime_receipt_replay_delivers_matching_receipt_kind(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;

    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let sequences = receipt_global_sequences(&deliveries)?;
    assert_eq!(
        sequences.len(),
        2,
        "PROPERTY: replay must deliver two matching receipt events"
    );
    assert!(
        deliveries
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Watermark(_))),
        "PROPERTY: catch-up must emit a coalesced watermark witness"
    );
    Ok(())
}

#[test]
fn subscription_runtime_receipt_live_delivery_after_core_checkout_receipt(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    let registry = test_registry()?;
    let mut session = open_session(Arc::clone(&store), &registry, None, 128)?;

    let receipt_sink = StoreReceiptSink::new(Arc::clone(&store), receipt_coord()?);
    let mut builder = Core::builder();
    builder.register(echo_descriptor(), EchoHandler)?;
    builder.receipt_sink(receipt_sink);
    let mut core = builder.build()?;
    core.invoke(OPERATION, b"hello".to_vec())
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;

    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    let envelope_bytes = deliveries
        .iter()
        .find_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.envelope_bytes.clone()),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .ok_or_else(|| std::io::Error::other("PROPERTY: expected live SUB_EVENT delivery"))?;
    let envelope: ReceiptStreamEnvelopeV1 = batpak::canonical::from_bytes(&envelope_bytes)?;
    let receipt: ReceiptEnvelope = batpak::canonical::from_bytes(&envelope.receipt)?;
    assert_eq!(
        envelope.descriptor_name, OPERATION,
        "PROPERTY: checkout receipt must surface operation descriptor"
    );
    assert_eq!(
        envelope.receipt_kind, RECEIPT_KIND,
        "PROPERTY: checkout receipt must match route filter"
    );
    assert_eq!(
        receipt.receipt_kind, RECEIPT_KIND,
        "PROPERTY: delivered inner receipt envelope must match route filter"
    );
    Ok(())
}

#[test]
fn subscription_runtime_receipt_wrong_receipt_kind_skipped(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;
    append_receipt(Arc::clone(&store), OTHER_RECEIPT_KIND, "ping")?;

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 8)?;
    assert_eq!(
        receipt_global_sequences(&deliveries)?.len(),
        1,
        "PROPERTY: non-matching receipt kinds must be skipped"
    );
    Ok(())
}

#[test]
fn subscription_runtime_receipt_malformed_receipt_payload_emits_receipt_decode_failed(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_malformed_receipt_event(&store)?;

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        deliveries.iter().any(|delivery| matches!(
            delivery,
            SessionDelivery::Error(error) if error.code == stream_code::RECEIPT_DECODE_FAILED
        )),
        "PROPERTY: malformed receipt payload must emit receipt_decode_failed"
    );
    Ok(())
}

#[test]
fn subscription_runtime_receipt_watermark_after_filtered_stream_catches_up(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_receipt(Arc::clone(&store), OTHER_RECEIPT_KIND, "ping")?;

    let mut session = open_session(Arc::clone(&store), &test_registry()?, None, 128)?;
    let deliveries = collect_deliveries(session.as_mut(), 4)?;
    assert!(
        deliveries
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Watermark(_))),
        "PROPERTY: filtered catch-up must emit SUB_WATERMARK"
    );
    assert!(
        !deliveries
            .iter()
            .any(|delivery| matches!(delivery, SessionDelivery::Event(_))),
        "PROPERTY: filtered catch-up must not deliver non-matching receipts"
    );
    Ok(())
}

#[test]
fn subscription_runtime_receipt_cumulative_ack_frees_delivery_window(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::ReceiptStream {
            receipt_kind: RECEIPT_KIND.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_receipt_schema_ref: None,
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
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;
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
fn subscription_runtime_receipt_slow_consumer_closes_with_err(
) -> Result<(), Box<dyn std::error::Error>> {
    let (store, _dir) = test_store()?;
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;

    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::ReceiptStream {
            receipt_kind: RECEIPT_KIND.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_receipt_schema_ref: None,
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
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;
    append_receipt(Arc::clone(&store), RECEIPT_KIND, OPERATION)?;
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
fn subscription_runtime_receipt_cancel_emits_client_cancelled(
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
fn subscription_runtime_receipt_disconnect_without_cancel_ends_session(
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
