use super::{AppendSubmission, AppendTicket, BatchAppendTicket, WriterCommand, WriterHandle};
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::{BatchAppendItem, Open, Store, StoreError};
use serde::Serialize;

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
        let event = submission.build_event(payload, kind, self.runtime.now_us())?;
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
        let writer = self.writer.as_ref().ok_or(StoreError::WriterCrashed)?;
        writer.fail_if_exited()?;
        Ok(writer)
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
