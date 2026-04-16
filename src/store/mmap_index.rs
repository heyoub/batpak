//! Mmap-first cold-start artifact.
//!
//! `index.fbati` is a fixed-width snapshot of the in-memory index written on
//! orderly close and after compaction. On open we validate the artifact,
//! mmap it, restore the interner snapshot, replay the entry section, then
//! replay only the durable tail after the recorded watermark.

use crate::coordinate::Coordinate;
use crate::event::HashChain;
use crate::store::checkpoint::WatermarkInfo;
use crate::store::index::{
    recommended_restore_chunk_count, DiskPos, IndexEntry, RoutingSummary, StoreIndex,
};
use crate::store::sidx::{kind_to_raw, raw_to_kind};
use crate::store::StoreError;
use memmap2::Mmap;
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::NamedTempFile;

pub(crate) const MMAP_INDEX_MAGIC: &[u8; 6] = b"FBATIX";
pub(crate) const MMAP_INDEX_VERSION: u16 = 3;
pub(crate) const MMAP_INDEX_FILENAME: &str = "index.fbati";

const PREFIX_LEN: usize = 6 + 2 + 4;
const HEADER_TAIL_LEN_V1: usize = 8 + 8 + 8 + 4 + 8 + 8;
const HEADER_TAIL_LEN_V2: usize = HEADER_TAIL_LEN_V1 + 8;
const HEADER_LEN_V1: usize = PREFIX_LEN + HEADER_TAIL_LEN_V1;
const HEADER_LEN_V2: usize = PREFIX_LEN + HEADER_TAIL_LEN_V2;
const MMAP_ENTRY_SIZE_V2: usize = 162;
const MMAP_ENTRY_SIZE_V3: usize = 170;

struct LoadedMmapIndex {
    mmap: Mmap,
    interner_strings: Vec<String>,
    routing: RoutingSummary,
    entries_offset: usize,
    entry_count: u64,
    entry_size: usize,
    watermark: WatermarkInfo,
    stored_allocator: u64,
}

pub(crate) struct LoadedMmapSnapshot {
    pub(crate) entries: Vec<IndexEntry>,
    pub(crate) interner_strings: Vec<String>,
    pub(crate) watermark: WatermarkInfo,
    pub(crate) stored_allocator: u64,
    pub(crate) routing: RoutingSummary,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MmapSummaryDataV2 {
    routing: RoutingSummary,
}

fn reject_symlink_leaf(path: &Path) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "refusing to write mmap index through symlink {}",
                path.display()
            ),
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

struct MmapIndexEntry {
    event_id: u128,
    entity_idx: u32,
    scope_idx: u32,
    kind: u16,
    wall_ms: u64,
    clock: u32,
    dag_lane: u32,
    dag_depth: u32,
    prev_hash: [u8; 32],
    event_hash: [u8; 32],
    segment_id: u64,
    frame_offset: u64,
    frame_length: u32,
    global_sequence: u64,
    correlation_id: u128,
    causation_id: u128,
}

impl MmapIndexEntry {
    #[cfg(test)]
    fn encode_into_v2(&self, buf: &mut [u8]) {
        debug_assert_eq!(buf.len(), MMAP_ENTRY_SIZE_V2);
        let mut pos = 0usize;

        macro_rules! put_le {
            ($val:expr, $n:expr) => {{
                buf[pos..pos + $n].copy_from_slice(&($val).to_le_bytes());
                pos += $n;
            }};
        }
        macro_rules! put_bytes {
            ($arr:expr) => {{
                let slice: &[u8] = &$arr;
                buf[pos..pos + slice.len()].copy_from_slice(slice);
                pos += slice.len();
            }};
        }

        put_le!(self.event_id, 16);
        put_le!(self.entity_idx, 4);
        put_le!(self.scope_idx, 4);
        put_le!(self.kind, 2);
        put_le!(self.wall_ms, 8);
        put_le!(self.clock, 4);
        put_bytes!(self.prev_hash);
        put_bytes!(self.event_hash);
        put_le!(self.segment_id, 8);
        put_le!(self.frame_offset, 8);
        put_le!(self.frame_length, 4);
        put_le!(self.global_sequence, 8);
        put_le!(self.correlation_id, 16);
        put_le!(self.causation_id, 16);
        debug_assert_eq!(pos, MMAP_ENTRY_SIZE_V2);
    }

    fn encode_into(&self, buf: &mut [u8]) {
        debug_assert_eq!(buf.len(), MMAP_ENTRY_SIZE_V3);
        let mut pos = 0usize;

        macro_rules! put_le {
            ($val:expr, $n:expr) => {{
                buf[pos..pos + $n].copy_from_slice(&($val).to_le_bytes());
                pos += $n;
            }};
        }
        macro_rules! put_bytes {
            ($arr:expr) => {{
                let slice: &[u8] = &$arr;
                buf[pos..pos + slice.len()].copy_from_slice(slice);
                pos += slice.len();
            }};
        }

        put_le!(self.event_id, 16);
        put_le!(self.entity_idx, 4);
        put_le!(self.scope_idx, 4);
        put_le!(self.kind, 2);
        put_le!(self.wall_ms, 8);
        put_le!(self.clock, 4);
        put_le!(self.dag_lane, 4);
        put_le!(self.dag_depth, 4);
        put_bytes!(self.prev_hash);
        put_bytes!(self.event_hash);
        put_le!(self.segment_id, 8);
        put_le!(self.frame_offset, 8);
        put_le!(self.frame_length, 4);
        put_le!(self.global_sequence, 8);
        put_le!(self.correlation_id, 16);
        put_le!(self.causation_id, 16);
        debug_assert_eq!(pos, MMAP_ENTRY_SIZE_V3);
    }

    fn decode_from(buf: &[u8], version: u16) -> Result<Self, StoreError> {
        let expected_size = if version >= 3 {
            MMAP_ENTRY_SIZE_V3
        } else {
            MMAP_ENTRY_SIZE_V2
        };
        if buf.len() != expected_size {
            return Err(StoreError::ser_msg("mmap entry buffer has wrong size"));
        }
        let mut pos = 0usize;
        macro_rules! get_le {
            ($t:ty, $n:expr) => {{
                let start = pos;
                let end = pos + $n;
                let arr: [u8; $n] = buf[start..end]
                    .try_into()
                    .expect("slice length matches const");
                pos += $n;
                <$t>::from_le_bytes(arr)
            }};
        }
        macro_rules! get_hash {
            () => {{
                let mut h = [0u8; 32];
                h.copy_from_slice(&buf[pos..pos + 32]);
                pos += 32;
                h
            }};
        }

        let event_id = get_le!(u128, 16);
        let entity_idx = get_le!(u32, 4);
        let scope_idx = get_le!(u32, 4);
        let kind = get_le!(u16, 2);
        let wall_ms = get_le!(u64, 8);
        let clock = get_le!(u32, 4);
        let (dag_lane, dag_depth) = if version >= 3 {
            (get_le!(u32, 4), get_le!(u32, 4))
        } else {
            (0, 0)
        };
        let decoded = Self {
            event_id,
            entity_idx,
            scope_idx,
            kind,
            wall_ms,
            clock,
            dag_lane,
            dag_depth,
            prev_hash: get_hash!(),
            event_hash: get_hash!(),
            segment_id: get_le!(u64, 8),
            frame_offset: get_le!(u64, 8),
            frame_length: get_le!(u32, 4),
            global_sequence: get_le!(u64, 8),
            correlation_id: get_le!(u128, 16),
            causation_id: get_le!(u128, 16),
        };
        debug_assert_eq!(pos, expected_size);
        Ok(decoded)
    }
}

fn entry_to_mmap(entry: &IndexEntry) -> MmapIndexEntry {
    MmapIndexEntry {
        event_id: entry.event_id,
        entity_idx: entry.entity_id.as_u32(),
        scope_idx: entry.scope_id.as_u32(),
        kind: kind_to_raw(entry.kind),
        wall_ms: entry.wall_ms,
        clock: entry.clock,
        dag_lane: entry.dag_lane,
        dag_depth: entry.dag_depth,
        prev_hash: entry.hash_chain.prev_hash,
        event_hash: entry.hash_chain.event_hash,
        segment_id: entry.disk_pos.segment_id,
        frame_offset: entry.disk_pos.offset,
        frame_length: entry.disk_pos.length,
        global_sequence: entry.global_sequence,
        correlation_id: entry.correlation_id,
        causation_id: entry.causation_id.unwrap_or(0),
    }
}

/// Atomically write the mmap-first index artifact.
pub(crate) fn write_mmap_index(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
) -> Result<(), StoreError> {
    let mut entries = index.all_entries();
    entries.sort_by_key(|entry| entry.global_sequence);
    let routing = RoutingSummary::from_sorted_entries(
        &entries,
        recommended_restore_chunk_count(entries.len()),
    );

    let mut interner_strings = vec![String::new()];
    interner_strings.extend(index.interner.to_snapshot());
    let interner_bytes = rmp_serde::to_vec_named(&interner_strings)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;
    let summary_bytes = rmp_serde::to_vec_named(&MmapSummaryDataV2 { routing })
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;

    let interner_count = u32::try_from(interner_strings.len())
        .map_err(|_| StoreError::ser_msg("interner snapshot too large for mmap index"))?;
    let entry_count = u64::try_from(entries.len())
        .map_err(|_| StoreError::ser_msg("entry count too large for mmap index"))?;
    let interner_bytes_len = u64::try_from(interner_bytes.len())
        .map_err(|_| StoreError::ser_msg("interner payload too large for mmap index"))?;
    let summary_bytes_len = u64::try_from(summary_bytes.len())
        .map_err(|_| StoreError::ser_msg("summary payload too large for mmap index"))?;

    let mut header_tail = Vec::with_capacity(HEADER_TAIL_LEN_V2);
    header_tail.extend_from_slice(&watermark_segment_id.to_le_bytes());
    header_tail.extend_from_slice(&watermark_offset.to_le_bytes());
    header_tail.extend_from_slice(&index.global_sequence().to_le_bytes());
    header_tail.extend_from_slice(&interner_count.to_le_bytes());
    header_tail.extend_from_slice(&entry_count.to_le_bytes());
    header_tail.extend_from_slice(&interner_bytes_len.to_le_bytes());
    header_tail.extend_from_slice(&summary_bytes_len.to_le_bytes());

    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header_tail);
    hasher.update(&interner_bytes);
    hasher.update(&summary_bytes);

    let final_path = data_dir.join(MMAP_INDEX_FILENAME);
    reject_symlink_leaf(&final_path)?;

    let tmp = NamedTempFile::new_in(data_dir)?;
    let mut file = tmp.reopen().map_err(StoreError::Io)?;
    {
        let mut writer = BufWriter::new(&mut file);
        writer.write_all(MMAP_INDEX_MAGIC).map_err(StoreError::Io)?;
        writer
            .write_all(&MMAP_INDEX_VERSION.to_le_bytes())
            .map_err(StoreError::Io)?;
        writer
            .write_all(&0u32.to_le_bytes())
            .map_err(StoreError::Io)?;
        writer.write_all(&header_tail).map_err(StoreError::Io)?;
        writer.write_all(&interner_bytes).map_err(StoreError::Io)?;
        writer.write_all(&summary_bytes).map_err(StoreError::Io)?;

        let mut buf = [0u8; MMAP_ENTRY_SIZE_V3];
        for entry in &entries {
            entry_to_mmap(entry).encode_into(&mut buf);
            hasher.update(&buf);
            writer.write_all(&buf).map_err(StoreError::Io)?;
        }

        writer.flush().map_err(StoreError::Io)?;
    }

    let crc = hasher.finalize();
    file.seek(SeekFrom::Start(8)).map_err(StoreError::Io)?;
    file.write_all(&crc.to_le_bytes()).map_err(StoreError::Io)?;
    file.sync_all().map_err(StoreError::Io)?;
    drop(file);

    tmp.persist(&final_path)
        .map_err(|e| StoreError::Io(e.error))?;

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

fn try_load_mmap_index(data_dir: &Path) -> Option<LoadedMmapIndex> {
    let path = data_dir.join(MMAP_INDEX_FILENAME);
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                error = %error,
                "failed to open mmap index"
            );
            return None;
        }
    };

    let mut prefix = [0u8; PREFIX_LEN];
    if let Err(error) = file.read_exact(&mut prefix) {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            error = %error,
            "mmap index header is unreadable"
        );
        return None;
    }

    if &prefix[..6] != MMAP_INDEX_MAGIC {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            "mmap index has wrong magic"
        );
        return None;
    }

    let version = u16::from_le_bytes(prefix[6..8].try_into().expect("prefix slice length"));
    if version != 1 && version != 2 && version != MMAP_INDEX_VERSION {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            version,
            expected = MMAP_INDEX_VERSION,
            "unsupported mmap index version"
        );
        return None;
    }

    let header_tail_len = if version == 1 {
        HEADER_TAIL_LEN_V1
    } else {
        HEADER_TAIL_LEN_V2
    };
    let header_len = if version == 1 {
        HEADER_LEN_V1
    } else {
        HEADER_LEN_V2
    };
    let mut header_tail = vec![0u8; header_tail_len];
    if let Err(error) = file.read_exact(&mut header_tail) {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            error = %error,
            "mmap index header tail is unreadable"
        );
        return None;
    }

    let expected_crc = u32::from_le_bytes(prefix[8..12].try_into().expect("prefix slice length"));
    let mut cursor = 0usize;
    let watermark_segment_id = u64::from_le_bytes(
        header_tail[cursor..cursor + 8]
            .try_into()
            .expect("tail slice length"),
    );
    cursor += 8;
    let watermark_offset = u64::from_le_bytes(
        header_tail[cursor..cursor + 8]
            .try_into()
            .expect("tail slice length"),
    );
    cursor += 8;
    let stored_allocator = u64::from_le_bytes(
        header_tail[cursor..cursor + 8]
            .try_into()
            .expect("tail slice length"),
    );
    cursor += 8;
    let interner_count = u32::from_le_bytes(
        header_tail[cursor..cursor + 4]
            .try_into()
            .expect("tail slice length"),
    );
    cursor += 4;
    let entry_count = u64::from_le_bytes(
        header_tail[cursor..cursor + 8]
            .try_into()
            .expect("tail slice length"),
    );
    cursor += 8;
    let interner_bytes_len = u64::from_le_bytes(
        header_tail[cursor..cursor + 8]
            .try_into()
            .expect("tail slice length"),
    );
    cursor += 8;
    let summary_bytes_len = if version == 1 {
        0usize
    } else {
        usize::try_from(u64::from_le_bytes(
            header_tail[cursor..cursor + 8]
                .try_into()
                .expect("tail slice length"),
        ))
        .ok()?
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
            return None;
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
            return None;
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
            return None;
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
            return None;
        }
    };
    let entry_size = if version >= 3 {
        MMAP_ENTRY_SIZE_V3
    } else {
        MMAP_ENTRY_SIZE_V2
    };
    let entry_bytes_len = match entry_count_usize.checked_mul(entry_size) {
        Some(len) => len,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index entry section length overflowed"
            );
            return None;
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
            return None;
        }
    };
    let entries_offset = match summary_offset.checked_add(summary_bytes_len) {
        Some(offset) => offset,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index summary offset overflowed"
            );
            return None;
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
            return None;
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
        return None;
    }

    let watermark_segment_path = data_dir.join(crate::store::segment::segment_filename(
        watermark_segment_id,
    ));
    match std::fs::metadata(&watermark_segment_path) {
        Ok(meta) if meta.len() >= watermark_offset => {}
        Ok(meta) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                watermark_segment = %watermark_segment_path.display(),
                file_len = meta.len(),
                watermark_offset,
                "mmap index watermark points past the segment tail"
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                watermark_segment = %watermark_segment_path.display(),
                error = %error,
                "mmap index watermark segment is missing"
            );
            return None;
        }
    }

    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(mmap) => mmap,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                error = %error,
                "failed to mmap index file"
            );
            return None;
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
        return None;
    }

    let interner_slice = &mmap[header_len..summary_offset];
    let interner_strings: Vec<String> = match rmp_serde::from_slice(interner_slice) {
        Ok(strings) => strings,
        Err(error) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                error = %error,
                "failed to decode mmap index interner snapshot"
            );
            return None;
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
        return None;
    }

    let routing = if version == 1 {
        RoutingSummary::default()
    } else {
        let summary_slice = &mmap[summary_offset..entries_offset];
        match rmp_serde::from_slice::<MmapSummaryDataV2>(summary_slice) {
            Ok(summary) => summary.routing,
            Err(error) => {
                tracing::warn!(
                    target: "batpak::mmap_index",
                    path = %path.display(),
                    error = %error,
                    "failed to decode mmap index summary section"
                );
                return None;
            }
        }
    };

    Some(LoadedMmapIndex {
        mmap,
        interner_strings,
        routing,
        entries_offset,
        entry_count,
        entry_size,
        watermark: WatermarkInfo {
            watermark_segment_id,
            watermark_offset,
        },
        stored_allocator,
    })
}

/// Restore the index from the mmap-first artifact. Returns the watermark and
/// allocator position if the artifact was present and valid.
#[cfg(test)]
pub(crate) fn try_restore_mmap_index(
    index: &StoreIndex,
    data_dir: &Path,
) -> Option<(WatermarkInfo, u64)> {
    let loaded = try_load_mmap_snapshot(data_dir)?;
    index.clear();
    index
        .interner
        .replace_from_full_snapshot(&loaded.interner_strings);
    index.restore_sorted_entries(loaded.entries, loaded.stored_allocator);
    Some((loaded.watermark, loaded.stored_allocator))
}

pub(crate) fn try_load_mmap_snapshot(data_dir: &Path) -> Option<LoadedMmapSnapshot> {
    let loaded = try_load_mmap_index(data_dir)?;

    let entry_count = match usize::try_from(loaded.entry_count) {
        Ok(count) => count,
        Err(_) => {
            tracing::warn!(
                target: "batpak::mmap_index",
                "mmap entry count is too large to restore"
            );
            return None;
        }
    };
    let entries_end = loaded.entries_offset + (entry_count * loaded.entry_size);
    let entries_slice = &loaded.mmap[loaded.entries_offset..entries_end];
    let chunk_ranges = if loaded.routing.chunks.is_empty() {
        let chunk_count = recommended_restore_chunk_count(entry_count);
        let base = entry_count / chunk_count;
        let remainder = entry_count % chunk_count;
        let mut start = 0usize;
        let mut ranges = Vec::new();
        for chunk_index in 0..chunk_count {
            let len = base + usize::from(chunk_index < remainder);
            if len == 0 {
                continue;
            }
            ranges.push((start, len));
            start += len;
        }
        ranges
    } else {
        loaded
            .routing
            .chunks
            .iter()
            .map(|chunk| {
                let start = usize::try_from(chunk.start).ok()?;
                let len = usize::try_from(chunk.len).ok()?;
                Some((start, len))
            })
            .collect::<Option<Vec<_>>>()?
    };

    let mut per_chunk = chunk_ranges
        .into_par_iter()
        .enumerate()
        .map(|(chunk_idx, (start, len))| {
            let start_byte = start * loaded.entry_size;
            let end_byte = start_byte + (len * loaded.entry_size);
            let mut rebuilt = Vec::with_capacity(len);
            let version = if loaded.entry_size == MMAP_ENTRY_SIZE_V3 {
                3
            } else {
                2
            };
            for chunk in entries_slice[start_byte..end_byte].chunks_exact(loaded.entry_size) {
                let entry = MmapIndexEntry::decode_from(chunk, version)?;
                let entity = loaded
                    .interner_strings
                    .get(entry.entity_idx as usize)
                    .ok_or_else(|| StoreError::ser_msg("mmap index entity_idx is out of range"))?;
                let scope = loaded
                    .interner_strings
                    .get(entry.scope_idx as usize)
                    .ok_or_else(|| StoreError::ser_msg("mmap index scope_idx is out of range"))?;
                let coord = Coordinate::new(entity, scope)?;
                rebuilt.push(IndexEntry {
                    event_id: entry.event_id,
                    correlation_id: entry.correlation_id,
                    causation_id: (entry.causation_id != 0).then_some(entry.causation_id),
                    coord,
                    entity_id: crate::store::interner::InternId(entry.entity_idx),
                    scope_id: crate::store::interner::InternId(entry.scope_idx),
                    kind: raw_to_kind(entry.kind),
                    wall_ms: entry.wall_ms,
                    clock: entry.clock,
                    dag_lane: entry.dag_lane,
                    dag_depth: entry.dag_depth,
                    hash_chain: HashChain {
                        prev_hash: entry.prev_hash,
                        event_hash: entry.event_hash,
                    },
                    disk_pos: DiskPos {
                        segment_id: entry.segment_id,
                        offset: entry.frame_offset,
                        length: entry.frame_length,
                    },
                    global_sequence: entry.global_sequence,
                });
            }
            Ok::<_, StoreError>((chunk_idx, rebuilt))
        })
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    per_chunk.sort_by_key(|(chunk_idx, _)| *chunk_idx);
    let mut rebuilt_entries = Vec::with_capacity(entry_count);
    for (_, chunk_entries) in per_chunk {
        rebuilt_entries.extend(chunk_entries);
    }
    let routing = if loaded.routing.chunks.is_empty() {
        RoutingSummary::from_sorted_entries(
            &rebuilt_entries,
            recommended_restore_chunk_count(rebuilt_entries.len()),
        )
    } else {
        loaded.routing.clone()
    };

    Some(LoadedMmapSnapshot {
        entries: rebuilt_entries,
        interner_strings: loaded.interner_strings,
        watermark: loaded.watermark,
        stored_allocator: loaded.stored_allocator,
        routing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;
    use crate::store::index::StoreIndex;
    use tempfile::TempDir;

    fn make_index(count: u64) -> StoreIndex {
        let idx = StoreIndex::new();
        for i in 0..count {
            let coord =
                Coordinate::new(format!("entity:{i}"), "scope:test").expect("valid coordinate");
            let entity_id = idx.interner.intern(coord.entity());
            let scope_id = idx.interner.intern(coord.scope());
            idx.insert(IndexEntry {
                event_id: (i + 1) as u128,
                correlation_id: (i + 1) as u128,
                causation_id: (i > 0).then_some(i as u128),
                coord,
                entity_id,
                scope_id,
                kind: EventKind::custom(0x1, (i & 0x0FFF) as u16),
                wall_ms: 10_000 + i,
                clock: u32::try_from(i).expect("fits u32"),
                dag_lane: 0,
                dag_depth: 0,
                hash_chain: HashChain::default(),
                disk_pos: DiskPos {
                    segment_id: 7,
                    offset: i * 64,
                    length: 64,
                },
                global_sequence: i,
            });
        }
        idx
    }

    #[test]
    fn mmap_index_roundtrip_restores_entries() {
        let tmp = TempDir::new().expect("temp dir");
        let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
        std::fs::write(&segment_path, vec![0u8; 4096]).expect("segment file");

        let src = make_index(8);
        write_mmap_index(&src, tmp.path(), 7, 512).expect("write mmap index");

        let snapshot = try_load_mmap_snapshot(tmp.path()).expect("load snapshot");
        assert_eq!(snapshot.routing.entry_count, 8);
        assert!(
            !snapshot.routing.chunks.is_empty(),
            "v2 mmap index must persist chunk summaries"
        );

        let dst = StoreIndex::new();
        let restored = try_restore_mmap_index(&dst, tmp.path()).expect("restore");
        assert_eq!(restored.0.watermark_segment_id, 7);
        assert_eq!(restored.0.watermark_offset, 512);
        assert_eq!(dst.len(), 8);
        assert_eq!(dst.visible_sequence(), 8);
    }

    #[test]
    fn corrupt_mmap_index_crc_is_rejected() {
        let tmp = TempDir::new().expect("temp dir");
        let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
        std::fs::write(&segment_path, vec![0u8; 4096]).expect("segment file");

        let idx = make_index(2);
        write_mmap_index(&idx, tmp.path(), 7, 128).expect("write mmap index");
        let path = tmp.path().join(MMAP_INDEX_FILENAME);
        let mut bytes = std::fs::read(&path).expect("read mmap index");
        *bytes.last_mut().expect("artifact has payload") ^= 0xFF;
        std::fs::write(&path, bytes).expect("rewrite corrupt mmap index");

        assert!(
            try_restore_mmap_index(&StoreIndex::new(), tmp.path()).is_none(),
            "corrupt mmap index must be rejected"
        );
    }

    #[test]
    fn mmap_index_requires_watermark_segment() {
        let tmp = TempDir::new().expect("temp dir");
        let idx = make_index(1);
        write_mmap_index(&idx, tmp.path(), 99, 0).expect("write mmap index");

        assert!(
            try_restore_mmap_index(&StoreIndex::new(), tmp.path()).is_none(),
            "mmap index must be ignored when the watermark segment is missing"
        );
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)] // test constructs v1 binary format with known small values
    fn v1_mmap_fallback_is_still_readable() {
        let tmp = TempDir::new().expect("temp dir");
        let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
        std::fs::write(&segment_path, vec![0u8; 4096]).expect("segment file");

        let idx = make_index(4);
        let mut entries = idx.all_entries();
        entries.sort_by_key(|entry| entry.global_sequence);
        let mut interner_strings = vec![String::new()];
        interner_strings.extend(idx.interner.to_snapshot());
        let interner_bytes = rmp_serde::to_vec_named(&interner_strings).expect("interner bytes");
        let mut header_tail = Vec::with_capacity(HEADER_TAIL_LEN_V1);
        header_tail.extend_from_slice(&7u64.to_le_bytes());
        header_tail.extend_from_slice(&128u64.to_le_bytes());
        header_tail.extend_from_slice(&idx.global_sequence().to_le_bytes());
        header_tail.extend_from_slice(&(interner_strings.len() as u32).to_le_bytes());
        header_tail.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        header_tail.extend_from_slice(&(interner_bytes.len() as u64).to_le_bytes());

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MMAP_INDEX_MAGIC);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&header_tail);
        bytes.extend_from_slice(&interner_bytes);

        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&header_tail);
        hasher.update(&interner_bytes);
        let mut buf = [0u8; MMAP_ENTRY_SIZE_V2];
        for entry in &entries {
            entry_to_mmap(entry).encode_into_v2(&mut buf);
            hasher.update(&buf);
            bytes.extend_from_slice(&buf);
        }
        let crc = hasher.finalize();
        bytes[8..12].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(tmp.path().join(MMAP_INDEX_FILENAME), bytes).expect("write v1 mmap index");

        let snapshot = try_load_mmap_snapshot(tmp.path()).expect("load v1 snapshot");
        assert_eq!(snapshot.entries.len(), 4);
        assert_eq!(snapshot.routing.entry_count, 4);
        assert!(
            !snapshot.routing.chunks.is_empty(),
            "v1 fallback should synthesize chunk summaries on load"
        );
    }

    #[test]
    fn v2_mmap_fallback_defaults_lane_depth_to_zero() {
        let tmp = TempDir::new().expect("temp dir");
        let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
        std::fs::write(&segment_path, vec![0u8; 4096]).expect("segment file");

        let idx = make_index(4);
        let mut entries = idx.all_entries();
        entries.sort_by_key(|entry| entry.global_sequence);
        let mut interner_strings = vec![String::new()];
        interner_strings.extend(idx.interner.to_snapshot());
        let interner_bytes = rmp_serde::to_vec_named(&interner_strings).expect("interner bytes");
        let routing = RoutingSummary::from_sorted_entries(
            &entries,
            recommended_restore_chunk_count(entries.len()),
        );
        let summary_bytes =
            rmp_serde::to_vec_named(&MmapSummaryDataV2 { routing }).expect("summary bytes");

        let mut header_tail = Vec::with_capacity(HEADER_TAIL_LEN_V2);
        header_tail.extend_from_slice(&7u64.to_le_bytes());
        header_tail.extend_from_slice(&128u64.to_le_bytes());
        header_tail.extend_from_slice(&idx.global_sequence().to_le_bytes());
        header_tail.extend_from_slice(&(interner_strings.len() as u32).to_le_bytes());
        header_tail.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        header_tail.extend_from_slice(&(interner_bytes.len() as u64).to_le_bytes());
        header_tail.extend_from_slice(&(summary_bytes.len() as u64).to_le_bytes());

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MMAP_INDEX_MAGIC);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&header_tail);
        bytes.extend_from_slice(&interner_bytes);
        bytes.extend_from_slice(&summary_bytes);

        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&header_tail);
        hasher.update(&interner_bytes);
        hasher.update(&summary_bytes);
        let mut buf = [0u8; MMAP_ENTRY_SIZE_V2];
        for entry in &entries {
            entry_to_mmap(entry).encode_into_v2(&mut buf);
            hasher.update(&buf);
            bytes.extend_from_slice(&buf);
        }
        let crc = hasher.finalize();
        bytes[8..12].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(tmp.path().join(MMAP_INDEX_FILENAME), bytes).expect("write v2 mmap index");

        let snapshot = try_load_mmap_snapshot(tmp.path()).expect("load v2 snapshot");
        assert_eq!(snapshot.entries.len(), 4);
        assert!(snapshot.entries.iter().all(|entry| entry.dag_lane == 0));
        assert!(snapshot.entries.iter().all(|entry| entry.dag_depth == 0));
    }
}
