use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::io::{BufWriter, Write};
use std::path::Path;
use tempfile::NamedTempFile;

pub(crate) const VISIBILITY_RANGES_MAGIC: &[u8; 6] = b"FBATVR";
pub(crate) const VISIBILITY_RANGES_VERSION: u16 = 1;
pub(crate) const VISIBILITY_RANGES_FILENAME: &str = "visibility_ranges.fbv";

#[derive(Serialize, Deserialize)]
struct VisibilityRangesData {
    ranges: Vec<VisibilityRangeEntry>,
}

#[derive(Serialize, Deserialize)]
struct VisibilityRangeEntry {
    start: u64,
    end: u64,
}

fn normalize_ranges(ranges: &[(u64, u64)]) -> Result<Vec<(u64, u64)>, StoreError> {
    let mut normalized: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for &(start, end) in ranges {
        if start >= end {
            return Err(StoreError::RangeMalformed { start, end });
        }
        normalized.push((start, end));
    }
    normalized.sort_by_key(|(start, _)| *start);

    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(normalized.len());
    for (start, end) in normalized {
        if let Some((_, merged_end)) = merged.last_mut() {
            if start <= *merged_end {
                *merged_end = (*merged_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    Ok(merged)
}

pub(crate) fn write_cancelled_ranges(
    data_dir: &Path,
    ranges: &[(u64, u64)],
) -> Result<(), StoreError> {
    let final_path = data_dir.join(VISIBILITY_RANGES_FILENAME);
    reject_symlink_leaf(&final_path)?;

    let normalized = normalize_ranges(ranges)?;
    if normalized.is_empty() {
        match std::fs::remove_file(&final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::Io(error)),
        }
        return Ok(());
    }

    let body = rmp_serde::to_vec_named(&VisibilityRangesData {
        ranges: normalized
            .into_iter()
            .map(|(start, end)| VisibilityRangeEntry { start, end })
            .collect(),
    })
    .map_err(|error| StoreError::Serialization(Box::new(error)))?;
    let crc = crc32fast::hash(&body);

    let tmp = NamedTempFile::new_in(data_dir)?;
    {
        let file = tmp.as_file();
        let mut writer = BufWriter::new(file);
        writer.write_all(VISIBILITY_RANGES_MAGIC)?;
        writer.write_all(&VISIBILITY_RANGES_VERSION.to_le_bytes())?;
        writer.write_all(&crc.to_le_bytes())?;
        writer.write_all(&body)?;
        writer.flush()?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(&final_path)
        .map_err(|error| StoreError::Io(error.error))?;
    Ok(())
}

pub(crate) fn try_load_cancelled_ranges(data_dir: &Path) -> Option<Vec<(u64, u64)>> {
    let path = data_dir.join(VISIBILITY_RANGES_FILENAME);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                target: "batpak::visibility",
                path = %path.display(),
                error = %error,
                "failed to read visibility-ranges metadata"
            );
            return None;
        }
    };

    const HEADER_LEN: usize = 6 + 2 + 4;
    if raw.len() < HEADER_LEN {
        tracing::warn!(
            target: "batpak::visibility",
            path = %path.display(),
            "visibility-ranges file too short; ignoring"
        );
        return None;
    }

    if &raw[..6] != VISIBILITY_RANGES_MAGIC {
        tracing::warn!(
            target: "batpak::visibility",
            path = %path.display(),
            "visibility-ranges file has wrong magic; ignoring"
        );
        return None;
    }

    let version = u16::from_le_bytes([raw[6], raw[7]]);
    if version != VISIBILITY_RANGES_VERSION {
        tracing::warn!(
            target: "batpak::visibility",
            path = %path.display(),
            version,
            "unsupported visibility-ranges version; ignoring"
        );
        return None;
    }

    let stored_crc = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let body = &raw[HEADER_LEN..];
    let actual_crc = crc32fast::hash(body);
    if stored_crc != actual_crc {
        tracing::warn!(
            target: "batpak::visibility",
            path = %path.display(),
            "visibility-ranges CRC mismatch; ignoring"
        );
        return None;
    }

    let data: VisibilityRangesData = match rmp_serde::from_slice(body) {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(
                target: "batpak::visibility",
                path = %path.display(),
                error = %error,
                "visibility-ranges deserialisation failed; ignoring"
            );
            return None;
        }
    };

    let raw_ranges: Vec<(u64, u64)> = data
        .ranges
        .into_iter()
        .map(|entry| (entry.start, entry.end))
        .collect();
    match normalize_ranges(&raw_ranges) {
        Ok(normalized) => Some(normalized),
        Err(err) => {
            tracing::warn!(
                target: "batpak::visibility",
                path = %path.display(),
                error = %err,
                "visibility-ranges file contained malformed entries; ignoring"
            );
            None
        }
    }
}

fn reject_symlink_leaf(path: &Path) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "refusing to write visibility-ranges metadata through symlink {}",
                path.display()
            ),
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}
