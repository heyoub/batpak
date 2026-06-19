use super::format;
use crate::store::cold_start::{
    validate_watermark_segment, FileLoad, ReservedKindFallbackStats, WatermarkInfo,
    WatermarkValidationError,
};
use crate::store::index::RoutingSummary;
use memmap2::Mmap;
use std::io::Read;
use std::path::Path;

pub(super) struct LoadedMmapIndex {
    pub(super) mmap: Mmap,
    pub(super) interner_strings: Vec<String>,
    pub(super) routing: RoutingSummary,
    pub(super) entries_offset: usize,
    pub(super) extension_blob_offset: usize,
    pub(super) extension_blob_len: usize,
    pub(super) entry_count: u64,
    pub(super) entry_size: usize,
    pub(super) version: u16,
    pub(super) watermark: WatermarkInfo,
    pub(super) stored_allocator: u64,
    pub(super) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

pub(super) fn invalid_load<T>(reason: impl Into<String>) -> FileLoad<T> {
    FileLoad::Invalid {
        reason: reason.into(),
    }
}

fn read_mmap_u32(bytes: Option<&[u8]>, field: &str) -> Result<u32, String> {
    bytes
        .and_then(format::read_le_u32)
        .ok_or_else(|| format!("mmap index {field} is unreadable"))
}

fn read_mmap_u64(bytes: Option<&[u8]>, field: &str) -> Result<u64, String> {
    bytes
        .and_then(format::read_le_u64)
        .ok_or_else(|| format!("mmap index {field} is unreadable"))
}

fn read_mmap_usize(bytes: Option<&[u8]>, field: &str) -> Result<usize, String> {
    let value = read_mmap_u64(bytes, field)?;
    usize::try_from(value).map_err(|_| format!("mmap index {field} is too large"))
}

pub(super) fn load_mmap_index(
    data_dir: &Path,
    clock: &dyn crate::store::Clock,
) -> FileLoad<LoadedMmapIndex> {
    let path = data_dir.join(super::MMAP_INDEX_FILENAME);
    let mut file = match crate::store::platform::fs::open_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return FileLoad::Missing,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                error = %error,
                "failed to open mmap index"
            );
            return invalid_load(format!("failed to open mmap index: {error}"));
        }
    };

    let mut prefix = [0u8; format::PREFIX_LEN];
    if let Err(error) = file.read_exact(&mut prefix) {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            error = %error,
            "mmap index header is unreadable"
        );
        return invalid_load(format!("mmap index header is unreadable: {error}"));
    }

    if &prefix[..6] != format::MMAP_INDEX_MAGIC {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            "mmap index has wrong magic"
        );
        return invalid_load("mmap index has wrong magic");
    }

    let version = match format::read_le_u16(&prefix[6..8]) {
        Some(version) => version,
        None => return invalid_load("mmap index version is unreadable"),
    };
    // A version STRICTLY NEWER than this binary supports is a canonical typed
    // refusal — NOT a silent rebuild-from-scan. A future writer may have written
    // segments or summaries this reader cannot interpret, so degrading to a scan
    // would risk a silent downgrade instead of a legally reachable state. Older
    // or otherwise-unknown (but not future) versions stay `Invalid`, which the
    // cold-start flow safely rebuilds from the durable segments.
    // justifies: INV-MMAP-SEALED-READS
    if version > format::MMAP_INDEX_VERSION {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            version,
            supported = format::MMAP_INDEX_VERSION,
            "mmap index declares a future format version; refusing canonically"
        );
        return FileLoad::FutureVersion {
            found: version,
            supported: format::MMAP_INDEX_VERSION,
        };
    }
    if !matches!(version, 1..=4) && version != format::MMAP_INDEX_VERSION {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            version,
            expected = format::MMAP_INDEX_VERSION,
            "unsupported mmap index version"
        );
        return invalid_load(format!("unsupported mmap index version {version}"));
    }

    let header_tail_len = format::header_tail_len(version);
    let header_len = format::header_len(version);
    let mut header_tail = vec![0u8; header_tail_len];
    if let Err(error) = file.read_exact(&mut header_tail) {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            error = %error,
            "mmap index header tail is unreadable"
        );
        return invalid_load(format!("mmap index header tail is unreadable: {error}"));
    }

    let expected_crc = match read_mmap_u32(prefix.get(8..12), "crc") {
        Ok(expected_crc) => expected_crc,
        Err(reason) => return invalid_load(reason),
    };
    let mut cursor = 0usize;
    let watermark_segment_id =
        match read_mmap_u64(header_tail.get(cursor..cursor + 8), "watermark segment id") {
            Ok(value) => value,
            Err(reason) => return invalid_load(reason),
        };
    cursor += 8;
    let watermark_offset =
        match read_mmap_u64(header_tail.get(cursor..cursor + 8), "watermark offset") {
            Ok(value) => value,
            Err(reason) => return invalid_load(reason),
        };
    cursor += 8;
    let stored_allocator =
        match read_mmap_u64(header_tail.get(cursor..cursor + 8), "stored allocator") {
            Ok(value) => value,
            Err(reason) => return invalid_load(reason),
        };
    cursor += 8;
    let interner_count = match read_mmap_u32(header_tail.get(cursor..cursor + 4), "interner count")
    {
        Ok(value) => value,
        Err(reason) => return invalid_load(reason),
    };
    cursor += 4;
    let entry_count = match read_mmap_u64(header_tail.get(cursor..cursor + 8), "entry count") {
        Ok(value) => value,
        Err(reason) => return invalid_load(reason),
    };
    cursor += 8;
    let interner_bytes_len = match read_mmap_u64(
        header_tail.get(cursor..cursor + 8),
        "interner section length",
    ) {
        Ok(value) => value,
        Err(reason) => return invalid_load(reason),
    };
    cursor += 8;
    let summary_bytes_len = if version == 1 {
        0usize
    } else {
        match read_mmap_usize(
            header_tail.get(cursor..cursor + 8),
            "summary section length",
        ) {
            Ok(value) => value,
            Err(reason) => return invalid_load(reason),
        }
    };
    if version != 1 {
        cursor += 8;
    }
    let extension_blob_len = if version >= 5 {
        match read_mmap_usize(header_tail.get(cursor..cursor + 8), "extension blob length") {
            Ok(value) => value,
            Err(reason) => return invalid_load(reason),
        }
    } else {
        0
    };

    let metadata = match file.metadata() {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                error = %error,
                "failed to stat mmap index"
            );
            return invalid_load(format!("failed to stat mmap index: {error}"));
        }
    };

    let file_len = match usize::try_from(metadata.len()) {
        Ok(len) => len,
        Err(_) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index file is too large for this platform"
            );
            return invalid_load("mmap index file is too large for this platform");
        }
    };
    let interner_bytes_len = match usize::try_from(interner_bytes_len) {
        Ok(len) => len,
        Err(_) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index interner section is too large"
            );
            return invalid_load("mmap index interner section is too large");
        }
    };
    let entry_count_usize = match usize::try_from(entry_count) {
        Ok(count) => count,
        Err(_) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index entry count is too large"
            );
            return invalid_load("mmap index entry count is too large");
        }
    };
    let entry_size = format::entry_size(version);
    let entry_bytes_len = match entry_count_usize.checked_mul(entry_size) {
        Some(len) => len,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index entry section length overflowed"
            );
            return invalid_load("mmap index entry section length overflowed");
        }
    };
    let summary_offset = match header_len.checked_add(interner_bytes_len) {
        Some(offset) => offset,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index header offset overflowed"
            );
            return invalid_load("mmap index header offset overflowed");
        }
    };
    let extension_blob_offset = match summary_offset.checked_add(summary_bytes_len) {
        Some(offset) => offset,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index summary offset overflowed"
            );
            return invalid_load("mmap index summary offset overflowed");
        }
    };
    let entries_offset = match extension_blob_offset.checked_add(extension_blob_len) {
        Some(offset) => offset,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index extension blob offset overflowed"
            );
            return invalid_load("mmap index extension blob offset overflowed");
        }
    };
    let expected_len = match entries_offset.checked_add(entry_bytes_len) {
        Some(len) => len,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index total length overflowed"
            );
            return invalid_load("mmap index total length overflowed");
        }
    };
    if file_len != expected_len {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            file_len,
            expected_len,
            "mmap index size does not match header"
        );
        return invalid_load(format!(
            "mmap index size does not match header: file {file_len}, expected {expected_len}"
        ));
    }

    match validate_watermark_segment(data_dir, watermark_segment_id, watermark_offset) {
        Ok(()) => {}
        Err(WatermarkValidationError::OffsetPastTail {
            path: watermark_segment_path,
            file_len,
            watermark_offset,
        }) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                watermark_segment = %watermark_segment_path.display(),
                file_len,
                watermark_offset,
                "mmap index watermark points past the segment tail"
            );
            return invalid_load(format!(
                "mmap index watermark {watermark_offset} points past segment tail {}",
                watermark_segment_path.display()
            ));
        }
        Err(WatermarkValidationError::MissingSegment {
            path: watermark_segment_path,
        }) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                watermark_segment = %watermark_segment_path.display(),
                "mmap index watermark segment is missing"
            );
            return invalid_load(format!(
                "mmap index watermark segment is missing: {}",
                watermark_segment_path.display()
            ));
        }
    }

    // SAFETY: Mmap::map requires a valid open file descriptor. The `file`
    // handle remains open for the duration of the mapping. The mmap index
    // file is read-only and not written to while open — external modification
    // would be a usage error, not a correctness concern for safe Rust callers.
    let evidence = crate::store::platform::evidence::collect_for_store_path(data_dir, clock);
    let admission =
        match crate::store::platform::mmap::admit_mmap_index(evidence.store_path.mmap_index) {
            Ok(admission) => admission,
            Err(error) => {
                tracing::warn!(
                    target: "batpak::mmap_index",
                    path = %path.display(),
                    error = %error,
                    "mmap index admission failed"
                );
                return invalid_load(format!("mmap index admission failed: {error}"));
            }
        };
    let mmap = match unsafe { crate::store::platform::mmap::map_mmap_index_file(&file, admission) }
    {
        Ok(mmap) => mmap,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                error = %error,
                "failed to mmap index file"
            );
            return invalid_load(format!("failed to mmap index file: {error}"));
        }
    };

    let actual_crc = crc32fast::hash(&mmap[12..]);
    if actual_crc != expected_crc {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            expected_crc,
            actual_crc,
            "mmap index CRC mismatch"
        );
        return invalid_load(format!(
            "mmap index CRC mismatch: expected {expected_crc}, actual {actual_crc}"
        ));
    }

    let interner_slice = &mmap[header_len..summary_offset];
    let interner_strings: Vec<String> = match crate::encoding::from_bytes(interner_slice) {
        Ok(strings) => strings,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                error = %error,
                "failed to decode mmap index interner snapshot"
            );
            return invalid_load(format!(
                "failed to decode mmap index interner snapshot: {error}"
            ));
        }
    };

    if interner_strings.len() != interner_count as usize {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            expected = interner_count,
            actual = interner_strings.len(),
            "mmap index interner count does not match header"
        );
        return invalid_load(format!(
            "mmap index interner count does not match header: expected {interner_count}, actual {}",
            interner_strings.len()
        ));
    }

    let (routing, cumulative_reserved_kind_fallbacks) = if version == 1 {
        (
            RoutingSummary::default(),
            ReservedKindFallbackStats::default(),
        )
    } else {
        let summary_slice = &mmap[summary_offset..extension_blob_offset];
        if version >= 4 {
            match crate::encoding::from_bytes::<format::MmapSummaryDataV4>(summary_slice) {
                Ok(summary) => (summary.routing, summary.reserved_kind_fallbacks),
                Err(error) => {
                    tracing::warn!(
                        target: "batpak::mmap_index",
                        path = %path.display(),
                        error = %error,
                        "failed to decode mmap index summary section"
                    );
                    return invalid_load(format!(
                        "failed to decode mmap index summary section: {error}"
                    ));
                }
            }
        } else {
            match crate::encoding::from_bytes::<format::MmapSummaryDataV2>(summary_slice) {
                Ok(summary) => (summary.routing, ReservedKindFallbackStats::default()),
                Err(error) => {
                    tracing::warn!(
                        target: "batpak::mmap_index",
                        path = %path.display(),
                        error = %error,
                        "failed to decode mmap index summary section"
                    );
                    return invalid_load(format!(
                        "failed to decode mmap index summary section: {error}"
                    ));
                }
            }
        }
    };

    FileLoad::Loaded(LoadedMmapIndex {
        mmap,
        interner_strings,
        routing,
        entries_offset,
        extension_blob_offset,
        extension_blob_len,
        entry_count,
        entry_size,
        version,
        watermark: WatermarkInfo {
            watermark_segment_id,
            watermark_offset,
        },
        stored_allocator,
        cumulative_reserved_kind_fallbacks,
    })
}
