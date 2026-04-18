pub(crate) mod checkpoint;
pub(crate) mod mmap;
pub(crate) mod rebuild;

use crate::coordinate::Coordinate;
use crate::event::{EventHeader, HashChain};
use crate::store::index::interner::InternId;
use crate::store::index::{DiskPos, IndexEntry};
use crate::store::StoreError;
use std::fs::File;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColdStartSource {
    Checkpoint,
    MmapIndex,
    Sidx,
}

impl ColdStartSource {
    fn label(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::MmapIndex => "mmap index",
            Self::Sidx => "SIDX",
        }
    }
}

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

pub(crate) fn reject_symlink_leaf(path: &Path, purpose: &str) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "refusing to write {purpose} through symlink {}",
                path.display()
            ),
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

pub(crate) fn write_artifact_atomically(
    data_dir: &Path,
    final_path: &Path,
    purpose: &str,
    write: impl FnOnce(&mut File) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    reject_symlink_leaf(final_path, purpose)?;
    let tmp = NamedTempFile::new_in(data_dir)?;
    let mut file = tmp.reopen().map_err(StoreError::Io)?;
    write(&mut file)?;
    file.sync_all().map_err(StoreError::Io)?;
    drop(file);
    persist_with_parent_fsync(tmp, final_path).map_err(StoreError::Io)?;
    Ok(())
}

/// Persist `named_temp` as `final_path` with parent-directory fsync.
///
/// The tempfile's contents must already be fsynced by the caller (this
/// function only handles the rename + directory-entry durability). On
/// unix the parent directory is opened and `sync_all`-ed after the
/// rename, so a crash immediately after this returns cannot lose the
/// directory entry that points at the new inode. On non-unix targets
/// `File::sync_all` on a directory is not meaningful, so the parent
/// fsync is skipped — the asymmetry is platform-level, not a shortcut:
/// windows POSIX semantics for directory fsync simply do not exist.
pub(crate) fn persist_with_parent_fsync(
    named_temp: tempfile::NamedTempFile,
    final_path: &Path,
) -> std::io::Result<()> {
    // Fsync the temp file one more time defensively — `write_artifact_atomically`
    // already does this, but callers that construct their own NamedTempFile
    // can rely on this helper being the durability boundary.
    {
        let handle = named_temp.as_file();
        handle.sync_all()?;
    }

    named_temp
        .persist(final_path)
        .map_err(|error| error.error)?;

    #[cfg(unix)]
    {
        if let Some(parent) = final_path.parent() {
            // Opening the parent dir read-only and calling sync_all is the
            // POSIX-idiomatic way to flush the directory entry for the
            // renamed-in file. Without this, a crash after the rename
            // returns can leave the rename itself undone on disk.
            let dir = std::fs::File::open(parent)?;
            dir.sync_all()?;
        }
    }
    #[cfg(not(unix))]
    {
        // Non-unix: File::sync_all on a directory handle is not meaningful
        // and returns an error on some platforms. The rename itself is
        // the durability point the OS provides here.
        let _ = final_path;
    }
    Ok(())
}

/// Canonical persisted-index row shared by cold-start artifact readers.
///
/// This is intentionally narrower than `EventHeader`: it carries only the
/// persisted facts shared across checkpoint, mmap, and SIDX restore paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColdStartIndexRow {
    pub(crate) source: ColdStartSource,
    pub(crate) event_id: u128,
    pub(crate) correlation_id: u128,
    pub(crate) causation_id: Option<u128>,
    pub(crate) entity_id: InternId,
    pub(crate) scope_id: InternId,
    pub(crate) kind: crate::event::EventKind,
    pub(crate) wall_ms: u64,
    pub(crate) clock: u32,
    pub(crate) dag_lane: u32,
    pub(crate) dag_depth: u32,
    pub(crate) hash_chain: HashChain,
    pub(crate) disk_pos: DiskPos,
    pub(crate) global_sequence: u64,
}

impl ColdStartIndexRow {
    fn resolve_part<'a>(
        &self,
        interner_strings: &'a [String],
        id: InternId,
        field: &str,
    ) -> Result<&'a str, StoreError> {
        interner_strings
            .get(id.to_usize())
            .map(String::as_str)
            .ok_or_else(|| {
                StoreError::ser_msg(&format!(
                    "{} {} is out of interner range",
                    self.source.label(),
                    field
                ))
            })
    }

    pub(crate) fn resolve_strings(
        &self,
        interner_strings: &[String],
    ) -> Result<(String, String), StoreError> {
        Ok((
            self.resolve_part(interner_strings, self.entity_id, "entity_id")?
                .to_owned(),
            self.resolve_part(interner_strings, self.scope_id, "scope_id")?
                .to_owned(),
        ))
    }

    pub(crate) fn to_index_entry(
        &self,
        interner_strings: &[String],
    ) -> Result<IndexEntry, StoreError> {
        let entity = self.resolve_part(interner_strings, self.entity_id, "entity_id")?;
        let scope = self.resolve_part(interner_strings, self.scope_id, "scope_id")?;
        let coord = Coordinate::new(entity, scope)?;
        Ok(IndexEntry {
            event_id: self.event_id,
            correlation_id: self.correlation_id,
            causation_id: self.causation_id,
            coord,
            entity_id: self.entity_id,
            scope_id: self.scope_id,
            kind: self.kind,
            wall_ms: self.wall_ms,
            clock: self.clock,
            dag_lane: self.dag_lane,
            dag_depth: self.dag_depth,
            hash_chain: self.hash_chain.clone(),
            disk_pos: self.disk_pos,
            global_sequence: self.global_sequence,
        })
    }

    pub(crate) fn to_event_header(&self) -> EventHeader {
        EventHeader::new(
            self.event_id,
            self.correlation_id,
            self.causation_id,
            (self.wall_ms * 1000) as i64,
            crate::coordinate::DagPosition::with_hlc(
                self.wall_ms,
                0,
                self.dag_depth,
                self.dag_lane,
                self.clock,
            ),
            0,
            self.kind,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ColdStartIndexRow, ColdStartSource};
    use crate::event::{EventKind, HashChain};
    use crate::store::index::interner::InternId;
    use crate::store::DiskPos;

    #[test]
    fn cold_start_row_to_event_header_preserves_lane_depth_and_ids() {
        let row = ColdStartIndexRow {
            source: ColdStartSource::Sidx,
            event_id: 1,
            correlation_id: 2,
            causation_id: Some(3),
            entity_id: InternId(1),
            scope_id: InternId(2),
            kind: EventKind::DATA,
            wall_ms: 1_700_000_000_000,
            clock: 9,
            dag_lane: 4,
            dag_depth: 2,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos::new(7, 64, 32),
            global_sequence: 11,
        };

        let header = row.to_event_header();
        assert_eq!(header.event_id, 1);
        assert_eq!(header.correlation_id, 2);
        assert_eq!(header.causation_id, Some(3));
        assert_eq!(header.timestamp_us, 1_700_000_000_000_000);
        assert_eq!(header.position.wall_ms, 1_700_000_000_000);
        assert_eq!(header.position.sequence, 9);
        assert_eq!(header.position.lane, 4);
        assert_eq!(header.position.depth, 2);
        assert_eq!(header.event_kind, EventKind::DATA);
        assert_eq!(header.payload_size, 0);
        assert_eq!(header.flags, 0);
        assert_eq!(header.content_hash, [0u8; 32]);
    }
}
