//! PROVES: invalid-ACK terminal errors carry the `malformed_stream_frame`
//! stream code (not the fallback `cursor_invalid`) for the two structural ACK
//! faults that share that classification.
//! CATCHES: the surviving delete-arm mutant in
//! `crates/syncbat/src/subscription_runtime/session.rs` (`ack_invalid_error`,
//! line 270) which would let "ack delivery index out of range" and "ack cursor
//! does not match sent cursor" fall through to the `cursor_invalid` arm.
//!
//! Everything else in the round-1 `syncbat-subscription-runtime` .missed list is
//! already killed by the existing suites and is intentionally NOT duplicated here:
//!   - cursor.rs / entity_cursor.rs / envelope.rs / operation_status.rs /
//!     operation_status_sink.rs: inline `#[cfg(test)]` helper/mutation modules.
//!   - registry.rs (all 87): `mutation_kill_syncbat-registry.rs`.
//!   - the per-stream apply_ack / delivery-index / watermark operators:
//!     `mutation_kill_syncbat-streams.rs`.
//!   - `operation_status.rs:166 schema_version -> 1`: EQUIVALENT (the real value
//!     is `u64::from(SCHEMA_VERSION)` == 1), documented in that file's inline test.

use std::sync::Arc;
use std::time::Duration;

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use flume::{bounded, Sender};
use syncbat::{
    CompositeSubscriptionRuntime, ReceiptEnvelope, ReceiptOutcome, ReceiptSink, RuntimeCursor,
    SessionControl, SessionDelivery, SessionError, SessionPoll, StoreReceiptSink, SubscriptionId,
    SubscriptionRegistry, SubscriptionRoute, SubscriptionRuntimeConfig, SubscriptionSession,
    SubscriptionSessionFactory, SubscriptionStore,
};

type DynErr = Box<dyn std::error::Error>;

// The session source-of-truth: these reasons must map to `malformed_stream_frame`,
// not the `cursor_invalid` fallback. The constant is not exported, so the literal
// is mirrored here (a drift would itself be a test failure).
const MALFORMED_STREAM_FRAME: &str = "malformed_stream_frame";
const CURSOR_INVALID: &str = "cursor_invalid";

const SUBSCRIPTION_ID: &str = "receipts.echo.v1";
const RECEIPT_KIND: &str = "receipt.echo.v1";
const WIRE_SCHEMA: &str = "batpak.receipt-stream-envelope.v1";
const OPERATION: &str = "mod.a.echo";

fn test_store() -> Result<(Arc<Store>, tempfile::TempDir), DynErr> {
    let dir = tempfile::TempDir::new()?;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
            .with_sync_every_n_events(1),
    )?;
    Ok((Arc::new(store), dir))
}

fn registry() -> Result<SubscriptionRegistry, DynErr> {
    let mut registry = SubscriptionRegistry::new();
    registry.insert(
        SubscriptionId::new(SUBSCRIPTION_ID)?,
        SubscriptionRoute::ReceiptStream {
            receipt_kind: RECEIPT_KIND.to_owned(),
            wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
            inner_receipt_schema_ref: None,
            backpressure_capacity: None,
        },
    )?;
    Ok(registry)
}

fn append(store: Arc<Store>) -> Result<(), DynErr> {
    let coord = Coordinate::new("syncbat:receipt", "scope:test")?;
    let sink = StoreReceiptSink::new(store, coord);
    let envelope =
        ReceiptEnvelope::from_descriptor(OPERATION, RECEIPT_KIND, ReceiptOutcome::Completed);
    sink.record_receipt(&envelope)
        .map_err(|error| -> DynErr { Box::new(error) })?;
    Ok(())
}

fn open(
    store: Arc<Store>,
) -> Result<(Box<dyn SubscriptionSession>, Sender<SessionControl>), DynErr> {
    let (control_tx, control_rx) = bounded(8);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(store),
        registry()?,
        SubscriptionRuntimeConfig::default(),
    );
    let session = runtime.open_session(SUBSCRIPTION_ID, None, 128, control_rx)?;
    Ok((session, control_tx))
}

fn collect(
    session: &mut dyn SubscriptionSession,
    max_steps: usize,
) -> Result<Vec<SessionDelivery>, DynErr> {
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

fn first_event(deliveries: &[SessionDelivery]) -> Option<(u64, RuntimeCursor)> {
    deliveries.iter().find_map(|delivery| match delivery {
        SessionDelivery::Event(event) => Some((event.delivery_index, event.cursor_after.clone())),
        SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => None,
    })
}

fn first_error(deliveries: &[SessionDelivery]) -> Option<SessionError> {
    deliveries.iter().find_map(|delivery| match delivery {
        SessionDelivery::Error(error) => Some(error.clone()),
        SessionDelivery::Event(_) | SessionDelivery::Watermark(_) | SessionDelivery::End(_) => None,
    })
}

fn send_ack(
    control_tx: &Sender<SessionControl>,
    delivery_index: u64,
    cursor: RuntimeCursor,
) -> Result<(), DynErr> {
    control_tx
        .send(SessionControl::Ack {
            delivery_index,
            cursor,
        })
        .map_err(|_| -> DynErr { std::io::Error::other("ack send failed").into() })
}

fn message_of(error: &SessionError) -> String {
    String::from_utf8_lossy(&error.message).into_owned()
}

/// An out-of-range ACK index is one of the two reasons in the deleted arm; its
/// terminal error must classify as `malformed_stream_frame`. With the arm gone
/// it would fall to `cursor_invalid`.
#[test]
fn ack_index_out_of_range_uses_malformed_stream_frame_code() -> Result<(), DynErr> {
    let (store, _dir) = test_store()?;
    append(Arc::clone(&store))?;
    let (mut session, control_tx) = open(Arc::clone(&store))?;
    let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
        .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;

    send_ack(&control_tx, 999, cursor)?;
    let error = first_error(&collect(session.as_mut(), 8)?)
        .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;

    assert_eq!(
        message_of(&error),
        "ack delivery index out of range",
        "wrong terminal reason"
    );
    assert_eq!(
        error.code, MALFORMED_STREAM_FRAME,
        "out-of-range ACK must be a malformed-frame fault, not cursor_invalid"
    );
    assert_ne!(error.code, CURSOR_INVALID);
    Ok(())
}

/// A valid index whose cursor does not match the one the session sent is the
/// other reason in the deleted arm; it must also classify as
/// `malformed_stream_frame`. We mismatch the cursor by acking index 1 with a
/// second, route-valid-but-different sent cursor.
#[test]
fn ack_cursor_mismatch_uses_malformed_stream_frame_code() -> Result<(), DynErr> {
    let (store, _dir) = test_store()?;
    append(Arc::clone(&store))?;
    append(Arc::clone(&store))?;
    let (mut session, control_tx) = open(Arc::clone(&store))?;

    let deliveries = collect(session.as_mut(), 8)?;
    let mut events = deliveries.iter().filter_map(|delivery| match delivery {
        SessionDelivery::Event(event) => Some((event.delivery_index, event.cursor_after.clone())),
        SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => None,
    });
    let (first_index, _first_cursor) = events
        .next()
        .ok_or_else(|| -> DynErr { std::io::Error::other("expected first event").into() })?;
    let (_second_index, second_cursor) = events
        .next()
        .ok_or_else(|| -> DynErr { std::io::Error::other("expected second event").into() })?;

    // ACK the first delivery index but hand over the (decodable, route-valid)
    // cursor that belongs to the second delivery: the sent-cursor check fails.
    send_ack(&control_tx, first_index, second_cursor)?;
    let error = first_error(&collect(session.as_mut(), 8)?)
        .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;

    assert_eq!(
        message_of(&error),
        "ack cursor does not match sent cursor",
        "wrong terminal reason"
    );
    assert_eq!(
        error.code, MALFORMED_STREAM_FRAME,
        "cursor-mismatch ACK must be a malformed-frame fault, not cursor_invalid"
    );
    assert_ne!(error.code, CURSOR_INVALID);
    Ok(())
}
