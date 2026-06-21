use super::super::fanout::CommittedEventEnvelope;
use super::super::staging::{PreparedBatch, StagedCommittedEvent};
use super::{kind_to_raw, Notification, WriterState};
use crate::store::index::{DiskPos, IndexEntry};
use crate::store::segment::sidx::SidxEntry;
use crate::store::stats::HlcPoint;
use crate::store::{AppendReceipt, EncodedBytes, ExtensionKey};
use std::collections::BTreeMap;

fn broadcast_all<T>(values: impl IntoIterator<Item = T>, mut broadcast: impl FnMut(&T)) -> usize {
    let mut count = 0usize;
    for value in values {
        count += 1;
        broadcast(&value);
    }
    count
}

#[derive(Clone, Copy)]
struct LanePublishPoint {
    publish_up_to: u64,
    frontier_point: HlcPoint,
}

fn lane_publish_points_from_notifications(
    notifications: &[Notification],
) -> BTreeMap<u32, LanePublishPoint> {
    let mut points = BTreeMap::new();
    for notification in notifications {
        let lane = notification.position.lane();
        let publish_up_to = notification.sequence.saturating_add(1);
        let frontier_point = HlcPoint {
            wall_ms: notification.position.wall_ms(),
            global_sequence: notification.sequence,
        };
        points
            .entry(lane)
            .and_modify(|current: &mut LanePublishPoint| {
                if publish_up_to > current.publish_up_to {
                    *current = LanePublishPoint {
                        publish_up_to,
                        frontier_point,
                    };
                }
            })
            .or_insert(LanePublishPoint {
                publish_up_to,
                frontier_point,
            });
    }
    points
}

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

#[derive(Clone, Copy)]
pub(super) struct CommitFrameView<'a> {
    pub(super) payload_bytes: &'a [u8],
    pub(super) flags: u8,
    pub(super) receipt_extensions: &'a BTreeMap<ExtensionKey, EncodedBytes>,
    pub(super) emit_envelope: bool,
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
        frame: CommitFrameView<'_>,
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
            receipt_extensions: frame.receipt_extensions.clone(),
        };
        let sidx_entry = SidxEntry {
            event_id: staged.meta.event_id,
            // Placeholder string-table slots: SidxEntryCollector::record rewrites
            // them to the correct entity/scope indexes when the footer is built.
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
        let envelope = if frame.emit_envelope {
            staged
                .stored_event(frame.payload_bytes, frame.flags)
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
                CommitFrameView {
                    payload_bytes: item.payload_bytes(),
                    flags: item.options().flags,
                    receipt_extensions: &receipt.extensions,
                    emit_envelope,
                },
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
        let push_notifications = broadcast_all(notifications, |notification| {
            self.subscribers.broadcast(notification)
        });
        let push_envelopes = broadcast_all(envelopes, |envelope| {
            self.reactor_subscribers.broadcast(envelope);
        });
        tracing::trace!(
            target: "batpak::fanout",
            push_notifications,
            push_envelopes,
            "commit fanout batch",
        );
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
        frontier_point: HlcPoint,
        notifications: impl IntoIterator<Item = Notification>,
        envelopes: impl IntoIterator<Item = CommittedEventEnvelope>,
    ) -> Result<(), crate::store::StoreError> {
        let notifications: Vec<Notification> = notifications.into_iter().collect();
        let lane_points = lane_publish_points_from_notifications(&notifications);
        self.index.publish_on_lanes(
            publish_up_to,
            lane_points
                .iter()
                .map(|(lane, point)| (*lane, point.publish_up_to)),
            "publish_then_broadcast_unfenced",
        )?;
        self.broadcast_commit_artifacts(notifications, envelopes);
        let mut watermark = self.watermark_handle.lock();
        watermark.advance_visible_and_emitted(frontier_point);
        for (lane, point) in lane_points {
            watermark.advance_visible_and_emitted_on_lane(lane, point.frontier_point);
        }
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
        frontier_point: Option<HlcPoint>,
        notifications: impl IntoIterator<Item = Notification>,
        envelopes: impl IntoIterator<Item = CommittedEventEnvelope>,
    ) -> Result<(), crate::store::StoreError> {
        let notifications: Vec<Notification> = notifications.into_iter().collect();
        let lane_points = lane_publish_points_from_notifications(&notifications);
        self.index.finish_visibility_fence_on_lanes(
            token,
            publish_up_to,
            lane_points
                .iter()
                .map(|(lane, point)| (*lane, point.publish_up_to)),
        )?;
        self.broadcast_commit_artifacts(notifications, envelopes);
        let mut watermark = self.watermark_handle.lock();
        if let Some(point) = frontier_point {
            watermark.advance_visible_and_emitted(point);
        }
        for (lane, point) in lane_points {
            watermark.advance_visible_and_emitted_on_lane(lane, point.frontier_point);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{broadcast_all, lane_publish_points_from_notifications, Notification};
    use crate::coordinate::{Coordinate, DagPosition};
    use crate::event::EventKind;

    fn notification_for(lane: u32, sequence: u64, wall_ms: u64) -> Notification {
        Notification {
            event_id: 0,
            correlation_id: 0,
            causation_id: None,
            coord: Coordinate::new("entity", "scope").expect("valid coordinate"),
            kind: EventKind::DATA,
            sequence,
            position: DagPosition::with_hlc(
                wall_ms,
                0,
                0,
                lane,
                u32::try_from(sequence).unwrap_or(u32::MAX),
            ),
        }
    }

    #[test]
    fn lane_publish_points_keep_first_on_equal_publish_up_to() {
        // Two notifications on the SAME lane with the SAME sequence (hence the
        // same `publish_up_to = sequence + 1`) but DIFFERENT wall_ms. The
        // `and_modify` only overwrites when the new `publish_up_to` is strictly
        // greater, so equal values must keep the FIRST point. The `> -> >=`
        // mutant would overwrite with the second (wall_ms = 222) instead.
        let notifications = vec![notification_for(7, 5, 111), notification_for(7, 5, 222)];

        let points = lane_publish_points_from_notifications(&notifications);
        let point = points.get(&7).expect("lane 7 must be present");

        assert_eq!(
            point.publish_up_to, 6,
            "PROPERTY: publish_up_to is sequence + 1"
        );
        assert_eq!(
            point.frontier_point.wall_ms, 111,
            "PROPERTY: equal publish_up_to must NOT overwrite the first lane point"
        );
    }

    #[test]
    fn lane_publish_points_advance_on_strictly_greater() {
        // Sanity companion: a strictly greater publish_up_to DOES overwrite, so
        // the test above is pinning the equality boundary, not blanket no-update.
        let notifications = vec![notification_for(3, 5, 111), notification_for(3, 9, 222)];

        let points = lane_publish_points_from_notifications(&notifications);
        let point = points.get(&3).expect("lane 3 must be present");

        assert_eq!(point.publish_up_to, 10);
        assert_eq!(point.frontier_point.wall_ms, 222);
    }

    #[test]
    fn broadcast_all_counts_every_pushed_item() {
        let mut pushed = Vec::new();
        let count = broadcast_all([10, 20, 30], |item| pushed.push(*item));

        assert_eq!(
            count, 3,
            "PROPERTY: fanout telemetry count must advance once per pushed item"
        );
        assert_eq!(
            pushed,
            vec![10, 20, 30],
            "PROPERTY: count helper must still broadcast each item in order"
        );
    }
}
