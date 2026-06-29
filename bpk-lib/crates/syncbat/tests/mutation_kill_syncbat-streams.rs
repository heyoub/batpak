//! PROVES: S12-SUBSCRIPTION-RUNTIME stream-session ACK range/regress checks and
//! per-delivery index advancement across every stream kind.
//! CATCHES: diff-scoped surviving mutants in apply_ack (`||`/`>`/`<` flips) and
//! the `delivery_index += 1` advancement in try_deliver / deliver_update /
//! maybe_emit_watermark that the existing suite did not kill.

use std::sync::Arc;
use std::time::Duration;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{Freshness, Store, StoreConfig};
use flume::{bounded, Sender};
use syncbat::{
    CompositeSubscriptionRuntime, RuntimeCursor, SessionControl, SessionDelivery, SessionPoll,
    SubscriptionId, SubscriptionRegistry, SubscriptionRoute, SubscriptionRuntimeConfig,
    SubscriptionSession, SubscriptionSessionFactory, SubscriptionStore,
};

type DynErr = Box<dyn std::error::Error>;

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

fn first_error_message(deliveries: &[SessionDelivery]) -> Option<String> {
    deliveries.iter().find_map(|delivery| match delivery {
        SessionDelivery::Error(error) => Some(String::from_utf8_lossy(&error.message).into_owned()),
        SessionDelivery::Event(_) | SessionDelivery::Watermark(_) | SessionDelivery::End(_) => None,
    })
}

fn first_event(deliveries: &[SessionDelivery]) -> Option<(u64, RuntimeCursor)> {
    deliveries.iter().find_map(|delivery| match delivery {
        SessionDelivery::Event(event) => Some((event.delivery_index, event.cursor_after.clone())),
        SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => None,
    })
}

fn event_indices(deliveries: &[SessionDelivery]) -> Vec<u64> {
    deliveries
        .iter()
        .filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => Some(event.delivery_index),
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        })
        .collect()
}

fn first_watermark_index(deliveries: &[SessionDelivery]) -> Option<u64> {
    deliveries.iter().find_map(|delivery| match delivery {
        SessionDelivery::Watermark(watermark) => Some(watermark.delivery_index),
        SessionDelivery::Event(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => None,
    })
}

fn open(
    store: Arc<Store>,
    registry: &SubscriptionRegistry,
    subscription_id: &str,
) -> Result<(Box<dyn SubscriptionSession>, Sender<SessionControl>), DynErr> {
    let (control_tx, control_rx) = bounded(8);
    let runtime = CompositeSubscriptionRuntime::new(
        SubscriptionStore::new(store),
        registry.clone(),
        SubscriptionRuntimeConfig::default(),
    );
    let session = runtime.open_session(subscription_id, None, 128, control_rx)?;
    Ok((session, control_tx))
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

// ---------------------------------------------------------------------------
// receipt stream
// ---------------------------------------------------------------------------
mod receipt {
    use super::*;
    use syncbat::{ReceiptEnvelope, ReceiptOutcome, ReceiptSink, StoreReceiptSink};

    const SUBSCRIPTION_ID: &str = "receipts.echo.v1";
    const RECEIPT_KIND: &str = "receipt.echo.v1";
    const WIRE_SCHEMA: &str = "batpak.receipt-stream-envelope.v1";
    const OPERATION: &str = "mod.a.echo";

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

    #[test]
    fn ack_index_zero_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(Arc::clone(&store))?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, 0, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(
            message.contains("out of range"),
            "index-0 ack must be out of range, got {message:?}"
        );
        Ok(())
    }

    #[test]
    fn ack_index_above_sent_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(Arc::clone(&store))?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, 999, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(
            message.contains("out of range"),
            "ack above last-sent must be out of range, got {message:?}"
        );
        Ok(())
    }

    #[test]
    fn reacking_same_index_is_not_a_regression() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(Arc::clone(&store))?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (index, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, index, cursor.clone())?;
        send_ack(&control_tx, index, cursor)?;
        let after = collect(session.as_mut(), 8)?;
        if let Some(message) = first_error_message(&after) {
            assert!(
                !message.contains("regress"),
                "re-acking the same index must not regress, got {message:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn earlier_index_after_later_ack_regresses() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(Arc::clone(&store))?;
        append(Arc::clone(&store))?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let deliveries = collect(session.as_mut(), 8)?;
        let mut events = deliveries.iter().filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => {
                Some((event.delivery_index, event.cursor_after.clone()))
            }
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        });
        let (first_index, first_cursor) = events
            .next()
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected first event").into() })?;
        let (second_index, second_cursor) = events
            .next()
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected second event").into() })?;
        send_ack(&control_tx, second_index, second_cursor)?;
        send_ack(&control_tx, first_index, first_cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected regress error").into() })?;
        assert!(
            message.contains("regress"),
            "acking an earlier index must regress, got {message:?}"
        );
        Ok(())
    }

    #[test]
    fn two_receipts_get_distinct_increasing_indices() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(Arc::clone(&store))?;
        append(Arc::clone(&store))?;
        let (mut session, _control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let indices = event_indices(&collect(session.as_mut(), 8)?);
        assert_eq!(
            indices,
            vec![1, 2],
            "each receipt delivery must advance delivery_index by one"
        );
        Ok(())
    }

    #[test]
    fn event_after_watermark_advances_past_watermark_index() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(Arc::clone(&store))?;
        append(Arc::clone(&store))?;
        let (mut session, _control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let first_batch = collect(session.as_mut(), 8)?;
        let watermark_index = first_watermark_index(&first_batch)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected watermark").into() })?;
        append(Arc::clone(&store))?;
        let (next_index, _) =
            first_event(&collect(session.as_mut(), 8)?).ok_or_else(|| -> DynErr {
                std::io::Error::other("expected post-watermark event").into()
            })?;
        assert!(
            next_index > watermark_index,
            "watermark must advance delivery_index so the next event index ({next_index}) exceeds the watermark index ({watermark_index})"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// entity stream
// ---------------------------------------------------------------------------
mod entity {
    use super::*;

    const SUBSCRIPTION_ID: &str = "orders.entity.v1";
    const ENTITY: &str = "entity:orders";
    const SCOPE: &str = "scope:open";
    const WIRE_SCHEMA: &str = "batpak.entity-stream-envelope.v1";
    const EVENT_KIND: EventKind = EventKind::custom(0x0A, 0x01);

    fn registry() -> Result<SubscriptionRegistry, DynErr> {
        let mut registry = SubscriptionRegistry::new();
        registry.insert(
            SubscriptionId::new(SUBSCRIPTION_ID)?,
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

    fn append(store: &Store, index: u64) -> Result<(), DynErr> {
        let coord = Coordinate::new(ENTITY, SCOPE)?;
        let _receipt = store
            .append(&coord, EVENT_KIND, &serde_json::json!({ "n": index }))
            .map_err(|error| -> DynErr { Box::new(error) })?;
        Ok(())
    }

    #[test]
    fn ack_index_zero_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, 0, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn ack_index_above_sent_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, 999, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn reacking_same_index_is_not_a_regression() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (index, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, index, cursor.clone())?;
        send_ack(&control_tx, index, cursor)?;
        if let Some(message) = first_error_message(&collect(session.as_mut(), 8)?) {
            assert!(!message.contains("regress"), "got {message:?}");
        }
        Ok(())
    }

    #[test]
    fn earlier_index_after_later_ack_regresses() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        append(&store, 1)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let deliveries = collect(session.as_mut(), 8)?;
        let mut events = deliveries.iter().filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => {
                Some((event.delivery_index, event.cursor_after.clone()))
            }
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        });
        let (first_index, first_cursor) = events
            .next()
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected first event").into() })?;
        let (second_index, second_cursor) = events
            .next()
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected second event").into() })?;
        send_ack(&control_tx, second_index, second_cursor)?;
        send_ack(&control_tx, first_index, first_cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected regress error").into() })?;
        assert!(message.contains("regress"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn two_events_get_distinct_increasing_indices() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        append(&store, 1)?;
        let (mut session, _control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let indices = event_indices(&collect(session.as_mut(), 8)?);
        assert_eq!(indices, vec![1, 2], "got {indices:?}");
        Ok(())
    }

    #[test]
    fn event_after_watermark_advances_past_watermark_index() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        append(&store, 1)?;
        let (mut session, _control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let first_batch = collect(session.as_mut(), 8)?;
        let watermark_index = first_watermark_index(&first_batch)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected watermark").into() })?;
        append(&store, 2)?;
        let (next_index, _) =
            first_event(&collect(session.as_mut(), 8)?).ok_or_else(|| -> DynErr {
                std::io::Error::other("expected post-watermark event").into()
            })?;
        assert!(
            next_index > watermark_index,
            "{next_index} !> {watermark_index}"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// event-category stream
// ---------------------------------------------------------------------------
mod event {
    use super::*;

    const SUBSCRIPTION_ID: &str = "orders.open.v1";
    const CATEGORY: u8 = 0x0A;
    const WIRE_SCHEMA: &str = "batpak.event-stream-envelope.v1";
    const EVENT_KIND: EventKind = EventKind::custom(0x0A, 0x01);

    fn registry() -> Result<SubscriptionRegistry, DynErr> {
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
        Ok(registry)
    }

    fn append(store: &Store, index: u64) -> Result<(), DynErr> {
        let coord = Coordinate::new("entity:event", "scope:event")?;
        let _receipt = store
            .append(&coord, EVENT_KIND, &serde_json::json!({ "n": index }))
            .map_err(|error| -> DynErr { Box::new(error) })?;
        Ok(())
    }

    #[test]
    fn ack_index_zero_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, 0, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn ack_index_above_sent_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, 999, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn reacking_same_index_is_not_a_regression() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (index, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected event").into() })?;
        send_ack(&control_tx, index, cursor.clone())?;
        send_ack(&control_tx, index, cursor)?;
        if let Some(message) = first_error_message(&collect(session.as_mut(), 8)?) {
            assert!(!message.contains("regress"), "got {message:?}");
        }
        Ok(())
    }

    #[test]
    fn earlier_index_after_later_ack_regresses() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        append(&store, 1)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let deliveries = collect(session.as_mut(), 8)?;
        let mut events = deliveries.iter().filter_map(|delivery| match delivery {
            SessionDelivery::Event(event) => {
                Some((event.delivery_index, event.cursor_after.clone()))
            }
            SessionDelivery::Watermark(_) | SessionDelivery::Error(_) | SessionDelivery::End(_) => {
                None
            }
        });
        let (first_index, first_cursor) = events
            .next()
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected first event").into() })?;
        let (second_index, second_cursor) = events
            .next()
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected second event").into() })?;
        send_ack(&control_tx, second_index, second_cursor)?;
        send_ack(&control_tx, first_index, first_cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected regress error").into() })?;
        assert!(message.contains("regress"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn event_after_watermark_advances_past_watermark_index() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, _control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let first_batch = collect(session.as_mut(), 8)?;
        let watermark_index = first_watermark_index(&first_batch)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected watermark").into() })?;
        append(&store, 1)?;
        let (next_index, _) =
            first_event(&collect(session.as_mut(), 8)?).ok_or_else(|| -> DynErr {
                std::io::Error::other("expected post-watermark event").into()
            })?;
        assert!(
            next_index > watermark_index,
            "{next_index} !> {watermark_index}"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// projection stream
// ---------------------------------------------------------------------------
mod projection {
    use super::*;
    use batpak_testkit::red_counters::AllCounter;
    use batpak_testkit::red_kinds::{kind_a, payload};
    use syncbat::TypedProjectionProjector;

    const SUBSCRIPTION_ID: &str = "counter.projection.v1";
    const PROJECTION_ID: &str = "testkit-all-counter";
    const ENTITY: &str = "entity:projection";
    const WIRE_SCHEMA: &str = "batpak.projection-stream-envelope.v1";

    fn registry() -> Result<SubscriptionRegistry, DynErr> {
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
        Ok(registry)
    }

    fn append(store: &Store, index: u32) -> Result<(), DynErr> {
        let coord = Coordinate::new(ENTITY, "scope:projection")?;
        let _receipt = store
            .append(&coord, kind_a(), &payload(index))
            .map_err(|error| -> DynErr { Box::new(error) })?;
        Ok(())
    }

    #[test]
    fn ack_index_zero_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        send_ack(&control_tx, 0, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn ack_index_above_sent_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        send_ack(&control_tx, 999, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn reacking_same_index_is_not_a_regression() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (index, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        send_ack(&control_tx, index, cursor.clone())?;
        send_ack(&control_tx, index, cursor)?;
        if let Some(message) = first_error_message(&collect(session.as_mut(), 8)?) {
            assert!(!message.contains("regress"), "got {message:?}");
        }
        Ok(())
    }

    #[test]
    fn live_update_advances_index_past_snapshot() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, 0)?;
        let (mut session, _control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (snapshot_index, _) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        append(&store, 1)?;
        let (live_index, _) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected live update").into() })?;
        assert!(
            live_index > snapshot_index,
            "live update index ({live_index}) must advance past snapshot index ({snapshot_index})"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// operation-status stream
// ---------------------------------------------------------------------------
mod operation_status {
    use super::*;
    use syncbat::operation_status::{OperationStatusFactV1, OperationStatusLifecycle};
    use syncbat::{operation_status_entity, OperationName};

    const SUBSCRIPTION_ID: &str = "echo.status.v1";
    const OPERATION: &str = "mod.a.echo";
    const RECEIPT_KIND: &str = "receipt.echo.v1";
    const WIRE_SCHEMA: &str = "batpak.operation-status-stream-envelope.v1";

    fn registry() -> Result<SubscriptionRegistry, DynErr> {
        let operation = OperationName::new(OPERATION)?;
        let entity = operation_status_entity(OPERATION)?;
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

    fn append(store: &Store, fact: &OperationStatusFactV1) -> Result<(), DynErr> {
        let entity = operation_status_entity(OPERATION)?;
        let coord = Coordinate::new(&entity, "scope:operation-status")?;
        let _receipt = store
            .append_typed(&coord, fact)
            .map_err(|error| -> DynErr { Box::new(error) })?;
        Ok(())
    }

    fn started() -> OperationStatusFactV1 {
        OperationStatusFactV1::started(OPERATION, RECEIPT_KIND)
    }

    fn completed() -> OperationStatusFactV1 {
        OperationStatusFactV1::terminal(
            OPERATION,
            OperationStatusLifecycle::Completed,
            RECEIPT_KIND,
            None,
            None,
            None,
            None,
        )
    }

    #[test]
    fn ack_index_zero_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, &started())?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        send_ack(&control_tx, 0, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn ack_index_above_sent_is_out_of_range() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, &started())?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (_, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        send_ack(&control_tx, 999, cursor)?;
        let message = first_error_message(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected ack error").into() })?;
        assert!(message.contains("out of range"), "got {message:?}");
        Ok(())
    }

    #[test]
    fn reacking_same_index_is_not_a_regression() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, &started())?;
        let (mut session, control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (index, cursor) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        send_ack(&control_tx, index, cursor.clone())?;
        send_ack(&control_tx, index, cursor)?;
        if let Some(message) = first_error_message(&collect(session.as_mut(), 8)?) {
            assert!(!message.contains("regress"), "got {message:?}");
        }
        Ok(())
    }

    #[test]
    fn live_update_advances_index_past_snapshot() -> Result<(), DynErr> {
        let (store, _dir) = test_store()?;
        append(&store, &started())?;
        let (mut session, _control_tx) = open(Arc::clone(&store), &registry()?, SUBSCRIPTION_ID)?;
        let (snapshot_index, _) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected snapshot").into() })?;
        append(&store, &completed())?;
        let (live_index, _) = first_event(&collect(session.as_mut(), 8)?)
            .ok_or_else(|| -> DynErr { std::io::Error::other("expected live update").into() })?;
        assert!(
            live_index > snapshot_index,
            "live update index ({live_index}) must advance past snapshot index ({snapshot_index})"
        );
        Ok(())
    }
}
