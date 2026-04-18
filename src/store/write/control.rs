use super::writer::{AppendGuards, WriterCommand, WriterHandle};
use crate::coordinate::Coordinate;
use crate::event::{EventKind, EventPayload};
use crate::store::append::checked_payload_len;
use crate::store::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, Open, Store, StoreError,
};

type AppendReply = Result<AppendReceipt, StoreError>;
type BatchAppendReply = Result<Vec<AppendReceipt>, StoreError>;
use crate::event::{Event, EventHeader};
use serde::Serialize;

struct Ticket<T> {
    rx: flume::Receiver<Result<T, StoreError>>,
}

impl<T> Ticket<T> {
    fn new(rx: flume::Receiver<Result<T, StoreError>>) -> Self {
        Self { rx }
    }

    fn wait(self) -> Result<T, StoreError> {
        self.rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    fn try_check(&self) -> Option<Result<T, StoreError>> {
        match self.rx.try_recv() {
            Ok(value) => Some(value),
            Err(flume::TryRecvError::Disconnected) => Some(Err(StoreError::WriterCrashed)),
            Err(flume::TryRecvError::Empty) => None,
        }
    }

    fn receiver(&self) -> &flume::Receiver<Result<T, StoreError>> {
        &self.rx
    }
}

/// Nonblocking handle for a single append result.
pub struct AppendTicket {
    inner: Ticket<AppendReceipt>,
}

impl AppendTicket {
    pub(crate) fn new(rx: flume::Receiver<AppendReply>) -> Self {
        Self {
            inner: Ticket::new(rx),
        }
    }

    /// Wait for the writer to finish this append.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the writer exits before sending
    /// a reply, or any append error returned by the writer.
    pub fn wait(self) -> AppendReply {
        self.inner.wait()
    }

    /// Check whether the append result is ready without blocking.
    pub fn try_check(&self) -> Option<AppendReply> {
        self.inner.try_check()
    }

    /// Expose the underlying receiver for optional async interop.
    pub fn receiver(&self) -> &flume::Receiver<AppendReply> {
        self.inner.receiver()
    }
}

/// Nonblocking handle for a batch append result.
pub struct BatchAppendTicket {
    inner: Ticket<Vec<AppendReceipt>>,
}

impl BatchAppendTicket {
    pub(crate) fn new(rx: flume::Receiver<BatchAppendReply>) -> Self {
        Self {
            inner: Ticket::new(rx),
        }
    }

    /// Wait for the writer to finish this batch.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the writer exits before sending
    /// a reply, or any batch-append error returned by the writer.
    pub fn wait(self) -> BatchAppendReply {
        self.inner.wait()
    }

    /// Check whether the batch result is ready without blocking.
    pub fn try_check(&self) -> Option<BatchAppendReply> {
        self.inner.try_check()
    }

    /// Expose the underlying receiver for optional async interop.
    pub fn receiver(&self) -> &flume::Receiver<BatchAppendReply> {
        self.inner.receiver()
    }
}

/// Producer-side staging buffer for batch submission.
pub struct Outbox<'a> {
    store: &'a Store<Open>,
    fence_token: Option<u64>,
    items: Vec<BatchAppendItem>,
}

impl<'a> Outbox<'a> {
    pub(crate) fn new(store: &'a Store<Open>, fence_token: Option<u64>) -> Self {
        Self {
            store,
            fence_token,
            items: Vec::new(),
        }
    }

    /// Stage a new batch item with default append options and no causation.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            kind,
            payload,
            AppendOptions::default(),
            CausationRef::None,
        )
    }

    /// Stage a new batch item with explicit append options.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_with_options(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        options: AppendOptions,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(coord, kind, payload, options, CausationRef::None)
    }

    /// Stage a new batch item with explicit causation and default append options.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_with_causation(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            kind,
            payload,
            AppendOptions::default(),
            causation,
        )
    }

    /// Stage a new batch item with explicit append options and causation.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_with_options_and_causation(
        &mut self,
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        let item = BatchAppendItem::new(coord, kind, payload, options, causation)?;
        self.items.push(item);
        Ok(self)
    }

    /// Stage a new batch item with a typed payload — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            T::KIND,
            payload,
            AppendOptions::default(),
            CausationRef::None,
        )
    }

    /// Stage a typed batch item with explicit append options — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed_with_options<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        options: AppendOptions,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(coord, T::KIND, payload, options, CausationRef::None)
    }

    /// Stage a typed batch item with explicit causation — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed_with_causation<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(
            coord,
            T::KIND,
            payload,
            AppendOptions::default(),
            causation,
        )
    }

    /// Stage a typed batch item with explicit append options and causation — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization or validation error raised while converting
    /// the payload into a staged [`BatchAppendItem`].
    pub fn stage_typed_with_options_and_causation<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<&mut Self, StoreError> {
        self.stage_with_options_and_causation(coord, T::KIND, payload, options, causation)
    }

    /// Stage a fully-formed batch item.
    pub fn push_item(&mut self, item: BatchAppendItem) -> &mut Self {
        self.items.push(item);
        self
    }

    /// Drain the staged items into a blocking batch append.
    ///
    /// Staged items are consumed from this [`Outbox`] before the enqueue/write
    /// path runs. Callers that need retry-after-error behavior must retain
    /// their own copy of the batch contents.
    ///
    /// # Errors
    /// Returns any enqueue, writer, fence, or batch-append error surfaced by
    /// the underlying flush path.
    pub fn flush(&mut self) -> Result<Vec<AppendReceipt>, StoreError> {
        let items = std::mem::take(&mut self.items);
        match self.fence_token {
            Some(token) => self.store.submit_batch_with_fence(items, token)?.wait(),
            None => self.store.append_batch(items),
        }
    }

    /// Drain the staged items into a nonblocking batch submission.
    ///
    /// Staged items are consumed from this [`Outbox`] before the submission is
    /// attempted. Callers that need retry-after-error behavior must retain
    /// their own copy of the batch contents.
    ///
    /// # Errors
    /// Returns any enqueue, writer, or fence error surfaced while turning the
    /// staged items into a batch submission ticket.
    pub fn submit_flush(&mut self) -> Result<BatchAppendTicket, StoreError> {
        let items = std::mem::take(&mut self.items);
        match self.fence_token {
            Some(token) => self.store.submit_batch_with_fence(items, token),
            None => self.store.submit_batch(items),
        }
    }

    /// Number of currently staged items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no items are staged.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Public visibility fence: writes become durable immediately but remain hidden
/// until the fence commits.
///
/// `Drop` is best-effort cancellation: it sends a `CancelVisibilityFence`
/// command to the writer without waiting for acknowledgement and logs at
/// `error` level if the send fails. For deterministic cleanup — especially
/// when the writer may have crashed — call [`VisibilityFence::cancel`]
/// explicitly instead of relying on drop.
pub struct VisibilityFence<'a> {
    store: &'a Store<Open>,
    token: u64,
    closed: bool,
}

impl<'a> VisibilityFence<'a> {
    pub(crate) fn new(store: &'a Store<Open>, token: u64) -> Self {
        Self {
            store,
            token,
            closed: false,
        }
    }

    /// Submit a root-cause append under this fence.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the fenced append.
    pub fn submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendTicket, StoreError> {
        self.store
            .submit_with_fence(coord, kind, payload, self.token)
    }

    /// Submit a reaction append under this fence.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the fenced reaction append.
    pub fn submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.store.submit_reaction_with_fence(
            coord,
            kind,
            payload,
            correlation_id,
            causation_id,
            self.token,
        )
    }

    /// Submit a typed root-cause append under this fence — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the fenced append.
    pub fn submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendTicket, StoreError> {
        self.submit(coord, T::KIND, payload)
    }

    /// Submit a typed reaction append under this fence — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the fenced reaction append.
    pub fn submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// Submit a batch append under this fence.
    ///
    /// # Errors
    /// Returns any enqueue, writer, or fence-state error surfaced while
    /// staging the fenced batch append.
    pub fn submit_batch(
        &self,
        items: Vec<BatchAppendItem>,
    ) -> Result<BatchAppendTicket, StoreError> {
        self.store.submit_batch_with_fence(items, self.token)
    }

    /// Build an outbox whose flush path uses this fence.
    pub fn outbox(&self) -> Outbox<'_> {
        Outbox::new(self.store, Some(self.token))
    }

    /// Publish all writes currently staged under this fence.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the writer exits before
    /// acknowledging the fence commit, or any fence-commit error returned by
    /// the writer.
    pub fn commit(mut self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.store
            .writer_handle()?
            .tx
            .send(WriterCommand::CommitVisibilityFence {
                token: self.token,
                respond: tx,
            })
            .map_err(|_| StoreError::WriterCrashed)?;
        self.closed = true;
        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// Cancel publication for this fence. Durable writes remain on disk but do
    /// not become visible through the index.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the writer exits before
    /// acknowledging the fence cancellation, or any fence-cancellation error
    /// returned by the writer.
    pub fn cancel(mut self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.store
            .writer_handle()?
            .tx
            .send(WriterCommand::CancelVisibilityFence {
                token: self.token,
                respond: tx,
            })
            .map_err(|_| StoreError::WriterCrashed)?;
        self.closed = true;
        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }
}

impl Drop for VisibilityFence<'_> {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let Some(writer) = self.store.writer.as_ref() else {
            return;
        };
        let (tx, _rx) = flume::bounded(1);
        // D4: best-effort cancel on drop. We do not wait for the writer's
        // ack here — doing so would turn every fence drop into a
        // synchronization point, and a dropped `VisibilityFence` is by
        // definition not on the hot correctness path (callers who need
        // correctness call `commit()` or `cancel()` explicitly). A send
        // failure here means the writer channel is already down, so the
        // writer has crashed or shut down — log that condition so
        // operators see it.
        match writer.tx.send(WriterCommand::CancelVisibilityFence {
            token: self.token,
            respond: tx,
        }) {
            Ok(()) => {}
            Err(_) => {
                tracing::error!(
                    fence_token = ?self.token,
                    "visibility-fence cancel send failed on drop; writer likely crashed — \
                     explicit cancel() recommended for deterministic cleanup"
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AppendSubmission {
    event_id: u128,
    correlation_id: u128,
    options: AppendOptions,
    fence_token: Option<u64>,
}

impl AppendSubmission {
    pub(crate) fn root() -> Self {
        let event_id = crate::id::generate_v7_id();
        Self {
            event_id,
            correlation_id: event_id,
            options: AppendOptions::default(),
            fence_token: None,
        }
    }

    pub(crate) fn root_under_fence(token: u64) -> Self {
        Self {
            fence_token: Some(token),
            ..Self::root()
        }
    }

    pub(crate) fn reaction(correlation_id: u128, causation_id: u128) -> Self {
        let event_id = crate::id::generate_v7_id();
        Self {
            event_id,
            correlation_id,
            options: AppendOptions {
                causation_id: (causation_id != 0).then_some(causation_id),
                ..AppendOptions::default()
            },
            fence_token: None,
        }
    }

    pub(crate) fn reaction_under_fence(
        token: u64,
        correlation_id: u128,
        causation_id: u128,
    ) -> Self {
        Self {
            fence_token: Some(token),
            ..Self::reaction(correlation_id, causation_id)
        }
    }

    pub(crate) fn with_options(options: AppendOptions) -> Self {
        let event_id = options
            .idempotency_key
            .unwrap_or_else(crate::id::generate_v7_id);
        Self {
            event_id,
            correlation_id: options.correlation_id.unwrap_or(event_id),
            options,
            fence_token: None,
        }
    }

    fn validate_route(self, store: &Store<Open>) -> Result<(), StoreError> {
        if self.fence_token.is_none() {
            store.ensure_no_active_public_fence()?;
        }
        Ok(())
    }

    fn validate_idempotency(self, store: &Store<Open>) -> Result<(), StoreError> {
        if store.runtime.require_idempotency_keys && self.options.idempotency_key.is_none() {
            return Err(StoreError::IdempotencyRequired);
        }
        Ok(())
    }

    fn build_event(
        self,
        payload: &impl Serialize,
        kind: EventKind,
        now_us: i64,
    ) -> Result<Event<Vec<u8>>, StoreError> {
        let payload_bytes =
            rmp_serde::to_vec_named(payload).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        let payload_len = checked_payload_len(&payload_bytes)?;
        let mut header = EventHeader::new(
            self.event_id,
            self.correlation_id,
            self.options.causation_id,
            now_us,
            crate::coordinate::DagPosition::root(),
            payload_len,
            kind,
        );
        if self.options.flags != 0 {
            header = header.with_flags(self.options.flags);
        }
        Ok(Event::new(header, payload_bytes))
    }

    fn guards(self) -> AppendGuards {
        let position_hint = self.options.position_hint.unwrap_or_default();
        AppendGuards {
            correlation_id: self.correlation_id,
            causation_id: self.options.causation_id,
            expected_sequence: self.options.expected_sequence,
            idempotency_key: self.options.idempotency_key,
            dag_lane: position_hint.lane,
            dag_depth: position_hint.depth,
        }
    }

    fn into_command(
        self,
        coord: Coordinate,
        kind: EventKind,
        event: Event<Vec<u8>>,
        respond: flume::Sender<Result<AppendReceipt, StoreError>>,
    ) -> WriterCommand {
        let guards = self.guards();
        match self.fence_token {
            Some(token) => WriterCommand::FenceAppend {
                token,
                coord,
                event: Box::new(event),
                kind,
                guards,
                respond,
            },
            None => WriterCommand::Append {
                coord,
                event: Box::new(event),
                kind,
                guards,
                respond,
            },
        }
    }
}

impl Store<Open> {
    pub(crate) fn submit_batch_with_fence(
        &self,
        items: Vec<BatchAppendItem>,
        token: u64,
    ) -> Result<BatchAppendTicket, StoreError> {
        self.submit_batch_with_fence_impl(items, Some(token))
    }

    pub(crate) fn submit_batch_with_fence_impl(
        &self,
        items: Vec<BatchAppendItem>,
        token: Option<u64>,
    ) -> Result<BatchAppendTicket, StoreError> {
        let (tx, rx) = flume::bounded(1);
        let command = match token {
            Some(token) => WriterCommand::FenceAppendBatch {
                token,
                items,
                respond: tx,
            },
            None => WriterCommand::AppendBatch { items, respond: tx },
        };
        self.writer_handle()?
            .tx
            .send(command)
            .map_err(|_| StoreError::WriterCrashed)?;
        Ok(BatchAppendTicket::new(rx))
    }

    pub(crate) fn submit_prepared(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        submission: AppendSubmission,
    ) -> Result<AppendTicket, StoreError> {
        submission.validate_route(self)?;
        submission.validate_idempotency(self)?;
        let event = submission.build_event(payload, kind, self.config.now_us())?;
        if event.payload.len() > self.config.single_append_max_bytes as usize {
            return Err(StoreError::Configuration(format!(
                "single append bytes {} exceeds max {}",
                event.payload.len(),
                self.config.single_append_max_bytes
            )));
        }

        let (tx, rx) = flume::bounded(1);
        let command = submission.into_command(coord.clone(), kind, event, tx);
        self.writer_handle()?
            .tx
            .send(command)
            .map_err(|_| StoreError::WriterCrashed)?;

        Ok(AppendTicket::new(rx))
    }

    pub(crate) fn writer_handle(&self) -> Result<&WriterHandle, StoreError> {
        self.writer.as_ref().ok_or(StoreError::WriterCrashed)
    }

    pub(crate) fn ensure_no_active_public_fence(&self) -> Result<(), StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Err(StoreError::VisibilityFenceActive);
        }
        Ok(())
    }

    pub(crate) fn submit_with_fence(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        token: u64,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::root_under_fence(token),
        )
    }

    pub(crate) fn submit_reaction_with_fence(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
        token: u64,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::reaction_under_fence(token, correlation_id, causation_id),
        )
    }

    pub(crate) fn submit_pressure_gate(&self) -> Option<crate::outcome::Outcome<AppendTicket>> {
        let writer = self.writer.as_ref()?;
        self.pressure_retry_outcome(writer.tx.len())
    }

    pub(crate) fn submit_pressure_gate_batch(
        &self,
    ) -> Option<crate::outcome::Outcome<BatchAppendTicket>> {
        let writer = self.writer.as_ref()?;
        self.pressure_retry_outcome(writer.tx.len())
    }

    pub(crate) fn pressure_retry_threshold(&self) -> usize {
        self.runtime.pressure_retry_threshold
    }

    fn pressure_retry_outcome<T>(&self, queued: usize) -> Option<crate::outcome::Outcome<T>> {
        if queued < self.pressure_retry_threshold() {
            return None;
        }

        Some(crate::outcome::Outcome::retry(
            10,
            1,
            1,
            format!(
                "writer mailbox at {queued}/{} queued commands",
                self.config.writer.channel_capacity
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaction_with_zero_causation_yields_none() {
        let submission = AppendSubmission::reaction(42, 0);
        assert_eq!(
            submission.options.causation_id, None,
            "causation_id=0 is the wire sentinel — reaction() must not produce Some(0)"
        );
    }

    #[test]
    fn reaction_with_nonzero_causation_is_preserved() {
        let submission = AppendSubmission::reaction(42, 99);
        assert_eq!(submission.options.causation_id, Some(99));
    }
}
