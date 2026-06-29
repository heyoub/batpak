//! Store-owned filename classification.
//!
//! This keeps segment, snapshot, and cold-start artifact recognition in one
//! place so lifecycle code does not grow local filename folklore.

use crate::store::segment::{id::SegmentNameError, SegmentId, SEGMENT_EXTENSION};
use std::path::Path;

pub(crate) const COMPACT_SOURCE_EXTENSION: &str = "compact-src";
pub(crate) const CURSOR_DIRECTORY: &str = "cursors";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StoreFileKind {
    Segment(SegmentId),
    MalformedSegment(SegmentNameError),
    VisibilityRanges,
    Checkpoint,
    MmapIndex,
    IdempotencyStore,
    PendingCompactionMarker,
    CompactSource,
    CursorDirectory,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForkStrategy {
    ShareIfPossible,
    DeepCopyAlways,
    CacheRegenerable,
    Exclude,
}

impl StoreFileKind {
    pub(crate) fn from_path(path: &Path) -> Self {
        if path.extension().is_some_and(|ext| ext == SEGMENT_EXTENSION) {
            return match SegmentId::from_filename(path) {
                Ok(segment_id) => Self::Segment(segment_id),
                Err(error) => Self::MalformedSegment(error),
            };
        }

        if path
            .extension()
            .is_some_and(|ext| ext == COMPACT_SOURCE_EXTENSION)
        {
            return Self::CompactSource;
        }

        match path.file_name().and_then(|name| name.to_str()) {
            Some(crate::store::hidden_ranges::VISIBILITY_RANGES_FILENAME) => Self::VisibilityRanges,
            Some(crate::store::cold_start::checkpoint::CHECKPOINT_FILENAME) => Self::Checkpoint,
            Some(crate::store::cold_start::mmap::MMAP_INDEX_FILENAME) => Self::MmapIndex,
            Some(crate::store::index::idemp::IDEMP_FILENAME) => Self::IdempotencyStore,
            Some(crate::store::cold_start::rebuild::COMPACTION_MARKER_FILENAME) => {
                Self::PendingCompactionMarker
            }
            Some(CURSOR_DIRECTORY) => Self::CursorDirectory,
            _ => Self::Other,
        }
    }

    pub(crate) fn segment_id(&self) -> Option<SegmentId> {
        match self {
            Self::Segment(segment_id) => Some(*segment_id),
            Self::MalformedSegment(_)
            | Self::VisibilityRanges
            | Self::Checkpoint
            | Self::MmapIndex
            | Self::IdempotencyStore
            | Self::PendingCompactionMarker
            | Self::CompactSource
            | Self::CursorDirectory
            | Self::Other => None,
        }
    }

    pub(crate) fn should_copy_into_snapshot(&self) -> bool {
        // The durable idempotency store is a correctness authority, so a
        // snapshot must carry it forward — otherwise restoring from the
        // snapshot would silently lose cross-compaction dedup memory.
        // justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
        matches!(
            self,
            Self::Segment(_)
                | Self::VisibilityRanges
                | Self::IdempotencyStore
                | Self::PendingCompactionMarker
        )
    }

    pub(crate) fn should_clear_from_snapshot_destination(&self) -> bool {
        matches!(
            self,
            Self::Segment(_)
                | Self::MalformedSegment(_)
                | Self::VisibilityRanges
                | Self::Checkpoint
                | Self::MmapIndex
                | Self::IdempotencyStore
                | Self::PendingCompactionMarker
                | Self::CompactSource
        )
    }

    pub(crate) fn fork_strategy(&self, active_segment_id: u64) -> ForkStrategy {
        match self {
            Self::Segment(segment_id) if segment_id.as_u64() < active_segment_id => {
                ForkStrategy::ShareIfPossible
            }
            Self::Segment(segment_id) if segment_id.as_u64() == active_segment_id => {
                ForkStrategy::DeepCopyAlways
            }
            Self::VisibilityRanges | Self::IdempotencyStore | Self::PendingCompactionMarker => {
                ForkStrategy::DeepCopyAlways
            }
            Self::Checkpoint | Self::MmapIndex => ForkStrategy::CacheRegenerable,
            Self::Segment(_)
            | Self::MalformedSegment(_)
            | Self::CompactSource
            | Self::CursorDirectory
            | Self::Other => ForkStrategy::Exclude,
        }
    }

    pub(crate) fn should_clear_from_fork_destination(&self) -> bool {
        matches!(
            self,
            Self::Segment(_)
                | Self::MalformedSegment(_)
                | Self::VisibilityRanges
                | Self::Checkpoint
                | Self::MmapIndex
                | Self::IdempotencyStore
                | Self::PendingCompactionMarker
                | Self::CompactSource
                | Self::CursorDirectory
        )
    }
}

#[cfg(test)]
mod tests {
    use super::StoreFileKind;
    use crate::store::segment::SegmentId;

    #[test]
    fn should_clear_from_fork_destination_discriminates_store_artifacts_from_other() {
        // PROPERTY: the fork pre-clear pass must wipe store-shaped artifacts left
        // in a destination but leave foreign files (`Other`) untouched. A blanket
        // `-> true` would also clear caller files, so assert BOTH polarities.
        // Kills `should_clear_from_fork_destination -> bool with true`.
        assert!(
            !StoreFileKind::Other.should_clear_from_fork_destination(),
            "a foreign (Other) file must NOT be cleared from a fork destination"
        );
        let segment_id = SegmentId::from_stem("0").expect("base-10 stem parses");
        assert!(
            StoreFileKind::Segment(segment_id).should_clear_from_fork_destination(),
            "a store segment MUST be cleared from a fork destination before copy"
        );
    }
}
