use super::super::staging::{
    PreparedBatch, StagedCommitMeta, StagedCommitTiming, StagedCommittedEvent,
};
use super::{
    segment, DagPosition, DiskPos, Event, EventHeader, EventKind, FenceLedger, FramePayloadRef,
    HashChain, WriterState,
};
use crate::store::append::{checked_append_bytes, BatchAppendItem};
use crate::store::stats::HlcPoint;
use crate::store::{AppendReceipt, StoreError};
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{debug, trace};

/// Entity name for batch system markers (BEGIN/COMMIT). Not user-visible.
const BATCH_MARKER_ENTITY: &str = "_batch";
/// Scope name for batch system markers (BEGIN/COMMIT). Not user-visible.
const BATCH_MARKER_SCOPE: &str = "_system";

#[derive(Clone, Copy, Debug)]
enum BatchFailureStage {
    Validation,
    Encoding,
    Writing,
    Syncing,
}

fn batch_failed(
    item_index: usize,
    stage: BatchFailureStage,
    source: impl Into<Box<StoreError>>,
) -> StoreError {
    tracing::debug!(item_index, ?stage, "batch failure surfaced");
    StoreError::batch_failed(item_index, source)
}

impl WriterState<'_> {
    /// STEPs 1-2: Validate batch size, total bytes, and reject CAS in batches.
    fn validate_batch(&self, items: &[BatchAppendItem]) -> Result<(), StoreError> {
        if items.len() > self.config.batch.max_size as usize {
            return Err(batch_failed(
                0,
                BatchFailureStage::Validation,
                StoreError::Configuration(format!(
                    "batch size {} exceeds max {}",
                    items.len(),
                    self.config.batch.max_size
                )),
            ));
        }
        let total_bytes = items.iter().try_fold(0usize, |total, item| {
            let options = item.options();
            let item_bytes = checked_append_bytes(item.payload_bytes().len(), &options.extensions)?;
            total
                .checked_add(item_bytes)
                .ok_or_else(|| StoreError::ser_msg("batch bytes overflow usize"))
        })?;
        if total_bytes > self.config.batch.max_bytes as usize {
            return Err(batch_failed(
                0,
                BatchFailureStage::Validation,
                StoreError::Configuration(format!(
                    "batch bytes {} exceeds max {}",
                    total_bytes, self.config.batch.max_bytes
                )),
            ));
        }
        for (idx, item) in items.iter().enumerate() {
            if item.options().expected_sequence.is_some() {
                return Err(batch_failed(
                    idx,
                    BatchFailureStage::Validation,
                    StoreError::Configuration("CAS not supported in batch append (v1)".into()),
                ));
            }
        }
        Ok(())
    }

    fn preflight_batch_idempotency(
        &self,
        items: &[BatchAppendItem],
    ) -> Result<Option<Vec<AppendReceipt>>, StoreError> {
        let mut cached_receipts: Vec<Option<AppendReceipt>> = vec![None; items.len()];
        let mut cached_count = 0usize;
        let mut keyed_count = 0usize;
        for (idx, item) in items.iter().enumerate() {
            if let Some(key) = item.options().idempotency_key {
                use crate::id::EntityIdType;
                keyed_count += 1;
                if let Some(entry) = self.index.get_by_id(key.as_u128()) {
                    let mut receipt = AppendReceipt {
                        event_id: crate::id::EventId::from(entry.event_id),
                        sequence: entry.global_sequence,
                        disk_pos: entry.disk_pos,
                        content_hash: entry.hash_chain.event_hash,
                        key_id: [0; 32],
                        signature: None,
                        extensions: entry.receipt_extensions.clone(),
                    };
                    self.runtime.signing_registry.sign_append_receipt(
                        &mut receipt,
                        &entry.coord,
                        entry.kind,
                        entry.hash_chain.prev_hash,
                    );
                    cached_receipts[idx] = Some(receipt);
                    cached_count += 1;
                }
            }
        }

        if keyed_count == 0 {
            return Ok(None);
        }

        if cached_count == items.len() {
            let mut receipts = Vec::with_capacity(cached_receipts.len());
            for receipt in cached_receipts {
                let Some(receipt) = receipt else {
                    return Err(StoreError::IdempotencyPartialBatch {
                        reason: "cached replay bookkeeping inconsistent".into(),
                    });
                };
                receipts.push(receipt);
            }
            return Ok(Some(receipts));
        }

        if cached_count > 0 {
            return Err(StoreError::IdempotencyPartialBatch {
                reason: "partial idempotency-key replay".into(),
            });
        }

        Ok(None)
    }

    fn precompute_batch_items(
        &self,
        prepared: &PreparedBatch,
        first_seq: u64,
    ) -> Result<Vec<StagedCommittedEvent>, StoreError> {
        #[derive(Clone)]
        struct BatchEntityState {
            entity_arc: Arc<str>,
            prev_hash: [u8; 32],
            next_clock: u32,
            last_wall_ms: u64,
        }

        let mut computed: Vec<StagedCommittedEvent> = Vec::with_capacity(prepared.len());
        let mut entity_states: std::collections::HashMap<Arc<str>, BatchEntityState> =
            std::collections::HashMap::new();

        let now_us = self.runtime.now_us();
        let now_ms = crate::store::config::wall_ms_from_timestamp_us(now_us)
            .map_err(|e| batch_failed(0, BatchFailureStage::Validation, e))?;

        for (idx, item) in prepared.items().iter().enumerate() {
            let entity = Arc::clone(item.entity_arc());
            let state = match entity_states.entry(Arc::clone(&entity)) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let latest = self.index.get_latest(&entity);
                    entry.insert(BatchEntityState {
                        entity_arc: Arc::clone(&entity),
                        prev_hash: latest
                            .as_ref()
                            .map(|entry| entry.hash_chain.event_hash)
                            .unwrap_or([0u8; 32]),
                        next_clock: super::checked_next_clock(
                            latest.as_ref().map(|entry| entry.clock),
                            &entity,
                        )?,
                        last_wall_ms: latest.as_ref().map(|entry| entry.wall_ms).unwrap_or(0),
                    })
                }
            };

            debug_assert!(
                Arc::ptr_eq(&state.entity_arc, item.entity_arc()),
                "batch entity Arc identity must be stable; state={:p} item={:p}",
                Arc::as_ptr(&state.entity_arc),
                Arc::as_ptr(item.entity_arc()),
            );

            let prev_hash = state.prev_hash;
            let clock = state.next_clock;
            let wall_ms = now_ms.max(state.last_wall_ms);

            use crate::id::EntityIdType;
            let event_id = item
                .options()
                .idempotency_key
                .map(|k| k.as_u128())
                .unwrap_or_else(|| crate::id::generate_v7_id_with_clock(self.runtime.clock()));

            let causation_id = item
                .causation()
                .resolve(
                    item.options().causation_id.map(|id| id.as_u128()),
                    idx,
                    |prior_idx| computed[prior_idx].event_id(),
                )
                .map_err(|e| batch_failed(idx, BatchFailureStage::Validation, e))?;

            #[cfg(feature = "blake3")]
            let event_hash = crate::event::hash::compute_hash(item.payload_bytes());
            #[cfg(not(feature = "blake3"))]
            let event_hash = [0u8; 32];

            state.prev_hash = event_hash;
            state.next_clock =
                clock
                    .checked_add(1)
                    .ok_or_else(|| StoreError::EntityClockOverflow {
                        entity: entity.to_string(),
                    })?;
            state.last_wall_ms = wall_ms;

            let global_seq = first_seq + idx as u64;
            self.watermark_handle.lock().advance_accepted(HlcPoint {
                wall_ms,
                global_sequence: global_seq,
            });
            let meta = StagedCommitMeta::new(
                event_id,
                item.options()
                    .correlation_id
                    .map(|id| id.as_u128())
                    .unwrap_or(event_id),
                causation_id,
                item.kind(),
                global_seq,
            );
            let position_hint = item.options().position_hint.unwrap_or_default();
            let timing = StagedCommitTiming::new(
                now_us,
                wall_ms,
                clock,
                position_hint.lane,
                position_hint.depth,
            );
            computed.push(StagedCommittedEvent::new(
                item.coord().clone(),
                meta,
                timing,
                HashChain {
                    prev_hash,
                    event_hash,
                },
            ));
        }
        Ok(computed)
    }

    fn write_batch_marker_frame(
        &mut self,
        batch_id: u64,
        kind: EventKind,
        payload_size: u32,
        item_index_for_error: usize,
        allow_rotation: bool,
    ) -> Result<u64, StoreError> {
        let now_us = self.runtime.now_us();
        let now_ms = crate::store::config::wall_ms_from_timestamp_us(now_us)
            .map_err(|e| batch_failed(item_index_for_error, BatchFailureStage::Validation, e))?;
        // BEGIN and COMMIT markers intentionally share `batch_id` as their
        // synthetic identity. These frames never enter the public event-id
        // index, so crash recovery can treat them as one batch envelope.
        let header = EventHeader::new(
            batch_id as u128,
            batch_id as u128,
            None,
            now_us,
            DagPosition::child_at(0, now_ms, 0),
            payload_size,
            kind,
        );
        let event = Event::new(header, Vec::<u8>::new());
        let receipt_extensions = BTreeMap::new();
        let payload = FramePayloadRef {
            event: &event,
            entity: BATCH_MARKER_ENTITY,
            scope: BATCH_MARKER_SCOPE,
            receipt_extensions: &receipt_extensions,
        };
        let frame = segment::frame_encode(&payload)
            .map_err(|e| batch_failed(item_index_for_error, BatchFailureStage::Encoding, e))?;

        if allow_rotation {
            self.maybe_rotate_segment()
                .map_err(|e| batch_failed(item_index_for_error, BatchFailureStage::Syncing, e))?;
        }

        let offset = self
            .active_segment
            .write_frame(&frame)
            .map_err(|e| batch_failed(item_index_for_error, BatchFailureStage::Writing, e))?;
        Ok(offset)
    }

    pub(super) fn handle_append_batch(
        &mut self,
        items: Vec<BatchAppendItem>,
        fence: Option<&mut FenceLedger>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        self.validate_batch(&items)?;

        if items.is_empty() {
            return Ok(Vec::new());
        }

        let keyed_count = items
            .iter()
            .filter(|item| item.options().idempotency_key.is_some())
            .count();
        if keyed_count != 0 && keyed_count != items.len() {
            return Err(StoreError::IdempotencyPartialBatch {
                reason: "batch must have all items keyed or all unkeyed".into(),
            });
        }

        if let Some(cached) = self.preflight_batch_idempotency(&items)? {
            return Ok(cached);
        }

        let prepared = PreparedBatch::from_items(items)?;
        self.handle_prepared_batch(&prepared, fence)
    }

    fn handle_prepared_batch(
        &mut self,
        prepared: &PreparedBatch,
        fence: Option<&mut FenceLedger>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        debug_assert_eq!(
            prepared.len(),
            prepared.items().len(),
            "PreparedBatch::len must equal items().len(); writer derives sequence \
             reservation, commit marker offset, and publish span from it"
        );
        debug_assert_eq!(
            prepared.total_bytes(),
            prepared
                .items()
                .iter()
                .map(|item| item.payload_bytes().len())
                .sum::<usize>()
        );

        let batch_id = self.index.global_sequence();
        let first_seq = self.index.reserve_sequences(prepared.len() as u64);

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchStart {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        let computed = self.precompute_batch_items(prepared, first_seq)?;
        let batch_frontier = computed
            .last()
            .map(|staged| HlcPoint {
                wall_ms: staged.timing.wall_ms,
                global_sequence: staged.global_sequence(),
            })
            .unwrap_or(HlcPoint::ORIGIN);

        let batch_count = u32::try_from(prepared.len())
            .map_err(|_| StoreError::ser_msg("prepared batch item count exceeds u32::MAX"))?;
        let marker_offset = self.write_batch_marker_frame(
            batch_id,
            EventKind::SYSTEM_BATCH_BEGIN,
            batch_count,
            0,
            true,
        )?;
        trace!(batch_id, offset = marker_offset, "batch marker written");

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchBeginWritten {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        let receipts = self.write_batch_event_frames(prepared, &computed, batch_id)?;

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchItemsComplete {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        let _commit_offset = self.write_batch_marker_frame(
            batch_id,
            EventKind::SYSTEM_BATCH_COMMIT,
            0,
            prepared.len() - 1,
            false,
        )?;
        trace!(batch_id, "batch commit marker written");

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchCommitWritten { batch_id },
            &self.config.fault_injector,
        )?;

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchFsync { batch_id },
            &self.config.fault_injector,
        )?;

        self.active_segment
            .sync_with_mode(&self.config.sync.mode)
            .map_err(|e| StoreError::batch_sync_failed(prepared.len(), e))?;
        self.watermark_handle.lock().advance_durable_to_accepted();

        let artifacts = self.materialize_batch_commit_artifacts(prepared, &computed, &receipts);
        for (sidx_entry, index_entry) in artifacts.sidx_entries.iter().zip(artifacts.entries.iter())
        {
            self.sidx_collector.record(
                sidx_entry.clone(),
                index_entry.coord.entity(),
                index_entry.coord.scope(),
            );
        }

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchPrePublish {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        self.index.insert_batch(artifacts.entries);
        let publish_span = u32::try_from(prepared.len())
            .map_err(|_| StoreError::ser_msg("prepared batch item count exceeds u32::MAX"))?;
        let publish_up_to = first_seq + u64::from(publish_span);

        if let Some(fence) = fence {
            fence.record_publish_up_to(publish_up_to, batch_frontier);
            self.index
                .note_visibility_fence_progress(fence.token, first_seq, publish_up_to)?;
            fence.extend_artifacts(artifacts.notifications, artifacts.envelopes);
        } else {
            self.publish_then_broadcast_unfenced(
                publish_up_to,
                batch_frontier,
                artifacts.notifications,
                artifacts.envelopes,
            )?;
        }

        debug!(batch_id, count = prepared.len(), "batch committed");
        Ok(receipts)
    }

    fn write_batch_event_frames(
        &mut self,
        prepared: &PreparedBatch,
        staged: &[StagedCommittedEvent],
        batch_id: u64,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        let mut receipts: Vec<AppendReceipt> = Vec::with_capacity(prepared.len());

        for (idx, item) in prepared.items().iter().enumerate() {
            let staged = &staged[idx];
            let item_options = item.options();
            let event = staged
                .borrowed_frame_event(item.payload_bytes())
                .map_err(|e| batch_failed(idx, BatchFailureStage::Encoding, e))?;

            let mut receipt = AppendReceipt {
                event_id: crate::id::EventId::from(staged.event_id()),
                sequence: staged.global_sequence(),
                disk_pos: DiskPos {
                    segment_id: *self.segment_id,
                    offset: 0,
                    length: 0,
                },
                content_hash: staged.hash_chain.event_hash,
                key_id: [0; 32],
                signature: None,
                extensions: item_options.extensions.clone(),
            };
            self.runtime.signing_registry.sign_append_receipt(
                &mut receipt,
                &staged.coord,
                staged.meta.kind,
                staged.hash_chain.prev_hash,
            );

            let frame_payload = FramePayloadRef {
                event: &event,
                entity: staged.coord.entity(),
                scope: staged.coord.scope(),
                receipt_extensions: &receipt.extensions,
            };
            let frame = segment::frame_encode(&frame_payload)
                .map_err(|e| batch_failed(idx, BatchFailureStage::Encoding, e))?;

            let offset = self
                .active_segment
                .write_frame(&frame)
                .map_err(|e| batch_failed(idx, BatchFailureStage::Writing, e))?;
            self.watermark_handle.lock().advance_written(HlcPoint {
                wall_ms: staged.timing.wall_ms,
                global_sequence: staged.global_sequence(),
            });

            receipt.disk_pos = DiskPos {
                segment_id: *self.segment_id,
                offset,
                length: u32::try_from(frame.len()).map_err(|_| {
                    batch_failed(
                        idx,
                        BatchFailureStage::Encoding,
                        StoreError::ser_msg("encoded batch frame length exceeds u32::MAX"),
                    )
                })?,
            };
            receipts.push(receipt);

            #[cfg(feature = "dangerous-test-hooks")]
            crate::store::fault::maybe_inject(
                crate::store::fault::InjectionPoint::BatchItemWritten {
                    batch_id,
                    item_index: idx,
                    total_items: prepared.len(),
                },
                &self.config.fault_injector,
            )?;
        }

        let _ = batch_id;
        Ok(receipts)
    }
}
