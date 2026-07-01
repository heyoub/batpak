use std::collections::BTreeMap;
use std::time::Duration;

use batpak::coordinate::Region;
use batpak::store::Subscription;
use flume::{Receiver, RecvTimeoutError, TryRecvError};

use super::config::SubscriptionRuntimeConfig;
use super::cursor::EntityStreamCursorV1;
use super::envelope::{
    entity_stream_envelope_bytes_from_stored, read_delivery_stored, warn_shredded_delivery,
};
use super::error::SubscriptionRuntimeError;
use super::registry::{EntityStreamRouteBinding, SubscriptionRegistry, SubscriptionRoute};
use super::session::{
    ack_invalid_error, client_cancel_end, cursor_mismatch_terminal, malformed_control_error,
    queue_capacity, slow_consumer_error, validate_open_limits, RuntimeCursor, SessionControl,
    SessionDelivery, SessionEventDelivery, SessionPoll, SessionWatermarkDelivery,
    SubscriptionSession, SubscriptionStore,
};

enum SessionPhase {
    Replaying,
    Live,
    Ended,
}

struct RouteBinding {
    entity: String,
    scope: String,
    wire_payload_schema_ref: String,
    inner_event_payload_schema_ref: Option<String>,
    queue_cap: u64,
}

/// Store-backed entity-stream subscription session.
pub struct EntityStreamSession {
    store: SubscriptionStore,
    subscription_id: String,
    route: RouteBinding,
    region: Region,
    config: SubscriptionRuntimeConfig,
    wake: Subscription,
    phase: SessionPhase,
    scan_after_global_sequence: Option<u64>,
    cursor_before_next: EntityStreamCursorV1,
    delivery_index: u64,
    last_sent_delivery_index: u64,
    last_acked_delivery_index: u64,
    last_delivered_cursor: Option<RuntimeCursor>,
    last_acked_cursor: Option<RuntimeCursor>,
    sent_cursors: BTreeMap<u64, RuntimeCursor>,
    last_watermarked_visible_seq: u64,
    last_query_returned_empty: bool,
    control_rx: Receiver<SessionControl>,
    terminal: Option<SessionDelivery>,
}

impl SubscriptionSession for EntityStreamSession {
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        Self::poll(self, timeout)
    }
}

impl EntityStreamSession {
    /// Open an entity-stream subscription session.
    ///
    /// # Errors
    /// Registry, cursor, or store subscription failures.
    pub fn open(
        store: SubscriptionStore,
        binding: EntityStreamRouteBinding,
        config: SubscriptionRuntimeConfig,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Self, SubscriptionRuntimeError> {
        let region = Region::entity(&binding.entity).with_scope(&binding.scope);
        let wake = store.inner.subscribe_lossy(&region);
        let parsed_resume = parse_resume_cursor(
            &binding.subscription_id,
            &binding.entity,
            &binding.scope,
            resume_cursor,
        )?;
        let queue_cap = queue_capacity(
            client_window,
            config.server_max_window,
            binding.backpressure_capacity,
        );
        validate_open_limits(config, client_window, queue_cap)?;
        let resume_after = parsed_resume.resume_after_global_sequence();
        Ok(Self {
            store,
            subscription_id: binding.subscription_id,
            route: RouteBinding {
                entity: binding.entity,
                scope: binding.scope,
                wire_payload_schema_ref: binding.wire_payload_schema_ref,
                inner_event_payload_schema_ref: binding.inner_event_payload_schema_ref,
                queue_cap,
            },
            region,
            config,
            wake,
            phase: SessionPhase::Replaying,
            scan_after_global_sequence: resume_after,
            cursor_before_next: parsed_resume,
            delivery_index: 1,
            last_sent_delivery_index: 0,
            last_acked_delivery_index: 0,
            last_delivered_cursor: None,
            last_acked_cursor: None,
            sent_cursors: BTreeMap::new(),
            last_watermarked_visible_seq: resume_after.unwrap_or(0),
            last_query_returned_empty: false,
            control_rx,
            terminal: None,
        })
    }

    /// Open an entity-stream subscription session from a registry lookup.
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
        let SubscriptionRoute::EntityStream { .. } = route else {
            return Err(SubscriptionRuntimeError::UnknownSubscription {
                id: subscription_id.to_owned(),
            });
        };
        let binding = route
            .entity_stream_binding(subscription_id)
            .ok_or_else(|| SubscriptionRuntimeError::InvalidRoute {
                reason: "entity stream route missing binding",
            })?;
        Self::open(
            store,
            binding,
            config,
            resume_cursor,
            client_window,
            control_rx,
        )
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
        if let Some(delivery) = self.scan_until_delivery_or_exhaustion()? {
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
                if let Some(delivery) = self.scan_until_delivery_or_exhaustion()? {
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

    fn drain_control(&mut self) -> Result<(), SubscriptionRuntimeError> {
        loop {
            match self.control_rx.try_recv() {
                Ok(control) => self.apply_control(control)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
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
        let decoded = match EntityStreamCursorV1::decode(cursor.as_bytes()) {
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
            decoded.validate_route(&self.subscription_id, &self.route.entity, &self.route.scope)
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
        self.scan_after_global_sequence = decoded.resume_after_global_sequence();
        Ok(())
    }

    fn scan_until_delivery_or_exhaustion(
        &mut self,
    ) -> Result<Option<SessionDelivery>, SubscriptionRuntimeError> {
        loop {
            if let Some(delivery) = self.try_deliver_matching_event()? {
                return Ok(Some(delivery));
            }
            if self.last_query_returned_empty {
                return Ok(None);
            }
        }
    }

    fn try_deliver_matching_event(
        &mut self,
    ) -> Result<Option<SessionDelivery>, SubscriptionRuntimeError> {
        let entries = self.store.inner.query_entries_after(
            &self.region,
            self.scan_after_global_sequence,
            self.config.query_page_size,
        );
        if entries.is_empty() {
            self.last_query_returned_empty = true;
            return Ok(None);
        }
        self.last_query_returned_empty = false;
        for entry in entries {
            let global_sequence = entry.global_sequence();
            self.scan_after_global_sequence = Some(global_sequence);
            let coord = entry.coord();
            if coord.entity() != self.route.entity.as_str() || coord.scope() != self.route.scope {
                continue;
            }
            let cursor_after = EntityStreamCursorV1::after_global_sequence(
                &self.subscription_id,
                &self.route.entity,
                &self.route.scope,
                global_sequence,
                entry.wall_ms(),
            );
            // Key-aware delivery read: decrypt at the core boundary. A crypto-shredded
            // event yields `None` — skip it LOUDLY and advance the cursor past it so
            // ordering stays coherent (never stall, never ship the ciphertext).
            let Some(stored) = read_delivery_stored(self.store.inner.as_ref(), entry.event_id())?
            else {
                warn_shredded_delivery("entity_stream", &self.subscription_id, entry.event_id());
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
            let envelope_bytes = entity_stream_envelope_bytes_from_stored(
                &self.subscription_id,
                &entry,
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
        if !self.last_query_returned_empty {
            return Ok(None);
        }
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
        self.scan_after_global_sequence = Some(visible.global_sequence);
        let cursor_after = EntityStreamCursorV1::after_global_sequence(
            &self.subscription_id,
            &self.route.entity,
            &self.route.scope,
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
    entity: &str,
    scope: &str,
    resume_cursor: Option<&[u8]>,
) -> Result<EntityStreamCursorV1, SubscriptionRuntimeError> {
    match resume_cursor {
        None => Ok(EntityStreamCursorV1::beginning(
            subscription_id,
            entity,
            scope,
        )),
        Some(bytes) => {
            let cursor = EntityStreamCursorV1::decode(bytes)?;
            cursor.validate_route(subscription_id, entity, scope)?;
            Ok(cursor)
        }
    }
}

fn runtime_cursor(cursor: &EntityStreamCursorV1) -> RuntimeCursor {
    RuntimeCursor::from_bytes(cursor.encode().to_vec())
}
