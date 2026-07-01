use std::collections::BTreeMap;
use std::time::Duration;

use batpak::coordinate::{EventCategory, Region};
use batpak::store::Subscription;
use flume::{Receiver, RecvTimeoutError, TryRecvError};

use super::config::SubscriptionRuntimeConfig;
use super::cursor::EventStreamCursorV1;
use super::envelope::{
    event_stream_envelope_bytes_from_stored, read_delivery_stored, warn_shredded_delivery,
};
use super::error::SubscriptionRuntimeError;
use super::registry::{SubscriptionRegistry, SubscriptionRoute};
use super::session::{
    ack_invalid_error, client_cancel_end, cursor_mismatch_terminal, malformed_control_error,
    queue_capacity, slow_consumer_error, validate_open_limits, RuntimeCursor, SessionControl,
    SessionDelivery, SessionEventDelivery, SessionPoll, SessionWatermarkDelivery,
    SubscriptionSession, SubscriptionSessionFactory, SubscriptionStore,
};

enum SessionPhase {
    Replaying,
    Live,
    Ended,
}

struct RouteBinding {
    category: u8,
    wire_payload_schema_ref: String,
    inner_event_payload_schema_ref: Option<String>,
    queue_cap: u64,
}

/// Parameters for opening an event-category subscription session.
pub struct EventSessionOpenParams {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Exported 4-bit event category filter.
    pub category: u8,
    /// Wire payload schema ref for stream envelopes.
    pub wire_payload_schema_ref: String,
    /// Optional inner payload schema ref carried inside the envelope.
    pub inner_event_payload_schema_ref: Option<String>,
    /// Optional route-specific queue clamp.
    pub backpressure_capacity: Option<usize>,
}

/// Store-backed event-subscription runtime.
#[derive(Clone)]
pub struct EventSubscriptionRuntime {
    store: SubscriptionStore,
    registry: SubscriptionRegistry,
    config: SubscriptionRuntimeConfig,
}

impl EventSubscriptionRuntime {
    /// Build a store-backed event subscription runtime.
    #[must_use]
    pub fn new(
        store: SubscriptionStore,
        registry: SubscriptionRegistry,
        config: SubscriptionRuntimeConfig,
    ) -> Self {
        Self {
            store,
            registry,
            config,
        }
    }
}

impl SubscriptionSessionFactory for EventSubscriptionRuntime {
    fn open_session(
        &self,
        subscription_id: &str,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError> {
        let session = EventStreamSession::open_from_registry(
            self.store.clone(),
            &self.registry,
            self.config,
            subscription_id,
            resume_cursor,
            client_window,
            control_rx,
        )?;
        Ok(Box::new(session))
    }
}

/// Store-backed event-category subscription session.
pub struct EventStreamSession {
    store: SubscriptionStore,
    subscription_id: String,
    route: RouteBinding,
    region: Region,
    config: SubscriptionRuntimeConfig,
    wake: Subscription,
    phase: SessionPhase,
    resume_after: Option<u64>,
    cursor_before_next: EventStreamCursorV1,
    delivery_index: u64,
    last_sent_delivery_index: u64,
    last_acked_delivery_index: u64,
    last_delivered_cursor: Option<RuntimeCursor>,
    last_acked_cursor: Option<RuntimeCursor>,
    sent_cursors: BTreeMap<u64, RuntimeCursor>,
    last_watermarked_visible_seq: u64,
    control_rx: Receiver<SessionControl>,
    terminal: Option<SessionDelivery>,
}

impl SubscriptionSession for EventStreamSession {
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        Self::poll(self, timeout)
    }
}

impl EventStreamSession {
    /// Open an event-category subscription session.
    ///
    /// # Errors
    /// Registry, cursor, or store subscription failures.
    pub fn open(
        store: SubscriptionStore,
        params: EventSessionOpenParams,
        config: SubscriptionRuntimeConfig,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Self, SubscriptionRuntimeError> {
        let region =
            Region::all().with_fact_category(EventCategory::new(params.category).map_err(
                |_| SubscriptionRuntimeError::CursorInvalid {
                    reason: "event category out of range",
                },
            )?);
        let wake = store.inner.subscribe_lossy(&region);
        let parsed_resume =
            parse_resume_cursor(&params.subscription_id, params.category, resume_cursor)?;
        let queue_cap = queue_capacity(
            client_window,
            config.server_max_window,
            params.backpressure_capacity,
        );
        validate_open_limits(config, client_window, queue_cap)?;
        Ok(Self {
            store,
            subscription_id: params.subscription_id,
            route: RouteBinding {
                category: params.category,
                wire_payload_schema_ref: params.wire_payload_schema_ref,
                inner_event_payload_schema_ref: params.inner_event_payload_schema_ref,
                queue_cap,
            },
            region,
            config,
            wake,
            phase: SessionPhase::Replaying,
            resume_after: parsed_resume.resume_after_global_sequence(),
            cursor_before_next: parsed_resume,
            delivery_index: 1,
            last_sent_delivery_index: 0,
            last_acked_delivery_index: 0,
            last_delivered_cursor: None,
            last_acked_cursor: None,
            sent_cursors: BTreeMap::new(),
            last_watermarked_visible_seq: 0,
            control_rx,
            terminal: None,
        })
    }

    /// Open an event-category subscription session from a registry lookup.
    ///
    /// # Errors
    /// Registry, cursor, or store subscription failures.
    pub fn open_from_registry(
        store: SubscriptionStore,
        registry: &SubscriptionRegistry,
        config: SubscriptionRuntimeConfig,
        subscription_id: &str,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Self, SubscriptionRuntimeError> {
        let route = registry.get(subscription_id).ok_or_else(|| {
            SubscriptionRuntimeError::UnknownSubscription {
                id: subscription_id.to_owned(),
            }
        })?;
        let SubscriptionRoute::EventCategory {
            category,
            wire_payload_schema_ref,
            inner_event_payload_schema_ref,
            backpressure_capacity,
        } = route
        else {
            return Err(SubscriptionRuntimeError::UnknownSubscription {
                id: subscription_id.to_owned(),
            });
        };
        Self::open(
            store,
            EventSessionOpenParams {
                subscription_id: subscription_id.to_owned(),
                category: *category,
                wire_payload_schema_ref: wire_payload_schema_ref.clone(),
                inner_event_payload_schema_ref: inner_event_payload_schema_ref.clone(),
                backpressure_capacity: *backpressure_capacity,
            },
            config,
            resume_cursor,
            client_window,
            control_rx,
        )
    }

    /// Apply one client control message, if any is pending.
    ///
    /// # Errors
    /// Invalid ACK or cursor state transitions the session to a terminal error.
    pub fn drain_control(&mut self) -> Result<(), SubscriptionRuntimeError> {
        loop {
            match self.control_rx.try_recv() {
                Ok(control) => self.apply_control(control)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }

    /// Poll the session for the next delivery frame.
    ///
    /// # Errors
    /// Store query or envelope encoding failures while delivering replay/live events.
    pub fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        if let Some(delivery) = self.terminal.take() {
            return Ok(SessionPoll::Delivery(delivery));
        }
        if matches!(self.phase, SessionPhase::Ended) {
            return Ok(SessionPoll::Ended);
        }
        self.drain_control()?;
        if let Some(delivery) = self.terminal.take() {
            return Ok(SessionPoll::Delivery(delivery));
        }
        if matches!(self.phase, SessionPhase::Ended) {
            return Ok(SessionPoll::Ended);
        }
        if let Some(delivery) = self.try_deliver_page()? {
            return Ok(SessionPoll::Delivery(delivery));
        }
        if matches!(self.phase, SessionPhase::Replaying) {
            if let Some(delivery) = self.maybe_emit_watermark()? {
                self.phase = SessionPhase::Live;
                return Ok(SessionPoll::Delivery(delivery));
            }
            self.phase = SessionPhase::Live;
        }
        match self.wake.filtered_receiver().recv_timeout(timeout) {
            Ok(_) => {
                self.drain_control()?;
                if let Some(delivery) = self.terminal.take() {
                    return Ok(SessionPoll::Delivery(delivery));
                }
                if matches!(self.phase, SessionPhase::Ended) {
                    return Ok(SessionPoll::Ended);
                }
                if let Some(delivery) = self.try_deliver_page()? {
                    return Ok(SessionPoll::Delivery(delivery));
                }
                if let Some(delivery) = self.maybe_emit_watermark()? {
                    return Ok(SessionPoll::Delivery(delivery));
                }
                Ok(SessionPoll::Blocked)
            }
            Err(RecvTimeoutError::Timeout) => Ok(SessionPoll::Blocked),
            Err(RecvTimeoutError::Disconnected) => Ok(SessionPoll::Blocked),
        }
    }

    fn apply_control(&mut self, control: SessionControl) -> Result<(), SubscriptionRuntimeError> {
        match control {
            SessionControl::Ack {
                delivery_index,
                cursor,
            } => self.apply_ack(delivery_index, &cursor)?,
            SessionControl::Cancel => {
                self.phase = SessionPhase::Ended;
                self.terminal = Some(client_cancel_end(
                    &self.subscription_id,
                    self.last_delivered_cursor.clone(),
                ));
            }
            SessionControl::Disconnected => {
                self.phase = SessionPhase::Ended;
                self.terminal = None;
            }
            SessionControl::Malformed => {
                self.phase = SessionPhase::Ended;
                self.terminal = Some(malformed_control_error(
                    &self.subscription_id,
                    self.last_delivered_cursor.clone(),
                    self.last_acked_cursor.clone(),
                ));
            }
        }
        Ok(())
    }

    fn apply_ack(
        &mut self,
        delivery_index: u64,
        cursor: &RuntimeCursor,
    ) -> Result<(), SubscriptionRuntimeError> {
        if delivery_index == 0 || delivery_index > self.last_sent_delivery_index {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(ack_invalid_error(
                &self.subscription_id,
                "ack delivery index out of range",
                self.last_delivered_cursor.clone(),
                self.last_acked_cursor.clone(),
            ));
            return Ok(());
        }
        let decoded = match EventStreamCursorV1::decode(cursor.as_bytes()) {
            Ok(cursor) => cursor,
            Err(SubscriptionRuntimeError::CursorInvalid { reason }) => {
                self.phase = SessionPhase::Ended;
                self.terminal = Some(ack_invalid_error(
                    &self.subscription_id,
                    reason,
                    self.last_delivered_cursor.clone(),
                    self.last_acked_cursor.clone(),
                ));
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if let Err(SubscriptionRuntimeError::CursorMismatch { reason }) =
            decoded.validate_route(&self.subscription_id, self.route.category)
        {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(cursor_mismatch_terminal(
                &self.subscription_id,
                reason,
                self.last_delivered_cursor.clone(),
                self.last_acked_cursor.clone(),
            ));
            return Ok(());
        }
        let Some(expected) = self.sent_cursors.get(&delivery_index) else {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(ack_invalid_error(
                &self.subscription_id,
                "ack delivery index has no sent cursor",
                self.last_delivered_cursor.clone(),
                self.last_acked_cursor.clone(),
            ));
            return Ok(());
        };
        if expected.as_bytes() != cursor.as_bytes() {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(ack_invalid_error(
                &self.subscription_id,
                "ack cursor does not match sent cursor",
                self.last_delivered_cursor.clone(),
                self.last_acked_cursor.clone(),
            ));
            return Ok(());
        }
        if delivery_index < self.last_acked_delivery_index {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(ack_invalid_error(
                &self.subscription_id,
                "ack delivery index regressed",
                self.last_delivered_cursor.clone(),
                self.last_acked_cursor.clone(),
            ));
            return Ok(());
        }
        self.last_acked_delivery_index = delivery_index;
        self.last_acked_cursor = Some(cursor.clone());
        Ok(())
    }

    fn try_deliver_page(&mut self) -> Result<Option<SessionDelivery>, SubscriptionRuntimeError> {
        let entries = self.store.inner.query_entries_after(
            &self.region,
            self.resume_after,
            self.config.query_page_size,
        );
        for entry in &entries {
            let cursor_after = EventStreamCursorV1::after_global_sequence(
                &self.subscription_id,
                self.route.category,
                entry.global_sequence(),
                entry.wall_ms(),
            );
            // Key-aware delivery read: decrypt at the core boundary. A crypto-shredded
            // event yields `None` — skip it LOUDLY and advance the cursor past it so
            // ordering stays coherent (never stall, never ship the ciphertext).
            let Some(stored) = read_delivery_stored(self.store.inner.as_ref(), entry.event_id())?
            else {
                warn_shredded_delivery("event_stream", &self.subscription_id, entry.event_id());
                self.resume_after = Some(entry.global_sequence());
                self.cursor_before_next = cursor_after;
                continue;
            };
            if self.in_flight() >= self.route.queue_cap {
                self.phase = SessionPhase::Ended;
                let error = SessionDelivery::Error(slow_consumer_error(
                    &self.subscription_id,
                    self.last_delivered_cursor.clone(),
                    self.last_acked_cursor.clone(),
                ));
                self.terminal = Some(error.clone());
                return Ok(Some(error));
            }
            let cursor_before = self.cursor_before_next.clone();
            let envelope_bytes = event_stream_envelope_bytes_from_stored(
                &self.subscription_id,
                entry,
                &stored,
                self.route.inner_event_payload_schema_ref.as_deref(),
            )?;
            let delivery_index = self.delivery_index;
            self.delivery_index += 1;
            self.last_sent_delivery_index = delivery_index;
            let cursor_after_runtime = runtime_cursor(&cursor_after);
            self.sent_cursors
                .insert(delivery_index, cursor_after_runtime.clone());
            self.last_delivered_cursor = Some(cursor_after_runtime.clone());
            self.cursor_before_next = cursor_after;
            self.resume_after = Some(entry.global_sequence());
            return Ok(Some(SessionDelivery::Event(SessionEventDelivery {
                subscription_id: self.subscription_id.clone(),
                delivery_index,
                cursor_before: runtime_cursor(&cursor_before),
                cursor_after: cursor_after_runtime,
                wire_payload_schema_ref: self.route.wire_payload_schema_ref.clone(),
                envelope_bytes,
            })));
        }
        Ok(None)
    }

    fn maybe_emit_watermark(
        &mut self,
    ) -> Result<Option<SessionDelivery>, SubscriptionRuntimeError> {
        let visible = self.store.inner.frontier().visible_hlc;
        if visible.global_sequence <= self.last_watermarked_visible_seq {
            return Ok(None);
        }
        if self.in_flight() >= self.route.queue_cap {
            self.phase = SessionPhase::Ended;
            let error = SessionDelivery::Error(slow_consumer_error(
                &self.subscription_id,
                self.last_delivered_cursor.clone(),
                self.last_acked_cursor.clone(),
            ));
            self.terminal = Some(error.clone());
            return Ok(Some(error));
        }
        self.last_watermarked_visible_seq = visible.global_sequence;
        let cursor_after = EventStreamCursorV1::after_global_sequence(
            &self.subscription_id,
            self.route.category,
            visible.global_sequence,
            visible.wall_ms,
        );
        let delivery_index = self.delivery_index;
        self.delivery_index += 1;
        self.last_sent_delivery_index = delivery_index;
        let cursor_after_runtime = runtime_cursor(&cursor_after);
        self.sent_cursors
            .insert(delivery_index, cursor_after_runtime.clone());
        self.last_delivered_cursor = Some(cursor_after_runtime.clone());
        self.cursor_before_next = cursor_after;
        self.resume_after = Some(visible.global_sequence);
        Ok(Some(SessionDelivery::Watermark(SessionWatermarkDelivery {
            subscription_id: self.subscription_id.clone(),
            delivery_index,
            cursor_after: cursor_after_runtime,
        })))
    }

    fn in_flight(&self) -> u64 {
        self.last_sent_delivery_index
            .saturating_sub(self.last_acked_delivery_index)
    }
}

fn parse_resume_cursor(
    subscription_id: &str,
    category: u8,
    resume_cursor: Option<&[u8]>,
) -> Result<EventStreamCursorV1, SubscriptionRuntimeError> {
    match resume_cursor {
        None => Ok(EventStreamCursorV1::beginning(subscription_id, category)),
        Some(bytes) => {
            let cursor = EventStreamCursorV1::decode(bytes)?;
            cursor.validate_route(subscription_id, category)?;
            Ok(cursor)
        }
    }
}

fn runtime_cursor(cursor: &EventStreamCursorV1) -> RuntimeCursor {
    RuntimeCursor::from_bytes(cursor.encode().to_vec())
}
