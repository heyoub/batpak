use super::{AppendSubmission, AppendTicket, BatchAppendTicket, WriterCommand, WriterHandle};
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::append::checked_append_bytes;
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
        Self::reject_reserved_item_kinds(&items)?;
        let _lifecycle = self.lifecycle_gate.lock();
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

    /// Reject any batch item carrying a reserved system/effect/tombstone kind.
    ///
    /// Called from every batch funnel (`submit_batch` and
    /// `submit_batch_with_fence_impl`) so neither the unfenced nor the fenced
    /// batch path can smuggle a forged substrate marker.
    fn reject_reserved_item_kinds(items: &[BatchAppendItem]) -> Result<(), StoreError> {
        for (i, item) in items.iter().enumerate() {
            let kind = item.kind();
            if kind.is_reserved() {
                return Err(StoreError::ReservedKind {
                    index: Some(i),
                    kind: kind.as_raw_u16(),
                });
            }
        }
        Ok(())
    }

    /// Public single-event append funnel. Rejects reserved kinds before
    /// delegating to [`Self::submit_prepared_internal`]; every public
    /// raw-`kind` single-event path (submit, submit_reaction, append,
    /// append_with_options, the fenced variants, and the `try_submit*` family)
    /// converges here, so this is the single guard point for that surface.
    pub(crate) fn submit_prepared(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        submission: AppendSubmission,
    ) -> Result<AppendTicket, StoreError> {
        if kind.is_reserved() {
            return Err(StoreError::ReservedKind {
                index: None,
                kind: kind.as_raw_u16(),
            });
        }
        self.submit_prepared_internal(coord, kind, payload, submission)
    }

    /// Internal-only append funnel that bypasses the reserved-kind guard.
    ///
    /// This is the substrate marker constructor the hardening contract requires:
    /// it is not `pub` and is unreachable from outside the crate. The only
    /// legitimate caller emitting a reserved kind through it is
    /// `Store::append_denial` (SYSTEM_DENIAL), which must still emit its audit
    /// receipt. All other reserved-kind emitters (lifecycle/open receipts,
    /// tombstones, batch markers) build their writer commands directly and
    /// never reach this method.
    pub(crate) fn submit_prepared_internal(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        submission: AppendSubmission,
    ) -> Result<AppendTicket, StoreError> {
        let _lifecycle = self.lifecycle_gate.lock();
        submission.validate_route(self)?;
        submission.validate_idempotency(self)?;
        let event = submission.build_event(payload, kind, self.runtime.now_us())?;
        let append_bytes =
            checked_append_bytes(event.payload.len(), submission.receipt_extensions())?;
        if append_bytes > self.config.single_append_max_bytes as usize {
            return Err(StoreError::Configuration(format!(
                "single append bytes {} exceeds max {}",
                append_bytes, self.config.single_append_max_bytes
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
        let writer = &self.state.0;
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
            AppendSubmission::root_under_fence(token, self.runtime.clock()),
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
            AppendSubmission::reaction_under_fence(
                token,
                self.runtime.clock(),
                correlation_id,
                causation_id,
            ),
        )
    }

    pub(crate) fn submit_pressure_gate(&self) -> Option<crate::outcome::Outcome<AppendTicket>> {
        self.pressure_retry_outcome(self.state.0.tx.len())
    }

    pub(crate) fn submit_pressure_gate_batch(
        &self,
    ) -> Option<crate::outcome::Outcome<BatchAppendTicket>> {
        self.pressure_retry_outcome(self.state.0.tx.len())
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
    use crate::store::StoreConfig;
    use tempfile::TempDir;

    #[test]
    fn pressure_retry_threshold_reflects_validated_config() {
        let dir = TempDir::new().expect("tempdir");
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_segment_max_bytes(4096)
                .with_writer_channel_capacity(10)
                .with_writer_pressure_retry_threshold_pct(60),
        )
        .expect("open store");
        assert_eq!(store.pressure_retry_threshold(), 6);
        assert!(store.submit_pressure_gate().is_none());
        assert!(
            store
                .pressure_retry_outcome::<BatchAppendTicket>(5)
                .is_none(),
            "PROPERTY: queued commands below the retry threshold must pass without retry advice"
        );
        assert!(
            store
                .pressure_retry_outcome::<BatchAppendTicket>(6)
                .is_some(),
            "PROPERTY: queued commands exactly at the retry threshold must produce retry advice; \
             a <= comparison waits one command too long"
        );
        store.close().expect("close store");
    }

    #[test]
    fn fenced_batch_rejects_reserved_item_kind() {
        use crate::coordinate::Coordinate;
        use crate::store::append::{BatchAppendItem, CausationRef};
        use crate::store::AppendOptions;

        let dir = TempDir::new().expect("tempdir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = Coordinate::new("entity:fenced-reserved", "scope:test").expect("coord");
        let payload = serde_json::json!({"forged": true});

        let fence = store
            .begin_visibility_fence()
            .expect("begin visibility fence");
        let forged = BatchAppendItem::new(
            coord,
            EventKind::SYSTEM_BATCH_BEGIN,
            &payload,
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("build forged batch item");
        let result = fence.submit_batch(vec![forged]);
        assert!(
            matches!(
                result,
                Err(StoreError::ReservedKind { index: Some(0), kind })
                    if kind == EventKind::SYSTEM_BATCH_BEGIN.as_raw_u16()
            ),
            "PROPERTY: submit_batch_with_fence_impl must reject reserved-kind items with \
             ReservedKind {{ index: Some(0) }}"
        );
        fence.cancel().expect("cancel fence");
        store.close().expect("close store");
    }
}
