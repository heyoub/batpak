use super::super::fanout::CommittedEventEnvelope;
use super::super::staging::{PreparedBatch, StagedCommittedEvent};
use super::{kind_to_raw, Notification, WriterState};
use crate::store::index::{DiskPos, IndexEntry};
use crate::store::segment::sidx::SidxEntry;
use crate::store::AppendReceipt;

pub(super) struct CommitArtifacts {
    pub(super) index_entry: IndexEntry,
    pub(super) sidx_entry: SidxEntry,
    pub(super) notification: Notification,
    pub(super) envelope: Option<CommittedEventEnvelope>,
}

#[derive(Clone, Copy)]
pub(super) struct CommitInternedIds {
    pub(super) entity_id: crate::store::index::interner::InternId,
    pub(super) scope_id: crate::store::index::interner::InternId,
}

pub(super) struct BatchCommitArtifacts {
    pub(super) entries: Vec<IndexEntry>,
    pub(super) sidx_entries: Vec<SidxEntry>,
    pub(super) notifications: Vec<Notification>,
    pub(super) envelopes: Vec<CommittedEventEnvelope>,
}

impl BatchCommitArtifacts {
    pub(super) fn with_capacity(len: usize) -> Self {
        Self {
            entries: Vec::with_capacity(len),
            sidx_entries: Vec::with_capacity(len),
            notifications: Vec::with_capacity(len),
            envelopes: Vec::with_capacity(len),
        }
    }

    fn push(&mut self, committed: CommitArtifacts) {
        self.entries.push(committed.index_entry);
        self.sidx_entries.push(committed.sidx_entry);
        self.notifications.push(committed.notification);
        if let Some(envelope) = committed.envelope {
            self.envelopes.push(envelope);
        }
    }
}

impl WriterState<'_> {
    pub(super) fn materialize_commit_artifacts(
        &self,
        staged: &StagedCommittedEvent,
        disk_pos: DiskPos,
        interned_ids: CommitInternedIds,
        payload_bytes: &[u8],
        flags: u8,
        emit_envelope: bool,
    ) -> CommitArtifacts {
        let coord = staged.coord.clone();
        let position = staged.position();
        let notification = Notification {
            event_id: staged.meta.event_id,
            correlation_id: staged.meta.correlation_id,
            causation_id: staged.meta.causation_id,
            coord: coord.clone(),
            kind: staged.meta.kind,
            sequence: staged.meta.global_sequence,
            position,
        };
        let index_entry = IndexEntry {
            event_id: staged.meta.event_id,
            correlation_id: staged.meta.correlation_id,
            causation_id: staged.meta.causation_id,
            coord: coord.clone(),
            entity_id: interned_ids.entity_id,
            scope_id: interned_ids.scope_id,
            kind: staged.meta.kind,
            wall_ms: staged.timing.wall_ms,
            clock: staged.timing.clock,
            dag_lane: staged.timing.dag_lane,
            dag_depth: staged.timing.dag_depth,
            hash_chain: staged.hash_chain.clone(),
            disk_pos,
            global_sequence: staged.meta.global_sequence,
        };
        let sidx_entry = SidxEntry {
            event_id: staged.meta.event_id,
            entity_idx: 0,
            scope_idx: 0,
            kind: kind_to_raw(staged.meta.kind),
            wall_ms: staged.timing.wall_ms,
            clock: staged.timing.clock,
            dag_lane: staged.timing.dag_lane,
            dag_depth: staged.timing.dag_depth,
            prev_hash: staged.hash_chain.prev_hash,
            event_hash: staged.hash_chain.event_hash,
            frame_offset: disk_pos.offset,
            frame_length: disk_pos.length,
            global_sequence: staged.meta.global_sequence,
            correlation_id: staged.meta.correlation_id,
            causation_id: staged.meta.causation_id.unwrap_or(0),
        };
        let envelope = if emit_envelope {
            staged
                .stored_event(payload_bytes, flags)
                .map(|stored| CommittedEventEnvelope {
                    notification: notification.clone(),
                    stored,
                })
                .ok()
        } else {
            None
        };
        CommitArtifacts {
            index_entry,
            sidx_entry,
            notification,
            envelope,
        }
    }

    /// STEP 12/14: Materialize all post-write views in one pass from the
    /// committed staged facts plus receipts. This is the product split over
    /// the same semantic source, so index/SIDX/notification/envelope derivation
    /// cannot silently drift apart.
    pub(super) fn materialize_batch_commit_artifacts(
        &self,
        prepared: &PreparedBatch,
        staged: &[StagedCommittedEvent],
        receipts: &[AppendReceipt],
    ) -> BatchCommitArtifacts {
        let emit_envelope = self.reactor_subscribers.has_subscribers();
        let mut artifacts = BatchCommitArtifacts::with_capacity(staged.len());
        let interned_ids = prepared.interned_ids(self.index);

        for ((item, staged), receipt) in prepared
            .items()
            .iter()
            .zip(staged.iter())
            .zip(receipts.iter())
        {
            let committed = self.materialize_commit_artifacts(
                staged,
                receipt.disk_pos,
                CommitInternedIds {
                    entity_id: interned_ids.entity_id(item),
                    scope_id: interned_ids.scope_id(item),
                },
                item.payload_bytes(),
                item.options().flags,
                emit_envelope,
            );
            artifacts.push(committed);
        }

        artifacts
    }

    pub(super) fn broadcast_commit_artifacts(
        &self,
        notifications: impl IntoIterator<Item = Notification>,
        envelopes: impl IntoIterator<Item = CommittedEventEnvelope>,
    ) {
        for notification in notifications {
            self.subscribers.broadcast(&notification);
        }
        for envelope in envelopes {
            self.reactor_subscribers.broadcast(&envelope);
        }
    }

    /// Publishes the index boundary for an unfenced commit, then notifies
    /// subscribers. ORDER IS LOAD-BEARING: subscribers woken by the broadcast
    /// must already be able to observe the events as visible — if the
    /// broadcast ran before the publish, a subscriber could read the
    /// notification, query the store, and see the entry still hidden.
    ///
    /// Call-sites for unfenced commits use only this helper; the raw
    /// `publish` + `broadcast_commit_artifacts` calls are intentionally kept
    /// private to this module so this ordering contract cannot be swapped by
    /// mistake.
    #[inline]
    pub(super) fn publish_then_broadcast_unfenced(
        &mut self,
        publish_up_to: u64,
        notifications: impl IntoIterator<Item = Notification>,
        envelopes: impl IntoIterator<Item = CommittedEventEnvelope>,
    ) -> Result<(), crate::store::StoreError> {
        self.index
            .publish(publish_up_to, "publish_then_broadcast_unfenced")?;
        self.broadcast_commit_artifacts(notifications, envelopes);
        Ok(())
    }

    /// Finishes a visibility fence (publishes the hidden range), then notifies
    /// subscribers. Same ordering contract as
    /// [`publish_then_broadcast_unfenced`]: visibility must be established
    /// before the broadcast, or a subscriber could observe a notification for
    /// an entry that is still hidden.
    ///
    /// `publish_up_to` is an `Option<u64>` because a fence with no recorded
    /// progress (no fenced appends committed) finishes without advancing
    /// the visible watermark; the index's `finish_visibility_fence` accepts
    /// that shape directly.
    #[inline]
    pub(super) fn fence_finish_then_broadcast(
        &mut self,
        token: u64,
        publish_up_to: Option<u64>,
        notifications: impl IntoIterator<Item = Notification>,
        envelopes: impl IntoIterator<Item = CommittedEventEnvelope>,
    ) -> Result<(), crate::store::StoreError> {
        self.index.finish_visibility_fence(token, publish_up_to)?;
        self.broadcast_commit_artifacts(notifications, envelopes);
        Ok(())
    }
}
