use super::*;

impl Store<Open> {
    /// Build a producer-side outbox for staged batch submission.
    pub fn outbox(&self) -> Outbox<'_> {
        Outbox::new(self, None)
    }

    /// Begin a public visibility fence. Only one fence may be active at a time.
    ///
    /// # Errors
    /// Returns an error if another public visibility fence is already active or
    /// if the writer cannot acknowledge the new fence.
    pub fn begin_visibility_fence(&self) -> Result<VisibilityFence<'_>, StoreError> {
        let token = self.index.begin_visibility_fence()?;
        let (tx, rx) = flume::bounded(1);
        let send_result = self
            .writer_handle()?
            .tx
            .send(WriterCommand::BeginVisibilityFence { token, respond: tx });
        if send_result.is_err() {
            let _ = self.index.cancel_visibility_fence(token);
            return Err(StoreError::WriterCrashed);
        }
        recv_writer_reply(&rx)?;
        Ok(VisibilityFence::new(self, token))
    }

    /// Snapshot the current writer mailbox pressure.
    pub fn writer_pressure(&self) -> WriterPressure {
        let writer = self.writer_ref();
        WriterPressure {
            queue_len: writer.tx.len(),
            capacity: self.config.writer.channel_capacity,
        }
    }

    /// Nonblocking root-cause append submission.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the append for background execution.
    pub fn submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(coord, kind, payload, AppendSubmission::root())
    }

    /// Nonblocking reaction append submission.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the reaction append for background execution.
    pub fn submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::reaction(correlation_id, causation_id),
        )
    }

    /// Nonblocking batch append submission.
    ///
    /// Every item's coordinate is revalidated synchronously at this entry so
    /// that invalid coordinates surface to the caller rather than being
    /// deferred to the writer thread. Each item's serialized payload is also
    /// checked against `single_append_max_bytes` (G1): a single oversized
    /// item, including encoded receipt-extension bytes, is rejected even when
    /// the batch-total cap would have allowed it.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidCoordinate`] if any item's coordinate
    /// fails validation, [`StoreError::BatchItemTooLarge`] if any item's
    /// serialized payload plus encoded receipt-extension bytes exceeds
    /// `single_append_max_bytes`, or any enqueue or writer error surfaced
    /// while staging the batch for background execution.
    pub fn submit_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<BatchAppendTicket, StoreError> {
        self.ensure_no_active_public_fence()?;
        let per_item_cap = self.config.single_append_max_bytes as usize;
        for (i, item) in items.iter().enumerate() {
            if let Err(err) = item.coord().validate() {
                return Err(StoreError::InvalidCoordinate {
                    index: Some(i),
                    reason: format!("{err}"),
                });
            }
            let options = item.options();
            let size = crate::store::append::checked_append_bytes(
                item.payload_bytes().len(),
                &options.extensions,
            )?;
            if size > per_item_cap {
                return Err(StoreError::BatchItemTooLarge {
                    index: i,
                    size,
                    limit: per_item_cap,
                });
            }
        }
        self.submit_batch_with_fence_impl(items, None)
    }

    /// Attempt a root-cause submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    pub fn try_submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit(coord, kind, payload)
            .map(crate::outcome::Outcome::ok)
    }

    /// Attempt a reaction submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    pub fn try_submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit_reaction(coord, kind, payload, correlation_id, causation_id)
            .map(crate::outcome::Outcome::ok)
    }

    /// Attempt a batch submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any enqueue or writer error surfaced when the operation
    /// proceeds past the soft-pressure gate.
    pub fn try_submit_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<crate::outcome::Outcome<BatchAppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate_batch() {
            return Ok(outcome);
        }
        self.submit_batch(items).map(crate::outcome::Outcome::ok)
    }

    /// WRITE: append a new root-cause event.
    /// correlation_id defaults to event_id (self-correlated). causation_id = None.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendReceipt, StoreError> {
        tracing::debug!(
            target: "batpak::flow",
            flow = "append",
            entity = coord.entity(),
            scope = coord.scope(),
            event_kind = kind.type_id()
        );
        self.submit(coord, kind, payload)?.wait()
    }

    /// WRITE: persist a gate denial as a normal per-entity chain event.
    ///
    /// # Errors
    /// Returns any serialization or writer error surfaced by the underlying
    /// append path.
    // justifies: Store::append_denial matches the substrate contract locked in this turn and mirrors the user-requested denial append surface; splitting it would add an extra request object without simplifying src/store/mod.rs.
    #[allow(clippy::too_many_arguments)]
    pub fn append_denial<Ctx>(
        &self,
        coord: &Coordinate,
        proposed_kind: EventKind,
        gate_set: &GateSet<Ctx>,
        failing: &Denial,
        proposed_content_hash: Option<[u8; 32]>,
        pipeline_id: Option<String>,
        options: AppendOptions,
    ) -> Result<DenialReceipt, StoreError> {
        let payload =
            gate_set.trace_denial(failing, proposed_kind, proposed_content_hash, pipeline_id);
        let receipt =
            self.append_with_options(coord, EventKind::SYSTEM_DENIAL, &payload, options)?;
        Ok(DenialReceipt {
            event_id: receipt.event_id,
            sequence: receipt.sequence,
            disk_pos: receipt.disk_pos,
            content_hash: receipt.content_hash,
            key_id: receipt.key_id,
            signature: receipt.signature,
            extensions: receipt.extensions,
        })
    }

    /// WRITE: append a reaction (caused by another event).
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        tracing::debug!(
            target: "batpak::flow",
            flow = "append_reaction",
            entity = coord.entity(),
            scope = coord.scope(),
            correlation_id = format_args!("{correlation_id:032x}"),
            causation_id = format_args!("{causation_id:032x}")
        );
        self.submit_reaction(coord, kind, payload, correlation_id, causation_id)?
            .wait()
    }

    /// WRITE: atomic batch append of multiple events.
    /// All events are committed together or none are visible.
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if a specific item fails validation,
    /// encoding, marker writing, or publish preparation. Returns
    /// `StoreError::BatchSyncFailed` if the batch reaches the final durability
    /// boundary and segment sync fails before publish.
    pub fn append_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        self.append_batch_with_options(items, AppendOptions::default())
    }

    /// WRITE: atomic batch append with a batch-level append option set.
    ///
    /// Only [`AppendOptions::gate`] is honored at the batch level. The gate
    /// waits on the last event in the batch, which covers earlier events
    /// because batch HLCs and watermarks are monotonic.
    ///
    /// # Errors
    /// Returns any batch append error surfaced by [`Store::append_batch`].
    /// Returns [`StoreError::WaitTimeout`] or [`StoreError::WriterCrashed`] if
    /// the optional batch-level gate is not satisfied after the batch commits.
    pub fn append_batch_with_options(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
        opts: AppendOptions,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        debug_assert!(
            items.iter().all(|item| item.options().gate.is_none()),
            "BatchAppendItem per-item DurabilityGate is ignored; pass the gate to append_batch_with_options instead"
        );
        let gate = opts.gate;
        let _consumed_options = opts;
        let receipts = self.submit_batch(items)?.wait()?;
        if let (Some(gate), Some(receipt)) = (gate, receipts.last()) {
            self.wait_for_gate(receipt, gate)?;
        }
        Ok(receipts)
    }

    /// WRITE: atomic batch append of reaction events.
    /// All events share the same correlation_id from the triggering event.
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if a specific item fails validation,
    /// encoding, marker writing, or publish preparation. Returns
    /// `StoreError::BatchSyncFailed` if the batch reaches the final durability
    /// boundary and segment sync fails before publish.
    pub fn append_reaction_batch(
        &self,
        correlation_id: u128,
        causation_id: u128,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        // Set correlation_id and causation_id on all items.
        let items: Vec<_> = items
            .into_iter()
            .map(|item| {
                let mut options = item.options();
                options.correlation_id = Some(correlation_id);
                // Only set causation_id if not already explicitly set.
                if item.causation().uses_options_fallback() {
                    options.causation_id = Some(causation_id);
                }
                item.with_options(options)
            })
            .collect();
        self.append_batch(items)
    }

    /// Crate-private accessor that encodes the `Store<Open>` typestate
    /// invariant: an `Open` store always holds a writer handle.
    ///
    /// Panics if the invariant is violated — which only happens when a
    /// `Store<Open>` has been partially moved out of during drop, a context
    /// in which every public method is already unreachable.
    // justifies: INV-TYPESTATE-OPEN-HAS-WRITER and src/store/lifecycle.rs make this a typestate construction guarantee, not contingent runtime input.
    #[allow(clippy::expect_used)]
    pub(crate) fn writer_ref(&self) -> &WriterHandle {
        self.writer
            .as_ref()
            .expect("invariant: Store<Open> is constructed with a writer handle")
    }

    /// WRITE: append with CAS, idempotency, custom correlation/causation.
    /// CAS and idempotency checks execute inside the writer thread under
    /// the entity lock — no TOCTOU race between check and commit.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::SequenceMismatch` if the expected sequence does not match.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_with_options(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        opts: AppendOptions,
    ) -> Result<AppendReceipt, StoreError> {
        let gate = opts.gate;
        tracing::debug!(
            target: "batpak::flow",
            flow = "append_with_options",
            entity = coord.entity(),
            scope = coord.scope(),
            has_cas = opts.expected_sequence.is_some(),
            has_idempotency = opts.idempotency_key.is_some()
        );
        let receipt = self
            .submit_prepared(coord, kind, payload, AppendSubmission::with_options(opts))?
            .wait()?;
        if let Some(gate) = gate {
            self.wait_for_gate(&receipt, gate)?;
        }
        Ok(receipt)
    }

    /// WRITE: apply a typestate transition — kind is read from `P::KIND`.
    ///
    /// Per FREEZE-7 the transition's event kind is structurally derived from
    /// the payload type parameter, so this API cannot be called with a
    /// mismatched payload/kind pair.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn apply_transition<
        From: crate::typestate::transition::StateMarker,
        To: crate::typestate::transition::StateMarker,
        P: EventPayload,
    >(
        &self,
        coord: &Coordinate,
        transition: crate::typestate::transition::Transition<From, To, P>,
    ) -> Result<AppendReceipt, StoreError> {
        let payload = transition.into_payload();
        self.append(coord, P::KIND, &payload)
    }

    /// WRITE (typed): append a root-cause event — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendReceipt, StoreError> {
        self.append(coord, T::KIND, payload)
    }

    /// WRITE (typed): append with options — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_typed_with_options<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        opts: AppendOptions,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_with_options(coord, T::KIND, payload, opts)
    }

    /// WRITE (typed): nonblocking submit — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendTicket, StoreError> {
        self.submit(coord, T::KIND, payload)
    }

    /// WRITE (typed): attempt submit without blocking under pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit(coord, T::KIND, payload)
    }

    /// WRITE (typed): append a reaction — kind derived from `T::KIND`.
    ///
    /// `correlation_id` and `causation_id` are still supplied explicitly;
    /// only the `kind` becomes implicit.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// WRITE (typed): nonblocking reaction submit — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// WRITE (typed): attempt reaction submit without blocking under pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_submission_waits_behind_lifecycle_gate() {
        let dir = TempDir::new().expect("temp dir");
        let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open store"));
        let lifecycle = store.lifecycle_gate.lock();
        let coord = Coordinate::new("entity:lifecycle-gated", "scope:test").expect("coord");
        let (started_tx, started_rx) = flume::bounded(1);
        let (done_tx, done_rx) = flume::bounded(1);
        let worker_store = Arc::clone(&store);

        let worker = std::thread::Builder::new()
            .name("batpak-lifecycle-gate-regression".into())
            .spawn(move || {
                started_tx.send(()).expect("notify started");
                let result = worker_store.append(
                    &coord,
                    EventKind::DATA,
                    &serde_json::json!({"blocked": true}),
                );
                done_tx.send(result).expect("send append result");
            })
            .expect("spawn append worker");

        started_rx.recv().expect("worker started");
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "PROPERTY: writer submissions must not pass the lifecycle gate while compaction/snapshot/close owns it"
        );

        drop(lifecycle);
        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("append completes after lifecycle gate opens")
            .expect("append succeeds");
        worker.join().expect("append worker joins");
    }
}
