use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::thread;
use std::time::Duration;

use batpak::event::sourcing::EventSourced;
use batpak::store::Cursor;
use batpak::store::ProjectionWatcher;
use batpak::store::ReplayInput;
use flume::{Receiver, RecvTimeoutError, TryRecvError};

use super::config::SubscriptionRuntimeConfig;
use super::cursor::ProjectionStreamCursorV1;
use super::envelope::ProjectionStreamEnvelopeV1;
use super::error::SubscriptionRuntimeError;
use super::operation_status_stream::OperationStatusStreamSession;
use super::projector::{ProjectionProjector, ProjectionRouteBinding};
use super::registry::{SubscriptionRegistry, SubscriptionRoute};
use super::session::{
    ack_invalid_error, client_cancel_end, cursor_mismatch_terminal, malformed_control_error,
    queue_capacity, slow_consumer_error, validate_open_limits, RuntimeCursor, SessionControl,
    SessionDelivery, SessionError, SessionEventDelivery, SessionPoll, SessionWatermarkDelivery,
    SubscriptionSession, SubscriptionSessionFactory, SubscriptionStore,
};

enum SessionPhase {
    Live,
    Ended,
}

struct RouteBinding {
    projection_id: String,
    entity: String,
    wire_payload_schema_ref: String,
    inner_projection_schema_ref: Option<String>,
    freshness: batpak::store::Freshness,
    queue_cap: u64,
}

type ProjectionUpdate<T> = Result<(u64, Option<T>), batpak::store::CursorWatcherError>;
type ProjectionUpdateRx<T> = Receiver<ProjectionUpdate<T>>;

/// Store-backed projection subscription session.
pub struct ProjectionStreamSession<T> {
    subscription_id: String,
    route: RouteBinding,
    _config: SubscriptionRuntimeConfig,
    resume_after_generation: u64,
    cursor_before_next: ProjectionStreamCursorV1,
    delivery_index: u64,
    last_sent_delivery_index: u64,
    last_acked_delivery_index: u64,
    last_delivered_cursor: Option<RuntimeCursor>,
    last_acked_cursor: Option<RuntimeCursor>,
    sent_cursors: BTreeMap<u64, RuntimeCursor>,
    control_rx: Receiver<SessionControl>,
    terminal: Option<SessionDelivery>,
    phase: SessionPhase,
    update_rx: ProjectionUpdateRx<T>,
}

impl<T> SubscriptionSession for ProjectionStreamSession<T>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    T::Input: ReplayInput,
{
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        Self::poll(self, timeout)
    }
}

impl<T> ProjectionStreamSession<T>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    T::Input: ReplayInput,
{
    /// Open a projection subscription session backed by a cursor watcher.
    ///
    /// # Errors
    /// Cursor validation or runtime configuration failures.
    pub fn open(
        _store: SubscriptionStore,
        route: ProjectionRouteBinding,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
        config: SubscriptionRuntimeConfig,
        watcher: ProjectionWatcher<T, Cursor>,
    ) -> Result<Self, SubscriptionRuntimeError> {
        let parsed_resume = parse_resume_cursor(
            &route.subscription_id,
            &route.projection_id,
            &route.entity,
            resume_cursor,
        )?;
        let resume_after_generation = parsed_resume.resume_after_entity_generation().unwrap_or(0);
        let queue_cap = queue_capacity(
            client_window,
            config.server_max_window,
            route.backpressure_capacity,
        );
        validate_open_limits(config, client_window, queue_cap)?;
        let update_rx = spawn_watcher_bridge(watcher)?;
        Ok(Self {
            subscription_id: route.subscription_id.clone(),
            route: RouteBinding {
                projection_id: route.projection_id,
                entity: route.entity,
                wire_payload_schema_ref: route.wire_payload_schema_ref,
                inner_projection_schema_ref: route.inner_projection_schema_ref,
                freshness: route.freshness,
                queue_cap,
            },
            _config: config,
            resume_after_generation,
            cursor_before_next: parsed_resume,
            delivery_index: 1,
            last_sent_delivery_index: 0,
            last_acked_delivery_index: 0,
            last_delivered_cursor: None,
            last_acked_cursor: None,
            sent_cursors: BTreeMap::new(),
            control_rx,
            terminal: None,
            phase: SessionPhase::Live,
            update_rx,
        })
    }

    /// Poll the session for the next delivery frame.
    ///
    /// # Errors
    /// Store projection or envelope encoding failures while delivering updates.
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
        match self.update_rx.recv_timeout(timeout) {
            Ok(Ok((generation, state))) => {
                if generation <= self.resume_after_generation {
                    return Ok(SessionPoll::Blocked);
                }
                self.drain_control()?;
                if let Some(delivery) = self.terminal.take() {
                    return Ok(SessionPoll::Delivery(delivery));
                }
                if matches!(self.phase, SessionPhase::Ended) {
                    return Ok(SessionPoll::Ended);
                }
                if let Some(delivery) = self.deliver_update(generation, state)? {
                    return Ok(SessionPoll::Delivery(delivery));
                }
                Ok(SessionPoll::Blocked)
            }
            Ok(Err(error)) => {
                self.phase = SessionPhase::Ended;
                let message = error.to_string();
                let terminal = SessionDelivery::Error(SessionError {
                    subscription_id: Some(self.subscription_id.clone()),
                    code: super::error::stream_code::CURSOR_INVALID,
                    last_delivered_cursor: self.last_delivered_cursor.clone(),
                    last_acked_cursor: self.last_acked_cursor.clone(),
                    message: message.into_bytes(),
                });
                self.terminal = Some(terminal.clone());
                Ok(SessionPoll::Delivery(terminal))
            }
            Err(RecvTimeoutError::Timeout) => Ok(SessionPoll::Blocked),
            Err(RecvTimeoutError::Disconnected) => {
                self.phase = SessionPhase::Ended;
                let terminal = SessionDelivery::Error(SessionError {
                    subscription_id: Some(self.subscription_id.clone()),
                    code: super::error::stream_code::CURSOR_INVALID,
                    last_delivered_cursor: self.last_delivered_cursor.clone(),
                    last_acked_cursor: self.last_acked_cursor.clone(),
                    message: b"projection watcher bridge disconnected".to_vec(),
                });
                self.terminal = Some(terminal.clone());
                Ok(SessionPoll::Delivery(terminal))
            }
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
        let decoded = match ProjectionStreamCursorV1::decode(cursor.as_bytes()) {
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
        if let Err(SubscriptionRuntimeError::CursorMismatch { reason }) = decoded.validate_route(
            &self.subscription_id,
            &self.route.projection_id,
            &self.route.entity,
        ) {
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

    fn deliver_update(
        &mut self,
        generation: u64,
        state: Option<T>,
    ) -> Result<Option<SessionDelivery>, SubscriptionRuntimeError> {
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
        let cursor_after = ProjectionStreamCursorV1::after_entity_generation(
            &self.subscription_id,
            &self.route.projection_id,
            &self.route.entity,
            generation,
        );
        let cursor_after_runtime = runtime_cursor(&cursor_after);
        let delivery_index = self.delivery_index;
        self.delivery_index += 1;
        self.last_sent_delivery_index = delivery_index;
        self.sent_cursors
            .insert(delivery_index, cursor_after_runtime.clone());
        self.last_delivered_cursor = Some(cursor_after_runtime.clone());
        self.cursor_before_next = cursor_after;
        self.resume_after_generation = generation;

        match state {
            Some(projected) => {
                let envelope_bytes = ProjectionStreamEnvelopeV1::encode(
                    &self.subscription_id,
                    &self.route.projection_id,
                    &self.route.entity,
                    generation,
                    &self.route.freshness,
                    self.route.inner_projection_schema_ref.as_deref(),
                    &projected,
                )?;
                Ok(Some(SessionDelivery::Event(SessionEventDelivery {
                    subscription_id: self.subscription_id.clone(),
                    delivery_index,
                    cursor_before: runtime_cursor(&cursor_before),
                    cursor_after: cursor_after_runtime,
                    wire_payload_schema_ref: self.route.wire_payload_schema_ref.clone(),
                    envelope_bytes,
                })))
            }
            None => Ok(Some(SessionDelivery::Watermark(SessionWatermarkDelivery {
                subscription_id: self.subscription_id.clone(),
                delivery_index,
                cursor_after: cursor_after_runtime,
            }))),
        }
    }

    fn in_flight(&self) -> u64 {
        self.last_sent_delivery_index
            .saturating_sub(self.last_acked_delivery_index)
    }
}

/// Typed projector backed by [`Store::watch_projection_with_cursor`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TypedProjectionProjector<T>(PhantomData<T>);

impl<T> TypedProjectionProjector<T> {
    /// Construct a typed projector for projection state `T`.
    #[must_use]
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> ProjectionProjector for TypedProjectionProjector<T>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    T::Input: ReplayInput,
{
    fn open(
        &self,
        store: SubscriptionStore,
        route: ProjectionRouteBinding,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
        config: SubscriptionRuntimeConfig,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError> {
        let freshness = route.freshness.clone();
        let watcher =
            store
                .inner
                .watch_projection_with_cursor::<T>(&route.entity, freshness, None)?;
        let session = ProjectionStreamSession::open(
            store,
            route,
            resume_cursor,
            client_window,
            control_rx,
            config,
            watcher,
        )?;
        Ok(Box::new(session))
    }
}

/// Store-backed composite subscription runtime dispatching event and projection routes.
#[derive(Clone)]
pub struct CompositeSubscriptionRuntime {
    store: SubscriptionStore,
    registry: SubscriptionRegistry,
    config: SubscriptionRuntimeConfig,
}

impl CompositeSubscriptionRuntime {
    /// Build a store-backed composite subscription runtime.
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

impl SubscriptionSessionFactory for CompositeSubscriptionRuntime {
    fn open_session(
        &self,
        subscription_id: &str,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError> {
        let route = self.registry.get(subscription_id).ok_or_else(|| {
            SubscriptionRuntimeError::UnknownSubscription {
                id: subscription_id.to_owned(),
            }
        })?;
        match route {
            SubscriptionRoute::EventCategory {
                category,
                wire_payload_schema_ref,
                inner_event_payload_schema_ref,
                backpressure_capacity,
            } => {
                let session = super::event_stream::EventStreamSession::open(
                    self.store.clone(),
                    super::event_stream::EventSessionOpenParams {
                        subscription_id: subscription_id.to_owned(),
                        category: *category,
                        wire_payload_schema_ref: wire_payload_schema_ref.clone(),
                        inner_event_payload_schema_ref: inner_event_payload_schema_ref.clone(),
                        backpressure_capacity: *backpressure_capacity,
                    },
                    self.config,
                    resume_cursor,
                    client_window,
                    control_rx,
                )?;
                Ok(Box::new(session))
            }
            SubscriptionRoute::Projection { projector, .. } => {
                let binding = route.projection_binding(subscription_id).ok_or_else(|| {
                    SubscriptionRuntimeError::InvalidRoute {
                        reason: "projection route missing binding",
                    }
                })?;
                projector.open(
                    self.store.clone(),
                    binding,
                    resume_cursor,
                    client_window,
                    control_rx,
                    self.config,
                )
            }
            SubscriptionRoute::OperationStatus { .. } => {
                let binding = route
                    .operation_status_binding(subscription_id)
                    .ok_or_else(|| SubscriptionRuntimeError::InvalidRoute {
                        reason: "operation-status route missing binding",
                    })?;
                let session = OperationStatusStreamSession::open(
                    &self.store,
                    binding,
                    resume_cursor,
                    client_window,
                    control_rx,
                    self.config,
                )?;
                Ok(Box::new(session))
            }
            SubscriptionRoute::ReceiptStream { .. } => {
                let session = super::receipt_stream::ReceiptStreamSession::open_from_registry(
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
            SubscriptionRoute::EntityStream { .. } => {
                let session = super::entity_stream::EntityStreamSession::open_from_registry(
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
    }
}

fn parse_resume_cursor(
    subscription_id: &str,
    projection_id: &str,
    entity: &str,
    resume_cursor: Option<&[u8]>,
) -> Result<ProjectionStreamCursorV1, SubscriptionRuntimeError> {
    match resume_cursor {
        None => Ok(ProjectionStreamCursorV1::beginning(
            subscription_id,
            projection_id,
            entity,
        )),
        Some(bytes) => {
            let cursor = ProjectionStreamCursorV1::decode(bytes)?;
            cursor.validate_route(subscription_id, projection_id, entity)?;
            Ok(cursor)
        }
    }
}

fn runtime_cursor(cursor: &ProjectionStreamCursorV1) -> RuntimeCursor {
    RuntimeCursor::from_bytes(cursor.encode().to_vec())
}

fn spawn_watcher_bridge<T>(
    mut watcher: ProjectionWatcher<T, Cursor>,
) -> Result<ProjectionUpdateRx<T>, SubscriptionRuntimeError>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    T::Input: ReplayInput,
{
    let (tx, rx) = flume::bounded(64);
    thread::Builder::new()
        .name("syncbat.projection-watch".to_owned())
        .spawn(move || loop {
            match watcher.recv() {
                Ok(update) => {
                    if tx.send(Ok(update)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                    break;
                }
            }
        })
        .map_err(|error| SubscriptionRuntimeError::Worker(error.to_string()))?;
    Ok(rx)
}
