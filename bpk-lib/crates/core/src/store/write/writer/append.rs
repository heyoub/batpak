use super::fence_runtime::FenceLedger;
use super::publish::{CommitFrameView, CommitInternedIds};
use super::{
    segment, Coordinate, DagPosition, DiskPos, Event, EventKind, FramePayloadRef, HashChain,
    StoreError, WriterState,
};
use super::{StagedCommitMeta, StagedCommitTiming, StagedCommittedEvent};
use crate::store::stats::HlcPoint;
use crate::store::{AppendReceipt, EncodedBytes, ExtensionKey};
use std::collections::BTreeMap;
use tracing::{debug, info, trace};

/// Options and guards for an append operation, passed through the channel.
/// CAS + idempotency checks execute on the single writer thread, so there
/// is no producer/producer race to guard against.
pub(crate) struct AppendGuards {
    pub correlation_id: u128,
    pub causation_id: Option<u128>,
    pub expected_sequence: Option<u32>,
    pub idempotency_key: Option<u128>,
    pub dag_lane: u32,
    pub dag_depth: u32,
    pub dag_branch_root: bool,
    pub extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

impl WriterState<'_> {
    /// The 10-step commit protocol.
    pub(super) fn handle_append(
        &mut self,
        coord: &Coordinate,
        mut event: Event<Vec<u8>>,
        kind: EventKind,
        guards: &AppendGuards,
        fence: Option<&mut FenceLedger>,
    ) -> Result<AppendReceipt, StoreError> {
        let correlation_id = guards.correlation_id;
        let causation_id = guards.causation_id;
        let entity = coord.entity();
        let scope = coord.scope();

        let latest = self.index.get_latest_committed(entity, guards.dag_lane);

        if let Some(expected) = guards.expected_sequence {
            let actual = latest.as_ref().map(|entry| entry.clock).unwrap_or(0);
            if actual != expected {
                return Err(StoreError::SequenceMismatch {
                    entity: entity.to_string(),
                    expected,
                    actual,
                });
            }
        }

        if let Some(key) = guards.idempotency_key {
            // Durable map FIRST: a hit here is a true no-op even if the
            // underlying event has been evicted by retention compaction.
            // justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
            if let Some(durable) = self.index.idemp.get(key) {
                let mut receipt = AppendReceipt {
                    event_id: crate::id::EventId::from(durable.event_id),
                    sequence: durable.global_sequence,
                    disk_pos: durable.disk_pos(),
                    content_hash: durable.content_hash,
                    key_id: [0; 32],
                    signature: None,
                    extensions: durable.receipt_extensions.clone(),
                };
                let coord = crate::coordinate::Coordinate::new(&durable.entity, &durable.scope)?;
                self.runtime.signing_registry.sign_append_receipt(
                    &mut receipt,
                    &coord,
                    durable.kind,
                    durable.prev_hash,
                );
                return Ok(receipt);
            }
            // Fall through to the live `by_id` path (covers entries recorded
            // before the durable store existed and preserves prior behavior).
            if let Some(entry) = self.index.get_by_id(key) {
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
                return Ok(receipt);
            }
            // Genuinely new key: enforce the soft-cap overflow policy BEFORE we
            // commit. FailClosed/Backpressure refuse here; Warn proceeds. Pass
            // the current frontier so out-of-window keys age out before we
            // fail-close on a fresh key.
            self.index
                .idemp
                .admit_new_key(key, self.index.global_sequence())?;
        }

        let prev_hash = latest
            .as_ref()
            .map(|entry| entry.hash_chain.event_hash)
            .unwrap_or([0u8; 32]);

        let clock = super::checked_next_clock(latest.as_ref().map(|entry| entry.clock), entity)?;

        let raw_ms = crate::store::config::wall_ms_from_timestamp_us(event.header.timestamp_us)?;
        let last_ms = latest.as_ref().map(|entry| entry.wall_ms).unwrap_or(0);
        let now_ms = raw_ms.max(last_ms);
        let global_seq = self.index.global_sequence();
        let frontier_point = HlcPoint {
            wall_ms: now_ms,
            global_sequence: global_seq,
        };

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::SingleAppendStart {
                entity: entity.to_string(),
            },
            &self.config.fault_injector,
        )?;

        self.watermark_handle
            .lock()
            .advance_accepted_on_lane(guards.dag_lane, frontier_point);
        let dag_depth = if guards.dag_branch_root {
            DagPosition::fork(guards.dag_depth, guards.dag_lane).depth()
        } else {
            guards.dag_depth
        };
        let position = DagPosition::with_hlc(now_ms, 0, dag_depth, guards.dag_lane, clock);
        event.header.position = position;
        event.header.event_kind = kind;
        event.header.correlation_id = crate::id::CorrelationId::from(correlation_id);
        event.header.causation_id = causation_id
            .filter(|&id| id != 0)
            .map(crate::id::CausationId::from);

        let event_hash = crate::event::hash::compute_hash(&event.payload);

        event.hash_chain = Some(HashChain {
            prev_hash,
            event_hash,
        });
        event.header.content_hash = event_hash;

        let mut receipt = AppendReceipt {
            event_id: event.header.event_id,
            sequence: global_seq,
            disk_pos: DiskPos {
                segment_id: *self.segment_id,
                offset: 0,
                length: 0,
            },
            content_hash: event_hash,
            key_id: [0; 32],
            signature: None,
            extensions: guards.extensions.clone(),
        };
        self.runtime
            .signing_registry
            .sign_append_receipt(&mut receipt, coord, kind, prev_hash);

        let frame_payload = FramePayloadRef {
            event: &event,
            entity,
            scope,
            receipt_extensions: &receipt.extensions,
        };
        let frame = segment::frame_encode(&frame_payload)?;

        if self.maybe_rotate_segment()? {
            info!(segment_id = *self.segment_id, "segment rotated");
        }

        let offset = self.active_segment.write_frame(&frame)?;
        self.watermark_handle
            .lock()
            .advance_written_on_lane(guards.dag_lane, frontier_point);
        trace!(offset = offset, len = frame.len(), "frame written");

        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::SingleAppendWritten {
                entity: entity.to_string(),
            },
            &self.config.fault_injector,
        )?;

        let disk_pos = DiskPos {
            segment_id: *self.segment_id,
            offset,
            length: u32::try_from(frame.len())
                .map_err(|_| StoreError::ser_msg("encoded frame length exceeds u32::MAX"))?,
        };
        receipt.disk_pos = disk_pos;
        let meta = {
            use crate::id::EntityIdType;
            StagedCommitMeta::new(
                event.header.event_id.as_u128(),
                correlation_id,
                causation_id,
                kind,
                global_seq,
            )
        };
        let timing = StagedCommitTiming::new(
            event.header.timestamp_us,
            now_ms,
            clock,
            guards.dag_lane,
            dag_depth,
        );
        let staged = StagedCommittedEvent::new(
            coord.clone(),
            meta,
            timing,
            HashChain {
                prev_hash,
                event_hash,
            },
        );
        let emit_envelope = self.reactor_subscribers.has_subscribers();
        let entity_id = self.index.interner.intern(coord.entity());
        let scope_id = self.index.interner.intern(coord.scope());
        let committed = self.materialize_commit_artifacts(
            &staged,
            disk_pos,
            CommitInternedIds {
                entity_id,
                scope_id,
            },
            CommitFrameView {
                payload_bytes: &event.payload,
                flags: event.header.flags,
                receipt_extensions: &receipt.extensions,
                emit_envelope,
            },
        );
        self.sidx_collector.record(
            committed.sidx_entry,
            committed.index_entry.coord.entity(),
            committed.index_entry.coord.scope(),
        );
        // Record the durable idempotency entry on every successful KEYED
        // append, capturing exactly the no-op reconstruction tuple. This
        // survives retention compaction and cold-start independent of the
        // event frame. justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
        if guards.idempotency_key.is_some() {
            self.index
                .idemp
                .record(crate::store::index::idemp::IdempEntry::from_index_entry(
                    &committed.index_entry,
                    global_seq,
                ));
        }
        self.index.insert(committed.index_entry);

        debug!(event_id = %event.header.event_id, clock = clock, "append committed");

        if let Some(fence) = fence {
            fence.record_publish_up_to(global_seq.saturating_add(1), frontier_point);
            self.index.note_visibility_fence_progress(
                fence.token,
                global_seq,
                global_seq.saturating_add(1),
            )?;
            fence.extend_artifacts([committed.notification], committed.envelope);
        } else {
            self.publish_then_broadcast_unfenced(
                global_seq + 1,
                frontier_point,
                [committed.notification],
                committed.envelope,
            )?;

            #[cfg(feature = "dangerous-test-hooks")]
            crate::store::fault::maybe_inject(
                crate::store::fault::InjectionPoint::SingleAppendPublished {
                    entity: entity.to_string(),
                },
                &self.config.fault_injector,
            )?;
        }

        Ok(receipt)
    }
}
