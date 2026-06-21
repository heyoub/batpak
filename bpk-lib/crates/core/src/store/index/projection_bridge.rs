use super::columnar::CachedProjectionSlot;
use super::{DiskPos, IndexEntry, StoreIndex};
use crate::event::EventKind;
use crate::store::stats::HlcPoint;
use std::any::TypeId;

#[inline]
pub(crate) fn projection_kind_matches(relevant_kinds: &[EventKind], kind: EventKind) -> bool {
    match relevant_kinds {
        [] => true,
        [only] => *only == kind,
        [first, second] => *first == kind || *second == kind,
        many => many.contains(&kind),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionReplayItem {
    pub(crate) global_sequence: u64,
    pub(crate) lane: u32,
    pub(crate) point: HlcPoint,
    pub(crate) disk_pos: DiskPos,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionReplayPlan {
    pub(crate) watermark: u64,
    pub(crate) generation: u64,
    pub(crate) items: Vec<ProjectionReplayItem>,
}

impl ProjectionReplayItem {
    fn from_entry(entry: &IndexEntry) -> Self {
        Self {
            global_sequence: entry.global_sequence,
            lane: entry.dag_lane,
            point: HlcPoint {
                wall_ms: entry.wall_ms,
                global_sequence: entry.global_sequence,
            },
            disk_pos: entry.disk_pos,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProjectionCacheStoreStatus {
    Stored,
    MissingEntity,
    UnsupportedTopology,
}

#[cfg(test)]
impl ProjectionCacheStoreStatus {
    pub(crate) fn is_stored(self) -> bool {
        matches!(self, Self::Stored)
    }
}

impl StoreIndex {
    pub(crate) fn entity_generation(&self, entity: &str) -> Option<u64> {
        let _read = self.swap_gate.read();
        self.scan.entity_generation(entity).or_else(|| {
            self.streams
                .get(entity)
                .map(|entries| entries.value().len() as u64)
        })
    }

    pub(crate) fn cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
    ) -> Option<CachedProjectionSlot> {
        let _read = self.swap_gate.read();
        self.scan.cached_projection(entity, type_id)
    }

    pub(crate) fn store_cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
        bytes: Vec<u8>,
        watermark: u64,
    ) -> ProjectionCacheStoreStatus {
        self.scan
            .store_cached_projection(entity, type_id, bytes, watermark)
    }

    pub(crate) fn projection_replay_plan(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionReplayPlan> {
        let _read = self.swap_gate.read();
        let visibility = self.sequence.snapshot();
        if let Some((watermark, generation, items)) =
            self.scan.projection_candidates(entity, relevant_kinds)
        {
            let items: Vec<ProjectionReplayItem> = items
                .into_iter()
                .filter_map(|hit| self.upgrade_hit_visible_on_lane(hit, &visibility))
                .map(|entry| ProjectionReplayItem::from_entry(&entry))
                .collect();
            return Some(ProjectionReplayPlan {
                watermark,
                generation,
                items,
            });
        }

        let stream = self.streams.get(entity)?;
        let mut items = Vec::new();
        let mut watermark = None;
        for entry in stream.value().values() {
            if !projection_kind_matches(relevant_kinds, entry.kind) {
                continue;
            }
            watermark = Some(entry.global_sequence);
            if !visibility.is_visible_on_lane(entry.global_sequence, entry.dag_lane) {
                continue;
            }
            items.push(ProjectionReplayItem::from_entry(entry));
        }

        Some(ProjectionReplayPlan {
            watermark: watermark?,
            generation: stream.value().len() as u64,
            items,
        })
    }
}
