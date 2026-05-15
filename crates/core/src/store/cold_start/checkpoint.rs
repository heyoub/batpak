//! Index checkpoint: fast cold-start by persisting the in-memory index to disk.
//!
//! On every orderly shutdown (and optionally on a timer), `write_checkpoint` serialises
//! the full `StoreIndex` to `<data_dir>/index.ckpt`.  On the next cold start,
//! `try_load_checkpoint` reads that file; if it is intact and the referenced watermark
//! segment still exists on disk, the caller may call `restore_from_checkpoint` instead
//! of scanning every segment from scratch.
//!
//! # File format
//!
//! ```text
//! [MAGIC: b"FBATCK"]   — 6 bytes, identifies the file type
//! [version: u16 LE]    — v2/v3/v4 fallback or v5 current
//! [crc32: u32 LE]      — CRC32 of the msgpack body that follows
//! [msgpack body]        — versioned checkpoint body serialised via rmp_serde
//! ```
//!
//! The magic + version occupy the first 8 bytes; the 4-byte CRC immediately follows;
//! the variable-length msgpack body fills the rest of the file.

use crate::event::{EventKind, HashChain};
use crate::store::cold_start::{
    validate_watermark_segment, ColdStartIndexRow, ColdStartSource, WatermarkValidationError,
};
use crate::store::index::interner::InternId;
use crate::store::index::{
    recommended_restore_chunk_count, DiskPos, IndexEntry, RoutingSummary, StoreIndex,
};
use crate::store::platform::fs::write_file_atomically;
use crate::store::segment::sidx::ReservedKindFallbackStats;
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use rayon::prelude::*;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufWriter, Write};
use std::path::Path;

// ── Constants ────────────────────────────────────────────────────────────────

/// Magic bytes at the start of every checkpoint file.
pub(crate) const CHECKPOINT_MAGIC: &[u8; 6] = b"FBATCK";

/// Format version stored in the checkpoint header.
/// v6: v5 plus receipt-extension maps in checkpoint entries.
/// v2 checkpoints remain readable as a fallback; v1 is rejected.
pub(crate) const CHECKPOINT_VERSION: u16 = 6;

/// Final checkpoint filename inside the data directory.
pub(crate) const CHECKPOINT_FILENAME: &str = "index.ckpt";

// ── Wire types ───────────────────────────────────────────────────────────────

/// Checkpoint format v2: includes interner snapshot + InternId-based entries.
#[derive(Serialize, Deserialize)]
struct CheckpointDataV2 {
    global_sequence: u64,
    watermark_segment_id: u64,
    watermark_offset: u64,
    /// Interner snapshot: ordered list of interned strings (index = InternId).
    /// The sentinel (empty string at index 0) is included.
    interner_strings: Vec<String>,
    entries: Vec<CheckpointEntry>,
}

/// Checkpoint format v3: v2 plus additive routing/chunk summaries.
#[derive(Serialize, Deserialize)]
struct CheckpointDataV3 {
    global_sequence: u64,
    watermark_segment_id: u64,
    watermark_offset: u64,
    interner_strings: Vec<String>,
    routing: RoutingSummary,
    entries: Vec<CheckpointEntry>,
}

/// Checkpoint format v4: v3 plus DAG lane/depth inside each entry.
#[derive(Serialize, Deserialize)]
struct CheckpointDataV4 {
    global_sequence: u64,
    watermark_segment_id: u64,
    watermark_offset: u64,
    interner_strings: Vec<String>,
    routing: RoutingSummary,
    entries: Vec<CheckpointEntry>,
}

/// Checkpoint format v6: v5 plus receipt-extension maps in entries.
#[derive(Serialize, Deserialize)]
struct CheckpointDataV6 {
    global_sequence: u64,
    watermark_segment_id: u64,
    watermark_offset: u64,
    interner_strings: Vec<String>,
    routing: RoutingSummary,
    #[serde(default)]
    reserved_kind_fallbacks: ReservedKindFallbackStats,
    entries: Vec<CheckpointEntry>,
}

/// Checkpoint entry v2: uses InternId u32s instead of raw entity/scope strings.
/// ~22 bytes smaller per entry than v1.
#[derive(Serialize, Deserialize)]
pub(crate) struct CheckpointEntry {
    #[serde(with = "crate::wire::u128_bytes")]
    pub event_id: u128,
    #[serde(with = "crate::wire::u128_bytes")]
    pub correlation_id: u128,
    #[serde(with = "crate::wire::option_u128_bytes")]
    pub causation_id: Option<u128>,
    /// InternId for entity string — index into interner_strings.
    pub entity_id: u32,
    /// InternId for scope string — index into interner_strings.
    pub scope_id: u32,
    pub kind: EventKind,
    pub wall_ms: u64,
    pub clock: u32,
    #[serde(default)]
    pub dag_lane: u32,
    #[serde(default)]
    pub dag_depth: u32,
    pub prev_hash: [u8; 32],
    pub event_hash: [u8; 32],
    pub segment_id: u64,
    pub offset: u64,
    pub length: u32,
    pub global_sequence: u64,
    #[serde(default)]
    pub receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

impl CheckpointEntry {
    fn to_disk_pos(&self) -> DiskPos {
        DiskPos::new(self.segment_id, self.offset, self.length)
    }

    fn to_cold_start_row(&self) -> ColdStartIndexRow {
        ColdStartIndexRow {
            source: ColdStartSource::Checkpoint,
            event_id: self.event_id,
            correlation_id: self.correlation_id,
            causation_id: self.causation_id.filter(|&id| id != 0),
            entity_id: InternId(self.entity_id),
            scope_id: InternId(self.scope_id),
            kind: self.kind,
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

    fn to_index_entry(&self, interner_strings: &[String]) -> Result<IndexEntry, StoreError> {
        let mut entry = self.to_cold_start_row().to_index_entry(interner_strings)?;
        entry.receipt_extensions = self.receipt_extensions.clone();
        Ok(entry)
    }
}

// ── Public-to-crate surface ───────────────────────────────────────────────────

/// Watermark and global-sequence information returned by [`try_load_checkpoint`].
///
/// The caller uses these values to know how far the durable log extends without
/// reading every segment file.
pub(crate) struct WatermarkInfo {
    /// Segment ID of the highest durably-written event.
    pub watermark_segment_id: u64,
    /// Byte offset within the watermark segment.
    pub watermark_offset: u64,
}

pub(crate) struct LoadedCheckpointData {
    pub(crate) entries: Vec<CheckpointEntry>,
    pub(crate) interner_strings: Vec<String>,
    pub(crate) watermark: WatermarkInfo,
    pub(crate) stored_allocator: u64,
    pub(crate) routing: RoutingSummary,
    pub(crate) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

pub(crate) struct LoadedCheckpointSnapshot {
    pub(crate) entries: Vec<IndexEntry>,
    pub(crate) interner_strings: Vec<String>,
    pub(crate) watermark: WatermarkInfo,
    pub(crate) stored_allocator: u64,
    pub(crate) routing: RoutingSummary,
    pub(crate) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
    pub(crate) receipt_extensions_hydrated: bool,
}

struct LoadedCheckpointFile {
    path: std::path::PathBuf,
    version: u16,
    body: Vec<u8>,
}

struct CheckpointSnapshotDataV6 {
    global_sequence: u64,
    watermark_segment_id: u64,
    watermark_offset: u64,
    interner_strings: Vec<String>,
    routing: RoutingSummary,
    reserved_kind_fallbacks: ReservedKindFallbackStats,
    entries: Vec<IndexEntry>,
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum CheckpointDataV6Field {
    GlobalSequence,
    WatermarkSegmentId,
    WatermarkOffset,
    InternerStrings,
    Routing,
    ReservedKindFallbacks,
    Entries,
}

struct CheckpointEntriesSeed<'a> {
    interner_strings: &'a [String],
}

struct CheckpointEntriesVisitor<'a> {
    interner_strings: &'a [String],
}

impl<'de, 'a> DeserializeSeed<'de> for CheckpointEntriesSeed<'a> {
    type Value = Vec<IndexEntry>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(CheckpointEntriesVisitor {
            interner_strings: self.interner_strings,
        })
    }
}

impl<'de, 'a> Visitor<'de> for CheckpointEntriesVisitor<'a> {
    type Value = Vec<IndexEntry>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence of checkpoint entries")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut entries = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(entry) = seq.next_element::<CheckpointEntry>()? {
            entries.push(
                entry
                    .to_index_entry(self.interner_strings)
                    .map_err(de::Error::custom)?,
            );
        }
        Ok(entries)
    }
}

struct CheckpointSnapshotDataV6Visitor;

impl<'de> Visitor<'de> for CheckpointSnapshotDataV6Visitor {
    type Value = CheckpointSnapshotDataV6;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("checkpoint v6 snapshot data")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut global_sequence = None;
        let mut watermark_segment_id = None;
        let mut watermark_offset = None;
        let mut interner_strings = None;
        let mut routing = None;
        let mut reserved_kind_fallbacks = None;
        let mut entries = None;

        while let Some(field) = map.next_key::<CheckpointDataV6Field>()? {
            match field {
                CheckpointDataV6Field::GlobalSequence => {
                    if global_sequence.is_some() {
                        return Err(de::Error::duplicate_field("global_sequence"));
                    }
                    global_sequence = Some(map.next_value()?);
                }
                CheckpointDataV6Field::WatermarkSegmentId => {
                    if watermark_segment_id.is_some() {
                        return Err(de::Error::duplicate_field("watermark_segment_id"));
                    }
                    watermark_segment_id = Some(map.next_value()?);
                }
                CheckpointDataV6Field::WatermarkOffset => {
                    if watermark_offset.is_some() {
                        return Err(de::Error::duplicate_field("watermark_offset"));
                    }
                    watermark_offset = Some(map.next_value()?);
                }
                CheckpointDataV6Field::InternerStrings => {
                    if interner_strings.is_some() {
                        return Err(de::Error::duplicate_field("interner_strings"));
                    }
                    interner_strings = Some(map.next_value()?);
                }
                CheckpointDataV6Field::Routing => {
                    if routing.is_some() {
                        return Err(de::Error::duplicate_field("routing"));
                    }
                    routing = Some(map.next_value()?);
                }
                CheckpointDataV6Field::ReservedKindFallbacks => {
                    if reserved_kind_fallbacks.is_some() {
                        return Err(de::Error::duplicate_field("reserved_kind_fallbacks"));
                    }
                    reserved_kind_fallbacks = Some(map.next_value()?);
                }
                CheckpointDataV6Field::Entries => {
                    if entries.is_some() {
                        return Err(de::Error::duplicate_field("entries"));
                    }
                    let strings = interner_strings.as_deref().ok_or_else(|| {
                        de::Error::custom("checkpoint v5 requires interner_strings before entries")
                    })?;
                    entries = Some(map.next_value_seed(CheckpointEntriesSeed {
                        interner_strings: strings,
                    })?);
                }
            }
        }

        Ok(CheckpointSnapshotDataV6 {
            global_sequence: global_sequence
                .ok_or_else(|| de::Error::missing_field("global_sequence"))?,
            watermark_segment_id: watermark_segment_id
                .ok_or_else(|| de::Error::missing_field("watermark_segment_id"))?,
            watermark_offset: watermark_offset
                .ok_or_else(|| de::Error::missing_field("watermark_offset"))?,
            interner_strings: interner_strings
                .ok_or_else(|| de::Error::missing_field("interner_strings"))?,
            routing: routing.ok_or_else(|| de::Error::missing_field("routing"))?,
            reserved_kind_fallbacks: reserved_kind_fallbacks.unwrap_or_default(),
            entries: entries.ok_or_else(|| de::Error::missing_field("entries"))?,
        })
    }
}

impl<'de> Deserialize<'de> for CheckpointSnapshotDataV6 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(CheckpointSnapshotDataV6Visitor)
    }
}

fn checkpoint_entries_to_index_entries(
    entries: &[CheckpointEntry],
    interner_strings: &[String],
) -> Result<Vec<IndexEntry>, StoreError> {
    entries
        .iter()
        .map(|ce| ce.to_index_entry(interner_strings))
        .collect()
}

fn read_checkpoint_file(data_dir: &Path) -> Option<LoadedCheckpointFile> {
    let path = data_dir.join(CHECKPOINT_FILENAME);

    let raw = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                error = %error,
                "failed to read checkpoint file"
            );
            return None;
        }
    };

    const HEADER_LEN: usize = 6 + 2 + 4;
    if raw.len() < HEADER_LEN {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            len = raw.len(),
            "checkpoint file too short to contain a valid header"
        );
        return None;
    }

    if &raw[..6] != CHECKPOINT_MAGIC.as_ref() {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            "checkpoint file has wrong magic bytes — ignoring"
        );
        return None;
    }

    let version = u16::from_le_bytes([raw[6], raw[7]]);
    let stored_crc = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let body = raw[HEADER_LEN..].to_vec();
    let computed_crc = crc32fast::hash(&body);
    if stored_crc != computed_crc {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            stored = stored_crc,
            computed = computed_crc,
            "checkpoint CRC mismatch — file is corrupt, ignoring"
        );
        return None;
    }

    Some(LoadedCheckpointFile {
        path,
        version,
        body,
    })
}

fn validate_checkpoint_watermark(
    data_dir: &Path,
    path: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
) -> Option<WatermarkInfo> {
    match validate_watermark_segment(data_dir, watermark_segment_id, watermark_offset) {
        Ok(()) => {}
        Err(WatermarkValidationError::MissingSegment { path: seg_path }) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                missing_segment = %seg_path.display(),
                "watermark segment referenced by checkpoint is missing — ignoring checkpoint"
            );
            return None;
        }
        Err(WatermarkValidationError::OffsetPastTail {
            path: seg_path,
            file_len,
            watermark_offset,
        }) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                watermark_segment = %seg_path.display(),
                file_len,
                watermark_offset,
                "checkpoint watermark points past the segment tail"
            );
            return None;
        }
    }

    Some(WatermarkInfo {
        watermark_segment_id,
        watermark_offset,
    })
}

fn decode_checkpoint_data(
    data_dir: &Path,
    path: &Path,
    version: u16,
    body: &[u8],
) -> Option<LoadedCheckpointData> {
    // ── 6. Deserialise msgpack body ───────────────────────────────────────────
    let (
        entries,
        interner_strings,
        watermark_segment_id,
        watermark_offset,
        global_sequence,
        routing,
        cumulative_reserved_kind_fallbacks,
    ) = match version {
        2 => {
            let data: CheckpointDataV2 = match rmp_serde::from_slice(body) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        target: "batpak::checkpoint",
                        path = %path.display(),
                        error = %e,
                        "checkpoint deserialisation failed — ignoring"
                    );
                    return None;
                }
            };
            let routing = RoutingSummary::from_sorted_entries(
                &checkpoint_entries_to_index_entries(&data.entries, &data.interner_strings).ok()?,
                recommended_restore_chunk_count(data.entries.len()),
            );
            (
                data.entries,
                data.interner_strings,
                data.watermark_segment_id,
                data.watermark_offset,
                data.global_sequence,
                routing,
                ReservedKindFallbackStats::default(),
            )
        }
        3 => {
            let data: CheckpointDataV3 = match rmp_serde::from_slice(body) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        target: "batpak::checkpoint",
                        path = %path.display(),
                        error = %e,
                        "checkpoint deserialisation failed — ignoring"
                    );
                    return None;
                }
            };
            (
                data.entries,
                data.interner_strings,
                data.watermark_segment_id,
                data.watermark_offset,
                data.global_sequence,
                data.routing,
                ReservedKindFallbackStats::default(),
            )
        }
        4 => {
            let data: CheckpointDataV4 = match rmp_serde::from_slice(body) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        target: "batpak::checkpoint",
                        path = %path.display(),
                        error = %e,
                        "checkpoint deserialisation failed — ignoring"
                    );
                    return None;
                }
            };
            (
                data.entries,
                data.interner_strings,
                data.watermark_segment_id,
                data.watermark_offset,
                data.global_sequence,
                data.routing,
                ReservedKindFallbackStats::default(),
            )
        }
        5 | 6 => {
            let data: CheckpointDataV6 = match rmp_serde::from_slice(body) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        target: "batpak::checkpoint",
                        path = %path.display(),
                        error = %e,
                        "checkpoint deserialisation failed — ignoring"
                    );
                    return None;
                }
            };
            (
                data.entries,
                data.interner_strings,
                data.watermark_segment_id,
                data.watermark_offset,
                data.global_sequence,
                data.routing,
                data.reserved_kind_fallbacks,
            )
        }
        _ => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                version,
                expected = CHECKPOINT_VERSION,
                "unsupported checkpoint version — ignoring"
            );
            return None;
        }
    };

    let watermark =
        validate_checkpoint_watermark(data_dir, path, watermark_segment_id, watermark_offset)?;

    tracing::debug!(
        target: "batpak::checkpoint",
        entries = entries.len(),
        global_sequence,
        watermark_segment_id,
        watermark_offset,
        "checkpoint loaded successfully"
    );

    Some(LoadedCheckpointData {
        entries,
        interner_strings,
        watermark,
        stored_allocator: global_sequence,
        routing,
        cumulative_reserved_kind_fallbacks,
    })
}

fn decode_checkpoint_snapshot_v6(
    data_dir: &Path,
    path: &Path,
    body: &[u8],
) -> Option<LoadedCheckpointSnapshot> {
    let data: CheckpointSnapshotDataV6 = match rmp_serde::from_slice(body) {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                error = %error,
                "checkpoint snapshot deserialisation failed — ignoring"
            );
            return None;
        }
    };

    let watermark = validate_checkpoint_watermark(
        data_dir,
        path,
        data.watermark_segment_id,
        data.watermark_offset,
    )?;

    tracing::debug!(
        target: "batpak::checkpoint",
        entries = data.entries.len(),
        global_sequence = data.global_sequence,
        watermark_segment_id = data.watermark_segment_id,
        watermark_offset = data.watermark_offset,
        "checkpoint snapshot loaded successfully"
    );

    Some(LoadedCheckpointSnapshot {
        entries: data.entries,
        interner_strings: data.interner_strings,
        watermark,
        stored_allocator: data.global_sequence,
        routing: data.routing,
        cumulative_reserved_kind_fallbacks: data.reserved_kind_fallbacks,
        receipt_extensions_hydrated: true,
    })
}

// ── write_checkpoint ─────────────────────────────────────────────────────────

/// Serialise the entire in-memory index to `<data_dir>/index.ckpt`.
///
/// The write is atomic: data is first written to a same-directory temporary file,
/// fsynced, then renamed over the final path.  A partial write caused by a
/// crash therefore never corrupts the previous good checkpoint.
///
/// # Errors
///
/// Returns [`StoreError::Serialization`] if msgpack serialisation fails.
/// Returns [`StoreError::Io`] if any filesystem operation (open, write, fsync,
/// rename) fails.
#[cfg(test)]
pub(crate) fn write_checkpoint(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
) -> Result<(), StoreError> {
    write_checkpoint_with_reserved_kind_fallbacks(
        index,
        data_dir,
        watermark_segment_id,
        watermark_offset,
        &ReservedKindFallbackStats::default(),
    )
}

pub(crate) fn write_checkpoint_with_reserved_kind_fallbacks(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
    reserved_kind_fallbacks: &ReservedKindFallbackStats,
) -> Result<(), StoreError> {
    // ── 1. Collect every entry from the index ────────────────────────────────
    // all_entries() is not a linearisable snapshot (DashMap limitation), but that
    // is acceptable here: the checkpoint is always written from a single
    // orchestrating call site that holds the writer quiesced (or after close()).
    // Entries appended after the snapshot starts will appear in the next checkpoint.
    let mut entries: Vec<CheckpointEntry> = index
        .all_entries()
        .into_iter()
        .map(|e| CheckpointEntry {
            event_id: e.event_id,
            correlation_id: e.correlation_id,
            causation_id: e.causation_id,
            entity_id: e.entity_id.as_u32(),
            scope_id: e.scope_id.as_u32(),
            kind: e.kind,
            wall_ms: e.wall_ms,
            clock: e.clock,
            dag_lane: e.dag_lane,
            dag_depth: e.dag_depth,
            prev_hash: e.hash_chain.prev_hash,
            event_hash: e.hash_chain.event_hash,
            segment_id: e.disk_pos.segment_id,
            offset: e.disk_pos.offset,
            length: e.disk_pos.length,
            global_sequence: e.global_sequence,
            receipt_extensions: e.receipt_extensions.clone(),
        })
        .collect();

    // ── 2. Sort ascending by global_sequence for deterministic restore order ──
    entries.sort_by_key(|e| e.global_sequence);

    // Snapshot the interner: sentinel ("") at index 0, then all interned strings in order.
    let mut interner_strings = vec![String::new()]; // sentinel at index 0
    interner_strings.extend(index.interner.to_snapshot());
    tracing::debug!(
        "checkpoint: {} entries, {} interned strings",
        entries.len(),
        index.interner.len()
    );

    let routing = RoutingSummary::from_sorted_entries(
        &checkpoint_entries_to_index_entries(&entries, &interner_strings)?,
        recommended_restore_chunk_count(entries.len()),
    );

    let data = CheckpointDataV6 {
        global_sequence: index.global_sequence(),
        watermark_segment_id,
        watermark_offset,
        interner_strings,
        routing,
        reserved_kind_fallbacks: reserved_kind_fallbacks.clone(),
        entries,
    };

    // ── 3. Serialise to msgpack ───────────────────────────────────────────────
    let body =
        rmp_serde::to_vec_named(&data).map_err(|e| StoreError::Serialization(Box::new(e)))?;

    // ── 4. Compute CRC of the body ────────────────────────────────────────────
    let crc: u32 = crc32fast::hash(&body);

    // ── 5. Write to a same-directory tempfile with fsync ─────────────────────
    let final_path = data_dir.join(CHECKPOINT_FILENAME);
    write_file_atomically(data_dir, &final_path, "checkpoint", |file| {
        let mut w = BufWriter::new(file);

        // Header: MAGIC (6) + version (2 LE) + crc (4 LE)
        w.write_all(CHECKPOINT_MAGIC)?;
        w.write_all(&CHECKPOINT_VERSION.to_le_bytes())?;
        w.write_all(&crc.to_le_bytes())?;
        w.write_all(&body)?;
        w.flush()?;
        Ok(())
    })?;

    tracing::debug!(
        target: "batpak::checkpoint",
        entries = data.global_sequence,
        watermark_segment_id,
        watermark_offset,
        body_bytes = body.len(),
        "checkpoint written"
    );

    Ok(())
}
// ── try_load_checkpoint ───────────────────────────────────────────────────────

/// Try to load a checkpoint from `<data_dir>/index.ckpt`.
///
/// Returns `None` — and emits a `tracing::warn!` — on any of:
/// - File not found (normal on first start).
/// - Bad magic bytes.
/// - Bad version number.
/// - CRC32 mismatch (corruption).
/// - Msgpack deserialisation error.
/// - The watermark segment file referenced in the checkpoint does not exist on
///   disk (indicates the data directory was modified externally after the
///   checkpoint was written).
///
/// On success returns the decoded checkpoint body plus routing summary.
/// `stored_allocator` is the `global_sequence` allocator position at checkpoint time,
/// which may be higher than `entries.len()` due to burned batch slots.
#[cfg(test)]
pub(crate) fn try_load_checkpoint(data_dir: &Path) -> Option<LoadedCheckpointData> {
    let loaded = read_checkpoint_file(data_dir)?;
    decode_checkpoint_data(data_dir, &loaded.path, loaded.version, &loaded.body)
}

// ── restore_from_checkpoint ───────────────────────────────────────────────────

/// Replay checkpoint entries into `index`, using the interner strings table
/// to resolve `entity_id` and `scope_id` back to string values.
///
/// `stored_allocator` is the `global_sequence` allocator value at checkpoint time.
/// It may be higher than `entries.len()` due to burned batch slots. After inserting
/// all entries, the allocator is restored to this value (not the count-derived value).
///
/// Entries must be sorted ascending by `global_sequence` (which
/// [`write_checkpoint`] guarantees).
///
/// # Errors
///
/// Returns [`StoreError::Coordinate`] if resolved strings are empty.
/// Returns [`StoreError::Serialization`] if an InternId is out of range.
#[cfg(test)]
pub(crate) fn restore_from_checkpoint(
    index: &StoreIndex,
    entries: Vec<CheckpointEntry>,
    interner_strings: &[String],
    stored_allocator: u64,
) -> Result<(), StoreError> {
    index.interner.replace_from_full_snapshot(interner_strings);
    let mut rebuilt_entries = Vec::with_capacity(entries.len());

    for ce in entries {
        rebuilt_entries.push(ce.to_index_entry(interner_strings)?);
    }

    index.restore_sorted_entries(rebuilt_entries, stored_allocator)?;
    Ok(())
}

pub(crate) fn try_load_checkpoint_snapshot(data_dir: &Path) -> Option<LoadedCheckpointSnapshot> {
    let raw = read_checkpoint_file(data_dir)?;
    if raw.version == CHECKPOINT_VERSION {
        return decode_checkpoint_snapshot_v6(data_dir, &raw.path, &raw.body);
    }

    let loaded = decode_checkpoint_data(data_dir, &raw.path, raw.version, &raw.body)?;
    let chunk_ranges = if loaded.routing.chunks.is_empty() {
        vec![(0usize, loaded.entries.len())]
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
            let end = start + len;
            let rebuilt = checkpoint_entries_to_index_entries(
                &loaded.entries[start..end],
                &loaded.interner_strings,
            )?;
            Ok::<_, StoreError>((chunk_idx, rebuilt))
        })
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    per_chunk.sort_by_key(|(chunk_idx, _)| *chunk_idx);
    let mut rebuilt_entries = Vec::with_capacity(loaded.entries.len());
    for (_, chunk_entries) in per_chunk {
        rebuilt_entries.extend(chunk_entries);
    }

    Some(LoadedCheckpointSnapshot {
        entries: rebuilt_entries,
        interner_strings: loaded.interner_strings,
        watermark: loaded.watermark,
        stored_allocator: loaded.stored_allocator,
        routing: loaded.routing,
        cumulative_reserved_kind_fallbacks: loaded.cumulative_reserved_kind_fallbacks,
        receipt_extensions_hydrated: false,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::store::index::StoreIndex;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    /// Build a minimal populated StoreIndex with `n` synthetic entries.
    fn make_index(n: u64) -> StoreIndex {
        let idx = StoreIndex::new();
        for i in 0..n {
            let coord =
                Coordinate::new(format!("entity:{i}"), "test-scope").expect("valid coordinate");
            let entity_id = idx.interner.intern(coord.entity());
            let scope_id = idx.interner.intern(coord.scope());
            let entry = IndexEntry {
                event_id: (i + 1) as u128,
                correlation_id: (i + 1) as u128,
                causation_id: if i == 0 { None } else { Some(i as u128) },
                coord,
                entity_id,
                scope_id,
                kind: EventKind::custom(0x1, (i & 0x0FFF) as u16),
                wall_ms: 1_700_000_000_000 + i * 1000,
                clock: u32::try_from(i).expect("i fits u32"),
                dag_lane: 0,
                dag_depth: 0,
                hash_chain: HashChain::default(),
                disk_pos: DiskPos {
                    segment_id: 0,
                    offset: i * 256,
                    length: 256,
                },
                global_sequence: i,
                receipt_extensions: BTreeMap::new(),
            };
            idx.insert(entry);
        }
        // Publish all entries so read methods see them.
        idx.publish(idx.global_sequence(), "checkpoint-test-publish")
            .expect("publish all entries");
        idx
    }

    /// Create a dummy segment file large enough for watermark validation.
    fn touch_segment(dir: &Path, segment_id: u64) {
        let name = format!("{segment_id:06}.fbat");
        std::fs::write(dir.join(name), vec![0u8; 8192]).expect("write dummy segment");
    }

    #[test]
    fn round_trip_empty_index() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = StoreIndex::new();
        write_checkpoint(&idx, dir, 0, 0).expect("write");

        let result = try_load_checkpoint(dir);
        assert!(result.is_some(), "checkpoint should load");

        let loaded = result.expect("some");
        let entries = loaded.entries;
        let wm = loaded.watermark;
        assert_eq!(entries.len(), 0);
        assert_eq!(wm.watermark_segment_id, 0);
        assert_eq!(wm.watermark_offset, 0);
    }

    #[test]
    fn round_trip_with_entries() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = make_index(16);
        write_checkpoint(&idx, dir, 0, 4096).expect("write");

        let raw = std::fs::read(dir.join(CHECKPOINT_FILENAME)).expect("read checkpoint");
        assert_eq!(
            u16::from_le_bytes([raw[6], raw[7]]),
            CHECKPOINT_VERSION,
            "write_checkpoint must encode the current checkpoint version"
        );
        let body = &raw[12..];
        let direct: CheckpointDataV6 =
            rmp_serde::from_slice(body).expect("checkpoint body should deserialize directly");
        assert_eq!(direct.entries.len(), 16);
        assert!(
            validate_watermark_segment(dir, 0, 4096).is_ok(),
            "round-trip fixture must satisfy watermark validation"
        );

        let loaded = try_load_checkpoint(dir).expect("should load");
        let routing = loaded.routing.clone();
        let entries = loaded.entries;
        let wm = loaded.watermark;
        assert_eq!(entries.len(), 16);
        assert_eq!(wm.watermark_offset, 4096);

        // Verify sort order
        let seqs: Vec<u64> = entries.iter().map(|e| e.global_sequence).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "entries must be sorted by global_sequence");
        assert_eq!(routing.entry_count, 16);
        assert!(
            !routing.entity_runs.is_empty(),
            "v4 checkpoints must persist entity-run summaries"
        );
        assert!(
            !routing.chunks.is_empty(),
            "current-version checkpoints must persist chunk summaries"
        );
    }

    #[test]
    fn current_version_snapshot_restores_checkpoint_directly() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = make_index(16);
        let reserved_kind_fallbacks = ReservedKindFallbackStats {
            system: 2,
            effect: 1,
            system_histogram: std::iter::once((0x000Au16, 2usize)).collect(),
            effect_histogram: std::iter::once((0x1001u16, 1usize)).collect(),
        };
        write_checkpoint_with_reserved_kind_fallbacks(&idx, dir, 0, 4096, &reserved_kind_fallbacks)
            .expect("write checkpoint");

        let loaded = try_load_checkpoint_snapshot(dir).expect("load checkpoint snapshot");

        assert_eq!(loaded.entries.len(), 16);
        assert_eq!(loaded.watermark.watermark_offset, 4096);
        assert!(
            loaded.receipt_extensions_hydrated,
            "PROPERTY: current checkpoint entries carry receipt-extension maps directly."
        );
        assert_eq!(
            loaded.cumulative_reserved_kind_fallbacks,
            reserved_kind_fallbacks,
            "PROPERTY: direct checkpoint restore must preserve persisted cumulative reserved-kind fallback stats."
        );
        assert_eq!(
            loaded.entries.first().map(|entry| entry.global_sequence),
            Some(0),
            "PROPERTY: direct checkpoint restore must preserve sorted global-sequence order."
        );
        assert_eq!(
            loaded.entries.last().map(|entry| entry.global_sequence),
            Some(15),
            "PROPERTY: direct checkpoint restore must preserve the full checkpoint entry set."
        );
    }

    #[test]
    fn current_version_checkpoint_restores_receipt_extensions_directly() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = StoreIndex::new();
        let coord = Coordinate::new("entity:checkpoint-ext", "scope:test").expect("coord");
        let entity_id = idx.interner.intern(coord.entity());
        let scope_id = idx.interner.intern(coord.scope());
        let mut receipt_extensions = BTreeMap::new();
        receipt_extensions.insert(
            ExtensionKey::new("app.audit").expect("valid extension key"),
            vec![0xCA, 0xFE, 0x01],
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
                segment_id: 0,
                offset: 0,
                length: 64,
            },
            global_sequence: 0,
            receipt_extensions: receipt_extensions.clone(),
        });
        idx.publish(idx.global_sequence(), "checkpoint-extension-test-publish")
            .expect("publish");

        write_checkpoint(&idx, dir, 0, 64).expect("write checkpoint");

        let loaded = try_load_checkpoint_snapshot(dir).expect("load checkpoint snapshot");
        assert!(
            loaded.receipt_extensions_hydrated,
            "PROPERTY: current checkpoints must not need frame hydration for receipt extensions."
        );
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(
            loaded.entries[0].receipt_extensions, receipt_extensions,
            "PROPERTY: checkpoint v6 must preserve opaque receipt-extension bytes in the snapshot artifact."
        );
    }

    #[test]
    fn checkpoint_entry_to_cold_start_row_preserves_index_fields() {
        let entry = CheckpointEntry {
            event_id: 0xAA,
            correlation_id: 0xBB,
            causation_id: Some(0xCC),
            entity_id: 1,
            scope_id: 2,
            kind: EventKind::custom(0x2, 0x34),
            wall_ms: 1234,
            clock: 7,
            dag_lane: 3,
            dag_depth: 5,
            prev_hash: [0x11; 32],
            event_hash: [0x22; 32],
            segment_id: 9,
            offset: 256,
            length: 64,
            global_sequence: 42,
            receipt_extensions: BTreeMap::new(),
        };
        let strings = vec![
            String::new(),
            "entity:checkpoint".to_owned(),
            "scope:test".to_owned(),
        ];

        let rebuilt = entry
            .to_cold_start_row()
            .to_index_entry(&strings)
            .expect("checkpoint row to index entry");

        assert_eq!(rebuilt.event_id, entry.event_id);
        assert_eq!(rebuilt.correlation_id, entry.correlation_id);
        assert_eq!(rebuilt.causation_id, entry.causation_id);
        assert_eq!(rebuilt.coord.entity(), "entity:checkpoint");
        assert_eq!(rebuilt.coord.scope(), "scope:test");
        assert_eq!(rebuilt.kind, entry.kind);
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
    fn checkpoint_entry_preserves_none_causation_in_cold_start_row() {
        let entry = CheckpointEntry {
            event_id: 1,
            correlation_id: 2,
            causation_id: None,
            entity_id: 1,
            scope_id: 2,
            kind: EventKind::DATA,
            wall_ms: 10,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            prev_hash: [0; 32],
            event_hash: [1; 32],
            segment_id: 3,
            offset: 4,
            length: 5,
            global_sequence: 6,
            receipt_extensions: BTreeMap::new(),
        };

        assert_eq!(entry.to_cold_start_row().causation_id, None);
    }

    #[test]
    fn restore_rebuilds_index() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let src = make_index(8);
        write_checkpoint(&src, dir, 0, 0).expect("write");

        let loaded = try_load_checkpoint(dir).expect("should load");
        let entries = loaded.entries;
        let interner_strings = loaded.interner_strings;
        let stored_alloc = loaded.stored_allocator;

        let dst = StoreIndex::new();
        restore_from_checkpoint(&dst, entries, &interner_strings, stored_alloc).expect("restore");

        assert_eq!(dst.len(), 8);
    }

    #[test]
    fn missing_file_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(
            try_load_checkpoint(tmp.path()).is_none(),
            "missing file should return None"
        );
    }

    #[test]
    fn bad_magic_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join(CHECKPOINT_FILENAME);
        std::fs::write(&path, b"BADMAGIC\x00\x00\x00\x00").expect("write");
        assert!(
            try_load_checkpoint(tmp.path()).is_none(),
            "bad magic should return None"
        );
    }

    #[test]
    fn crc_mismatch_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = make_index(4);
        write_checkpoint(&idx, dir, 0, 0).expect("write");

        // Corrupt the last byte of the file
        let path = dir.join(CHECKPOINT_FILENAME);
        let mut raw = std::fs::read(&path).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        std::fs::write(&path, &raw).expect("rewrite");

        assert!(
            try_load_checkpoint(dir).is_none(),
            "CRC mismatch should return None"
        );
    }

    #[test]
    fn missing_watermark_segment_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        // Write checkpoint referencing segment 99, but do NOT create that file.
        touch_segment(dir, 0); // segment 0 exists but 99 does not

        let idx = make_index(2);
        write_checkpoint(&idx, dir, 99, 0).expect("write");

        assert!(
            try_load_checkpoint(dir).is_none(),
            "missing watermark segment should return None"
        );
    }

    #[test]
    fn wrong_version_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = StoreIndex::new();
        write_checkpoint(&idx, dir, 0, 0).expect("write");

        // Overwrite the two version bytes with an unsupported future version
        let path = dir.join(CHECKPOINT_FILENAME);
        let mut raw = std::fs::read(&path).expect("read");
        // bytes [6..8] are the version — set to 99
        raw[6] = 99;
        raw[7] = 0;
        // Also fix the CRC so it doesn't fail there first
        let body_crc = crc32fast::hash(&raw[12..]);
        let crc_bytes = body_crc.to_le_bytes();
        raw[8] = crc_bytes[0];
        raw[9] = crc_bytes[1];
        raw[10] = crc_bytes[2];
        raw[11] = crc_bytes[3];
        std::fs::write(&path, &raw).expect("rewrite");

        assert!(
            try_load_checkpoint(dir).is_none(),
            "wrong version should return None"
        );
    }

    #[test]
    fn v2_checkpoint_fallback_is_still_readable() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = make_index(6);
        let mut entries: Vec<CheckpointEntry> = idx
            .all_entries()
            .into_iter()
            .map(|e| CheckpointEntry {
                event_id: e.event_id,
                correlation_id: e.correlation_id,
                causation_id: e.causation_id,
                entity_id: e.entity_id.as_u32(),
                scope_id: e.scope_id.as_u32(),
                kind: e.kind,
                wall_ms: e.wall_ms,
                clock: e.clock,
                dag_lane: e.dag_lane,
                dag_depth: e.dag_depth,
                prev_hash: e.hash_chain.prev_hash,
                event_hash: e.hash_chain.event_hash,
                segment_id: e.disk_pos.segment_id,
                offset: e.disk_pos.offset,
                length: e.disk_pos.length,
                global_sequence: e.global_sequence,
                receipt_extensions: BTreeMap::new(),
            })
            .collect();
        entries.sort_by_key(|entry| entry.global_sequence);
        let mut interner_strings = vec![String::new()];
        interner_strings.extend(idx.interner.to_snapshot());
        let body = rmp_serde::to_vec_named(&CheckpointDataV2 {
            global_sequence: idx.global_sequence(),
            watermark_segment_id: 0,
            watermark_offset: 0,
            interner_strings,
            entries,
        })
        .expect("serialize v2 checkpoint");
        let crc = crc32fast::hash(&body);
        let path = dir.join(CHECKPOINT_FILENAME);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&body);
        std::fs::write(&path, bytes).expect("write v2 checkpoint");

        let loaded = try_load_checkpoint(dir).expect("load v2 checkpoint");
        assert_eq!(loaded.entries.len(), 6);
        assert_eq!(loaded.routing.entry_count, 6);
        assert!(
            !loaded.routing.chunks.is_empty(),
            "v2 fallback should synthesize chunk summaries on load"
        );
    }

    #[test]
    fn v3_checkpoint_defaults_lane_depth_to_zero() {
        #[derive(Serialize)]
        struct LegacyCheckpointEntryV3 {
            #[serde(with = "crate::wire::u128_bytes")]
            event_id: u128,
            #[serde(with = "crate::wire::u128_bytes")]
            correlation_id: u128,
            #[serde(with = "crate::wire::option_u128_bytes")]
            causation_id: Option<u128>,
            entity_id: u32,
            scope_id: u32,
            kind: EventKind,
            wall_ms: u64,
            clock: u32,
            prev_hash: [u8; 32],
            event_hash: [u8; 32],
            segment_id: u64,
            offset: u64,
            length: u32,
            global_sequence: u64,
        }

        #[derive(Serialize)]
        struct LegacyCheckpointDataV3 {
            global_sequence: u64,
            watermark_segment_id: u64,
            watermark_offset: u64,
            interner_strings: Vec<String>,
            routing: RoutingSummary,
            entries: Vec<LegacyCheckpointEntryV3>,
        }

        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let idx = make_index(4);
        let mut legacy_entries: Vec<LegacyCheckpointEntryV3> = idx
            .all_entries()
            .into_iter()
            .map(|e| LegacyCheckpointEntryV3 {
                event_id: e.event_id,
                correlation_id: e.correlation_id,
                causation_id: e.causation_id,
                entity_id: e.entity_id.as_u32(),
                scope_id: e.scope_id.as_u32(),
                kind: e.kind,
                wall_ms: e.wall_ms,
                clock: e.clock,
                prev_hash: e.hash_chain.prev_hash,
                event_hash: e.hash_chain.event_hash,
                segment_id: e.disk_pos.segment_id,
                offset: e.disk_pos.offset,
                length: e.disk_pos.length,
                global_sequence: e.global_sequence,
            })
            .collect();
        legacy_entries.sort_by_key(|entry| entry.global_sequence);
        let mut interner_strings = vec![String::new()];
        interner_strings.extend(idx.interner.to_snapshot());
        let mut sorted_entries = idx.all_entries();
        sorted_entries.sort_by_key(|entry| entry.global_sequence);
        let routing = RoutingSummary::from_sorted_entries(
            &sorted_entries,
            recommended_restore_chunk_count(sorted_entries.len()),
        );
        let body = rmp_serde::to_vec_named(&LegacyCheckpointDataV3 {
            global_sequence: idx.global_sequence(),
            watermark_segment_id: 0,
            watermark_offset: 0,
            interner_strings,
            routing,
            entries: legacy_entries,
        })
        .expect("serialize v3 checkpoint");
        let crc = crc32fast::hash(&body);
        let path = dir.join(CHECKPOINT_FILENAME);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&body);
        std::fs::write(&path, bytes).expect("write v3 checkpoint");

        let loaded = try_load_checkpoint_snapshot(dir).expect("load v3 checkpoint snapshot");
        assert!(loaded.entries.iter().all(|entry| entry.dag_lane == 0));
        assert!(loaded.entries.iter().all(|entry| entry.dag_depth == 0));
    }

    #[test]
    fn restore_advances_global_sequence() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let src = make_index(16);
        write_checkpoint(&src, dir, 0, 0).expect("write");

        let loaded = try_load_checkpoint(dir).expect("should load");
        let entries = loaded.entries;
        let interner_strings = loaded.interner_strings;
        let stored_alloc = loaded.stored_allocator;
        assert_eq!(entries.len(), 16);

        let dst = StoreIndex::new();
        restore_from_checkpoint(&dst, entries, &interner_strings, stored_alloc).expect("restore");

        // After restoring 16 entries, global_sequence should be 16
        // (each insert() call increments the counter by 1).
        assert_eq!(
            dst.global_sequence(),
            16,
            "PROPERTY: global_sequence after restore must equal the number of restored entries."
        );
        // Visibility watermark must also advance to 16 (restore_from_checkpoint
        // calls publish(global_sequence()) at the end).
        assert_eq!(
            dst.visible_sequence(),
            16,
            "PROPERTY: visible_sequence after restore must equal global_sequence."
        );
    }

    #[test]
    fn to_cold_start_row_normalizes_zero_causation() {
        let entry = CheckpointEntry {
            event_id: 1,
            correlation_id: 1,
            causation_id: Some(0),
            entity_id: 0,
            scope_id: 0,
            kind: EventKind::custom(0x1, 1),
            wall_ms: 1_700_000_000_000,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            prev_hash: [0u8; 32],
            event_hash: [0u8; 32],
            segment_id: 0,
            offset: 0,
            length: 64,
            global_sequence: 0,
            receipt_extensions: BTreeMap::new(),
        };
        let row = entry.to_cold_start_row();
        assert_eq!(
            row.causation_id, None,
            "INVARIANT: Some(0) causation in checkpoint must normalize to None on restore"
        );
    }

    #[test]
    fn to_cold_start_row_preserves_nonzero_causation() {
        let entry = CheckpointEntry {
            event_id: 2,
            correlation_id: 1,
            causation_id: Some(99),
            entity_id: 0,
            scope_id: 0,
            kind: EventKind::custom(0x1, 1),
            wall_ms: 1_700_000_000_000,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            prev_hash: [0u8; 32],
            event_hash: [0u8; 32],
            segment_id: 0,
            offset: 0,
            length: 64,
            global_sequence: 1,
            receipt_extensions: BTreeMap::new(),
        };
        let row = entry.to_cold_start_row();
        assert_eq!(row.causation_id, Some(99));
    }
}
