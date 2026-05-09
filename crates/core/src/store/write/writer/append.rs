use super::fence_runtime::FenceLedger;
use super::publish::CommitInternedIds;
use super::{
    segment, Coordinate, DagPosition, DiskPos, Event, EventKind, FramePayloadRef, HashChain,
    StoreError, WriterState,
};
use super::{StagedCommitMeta, StagedCommitTiming, StagedCommittedEvent};
use crate::store::stats::HlcPoint;
use crate::store::AppendReceipt;
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

        let latest = self.index.get_latest(entity);

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
            if let Some(entry) = self.index.get_by_id(key) {
                let mut receipt = AppendReceipt {
                    event_id: entry.event_id,
                    sequence: entry.global_sequence,
                    disk_pos: entry.disk_pos,
                    content_hash: entry.hash_chain.event_hash,
                    key_id: [0; 32],
                    signature: None,
                    extensions: BTreeMap::new(),
                };
                self.runtime.signing_registry.sign_append_receipt(
                    &mut receipt,
                    &entry.coord,
                    entry.kind,
                    entry.hash_chain.prev_hash,
                );
                return Ok(receipt);
            }
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
            .advance_accepted(frontier_point);
        let position = DagPosition::with_hlc(now_ms, 0, guards.dag_depth, guards.dag_lane, clock);
        event.header.position = position;
        event.header.event_kind = kind;
        event.header.correlation_id = correlation_id;
        event.header.causation_id = causation_id.filter(|&id| id != 0);

        #[cfg(feature = "blake3")]
        let event_hash = crate::event::hash::compute_hash(&event.payload);
        #[cfg(not(feature = "blake3"))]
        let event_hash = [0u8; 32];

        event.hash_chain = Some(HashChain {
            prev_hash,
            event_hash,
        });
        event.header.content_hash = event_hash;

        let frame_payload = FramePayloadRef {
            event: &event,
            entity,
            scope,
        };
        let frame = segment::frame_encode(&frame_payload)?;

        if self.maybe_rotate_segment()? {
            info!(segment_id = *self.segment_id, "segment rotated");
        }

        let offset = self.active_segment.write_frame(&frame)?;
        self.watermark_handle.lock().advance_written(frontier_point);
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
        let meta = StagedCommitMeta::new(
            event.header.event_id,
            correlation_id,
            causation_id,
            kind,
            global_seq,
        );
        let timing = StagedCommitTiming::new(
            event.header.timestamp_us,
            now_ms,
            clock,
            guards.dag_lane,
            guards.dag_depth,
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
            &event.payload,
            event.header.flags,
            emit_envelope,
        );
        self.sidx_collector.record(
            committed.sidx_entry,
            committed.index_entry.coord.entity(),
            committed.index_entry.coord.scope(),
        );
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

        let mut receipt = AppendReceipt {
            event_id: event.header.event_id,
            sequence: global_seq,
            disk_pos,
            content_hash: event_hash,
            key_id: [0; 32],
            signature: None,
            extensions: BTreeMap::new(),
        };
        self.runtime
            .signing_registry
            .sign_append_receipt(&mut receipt, coord, kind, prev_hash);
        Ok(receipt)
    }
}
