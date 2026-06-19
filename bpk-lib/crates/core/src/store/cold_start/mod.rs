pub(crate) mod checkpoint;
pub(crate) mod mmap;
/// Rebuild-path reports and open-index diagnostics.
pub mod rebuild;
pub(crate) mod row;

#[cfg(test)]
pub(crate) use row::raw_to_kind;
pub(crate) use row::{
    kind_to_raw, raw_to_kind_counted, ColdStartIndexRow, ColdStartSource,
    ReservedKindFallbackStats, WatermarkInfo,
};

use crate::store::{file_classification::StoreFileKind, platform, StoreError};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColdStartArtifactKind {
    MmapIndex,
    Checkpoint,
}

#[derive(Debug)]
pub(crate) enum FileLoad<T> {
    Missing,
    Loaded(T),
    Invalid {
        reason: String,
    },
    /// The on-disk artifact declares a format version strictly newer than this
    /// binary supports. This is a CANONICAL TYPED REFUSAL, distinct from
    /// `Invalid` (corrupt/older → safe to rebuild from scan): a future-version
    /// artifact must NOT be silently rebuilt, because a future writer may have
    /// written data this reader cannot interpret. The cold-start flow propagates
    /// this as a hard error rather than swallowing it into a rebuild.
    FutureVersion {
        /// Version stamped on the on-disk file.
        found: u16,
        /// The maximum version this binary understands.
        supported: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColdStartPolicy {
    try_checkpoint: bool,
    try_mmap_index: bool,
}

impl ColdStartPolicy {
    pub(crate) fn new(enable_checkpoint: bool, enable_mmap_index: bool) -> Self {
        Self {
            try_checkpoint: enable_checkpoint,
            try_mmap_index: enable_mmap_index,
        }
    }

    pub(crate) fn try_checkpoint(self) -> bool {
        self.try_checkpoint
    }

    pub(crate) fn try_mmap_index(self) -> bool {
        self.try_mmap_index
    }

    pub(crate) fn write_target(self) -> Option<ColdStartArtifactKind> {
        if self.try_mmap_index {
            Some(ColdStartArtifactKind::MmapIndex)
        } else if self.try_checkpoint {
            Some(ColdStartArtifactKind::Checkpoint)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub(crate) enum WatermarkValidationError {
    MissingSegment {
        path: PathBuf,
    },
    OffsetPastTail {
        path: PathBuf,
        file_len: u64,
        watermark_offset: u64,
    },
}

pub(crate) fn latest_segment_watermark(data_dir: &Path) -> Result<(u64, u64), StoreError> {
    let mut max: Option<(u64, PathBuf)> = None;
    for entry in platform::fs::read_dir(data_dir).map_err(StoreError::Io)? {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        let segment_id = match StoreFileKind::from_path(&path) {
            StoreFileKind::Segment(segment_id) => segment_id.as_u64(),
            StoreFileKind::MalformedSegment(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping malformed segment filename"
                );
                continue;
            }
            StoreFileKind::VisibilityRanges
            | StoreFileKind::Checkpoint
            | StoreFileKind::MmapIndex
            | StoreFileKind::IdempotencyStore
            | StoreFileKind::PendingCompactionMarker
            | StoreFileKind::CompactSource
            | StoreFileKind::CursorDirectory
            | StoreFileKind::Other => continue,
        };
        if max
            .as_ref()
            .map(|(current, _)| segment_id > *current)
            .unwrap_or(true)
        {
            max = Some((segment_id, path));
        }
    }

    match max {
        Some((segment_id, path)) => {
            let offset = platform::fs::metadata(&path).map_err(StoreError::Io)?.len();
            Ok((segment_id, offset))
        }
        None => Ok((0, 0)),
    }
}

pub(crate) fn validate_watermark_segment(
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
) -> Result<(), WatermarkValidationError> {
    let watermark_segment_path = data_dir.join(crate::store::segment::segment_filename(
        watermark_segment_id,
    ));
    match platform::fs::metadata(&watermark_segment_path) {
        Ok(meta) if meta.len() >= watermark_offset => Ok(()),
        Ok(meta) => Err(WatermarkValidationError::OffsetPastTail {
            path: watermark_segment_path,
            file_len: meta.len(),
            watermark_offset,
        }),
        Err(_) => Err(WatermarkValidationError::MissingSegment {
            path: watermark_segment_path,
        }),
    }
}
