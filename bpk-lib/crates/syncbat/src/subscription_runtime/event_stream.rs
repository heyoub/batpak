use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use batpak::coordinate::{EventCategory, Region};
use batpak::store::{Open, Store, Subscription};
use flume::{Receiver, RecvTimeoutError, TryRecvError};

use super::config::SubscriptionRuntimeConfig;
use super::cursor::EventStreamCursorV1;
use super::envelope::EventStreamEnvelopeV1;
use super::error::{stream_code, SubscriptionRuntimeError};
use super::registry::{SubscriptionRegistry, SubscriptionRoute};

/// One server-side delivery frame produced by the runtime engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionDelivery {
    /// Deliver one committed event.
    Event(SessionEventDelivery),
    /// Coalesced source-frontier watermark.
    Watermark(SessionWatermarkDelivery),
    /// Terminal stream error.
    Error(SessionError),
    /// Terminal stream end.
    End(SessionEnd),
}

/// One delivered event with cursor and envelope bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEventDelivery {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Monotonic per-session delivery index.
    pub delivery_index: u64,
    /// Cursor before this delivery.
    pub cursor_before: EventStreamCursorV1,
    /// Cursor after this delivery.
    pub cursor_after: EventStreamCursorV1,
    /// Route-declared wire payload schema ref.
    pub wire_payload_schema_ref: String,
    /// Canonical envelope bytes for `payload_hex`.
    pub envelope_bytes: Vec<u8>,
}

/// Coalesced watermark delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionWatermarkDelivery {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Monotonic per-session delivery index.
    pub delivery_index: u64,
    /// Frontier cursor after the watermark point.
    pub cursor_after: EventStreamCursorV1,
}

/// Terminal stream error delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionError {
    /// Globally unique subscription id when known.
    pub subscription_id: Option<String>,
    /// Stable error code token.
    pub code: &'static str,
    /// Optional last delivered cursor.
    pub last_delivered_cursor: Option<EventStreamCursorV1>,
    /// Optional last acknowledged cursor.
    pub last_acked_cursor: Option<EventStreamCursorV1>,
    /// UTF-8 message bytes.
    pub message: Vec<u8>,
}

/// Terminal stream end delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEnd {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Stable end reason code.
    pub reason_code: &'static str,
    /// Final cursor after stream end, if any.
    pub cursor_after: Option<EventStreamCursorV1>,
}

/// Result of one runtime poll step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionPoll {
    /// Produced one delivery frame.
    Delivery(SessionDelivery),
    /// No work available within the timeout.
    Blocked,
    /// Session has ended.
    Ended,
}

/// Runtime session polled by transport adapters.
pub trait SubscriptionSession: Send {
    /// Poll for the next delivery frame.
    ///
    /// # Errors
    /// Runtime failures while producing the next delivery.
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError>;
}

/// Factory that opens subscription sessions from wire `SUBSCRIBE` inputs.
pub trait SubscriptionSessionFactory {
    /// Open one session for a validated subscription id.
    ///
    /// # Errors
    /// Unknown subscription, invalid cursor, invalid runtime config, or store failures.
    fn open_session(
        &self,
        subscription_id: &str,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError>;
}

/// Client control input accepted after subscribe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionControl {
    /// Cumulative delivery acknowledgement.
    Ack {
        /// Highest delivered index acknowledged by the client.
        delivery_index: u64,
        /// Authoritative resume cursor after the acknowledged point.
        cursor: EventStreamCursorV1,
    },
    /// Client-initiated cancellation.
    Cancel,
    /// Peer disconnected without a semantic cancel frame.
    Disconnected,
    /// Malformed post-subscribe control frame.
    Malformed,
}

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

/// Cloneable syncbat-owned store handle for subscription runtime sessions.
#[derive(Clone)]
pub struct SubscriptionStore {
    inner: Arc<Store<Open>>,
}

impl SubscriptionStore {
    /// Wrap an open BatPak store for syncbat subscription delivery.
    #[must_use]
    pub fn new(store: Arc<Store<Open>>) -> Self {
        Self { inner: store }
    }
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
        let session = EventStreamSession::open(
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
    last_delivered_cursor: Option<EventStreamCursorV1>,
    last_acked_cursor: Option<EventStreamCursorV1>,
    sent_cursors: BTreeMap<u64, EventStreamCursorV1>,
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
        registry: &SubscriptionRegistry,
        config: SubscriptionRuntimeConfig,
        subscription_id: &str,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Self, SubscriptionRuntimeError> {
        config.validate()?;
        if client_window == 0 {
            return Err(SubscriptionRuntimeError::InvalidConfig {
                reason: "client window is zero",
            });
        }
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
        } = route;
        let category = *category;
        let region =
            Region::all().with_fact_category(EventCategory::new(category).map_err(|_| {
                SubscriptionRuntimeError::CursorInvalid {
                    reason: "event category out of range",
                }
            })?);
        let wake = store.inner.subscribe_lossy(&region);
        let parsed_resume = parse_resume_cursor(subscription_id, category, resume_cursor)?;
        let queue_cap = queue_capacity(
            client_window,
            config.server_max_window,
            *backpressure_capacity,
        );
        if queue_cap == 0 {
            return Err(SubscriptionRuntimeError::InvalidConfig {
                reason: "delivery queue capacity is zero",
            });
        }
        Ok(Self {
            store,
            subscription_id: subscription_id.to_owned(),
            route: RouteBinding {
                category,
                wire_payload_schema_ref: wire_payload_schema_ref.clone(),
                inner_event_payload_schema_ref: inner_event_payload_schema_ref.clone(),
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
                self.terminal = Some(self.client_cancel_end());
            }
            SessionControl::Disconnected => {
                self.phase = SessionPhase::Ended;
                self.terminal = None;
            }
            SessionControl::Malformed => {
                self.phase = SessionPhase::Ended;
                self.terminal = Some(self.malformed_control_error());
            }
        }
        Ok(())
    }

    fn apply_ack(
        &mut self,
        delivery_index: u64,
        cursor: &EventStreamCursorV1,
    ) -> Result<(), SubscriptionRuntimeError> {
        if delivery_index == 0 || delivery_index > self.last_sent_delivery_index {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(self.ack_invalid_error("ack delivery index out of range"));
            return Ok(());
        }
        if let Err(SubscriptionRuntimeError::CursorMismatch { reason }) =
            cursor.validate_route(&self.subscription_id, self.route.category)
        {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(self.cursor_mismatch_terminal(reason));
            return Ok(());
        }
        let Some(expected) = self.sent_cursors.get(&delivery_index) else {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(self.ack_invalid_error("ack delivery index has no sent cursor"));
            return Ok(());
        };
        if expected != cursor {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(self.ack_invalid_error("ack cursor does not match sent cursor"));
            return Ok(());
        }
        if delivery_index < self.last_acked_delivery_index {
            self.phase = SessionPhase::Ended;
            self.terminal = Some(self.ack_invalid_error("ack delivery index regressed"));
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
        if entries.is_empty() {
            return Ok(None);
        }
        let entry = &entries[0];
        if self.in_flight() >= self.route.queue_cap {
            self.phase = SessionPhase::Ended;
            let error = slow_consumer_error(self);
            self.terminal = Some(SessionDelivery::Error(error.clone()));
            return Ok(Some(SessionDelivery::Error(error)));
        }
        let cursor_before = self.cursor_before_next.clone();
        let cursor_after = EventStreamCursorV1::after_global_sequence(
            &self.subscription_id,
            self.route.category,
            entry.global_sequence(),
            entry.wall_ms(),
        );
        let envelope_bytes = EventStreamEnvelopeV1::encode_for_entry(
            self.store.inner.as_ref(),
            &self.subscription_id,
            entry,
            self.route.inner_event_payload_schema_ref.as_deref(),
        )?;
        let delivery_index = self.delivery_index;
        self.delivery_index += 1;
        self.last_sent_delivery_index = delivery_index;
        self.sent_cursors
            .insert(delivery_index, cursor_after.clone());
        self.last_delivered_cursor = Some(cursor_after.clone());
        self.cursor_before_next = cursor_after.clone();
        self.resume_after = Some(entry.global_sequence());
        Ok(Some(SessionDelivery::Event(SessionEventDelivery {
            subscription_id: self.subscription_id.clone(),
            delivery_index,
            cursor_before,
            cursor_after,
            wire_payload_schema_ref: self.route.wire_payload_schema_ref.clone(),
            envelope_bytes,
        })))
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
            let error = slow_consumer_error(self);
            self.terminal = Some(SessionDelivery::Error(error.clone()));
            return Ok(Some(SessionDelivery::Error(error)));
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
        self.sent_cursors
            .insert(delivery_index, cursor_after.clone());
        self.last_delivered_cursor = Some(cursor_after.clone());
        Ok(Some(SessionDelivery::Watermark(SessionWatermarkDelivery {
            subscription_id: self.subscription_id.clone(),
            delivery_index,
            cursor_after,
        })))
    }

    fn in_flight(&self) -> u64 {
        self.last_sent_delivery_index
            .saturating_sub(self.last_acked_delivery_index)
    }
}

fn queue_capacity(client_window: u32, server_max_window: usize, route_cap: Option<usize>) -> u64 {
    let client = u64::from(client_window);
    let server = u64::try_from(server_max_window).unwrap_or(u64::MAX);
    let route = route_cap
        .and_then(|cap| u64::try_from(cap).ok())
        .unwrap_or(server);
    client.min(server).min(route)
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

fn slow_consumer_error(session: &EventStreamSession) -> SessionError {
    SessionError {
        subscription_id: Some(session.subscription_id.clone()),
        code: stream_code::SLOW_CONSUMER,
        last_delivered_cursor: session.last_delivered_cursor.clone(),
        last_acked_cursor: session.last_acked_cursor.clone(),
        message: b"delivery window full".to_vec(),
    }
}

impl EventStreamSession {
    /// End the session due to client cancellation.
    #[must_use]
    pub fn client_cancel_end(&self) -> SessionDelivery {
        SessionDelivery::End(SessionEnd {
            subscription_id: self.subscription_id.clone(),
            reason_code: stream_code::CLIENT_CANCELLED,
            cursor_after: self.last_delivered_cursor.clone(),
        })
    }

    /// End the session due to a malformed post-subscribe control frame.
    #[must_use]
    pub fn malformed_control_error(&self) -> SessionDelivery {
        SessionDelivery::Error(SessionError {
            subscription_id: Some(self.subscription_id.clone()),
            code: stream_code::MALFORMED_STREAM_FRAME,
            last_delivered_cursor: self.last_delivered_cursor.clone(),
            last_acked_cursor: self.last_acked_cursor.clone(),
            message: b"malformed stream control frame".to_vec(),
        })
    }

    /// End the session due to an invalid ACK/cursor.
    #[must_use]
    pub fn ack_invalid_error(&self, reason: &'static str) -> SessionDelivery {
        SessionDelivery::Error(SessionError {
            subscription_id: Some(self.subscription_id.clone()),
            code: match reason {
                "ack cursor does not match sent cursor" | "ack delivery index out of range" => {
                    stream_code::MALFORMED_STREAM_FRAME
                }
                _ => stream_code::CURSOR_INVALID,
            },
            last_delivered_cursor: self.last_delivered_cursor.clone(),
            last_acked_cursor: self.last_acked_cursor.clone(),
            message: reason.as_bytes().to_vec(),
        })
    }

    fn cursor_mismatch_terminal(&self, reason: &'static str) -> SessionDelivery {
        SessionDelivery::Error(SessionError {
            subscription_id: Some(self.subscription_id.clone()),
            code: stream_code::CURSOR_MISMATCH,
            last_delivered_cursor: self.last_delivered_cursor.clone(),
            last_acked_cursor: self.last_acked_cursor.clone(),
            message: reason.as_bytes().to_vec(),
        })
    }

    /// Unknown subscription terminal error before session open.
    #[must_use]
    pub fn unknown_subscription_error(subscription_id: &str) -> SessionDelivery {
        SessionDelivery::Error(SessionError {
            subscription_id: Some(subscription_id.to_owned()),
            code: stream_code::UNKNOWN_SUBSCRIPTION,
            last_delivered_cursor: None,
            last_acked_cursor: None,
            message: subscription_id.as_bytes().to_vec(),
        })
    }

    /// Cursor invalid terminal error before session open.
    #[must_use]
    pub fn cursor_invalid_error(reason: &'static str) -> SessionDelivery {
        SessionDelivery::Error(SessionError {
            subscription_id: None,
            code: stream_code::CURSOR_INVALID,
            last_delivered_cursor: None,
            last_acked_cursor: None,
            message: reason.as_bytes().to_vec(),
        })
    }

    /// Cursor mismatch terminal error before session open.
    #[must_use]
    pub fn cursor_mismatch_error(reason: &'static str) -> SessionDelivery {
        SessionDelivery::Error(SessionError {
            subscription_id: None,
            code: stream_code::CURSOR_MISMATCH,
            last_delivered_cursor: None,
            last_acked_cursor: None,
            message: reason.as_bytes().to_vec(),
        })
    }
}
