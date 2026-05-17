//! Mmap-first cold-start artifact.
//!
//! `index.fbati` is a fixed-width snapshot of the in-memory index written on
//! orderly close and after compaction. On open we validate the artifact,
//! mmap it, restore the interner snapshot, replay the entry section, then
//! replay only the durable tail after the recorded watermark.

use crate::event::HashChain;
#[cfg(test)]
use crate::store::cold_start::raw_to_kind;
use crate::store::cold_start::{
    kind_to_raw, raw_to_kind_counted, validate_watermark_segment, ColdStartIndexRow,
    ColdStartSource, ReservedKindFallbackStats, WatermarkInfo, WatermarkValidationError,
};
use crate::store::index::interner::InternId;
use crate::store::index::{
    recommended_restore_chunk_count, restore_chunk_ranges, DiskPos, IndexEntry, RoutingSummary,
    StoreIndex,
};
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use memmap2::Mmap;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub(crate) const MMAP_INDEX_MAGIC: &[u8; 6] = b"FBATIX";
pub(crate) const MMAP_INDEX_VERSION: u16 = 5;
pub(crate) const MMAP_INDEX_FILENAME: &str = "index.fbati";

const PREFIX_LEN: usize = 6 + 2 + 4;
const HEADER_TAIL_LEN_V1: usize = 8 + 8 + 8 + 4 + 8 + 8;
const HEADER_TAIL_LEN_V2: usize = HEADER_TAIL_LEN_V1 + 8;
const HEADER_TAIL_LEN_V3: usize = HEADER_TAIL_LEN_V2 + 8;
const HEADER_LEN_V1: usize = PREFIX_LEN + HEADER_TAIL_LEN_V1;
const HEADER_LEN_V2: usize = PREFIX_LEN + HEADER_TAIL_LEN_V2;
const HEADER_LEN_V3: usize = PREFIX_LEN + HEADER_TAIL_LEN_V3;
const MMAP_ENTRY_SIZE_V2: usize = 162;
const MMAP_ENTRY_SIZE_V3: usize = 170;
const MMAP_ENTRY_SIZE_V5: usize = MMAP_ENTRY_SIZE_V3 + 8 + 8 + 32;

fn read_le_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

fn read_le_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_le_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

struct LoadedMmapIndex {
    mmap: Mmap,
    interner_strings: Vec<String>,
    routing: RoutingSummary,
    entries_offset: usize,
    extension_blob_offset: usize,
    extension_blob_len: usize,
    entry_count: u64,
    entry_size: usize,
    version: u16,
    watermark: WatermarkInfo,
    stored_allocator: u64,
    cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

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

#[derive(serde::Serialize, serde::Deserialize)]
struct MmapSummaryDataV2 {
    routing: RoutingSummary,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MmapSummaryDataV4 {
    routing: RoutingSummary,
    #[serde(default)]
    reserved_kind_fallbacks: ReservedKindFallbackStats,
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
    extension_offset: u64,
    extension_len: u64,
    extension_hash: [u8; 32],
}

impl MmapIndexEntry {
    fn to_disk_pos(&self) -> DiskPos {
        DiskPos::new(self.segment_id, self.frame_offset, self.frame_length)
    }

    #[cfg(test)]
    fn to_cold_start_row(&self) -> ColdStartIndexRow {
        self.to_cold_start_row_counted(&mut ReservedKindFallbackStats::default())
    }

    fn to_cold_start_row_counted(
        &self,
        counts: &mut ReservedKindFallbackStats,
    ) -> ColdStartIndexRow {
        ColdStartIndexRow {
            source: ColdStartSource::MmapIndex,
            event_id: self.event_id,
            correlation_id: self.correlation_id,
            causation_id: (self.causation_id != 0).then_some(self.causation_id),
            entity_id: InternId(self.entity_idx),
            scope_id: InternId(self.scope_idx),
            kind: raw_to_kind_counted(self.kind, counts),
            wall_ms: self.wall_ms,
            clock: self.clock,
            dag_lane: self.dag_lane,
            dag_depth: self.dag_depth,
            hash_chain: HashChain {
                prev_hash: self.prev_hash,
                event_hash: self.event_hash,
            },
            disk_pos: self.to_disk_pos(),
            global_sequence: self.global_sequence,
        }
    }

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

    fn encode_into_v5(&self, buf: &mut [u8]) {
        debug_assert_eq!(buf.len(), MMAP_ENTRY_SIZE_V5);
        self.encode_into(&mut buf[..MMAP_ENTRY_SIZE_V3]);
        let mut pos = MMAP_ENTRY_SIZE_V3;

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

        put_le!(self.extension_offset, 8);
        put_le!(self.extension_len, 8);
        put_bytes!(self.extension_hash);
        debug_assert_eq!(pos, MMAP_ENTRY_SIZE_V5);
    }

    fn decode_from(buf: &[u8], version: u16) -> Result<Self, StoreError> {
        let expected_size = if version >= 5 {
            MMAP_ENTRY_SIZE_V5
        } else if version >= 3 {
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
            extension_offset: if version >= 5 { get_le!(u64, 8) } else { 0 },
            extension_len: if version >= 5 { get_le!(u64, 8) } else { 0 },
            extension_hash: if version >= 5 { get_hash!() } else { [0u8; 32] },
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
        extension_offset: 0,
        extension_len: 0,
        extension_hash: [0u8; 32],
    }
}

fn extension_blob_digest(bytes: &[u8]) -> [u8; 32] {
    #[cfg(feature = "blake3")]
    {
        crate::event::hash::compute_hash(bytes)
    }
    #[cfg(not(feature = "blake3"))]
    {
        let mut digest = [0u8; 32];
        for (seed, chunk) in (0_u32..).zip(digest.chunks_exact_mut(4)) {
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(&seed.to_le_bytes());
            hasher.update(bytes);
            chunk.copy_from_slice(&hasher.finalize().to_le_bytes());
        }
        digest
    }
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
    entry: &MmapIndexEntry,
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
    let interner_bytes = rmp_serde::to_vec_named(&interner_strings)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;
    let summary_bytes = rmp_serde::to_vec_named(&MmapSummaryDataV4 {
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
        let mut mmap_entry = entry_to_mmap(entry);
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

    let mut header_tail = Vec::with_capacity(HEADER_TAIL_LEN_V3);
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
            writer.write_all(&extension_blob).map_err(StoreError::Io)?;

            let mut buf = [0u8; MMAP_ENTRY_SIZE_V5];
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

    let version = read_le_u16(&prefix[6..8])?;
    if !matches!(version, 1..=4) && version != MMAP_INDEX_VERSION {
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
    } else if version >= 5 {
        HEADER_TAIL_LEN_V3
    } else {
        HEADER_TAIL_LEN_V2
    };
    let header_len = if version == 1 {
        HEADER_LEN_V1
    } else if version >= 5 {
        HEADER_LEN_V3
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

    let expected_crc = read_le_u32(&prefix[8..12])?;
    let mut cursor = 0usize;
    let watermark_segment_id = read_le_u64(header_tail.get(cursor..cursor + 8)?)?;
    cursor += 8;
    let watermark_offset = read_le_u64(header_tail.get(cursor..cursor + 8)?)?;
    cursor += 8;
    let stored_allocator = read_le_u64(header_tail.get(cursor..cursor + 8)?)?;
    cursor += 8;
    let interner_count = read_le_u32(header_tail.get(cursor..cursor + 4)?)?;
    cursor += 4;
    let entry_count = read_le_u64(header_tail.get(cursor..cursor + 8)?)?;
    cursor += 8;
    let interner_bytes_len = read_le_u64(header_tail.get(cursor..cursor + 8)?)?;
    cursor += 8;
    let summary_bytes_len = if version == 1 {
        0usize
    } else {
        usize::try_from(read_le_u64(header_tail.get(cursor..cursor + 8)?)?).ok()?
    };
    if version != 1 {
        cursor += 8;
    }
    let extension_blob_len = if version >= 5 {
        usize::try_from(read_le_u64(header_tail.get(cursor..cursor + 8)?)?).ok()?
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
    let entry_size = if version >= 5 {
        MMAP_ENTRY_SIZE_V5
    } else if version >= 3 {
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
    let extension_blob_offset = match summary_offset.checked_add(summary_bytes_len) {
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
    let entries_offset = match extension_blob_offset.checked_add(extension_blob_len) {
        Some(offset) => offset,
        None => {
            tracing::warn!(
                target: "batpak::mmap_index",
                path = %path.display(),
                "mmap index extension blob offset overflowed"
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
            return None;
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
            return None;
        }
    }

    // SAFETY: Mmap::map requires a valid open file descriptor. The `file`
    // handle remains open for the duration of the mapping. The mmap index
    // file is read-only and not written to while open — external modification
    // would be a usage error, not a correctness concern for safe Rust callers.
    let evidence = crate::store::platform::evidence::collect_for_store_path(data_dir);
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
                return None;
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

    let (routing, cumulative_reserved_kind_fallbacks) = if version == 1 {
        (
            RoutingSummary::default(),
            ReservedKindFallbackStats::default(),
        )
    } else {
        let summary_slice = &mmap[summary_offset..extension_blob_offset];
        if version >= 4 {
            match rmp_serde::from_slice::<MmapSummaryDataV4>(summary_slice) {
                Ok(summary) => (summary.routing, summary.reserved_kind_fallbacks),
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
        } else {
            match rmp_serde::from_slice::<MmapSummaryDataV2>(summary_slice) {
                Ok(summary) => (summary.routing, ReservedKindFallbackStats::default()),
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
        }
    };

    Some(LoadedMmapIndex {
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

/// Restore the index from the mmap-first artifact. Returns the watermark and
/// allocator position if the artifact was present and valid.
#[cfg(test)]
pub(crate) fn try_restore_mmap_index(
    index: &StoreIndex,
    data_dir: &Path,
) -> Option<(WatermarkInfo, u64)> {
    let loaded = try_load_mmap_snapshot(data_dir)?;
    index
        .interner
        .replace_from_full_snapshot(&loaded.interner_strings);
    index
        .restore_sorted_entries(loaded.entries, loaded.stored_allocator)
        .ok()?;
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
    let entries_len = entry_count.checked_mul(loaded.entry_size)?;
    let entries_end = loaded.entries_offset.checked_add(entries_len)?;
    let entries_slice = loaded.mmap.get(loaded.entries_offset..entries_end)?;
    let extension_blob_end = loaded
        .extension_blob_offset
        .checked_add(loaded.extension_blob_len)?;
    let extension_blob_slice = loaded
        .mmap
        .get(loaded.extension_blob_offset..extension_blob_end)?;
    let chunk_ranges = restore_chunk_ranges(entry_count, &loaded.routing);

    let mut per_chunk = chunk_ranges
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
                let entry = MmapIndexEntry::decode_from(chunk, loaded.version)?;
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
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
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

    Some(LoadedMmapSnapshot {
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
mod tests {
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::store::index::StoreIndex;
    use std::collections::BTreeMap;
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
                kind: EventKind::custom(
                    0x1,
                    u16::try_from(i & 0x0FFF).expect("masked to 12 bits, fits u16"),
                ),
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
                receipt_extensions: BTreeMap::new(),
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
    fn mmap_index_roundtrip_restores_receipt_extensions() {
        let tmp = TempDir::new().expect("temp dir");
        let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
        std::fs::write(&segment_path, vec![0u8; 4096]).expect("segment file");

        let idx = StoreIndex::new();
        let coord = Coordinate::new("entity:mmap-ext", "scope:test").expect("coord");
        let entity_id = idx.interner.intern(coord.entity());
        let scope_id = idx.interner.intern(coord.scope());
        let mut receipt_extensions = BTreeMap::new();
        receipt_extensions.insert(
            ExtensionKey::new("app.audit").expect("valid extension key"),
            vec![0xFA, 0xCE, 0x05],
        );
        idx.insert(IndexEntry {
            event_id: 1,
            correlation_id: 1,
            causation_id: None,
            coord,
            entity_id,
            scope_id,
            kind: EventKind::DATA,
            wall_ms: 1_700_000_000_000,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos {
                segment_id: 7,
                offset: 0,
                length: 64,
            },
            global_sequence: 0,
            receipt_extensions: receipt_extensions.clone(),
        });

        write_mmap_index(&idx, tmp.path(), 7, 512).expect("write mmap index");

        let snapshot = try_load_mmap_snapshot(tmp.path()).expect("load snapshot");
        assert!(
            snapshot.receipt_extensions_hydrated,
            "PROPERTY: mmap v5 snapshots must carry receipt-extension maps directly."
        );
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.entries[0].receipt_extensions, receipt_extensions,
            "PROPERTY: mmap v5 extension blob table must preserve opaque receipt-extension bytes."
        );
    }

    #[test]
    fn mmap_entry_to_cold_start_row_preserves_index_fields() {
        let entry = MmapIndexEntry {
            event_id: 0x11,
            entity_idx: 1,
            scope_idx: 2,
            kind: kind_to_raw(EventKind::custom(0x4, 0x55)),
            wall_ms: 2000,
            clock: 9,
            dag_lane: 6,
            dag_depth: 7,
            prev_hash: [0x33; 32],
            event_hash: [0x44; 32],
            segment_id: 3,
            frame_offset: 128,
            frame_length: 96,
            global_sequence: 77,
            correlation_id: 0x22,
            causation_id: 0x33,
            extension_offset: 0,
            extension_len: 0,
            extension_hash: [0u8; 32],
        };
        let strings = vec![
            String::new(),
            "entity:mmap".to_owned(),
            "scope:test".to_owned(),
        ];

        let rebuilt = entry
            .to_cold_start_row()
            .to_index_entry(&strings)
            .expect("mmap row to index entry");

        assert_eq!(rebuilt.event_id, entry.event_id);
        assert_eq!(rebuilt.correlation_id, entry.correlation_id);
        assert_eq!(rebuilt.causation_id, Some(entry.causation_id));
        assert_eq!(rebuilt.coord.entity(), "entity:mmap");
        assert_eq!(rebuilt.coord.scope(), "scope:test");
        assert_eq!(rebuilt.kind, raw_to_kind(entry.kind));
        assert_eq!(rebuilt.wall_ms, entry.wall_ms);
        assert_eq!(rebuilt.clock, entry.clock);
        assert_eq!(rebuilt.dag_lane, entry.dag_lane);
        assert_eq!(rebuilt.dag_depth, entry.dag_depth);
        assert_eq!(rebuilt.hash_chain.prev_hash, entry.prev_hash);
        assert_eq!(rebuilt.hash_chain.event_hash, entry.event_hash);
        assert_eq!(rebuilt.disk_pos, entry.to_disk_pos());
        assert_eq!(rebuilt.global_sequence, entry.global_sequence);
    }

    #[test]
    fn mmap_entry_normalizes_zero_causation_to_none() {
        let row = MmapIndexEntry {
            event_id: 1,
            entity_idx: 1,
            scope_idx: 2,
            kind: kind_to_raw(EventKind::DATA),
            wall_ms: 10,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            prev_hash: [0; 32],
            event_hash: [1; 32],
            segment_id: 3,
            frame_offset: 4,
            frame_length: 5,
            global_sequence: 6,
            correlation_id: 2,
            causation_id: 0,
            extension_offset: 0,
            extension_len: 0,
            extension_hash: [0u8; 32],
        }
        .to_cold_start_row();

        assert_eq!(row.causation_id, None);
        assert_eq!(row.disk_pos, DiskPos::new(3, 4, 5));
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
    fn mmap_index_rejects_extension_blob_digest_mismatch() {
        let tmp = TempDir::new().expect("temp dir");
        let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
        std::fs::write(&segment_path, vec![0u8; 4096]).expect("segment file");

        let idx = StoreIndex::new();
        let coord = Coordinate::new("entity:mmap-ext-corrupt", "scope:test").expect("coord");
        let entity_id = idx.interner.intern(coord.entity());
        let scope_id = idx.interner.intern(coord.scope());
        let mut receipt_extensions = BTreeMap::new();
        receipt_extensions.insert(
            ExtensionKey::new("app.audit").expect("valid extension key"),
            vec![0x10, 0x20, 0x30],
        );
        idx.insert(IndexEntry {
            event_id: 1,
            correlation_id: 1,
            causation_id: None,
            coord,
            entity_id,
            scope_id,
            kind: EventKind::DATA,
            wall_ms: 1_700_000_000_000,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos {
                segment_id: 7,
                offset: 0,
                length: 64,
            },
            global_sequence: 0,
            receipt_extensions,
        });

        write_mmap_index(&idx, tmp.path(), 7, 128).expect("write mmap index");
        let path = tmp.path().join(MMAP_INDEX_FILENAME);
        let mut bytes = std::fs::read(&path).expect("read mmap index");
        let tail = &bytes[PREFIX_LEN..HEADER_LEN_V3];
        let mut cursor = 8 + 8 + 8 + 4 + 8;
        let interner_bytes_len =
            usize::try_from(read_le_u64(&tail[cursor..cursor + 8]).expect("interner len"))
                .expect("fits usize");
        cursor += 8;
        let summary_bytes_len =
            usize::try_from(read_le_u64(&tail[cursor..cursor + 8]).expect("summary len"))
                .expect("fits usize");
        cursor += 8;
        let extension_blob_len =
            usize::try_from(read_le_u64(&tail[cursor..cursor + 8]).expect("extension len"))
                .expect("fits usize");
        assert!(
            extension_blob_len > 0,
            "fixture should write extension blob bytes"
        );
        let extension_blob_offset = HEADER_LEN_V3 + interner_bytes_len + summary_bytes_len;
        bytes[extension_blob_offset] ^= 0xFF;
        let crc = crc32fast::hash(&bytes[12..]);
        bytes[8..12].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, bytes).expect("rewrite corrupt mmap index");

        assert!(
            try_load_mmap_snapshot(tmp.path()).is_none(),
            "PROPERTY: mmap v5 must reject extension blob bytes that pass artifact CRC but fail row digest validation."
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
        header_tail.extend_from_slice(
            &u32::try_from(interner_strings.len())
                .expect("test interner string count fits in u32")
                .to_le_bytes(),
        );
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
        assert!(
            !snapshot.receipt_extensions_hydrated,
            "PROPERTY: v1 mmap snapshots must require authoritative frame hydration for receipt extensions."
        );
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
        header_tail.extend_from_slice(
            &u32::try_from(interner_strings.len())
                .expect("test interner string count fits in u32")
                .to_le_bytes(),
        );
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
        assert!(
            !snapshot.receipt_extensions_hydrated,
            "PROPERTY: v2 mmap snapshots must require authoritative frame hydration for receipt extensions."
        );
        assert!(snapshot.entries.iter().all(|entry| entry.dag_lane == 0));
        assert!(snapshot.entries.iter().all(|entry| entry.dag_depth == 0));
    }
}
