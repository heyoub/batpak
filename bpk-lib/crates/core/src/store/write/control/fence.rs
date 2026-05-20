use super::{AppendTicket, BatchAppendTicket, Outbox, WriterCommand};
use crate::coordinate::Coordinate;
use crate::event::{EventKind, EventPayload};
use crate::store::{BatchAppendItem, Open, Store, StoreError};
use flume::TrySendError;
use serde::Serialize;

/// Public visibility fence: writes become durable immediately but remain hidden
/// until the fence commits.
///
/// `Drop` is best-effort cancellation: it tries to enqueue a
/// `CancelVisibilityFence` command without waiting for acknowledgement. If the
/// writer channel is full, drop offloads the blocking send to a detached
/// helper thread so the caller thread does not stall. For deterministic
/// cleanup — especially when the writer may have crashed — call
/// [`VisibilityFence::cancel`] explicitly instead of relying on drop.
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

    pub(crate) fn token(&self) -> u64 {
        self.token
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
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
    ) -> Result<AppendTicket, StoreError> {
        use crate::id::EntityIdType;
        self.store.submit_reaction_with_fence(
            coord,
            kind,
            payload,
            correlation_id.as_u128(),
            causation_id.as_u128(),
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
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
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
        crate::store::recv_writer_reply(&rx)
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
        crate::store::recv_writer_reply(&rx)
    }
}

impl<'a> VisibilityFence<'a> {
    /// F13: surface the `Drop`-time cancel enqueue as a `Result` so the
    /// `Drop` impl can distinguish "already closed / writer gone" (both
    /// silent) from a genuine enqueue/spawn failure (logged).
    ///
    /// Returns:
    /// * `Ok(())` when the cancel command was enqueued directly, offloaded
    ///   to a helper thread, or when there is no action to take
    ///   (`self.closed` is true, or the store is no longer holding a
    ///   writer handle).
    /// * `Err(String)` when the writer channel is disconnected or the helper
    ///   thread could not be spawned. We never panic in `Drop` under any
    ///   circumstance.
    fn try_cancel_on_drop(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        let Some(writer) = self.store.writer.as_ref() else {
            return Ok(());
        };
        let writer_tx = writer.tx.clone();
        let (tx, _rx) = flume::bounded(1);
        // D4: best-effort cancel on drop. We do not wait for the writer's
        // ack here — doing so would turn every fence drop into a
        // synchronization point, and a dropped `VisibilityFence` is by
        // definition not on the hot correctness path (callers who need
        // correctness call `commit()` or `cancel()` explicitly). Use
        // `try_send` first so drop never blocks the caller thread under
        // writer backpressure; if the channel is full, hand the blocking
        // send off to a detached helper thread.
        let command = WriterCommand::CancelVisibilityFence {
            token: self.token,
            respond: tx,
        };
        match writer_tx.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                Err("writer channel disconnected during fence drop".to_string())
            }
            Err(TrySendError::Full(command)) => std::thread::Builder::new()
                .name("batpak-fence-drop-cancel".to_string())
                .spawn(move || {
                    let _ = writer_tx.send(command);
                })
                .map(|_| ())
                .map_err(|error| format!("failed to spawn drop-cancel helper: {error}")),
        }
    }
}

impl Drop for VisibilityFence<'_> {
    fn drop(&mut self) {
        // F13: no panic in Drop under any circumstance. A send error is
        // surfaced via tracing at `error` level so operators see the
        // writer-gone condition; callers that require deterministic
        // cleanup must call `cancel()` explicitly.
        if let Err(e) = self.try_cancel_on_drop() {
            tracing::error!(
                fence_token = ?self.token,
                err = %e,
                "visibility-fence cancel enqueue failed on drop; explicit cancel() \
                 recommended for deterministic cleanup"
            );
        }
    }
}
