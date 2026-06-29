use super::{CachedProjectionSlot, ColumnarIndex, ColumnarVariant, ProjectionCandidates};
use crate::event::EventKind;
use crate::store::index::ProjectionCacheStoreStatus;
use std::any::TypeId;

impl ColumnarIndex {
    pub(crate) fn entity_generation(&self, entity: &str) -> Option<u64> {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => lock.read().entity_generation(entity),
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => None,
        }
    }

    pub(crate) fn cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
    ) -> Option<CachedProjectionSlot> {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => lock.read().cached_projection(entity, type_id),
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => None,
        }
    }

    pub(crate) fn store_cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
        bytes: Vec<u8>,
        watermark: u64,
    ) -> ProjectionCacheStoreStatus {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => match lock
                .write()
                .store_cached_projection(entity, type_id, bytes, watermark)
            {
                true => ProjectionCacheStoreStatus::Stored,
                false => ProjectionCacheStoreStatus::MissingEntity,
            },
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => {
                ProjectionCacheStoreStatus::UnsupportedTopology
            }
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => {
                ProjectionCacheStoreStatus::UnsupportedTopology
            }
        }
    }

    pub(crate) fn projection_candidates(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionCandidates> {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => {
                lock.read().projection_candidates(entity, relevant_kinds)
            }
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => None,
        }
    }
}
