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
//! [version: u16 LE]    — v2/v3/v4/v5 fallback or v6 current
//! [crc32: u32 LE]      — CRC32 of the msgpack body that follows
//! [msgpack body]        — versioned checkpoint body serialised via rmp_serde
//! ```
//!
//! The magic + version occupy the first 8 bytes; the 4-byte CRC immediately follows;
//! the variable-length msgpack body fills the rest of the file.

mod format;
mod snapshot;
mod write;

pub(crate) use format::CHECKPOINT_FILENAME;
pub(crate) use snapshot::load_checkpoint_snapshot;
#[cfg(test)]
pub(crate) use snapshot::try_load_checkpoint;
#[cfg(test)]
pub(crate) use snapshot::try_load_checkpoint_snapshot;
#[cfg(test)]
pub(crate) use write::write_checkpoint;
pub(crate) use write::write_checkpoint_with_reserved_kind_fallbacks;

use crate::event::{EventKind, HashChain};
use crate::store::cold_start::{ColdStartIndexRow, ColdStartSource};
use crate::store::index::interner::InternId;
#[cfg(test)]
use crate::store::index::StoreIndex;
use crate::store::index::{DiskPos, IndexEntry};
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

fn checkpoint_entries_to_index_entries(
    entries: &[CheckpointEntry],
    interner_strings: &[String],
) -> Result<Vec<IndexEntry>, StoreError> {
    entries
        .iter()
        .map(|ce| ce.to_index_entry(interner_strings))
        .collect()
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
