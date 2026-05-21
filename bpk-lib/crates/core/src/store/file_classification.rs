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
    PendingCompactionMarker,
    CompactSource,
    CursorDirectory,
    Other,
}

impl StoreFileKind {
    pub(crate) fn from_path(path: &Path) -> Self {
        if path
            .extension()
            .map(|ext| ext == SEGMENT_EXTENSION)
            .unwrap_or(false)
        {
            return match SegmentId::from_filename(path) {
                Ok(segment_id) => Self::Segment(segment_id),
                Err(error) => Self::MalformedSegment(error),
            };
        }

        if path
            .extension()
            .map(|ext| ext == COMPACT_SOURCE_EXTENSION)
            .unwrap_or(false)
        {
            return Self::CompactSource;
        }

        match path.file_name().and_then(|name| name.to_str()) {
            Some(crate::store::hidden_ranges::VISIBILITY_RANGES_FILENAME) => Self::VisibilityRanges,
            Some(crate::store::cold_start::checkpoint::CHECKPOINT_FILENAME) => Self::Checkpoint,
            Some(crate::store::cold_start::mmap::MMAP_INDEX_FILENAME) => Self::MmapIndex,
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
            | Self::PendingCompactionMarker
            | Self::CompactSource
            | Self::CursorDirectory
            | Self::Other => None,
        }
    }

    pub(crate) fn should_copy_into_snapshot(&self) -> bool {
        matches!(
            self,
            Self::Segment(_) | Self::VisibilityRanges | Self::PendingCompactionMarker
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
                | Self::PendingCompactionMarker
                | Self::CompactSource
        )
    }
}
