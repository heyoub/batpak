use super::columnar::{CachedProjectionSlot, ProjectionCacheStoreStatus};
use super::{DiskPos, StoreIndex};
use crate::event::EventKind;
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
    pub(crate) disk_pos: DiskPos,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionReplayPlan {
    pub(crate) watermark: u64,
    pub(crate) generation: u64,
    pub(crate) items: Vec<ProjectionReplayItem>,
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
        if let Some((watermark, generation, items)) =
            self.scan.projection_candidates(entity, relevant_kinds)
        {
            return Some(ProjectionReplayPlan {
                watermark,
                generation,
                items: items
                    .into_iter()
                    .map(|(global_sequence, disk_pos)| ProjectionReplayItem {
                        global_sequence,
                        disk_pos,
                    })
                    .collect(),
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
            items.push(ProjectionReplayItem {
                global_sequence: entry.global_sequence,
                disk_pos: entry.disk_pos,
            });
        }

        Some(ProjectionReplayPlan {
            watermark: watermark?,
            generation: stream.value().len() as u64,
            items,
        })
    }
}
