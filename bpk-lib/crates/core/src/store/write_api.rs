use super::*;

impl Store<Open> {
    /// Advanced producer API: build an outbox for staged batch submission.
    ///
    /// The beginner write path is [`Store::append_typed`] or [`Store::append`].
    /// Use an outbox when a producer needs to stage multiple items before
    /// flushing them as one batch.
    pub fn outbox(&self) -> Outbox<'_> {
        Outbox::new(self, None)
    }

    /// Advanced producer API: begin a public visibility fence.
    ///
    /// Only one fence may be active at a time. Writes submitted through the
    /// returned [`VisibilityFence`] become durable but stay hidden until the
    /// fence commits.
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
            if let Err(error) = self.index.cancel_visibility_fence(token) {
                tracing::error!(
                    token,
                    error = %error,
                    "failed to roll back visibility fence after writer enqueue failure"
                );
            }
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

    /// Advanced producer API: nonblocking root-cause append submission.
    ///
    /// The beginner write path is [`Store::append_typed`] or [`Store::append`].
    /// Use `submit*` when the caller needs an [`AppendTicket`] and explicit
    /// control over waiting for the writer result.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the append for background execution.
    /// Returns [`StoreError::ReservedKind`] if `kind` is a reserved
    /// system/effect/tombstone kind (see [`EventKind::is_reserved`]); reserved
    /// kinds are emitted only by the substrate.
    pub fn submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::root(self.runtime.clock()),
        )
    }

    /// Advanced producer API: nonblocking reaction append submission.
    ///
    /// The beginner write path is [`Store::append_typed`] or [`Store::append`].
    /// Use this when constructing a causation-linked producer pipeline that
    /// waits on [`AppendTicket`] explicitly.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the reaction append for background execution.
    /// Returns [`StoreError::ReservedKind`] if `kind` is a reserved
    /// system/effect/tombstone kind (see [`EventKind::is_reserved`]); reserved
    /// kinds are emitted only by the substrate.
    pub fn submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
    ) -> Result<AppendTicket, StoreError> {
        use crate::id::EntityIdType;
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::reaction(
                self.runtime.clock(),
                correlation_id.as_u128(),
                causation_id.as_u128(),
            ),
        )
    }

    /// Advanced producer API: nonblocking batch append submission.
    ///
    /// The beginner write path is [`Store::append_typed`] or [`Store::append`].
    /// Use this when the caller needs an explicit [`BatchAppendTicket`].
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
    /// Returns [`StoreError::ReservedKind`] `{ index: Some(i), .. }` directly
    /// (NOT wrapped in `BatchFailed`) if item `i` carries a reserved kind.
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

    /// Advanced producer API: attempt a root-cause submission without blocking
    /// if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    /// Returns [`StoreError::ReservedKind`] if `kind` is a reserved
    /// system/effect/tombstone kind (see [`EventKind::is_reserved`]); reserved
    /// kinds are emitted only by the substrate.
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

    /// Advanced producer API: attempt a reaction submission without blocking if
    /// the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    /// Returns [`StoreError::ReservedKind`] if `kind` is a reserved
    /// system/effect/tombstone kind (see [`EventKind::is_reserved`]); reserved
    /// kinds are emitted only by the substrate.
    pub fn try_submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
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

    /// Advanced producer API: attempt a batch submission without blocking if
    /// the writer is under pressure.
    ///
    /// # Errors
    /// Returns any enqueue or writer error surfaced when the operation
    /// proceeds past the soft-pressure gate.
    /// Returns [`StoreError::ReservedKind`] `{ index: Some(i), .. }` directly
    /// (NOT wrapped in `BatchFailed`) if item `i` carries a reserved kind.
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
    /// Returns [`StoreError::ReservedKind`] if `kind` is a reserved
    /// system/effect/tombstone kind (see [`EventKind::is_reserved`]); reserved
    /// kinds are emitted only by the substrate.
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
        // SYSTEM_DENIAL is a reserved kind, so the public funnel would reject
        // it. Route directly through the internal funnel so the substrate audit
        // receipt still emits. The batch-level gate semantics from
        // `append_with_options` are not part of the denial contract.
        let gate = options.gate;
        let receipt = self
            .submit_prepared_internal(
                coord,
                EventKind::SYSTEM_DENIAL,
                &payload,
                AppendSubmission::with_options(options, self.runtime.clock()),
            )?
            .wait()?;
        if let Some(gate) = gate {
            self.wait_for_gate(&receipt, gate)?;
        }
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
    /// Returns [`StoreError::ReservedKind`] if `kind` is a reserved
    /// system/effect/tombstone kind (see [`EventKind::is_reserved`]); reserved
    /// kinds are emitted only by the substrate.
    pub fn append_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
    ) -> Result<AppendReceipt, StoreError> {
        use crate::id::EntityIdType;
        tracing::debug!(
            target: "batpak::flow",
            flow = "append_reaction",
            entity = coord.entity(),
            scope = coord.scope(),
            correlation_id = format_args!("{:032x}", correlation_id.as_u128()),
            causation_id = format_args!("{:032x}", causation_id.as_u128())
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
    /// Returns [`StoreError::ReservedKind`] `{ index: Some(i), .. }` directly
    /// (NOT wrapped in `BatchFailed`) if item `i` carries a reserved kind.
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
    /// Returns [`StoreError::ReservedKind`] `{ index: Some(i), .. }` directly
    /// (NOT wrapped in `BatchFailed`) if item `i` carries a reserved kind.
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
    /// Returns [`StoreError::ReservedKind`] `{ index: Some(i), .. }` directly
    /// (NOT wrapped in `BatchFailed`) if item `i` carries a reserved kind.
    pub fn append_reaction_batch(
        &self,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
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
    /// Returns [`StoreError::ReservedKind`] if `kind` is a reserved
    /// system/effect/tombstone kind (see [`EventKind::is_reserved`]); reserved
    /// kinds are emitted only by the substrate.
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
            .submit_prepared(
                coord,
                kind,
                payload,
                AppendSubmission::with_options(opts, self.runtime.clock()),
            )?
            .wait()?;
        if let Some(gate) = gate {
            self.wait_for_gate(&receipt, gate)?;
        }
        Ok(receipt)
    }

    // ─── Typed version-stamping lowerings ───────────────────────────────────
    //
    // `append_typed::<T>` and friends previously erased `T` straight to
    // `append(coord, T::KIND, payload)`, so `EventPayload::PAYLOAD_VERSION` never
    // reached the header. These crate-private funnels thread the version as a
    // scalar into the submission so the typed seam (and only the typed seam)
    // stamps a non-zero `payload_version`. Every untyped / batch / denial /
    // lifecycle path leaves the `0` sentinel, which the decode seam reads as
    // "tolerant decode as current".

    /// Versioned root submit. Mirrors [`Store::submit`] but stamps `version`.
    fn submit_versioned(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        version: u16,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::root(self.runtime.clock()).with_payload_version(version),
        )
    }

    /// Versioned options submit. Mirrors [`Store::append_with_options`]'s funnel.
    fn submit_with_options_versioned(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        opts: AppendOptions,
        version: u16,
    ) -> Result<AppendReceipt, StoreError> {
        let gate = opts.gate;
        let receipt = self
            .submit_prepared(
                coord,
                kind,
                payload,
                AppendSubmission::with_options(opts, self.runtime.clock())
                    .with_payload_version(version),
            )?
            .wait()?;
        if let Some(gate) = gate {
            self.wait_for_gate(&receipt, gate)?;
        }
        Ok(receipt)
    }

    /// Versioned reaction submit. Mirrors [`Store::submit_reaction`].
    fn submit_reaction_versioned(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
        version: u16,
    ) -> Result<AppendTicket, StoreError> {
        use crate::id::EntityIdType;
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::reaction(
                self.runtime.clock(),
                correlation_id.as_u128(),
                causation_id.as_u128(),
            )
            .with_payload_version(version),
        )
    }

    /// Versioned non-blocking root submit. Mirrors [`Store::try_submit`].
    fn try_submit_versioned(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        version: u16,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit_versioned(coord, kind, payload, version)
            .map(crate::outcome::Outcome::ok)
    }

    /// Versioned non-blocking reaction submit. Mirrors [`Store::try_submit_reaction`].
    fn try_submit_reaction_versioned(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
        version: u16,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit_reaction_versioned(coord, kind, payload, correlation_id, causation_id, version)
            .map(crate::outcome::Outcome::ok)
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
        self.submit_versioned(coord, P::KIND, &payload, P::PAYLOAD_VERSION)?
            .wait()
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
        self.submit_versioned(coord, T::KIND, payload, T::PAYLOAD_VERSION)?
            .wait()
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
        self.submit_with_options_versioned(coord, T::KIND, payload, opts, T::PAYLOAD_VERSION)
    }

    /// Advanced typed producer API: nonblocking submit — kind derived from
    /// `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_versioned(coord, T::KIND, payload, T::PAYLOAD_VERSION)
    }

    /// Advanced typed producer API: attempt submit without blocking under
    /// pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit_versioned(coord, T::KIND, payload, T::PAYLOAD_VERSION)
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
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
    ) -> Result<AppendReceipt, StoreError> {
        self.submit_reaction_versioned(
            coord,
            T::KIND,
            payload,
            correlation_id,
            causation_id,
            T::PAYLOAD_VERSION,
        )?
        .wait()
    }

    /// Advanced typed producer API: nonblocking reaction submit — kind derived
    /// from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_reaction_versioned(
            coord,
            T::KIND,
            payload,
            correlation_id,
            causation_id,
            T::PAYLOAD_VERSION,
        )
    }

    /// Advanced typed producer API: attempt reaction submit without blocking
    /// under pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: crate::id::CorrelationId,
        causation_id: crate::id::CausationId,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit_reaction_versioned(
            coord,
            T::KIND,
            payload,
            correlation_id,
            causation_id,
            T::PAYLOAD_VERSION,
        )
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

    #[test]
    fn append_rejects_reserved_kinds_and_admits_data() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = Coordinate::new("entity:reserved", "scope:test").expect("coord");
        let payload = serde_json::json!({"forged": true});

        // append() with a reserved system marker is rejected with index: None.
        let err = store
            .append(&coord, EventKind::SYSTEM_BATCH_BEGIN, &payload)
            .expect_err("PROPERTY: append must reject reserved system kinds");
        assert!(
            matches!(
                err,
                StoreError::ReservedKind {
                    index: None,
                    kind
                } if kind == EventKind::SYSTEM_BATCH_BEGIN.as_raw_u16()
            ),
            "PROPERTY: reserved single-event append must surface ReservedKind {{ index: None }}, got {err:?}"
        );

        // append_with_options() with TOMBSTONE and EFFECT_ERROR are rejected too.
        for reserved in [EventKind::TOMBSTONE, EventKind::EFFECT_ERROR] {
            let err = store
                .append_with_options(&coord, reserved, &payload, AppendOptions::default())
                .expect_err("PROPERTY: append_with_options must reject reserved kinds");
            assert!(
                matches!(
                    err,
                    StoreError::ReservedKind { index: None, kind } if kind == reserved.as_raw_u16()
                ),
                "PROPERTY: reserved append_with_options must surface ReservedKind, got {err:?}"
            );
        }

        // DATA still appends successfully through the same funnel.
        store
            .append(&coord, EventKind::DATA, &payload)
            .expect("PROPERTY: DATA append must still succeed after the reserved-kind guard");

        store.close().expect("close store");
    }

    #[test]
    fn append_batch_rejects_reserved_item_and_admits_clean_batch() {
        use crate::store::append::{BatchAppendItem, CausationRef};

        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = Coordinate::new("entity:reserved-batch", "scope:test").expect("coord");
        let payload = serde_json::json!({"n": 1});

        let forged = BatchAppendItem::new(
            coord.clone(),
            EventKind::SYSTEM_BATCH_COMMIT,
            &payload,
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("build forged batch item");
        let result = store.append_batch(vec![forged]);
        assert!(
            matches!(
                result,
                Err(StoreError::ReservedKind { index: Some(0), kind })
                    if kind == EventKind::SYSTEM_BATCH_COMMIT.as_raw_u16()
            ),
            "PROPERTY: reserved batch item must surface ReservedKind {{ index: Some(0) }}"
        );

        let clean = BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &payload,
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("build clean batch item");
        store
            .append_batch(vec![clean])
            .expect("PROPERTY: a clean batch must still commit after the reserved-kind guard");

        store.close().expect("close store");
    }
}
