use crate::store::{platform, HiddenRangesCorruption, StoreError};
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
    crate::store::platform::fs::reject_symlink_leaf(&final_path, "visibility-ranges metadata")?;

    let normalized = normalize_ranges(ranges)?;
    if normalized.is_empty() {
        match platform::fs::remove_file(&final_path) {
            Ok(()) => crate::store::platform::sync::sync_parent_dir(&final_path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::Io(error)),
        }
        return Ok(());
    }

    let body = crate::encoding::to_bytes(&VisibilityRangesData {
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
    crate::store::platform::sync::sync_file_all_io(tmp.as_file()).map_err(StoreError::Io)?;
    let admission = crate::store::platform::sync::admit_current_parent_dir_sync()?;
    crate::store::platform::sync::persist_temp_with_parent_sync(tmp, &final_path, admission)
        .map_err(StoreError::Io)?;
    Ok(())
}

/// Load the hidden-ranges metadata, failing closed on corruption.
///
/// The outcome ladder distinguishes "file absent" from "file present but
/// invalid" — a first-open store has no file and returns `Ok(None)`, but a
/// store whose metadata was corrupted mid-write must not silently forget
/// its cancelled ranges (doing so resurrects previously-hidden events):
///
/// - No file at the expected path → `Ok(None)` (first open).
/// - Valid file → `Ok(Some(ranges))`.
/// - File present but unreadable / wrong magic / unsupported version /
///   CRC mismatch / malformed →
///   `Err(StoreError::HiddenRangesCorrupt { .. })`.
///
/// The caller must remediate (repair or manually clear the file) before
/// re-opening.
pub(crate) fn load_cancelled_ranges(
    data_dir: &Path,
) -> Result<Option<Vec<(u64, u64)>>, StoreError> {
    let path = data_dir.join(VISIBILITY_RANGES_FILENAME);
    let raw = match platform::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(corrupt_ranges(
                &path,
                HiddenRangesCorruption::ReadFailed(error),
            ));
        }
    };

    const HEADER_LEN: usize = 6 + 2 + 4;
    if raw.len() < HEADER_LEN {
        return Err(corrupt_ranges(
            &path,
            HiddenRangesCorruption::TooShort {
                actual: raw.len(),
                required: HEADER_LEN,
            },
        ));
    }

    if &raw[..6] != VISIBILITY_RANGES_MAGIC {
        return Err(corrupt_ranges(&path, HiddenRangesCorruption::BadMagic));
    }

    let version = u16::from_le_bytes([raw[6], raw[7]]);
    if version != VISIBILITY_RANGES_VERSION {
        return Err(corrupt_ranges(
            &path,
            HiddenRangesCorruption::UnsupportedVersion {
                observed: version,
                expected: VISIBILITY_RANGES_VERSION,
            },
        ));
    }

    let stored_crc = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let body = &raw[HEADER_LEN..];
    let actual_crc = crc32fast::hash(body);
    if stored_crc != actual_crc {
        return Err(corrupt_ranges(
            &path,
            HiddenRangesCorruption::CrcMismatch {
                stored: stored_crc,
                computed: actual_crc,
            },
        ));
    }

    let data: VisibilityRangesData = match crate::encoding::from_bytes(body) {
        Ok(data) => data,
        Err(error) => {
            return Err(corrupt_ranges(
                &path,
                HiddenRangesCorruption::DecodeFailed(error),
            ));
        }
    };

    let raw_ranges: Vec<(u64, u64)> = data
        .ranges
        .into_iter()
        .map(|entry| (entry.start, entry.end))
        .collect();
    match normalize_ranges(&raw_ranges) {
        Ok(normalized) => Ok(Some(normalized)),
        Err(err) => Err(corrupt_ranges(
            &path,
            HiddenRangesCorruption::MalformedEntries {
                source: Box::new(err),
            },
        )),
    }
}

fn corrupt_ranges(path: &Path, kind: HiddenRangesCorruption) -> StoreError {
    tracing::warn!(
        target: "batpak::visibility",
        path = %path.display(),
        reason = %kind,
        "visibility-ranges metadata unreadable; failing closed"
    );
    StoreError::HiddenRangesCorrupt {
        path: path.to_path_buf(),
        kind,
    }
}
