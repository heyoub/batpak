pub(crate) mod checkpoint;
pub(crate) mod mmap;
pub(crate) mod rebuild;
pub(crate) mod row;

#[cfg(test)]
pub(crate) use row::raw_to_kind;
pub(crate) use row::{
    kind_to_raw, raw_to_kind_counted, ColdStartIndexRow, ColdStartSource,
    ReservedKindFallbackStats, WatermarkInfo,
};

use crate::store::StoreError;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColdStartArtifactKind {
    MmapIndex,
    Checkpoint,
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
    for entry in std::fs::read_dir(data_dir).map_err(StoreError::Io)? {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        let is_segment = path
            .extension()
            .map(|ext| ext == crate::store::segment::SEGMENT_EXTENSION)
            .unwrap_or(false);
        if !is_segment {
            continue;
        }
        let Some(segment_id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u64>().ok())
        else {
            continue;
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
            let offset = std::fs::metadata(&path).map_err(StoreError::Io)?.len();
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
    match std::fs::metadata(&watermark_segment_path) {
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
