use crate::store::file_classification::StoreFileKind;
use crate::store::platform;
use crate::store::platform::fs::StoreFs;
use crate::store::segment;
use crate::store::StoreError;
use std::path::{Path, PathBuf};

pub(crate) const COMPACTION_MARKER_FILENAME: &str = "compaction.pending.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingCompaction {
    pub(crate) merged_id: u64,
    pub(crate) source_segment_ids: Vec<u64>,
}

fn pending_compaction_path(data_dir: &Path) -> PathBuf {
    data_dir.join(COMPACTION_MARKER_FILENAME)
}

pub(super) fn compaction_source_temp_path(data_dir: &Path, merged_id: u64) -> PathBuf {
    data_dir.join(format!(
        "{merged_id:06}.{}.compact-src",
        segment::SEGMENT_EXTENSION
    ))
}

pub(super) fn load_pending_compaction(
    data_dir: &Path,
) -> Result<Option<PendingCompaction>, StoreError> {
    let path = pending_compaction_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = platform::fs::read(&path).map_err(StoreError::Io)?;
    let marker = serde_json::from_slice::<PendingCompaction>(&bytes)
        .map_err(|_| StoreError::DataDirMalformed { path: path.clone() })?;
    Ok(Some(marker))
}

pub(crate) fn write_pending_compaction(
    data_dir: &Path,
    merged_id: u64,
    source_segment_ids: &[u64],
    fs: &dyn StoreFs,
) -> Result<(), StoreError> {
    let marker = PendingCompaction {
        merged_id,
        source_segment_ids: source_segment_ids.to_vec(),
    };
    let final_path = pending_compaction_path(data_dir);
    crate::store::platform::fs::write_file_atomically_with_fs(
        data_dir,
        &final_path,
        "compaction marker",
        |file| {
            serde_json::to_writer(file, &marker).map_err(|e| StoreError::Serialization(Box::new(e)))
        },
        fs,
    )
}

pub(crate) fn clear_pending_compaction(
    data_dir: &Path,
    fs: &dyn StoreFs,
) -> Result<(), StoreError> {
    let path = pending_compaction_path(data_dir);
    match fs.remove_file(&path) {
        Ok(()) => {
            crate::store::platform::sync::sync_parent_dir(&path)?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StoreError::Io(err)),
    }
}

pub(super) fn segment_paths(data_dir: &Path) -> Result<Vec<(u64, PathBuf)>, StoreError> {
    let mut entries = Vec::new();
    for entry in platform::fs::read_dir(data_dir).map_err(StoreError::Io)? {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        let kind = StoreFileKind::from_path(&path);
        if let StoreFileKind::MalformedSegment(error) = &kind {
            tracing::warn!(
                path = %path.display(),
                %error,
                "skipping malformed segment filename"
            );
            continue;
        }
        // Every non-segment artifact (keyset, idempotency store, checkpoint, …)
        // reports no segment id and is skipped.
        let Some(segment_id) = kind.segment_id() else {
            continue;
        };
        entries.push((segment_id.as_u64(), path));
    }
    if let Some(marker) = load_pending_compaction(data_dir)? {
        let merged_present = entries
            .iter()
            .any(|(segment_id, _)| *segment_id == marker.merged_id);
        let temp_source_path = compaction_source_temp_path(data_dir, marker.merged_id);
        let temp_source_exists = temp_source_path.exists();
        let stale_finalized_marker = merged_present
            && !temp_source_exists
            && marker
                .source_segment_ids
                .iter()
                .filter(|&&segment_id| segment_id != marker.merged_id)
                .all(|segment_id| !entries.iter().any(|(id, _)| id == segment_id));

        if !stale_finalized_marker {
            if merged_present {
                entries.retain(|(segment_id, _)| {
                    *segment_id == marker.merged_id
                        || !marker
                            .source_segment_ids
                            .iter()
                            .any(|source_id| source_id == segment_id)
                });
            } else {
                if temp_source_exists {
                    entries.retain(|(segment_id, _)| *segment_id != marker.merged_id);
                    entries.push((marker.merged_id, temp_source_path));
                }
                for source_id in marker
                    .source_segment_ids
                    .iter()
                    .copied()
                    .filter(|source_id| *source_id != marker.merged_id)
                {
                    if !entries
                        .iter()
                        .any(|(segment_id, _)| *segment_id == source_id)
                    {
                        return Err(StoreError::DataDirMalformed {
                            path: pending_compaction_path(data_dir),
                        });
                    }
                }
            }
        }
    }
    entries.sort_by_key(|(segment_id, _)| *segment_id);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{clear_pending_compaction, pending_compaction_path};
    use crate::store::platform::fs::RealFs;
    use crate::store::StoreError;

    #[test]
    fn clear_pending_compaction_propagates_non_not_found_errors() {
        // The match guard `err.kind() == NotFound` must let real I/O errors
        // through. Replacing it with `true` would swallow EVERY removal failure as
        // Ok(()). A DIRECTORY at the marker path makes `remove_file` fail with a
        // non-NotFound error (EISDIR/EPERM), which must surface as Err.
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = pending_compaction_path(dir.path());
        std::fs::create_dir(&marker).expect("create directory at marker path");

        let result = clear_pending_compaction(dir.path(), &RealFs);
        assert!(
            matches!(result, Err(StoreError::Io(_))),
            "a non-NotFound removal failure must propagate as Err(Io), not be \
             swallowed by the NotFound guard; got {result:?}"
        );
    }

    #[test]
    fn clear_pending_compaction_is_ok_when_marker_is_absent() {
        // The NotFound arm: an absent marker is a clean no-op success — this
        // anchors that the error-path test above is the discriminating case.
        let dir = tempfile::tempdir().expect("tempdir");
        clear_pending_compaction(dir.path(), &RealFs).expect("absent marker clears cleanly");
    }
}
