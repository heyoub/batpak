//! Mmap-first cold-start artifact.
//!
//! `index.fbati` is a fixed-width snapshot of the in-memory index written on
//! orderly close and after compaction. On open we validate the artifact,
//! mmap it, restore the interner snapshot, replay the entry section, then
//! replay only the durable tail after the recorded watermark.

mod format;
mod load;

pub(crate) use format::MMAP_INDEX_FILENAME;

use crate::store::cold_start::{FileLoad, ReservedKindFallbackStats, WatermarkInfo};
use crate::store::index::{
    recommended_restore_chunk_count, restore_chunk_ranges, IndexEntry, RoutingSummary, StoreIndex,
};
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use load::{invalid_load, load_mmap_index};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

pub(crate) struct LoadedMmapSnapshot {
    pub(crate) entries: Vec<IndexEntry>,
    pub(crate) interner_strings: Vec<String>,
    pub(crate) watermark: WatermarkInfo,
    pub(crate) stored_allocator: u64,
    pub(crate) routing: RoutingSummary,
    pub(crate) reopen_reserved_kind_fallbacks: ReservedKindFallbackStats,
    pub(crate) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
    pub(crate) receipt_extensions_hydrated: bool,
}

fn extension_blob_digest(bytes: &[u8]) -> [u8; 32] {
    crate::event::hash::compute_hash(bytes)
}

fn encode_receipt_extensions(
    extensions: &BTreeMap<ExtensionKey, EncodedBytes>,
) -> Result<Vec<u8>, StoreError> {
    if extensions.is_empty() {
        return Ok(Vec::new());
    }
    crate::canonical::to_bytes(extensions)
        .map_err(|error| StoreError::Serialization(Box::new(error)))
}

fn decode_receipt_extensions_from_blob(
    entry: &format::MmapIndexEntry,
    extension_blob: &[u8],
) -> Result<BTreeMap<ExtensionKey, EncodedBytes>, StoreError> {
    if entry.extension_len == 0 {
        return Ok(BTreeMap::new());
    }
    let offset = usize::try_from(entry.extension_offset)
        .map_err(|_| StoreError::ser_msg("mmap receipt-extension offset is too large"))?;
    let len = usize::try_from(entry.extension_len)
        .map_err(|_| StoreError::ser_msg("mmap receipt-extension length is too large"))?;
    let end = offset
        .checked_add(len)
        .ok_or_else(|| StoreError::ser_msg("mmap receipt-extension range overflowed"))?;
    if end > extension_blob.len() {
        return Err(StoreError::ser_msg(
            "mmap receipt-extension range exceeds blob section",
        ));
    }
    let bytes = &extension_blob[offset..end];
    if extension_blob_digest(bytes) != entry.extension_hash {
        return Err(StoreError::ser_msg(
            "mmap receipt-extension blob digest mismatch",
        ));
    }
    crate::canonical::from_bytes(bytes).map_err(|error| StoreError::Serialization(Box::new(error)))
}

/// Atomically write the mmap-first index artifact.
#[cfg(test)]
pub(crate) fn write_mmap_index(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
) -> Result<(), StoreError> {
    write_mmap_index_with_reserved_kind_fallbacks(
        index,
        data_dir,
        watermark_segment_id,
        watermark_offset,
        &ReservedKindFallbackStats::default(),
    )
}

pub(crate) fn write_mmap_index_with_reserved_kind_fallbacks(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
    reserved_kind_fallbacks: &ReservedKindFallbackStats,
) -> Result<(), StoreError> {
    let mut entries = index.all_entries();
    entries.sort_by_key(|entry| entry.global_sequence);
    let routing = RoutingSummary::from_sorted_entries(
        &entries,
        recommended_restore_chunk_count(entries.len()),
    );

    let mut interner_strings = vec![String::new()];
    interner_strings.extend(index.interner.to_snapshot());
    let interner_bytes = crate::encoding::to_bytes(&interner_strings)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;
    let summary_bytes = crate::encoding::to_bytes(&format::MmapSummaryDataV4 {
        routing,
        reserved_kind_fallbacks: reserved_kind_fallbacks.clone(),
    })
    .map_err(|e| StoreError::Serialization(Box::new(e)))?;

    let interner_count = u32::try_from(interner_strings.len())
        .map_err(|_| StoreError::ser_msg("interner snapshot too large for mmap index"))?;
    let entry_count = u64::try_from(entries.len())
        .map_err(|_| StoreError::ser_msg("entry count too large for mmap index"))?;
    let interner_bytes_len = u64::try_from(interner_bytes.len())
        .map_err(|_| StoreError::ser_msg("interner payload too large for mmap index"))?;
    let summary_bytes_len = u64::try_from(summary_bytes.len())
        .map_err(|_| StoreError::ser_msg("summary payload too large for mmap index"))?;

    let mut mmap_entries = Vec::with_capacity(entries.len());
    let mut extension_blob = Vec::new();
    for entry in &entries {
        let extension_bytes = encode_receipt_extensions(&entry.receipt_extensions)?;
        let mut mmap_entry = format::MmapIndexEntry::from_index_entry(entry);
        if !extension_bytes.is_empty() {
            mmap_entry.extension_offset = u64::try_from(extension_blob.len()).map_err(|_| {
                StoreError::ser_msg("receipt-extension blob offset too large for mmap index")
            })?;
            mmap_entry.extension_len = u64::try_from(extension_bytes.len()).map_err(|_| {
                StoreError::ser_msg("receipt-extension blob length too large for mmap index")
            })?;
            mmap_entry.extension_hash = extension_blob_digest(&extension_bytes);
            extension_blob.extend_from_slice(&extension_bytes);
        }
        mmap_entries.push(mmap_entry);
    }
    let extension_blob_len = u64::try_from(extension_blob.len())
        .map_err(|_| StoreError::ser_msg("receipt-extension blob too large for mmap index"))?;

    let mut header_tail = Vec::with_capacity(format::header_tail_len(format::MMAP_INDEX_VERSION));
    header_tail.extend_from_slice(&watermark_segment_id.to_le_bytes());
    header_tail.extend_from_slice(&watermark_offset.to_le_bytes());
    header_tail.extend_from_slice(&index.global_sequence().to_le_bytes());
    header_tail.extend_from_slice(&interner_count.to_le_bytes());
    header_tail.extend_from_slice(&entry_count.to_le_bytes());
    header_tail.extend_from_slice(&interner_bytes_len.to_le_bytes());
    header_tail.extend_from_slice(&summary_bytes_len.to_le_bytes());
    header_tail.extend_from_slice(&extension_blob_len.to_le_bytes());

    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header_tail);
    hasher.update(&interner_bytes);
    hasher.update(&summary_bytes);
    hasher.update(&extension_blob);

    let final_path = data_dir.join(MMAP_INDEX_FILENAME);
    crate::store::platform::fs::write_file_atomically(
        data_dir,
        &final_path,
        "mmap index",
        |file| {
            let mut writer = BufWriter::new(&mut *file);
            writer
                .write_all(format::MMAP_INDEX_MAGIC)
                .map_err(StoreError::Io)?;
            writer
                .write_all(&format::MMAP_INDEX_VERSION.to_le_bytes())
                .map_err(StoreError::Io)?;
            writer
                .write_all(&0u32.to_le_bytes())
                .map_err(StoreError::Io)?;
            writer.write_all(&header_tail).map_err(StoreError::Io)?;
            writer.write_all(&interner_bytes).map_err(StoreError::Io)?;
            writer.write_all(&summary_bytes).map_err(StoreError::Io)?;
            writer.write_all(&extension_blob).map_err(StoreError::Io)?;

            let mut buf = [0u8; format::MMAP_ENTRY_SIZE_V5];
            for entry in &mmap_entries {
                entry.encode_into_v5(&mut buf);
                hasher.update(&buf);
                writer.write_all(&buf).map_err(StoreError::Io)?;
            }

            writer.flush().map_err(StoreError::Io)?;
            drop(writer);

            let crc = hasher.finalize();
            file.seek(SeekFrom::Start(8)).map_err(StoreError::Io)?;
            file.write_all(&crc.to_le_bytes()).map_err(StoreError::Io)?;
            Ok(())
        },
    )?;

    tracing::debug!(
        target: "batpak::mmap_index",
        entry_count,
        interner_count,
        watermark_segment_id,
        watermark_offset,
        "mmap index written"
    );

    Ok(())
}

/// Restore the index from the mmap-first artifact. Returns the watermark and
/// allocator position if the artifact was present and valid.
#[cfg(test)]
pub(crate) fn try_restore_mmap_index(
    index: &StoreIndex,
    data_dir: &Path,
) -> Option<(WatermarkInfo, u64)> {
    let clock = crate::store::SystemClock::new();
    let loaded = try_load_mmap_snapshot(data_dir, &clock)?;
    index
        .interner
        .replace_from_full_snapshot(&loaded.interner_strings);
    index
        .restore_sorted_entries(loaded.entries, loaded.stored_allocator)
        .ok()?;
    Some((loaded.watermark, loaded.stored_allocator))
}

#[cfg(test)]
pub(crate) fn try_load_mmap_snapshot(
    data_dir: &Path,
    clock: &dyn crate::store::Clock,
) -> Option<LoadedMmapSnapshot> {
    match load_mmap_snapshot(data_dir, clock) {
        FileLoad::Loaded(snapshot) => Some(snapshot),
        FileLoad::Missing | FileLoad::Invalid { .. } | FileLoad::FutureVersion { .. } => None,
    }
}

pub(crate) fn load_mmap_snapshot(
    data_dir: &Path,
    clock: &dyn crate::store::Clock,
) -> FileLoad<LoadedMmapSnapshot> {
    let loaded = match load_mmap_index(data_dir, clock) {
        FileLoad::Loaded(loaded) => loaded,
        FileLoad::Missing => return FileLoad::Missing,
        FileLoad::Invalid { reason } => return FileLoad::Invalid { reason },
        FileLoad::FutureVersion { found, supported } => {
            return FileLoad::FutureVersion { found, supported }
        }
    };

    let entry_count = match usize::try_from(loaded.entry_count) {
        Ok(count) => count,
        Err(_) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                "mmap entry count is too large to restore"
            );
            return invalid_load("mmap entry count is too large to restore");
        }
    };
    let entries_len = match entry_count.checked_mul(loaded.entry_size) {
        Some(len) => len,
        None => return invalid_load("mmap restore entry bytes length overflowed"),
    };
    let entries_end = match loaded.entries_offset.checked_add(entries_len) {
        Some(end) => end,
        None => return invalid_load("mmap restore entries range overflowed"),
    };
    let entries_slice = match loaded.mmap.get(loaded.entries_offset..entries_end) {
        Some(slice) => slice,
        None => return invalid_load("mmap restore entries range is out of bounds"),
    };
    let extension_blob_end = loaded
        .extension_blob_offset
        .checked_add(loaded.extension_blob_len);
    let extension_blob_end = match extension_blob_end {
        Some(end) => end,
        None => return invalid_load("mmap restore extension blob range overflowed"),
    };
    let extension_blob_slice = match loaded
        .mmap
        .get(loaded.extension_blob_offset..extension_blob_end)
    {
        Some(slice) => slice,
        None => return invalid_load("mmap restore extension blob range is out of bounds"),
    };
    let chunk_ranges = restore_chunk_ranges(entry_count, &loaded.routing);

    let per_chunk = chunk_ranges
        .into_par_iter()
        .enumerate()
        .map(|(chunk_idx, (start, len))| {
            let start_byte = start
                .checked_mul(loaded.entry_size)
                .ok_or_else(|| StoreError::ser_msg("mmap restore chunk start overflowed"))?;
            let len_bytes = len
                .checked_mul(loaded.entry_size)
                .ok_or_else(|| StoreError::ser_msg("mmap restore chunk len overflowed"))?;
            let end_byte = start_byte
                .checked_add(len_bytes)
                .ok_or_else(|| StoreError::ser_msg("mmap restore chunk range overflowed"))?;
            let chunk_bytes = entries_slice
                .get(start_byte..end_byte)
                .ok_or_else(|| StoreError::ser_msg("mmap restore chunk range out of bounds"))?;
            let mut rebuilt = Vec::with_capacity(len);
            let mut reserved_kind_fallbacks = ReservedKindFallbackStats::default();
            for chunk in chunk_bytes.chunks_exact(loaded.entry_size) {
                let entry = format::MmapIndexEntry::decode_from(chunk, loaded.version)?;
                let mut rebuilt_entry = entry
                    .to_cold_start_row_counted(&mut reserved_kind_fallbacks)
                    .to_index_entry(&loaded.interner_strings)?;
                if loaded.version >= 5 {
                    rebuilt_entry.receipt_extensions =
                        decode_receipt_extensions_from_blob(&entry, extension_blob_slice)?;
                }
                rebuilt.push(rebuilt_entry);
            }
            Ok::<_, StoreError>((chunk_idx, rebuilt, reserved_kind_fallbacks))
        })
        .collect::<Result<Vec<_>, _>>();
    let mut per_chunk = match per_chunk {
        Ok(per_chunk) => per_chunk,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                error = %error,
                "mmap snapshot entry rebuild failed"
            );
            return invalid_load(format!("mmap snapshot entry rebuild failed: {error}"));
        }
    };
    per_chunk.sort_by_key(|(chunk_idx, _, _)| *chunk_idx);
    let mut rebuilt_entries = Vec::with_capacity(entry_count);
    let mut reserved_kind_fallbacks = ReservedKindFallbackStats::default();
    for (_, chunk_entries, chunk_counts) in per_chunk {
        rebuilt_entries.extend(chunk_entries);
        reserved_kind_fallbacks = reserved_kind_fallbacks.add(&chunk_counts);
    }
    let routing = if loaded.routing.chunks.is_empty() {
        RoutingSummary::from_sorted_entries(
            &rebuilt_entries,
            recommended_restore_chunk_count(rebuilt_entries.len()),
        )
    } else {
        loaded.routing.clone()
    };

    FileLoad::Loaded(LoadedMmapSnapshot {
        entries: rebuilt_entries,
        interner_strings: loaded.interner_strings,
        watermark: loaded.watermark,
        stored_allocator: loaded.stored_allocator,
        routing,
        reopen_reserved_kind_fallbacks: reserved_kind_fallbacks,
        cumulative_reserved_kind_fallbacks: loaded.cumulative_reserved_kind_fallbacks,
        receipt_extensions_hydrated: loaded.version >= 5,
    })
}

#[cfg(test)]
mod tests;
