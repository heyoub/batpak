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
//! [version: u16 LE]    — must equal CHECKPOINT_VERSION (1)
//! [crc32: u32 LE]      — CRC32 of the msgpack body that follows
//! [msgpack body]        — CheckpointData serialised via rmp_serde::to_vec_named
//! ```
//!
//! The magic + version occupy the first 8 bytes; the 4-byte CRC immediately follows;
//! the variable-length msgpack body fills the rest of the file.

use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::index::{DiskPos, IndexEntry, StoreIndex};
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;

// ── Constants ────────────────────────────────────────────────────────────────

/// Magic bytes at the start of every checkpoint file.
pub(crate) const CHECKPOINT_MAGIC: &[u8; 6] = b"FBATCK";

/// Format version stored in the checkpoint header.
/// v2: interner snapshot + InternId entries (smaller, faster restore).
/// v1 checkpoints are rejected — graceful fallback to full rebuild.
pub(crate) const CHECKPOINT_VERSION: u16 = 2;

/// Final checkpoint filename inside the data directory.
pub(crate) const CHECKPOINT_FILENAME: &str = "index.ckpt";

/// Temporary filename used during an atomic write-then-rename.
pub(crate) const CHECKPOINT_TMP: &str = "index.ckpt.tmp";

// ── Wire types ───────────────────────────────────────────────────────────────

/// Checkpoint format v2: includes interner snapshot + InternId-based entries.
#[derive(Serialize, Deserialize)]
struct CheckpointData {
    global_sequence: u64,
    watermark_segment_id: u64,
    watermark_offset: u64,
    /// Interner snapshot: ordered list of interned strings (index = InternId).
    /// The sentinel (empty string at index 0) is included.
    interner_strings: Vec<String>,
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
    pub prev_hash: [u8; 32],
    pub event_hash: [u8; 32],
    pub segment_id: u64,
    pub offset: u64,
    pub length: u32,
    pub global_sequence: u64,
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

// ── write_checkpoint ─────────────────────────────────────────────────────────

/// Serialise the entire in-memory index to `<data_dir>/index.ckpt`.
///
/// The write is atomic: data is first written to `<data_dir>/index.ckpt.tmp`,
/// fsynced, then renamed over the final path.  A partial write caused by a
/// crash therefore never corrupts the previous good checkpoint.
///
/// # Errors
///
/// Returns [`StoreError::Serialization`] if msgpack serialisation fails.
/// Returns [`StoreError::Io`] if any filesystem operation (open, write, fsync,
/// rename) fails.
pub(crate) fn write_checkpoint(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
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
            prev_hash: e.hash_chain.prev_hash,
            event_hash: e.hash_chain.event_hash,
            segment_id: e.disk_pos.segment_id,
            offset: e.disk_pos.offset,
            length: e.disk_pos.length,
            global_sequence: e.global_sequence,
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

    let data = CheckpointData {
        global_sequence: index.global_sequence(),
        watermark_segment_id,
        watermark_offset,
        interner_strings,
        entries,
    };

    // ── 3. Serialise to msgpack ───────────────────────────────────────────────
    let body = rmp_serde::to_vec_named(&data)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;

    // ── 4. Compute CRC of the body ────────────────────────────────────────────
    let crc: u32 = crc32fast::hash(&body);

    // ── 5. Write to .tmp with fsync ───────────────────────────────────────────
    let tmp_path = data_dir.join(CHECKPOINT_TMP);
    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        let mut w = BufWriter::new(&file);

        // Header: MAGIC (6) + version (2 LE) + crc (4 LE)
        w.write_all(CHECKPOINT_MAGIC)?;
        w.write_all(&CHECKPOINT_VERSION.to_le_bytes())?;
        w.write_all(&crc.to_le_bytes())?;
        w.write_all(&body)?;
        w.flush()?;

        // fsync before rename so the data reaches stable storage.
        file.sync_all()?;
    }

    // ── 6. Atomic rename to final name ────────────────────────────────────────
    let final_path = data_dir.join(CHECKPOINT_FILENAME);
    std::fs::rename(&tmp_path, &final_path)?;

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
/// On success returns `(entries, interner_strings, WatermarkInfo)` where entries
/// are sorted ascending by `global_sequence` and ready to be passed to
/// [`restore_from_checkpoint`] along with the interner strings table.
pub(crate) fn try_load_checkpoint(
    data_dir: &Path,
) -> Option<(Vec<CheckpointEntry>, Vec<String>, WatermarkInfo)> {
    let path = data_dir.join(CHECKPOINT_FILENAME);

    // ── 1. Read raw bytes ─────────────────────────────────────────────────────
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Normal on first start — no warning needed.
            return None;
        }
        Err(e) => {
            tracing::warn!(
                target: "batpak::checkpoint",
                path = %path.display(),
                error = %e,
                "failed to read checkpoint file"
            );
            return None;
        }
    };

    // ── 2. Validate header length: 6 (magic) + 2 (version) + 4 (crc) = 12 ───
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

    // ── 3. Verify magic ───────────────────────────────────────────────────────
    if &raw[..6] != CHECKPOINT_MAGIC.as_ref() {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            "checkpoint file has wrong magic bytes — ignoring"
        );
        return None;
    }

    // ── 4. Verify version ─────────────────────────────────────────────────────
    let version = u16::from_le_bytes([raw[6], raw[7]]);
    if version != CHECKPOINT_VERSION {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            version,
            expected = CHECKPOINT_VERSION,
            "unsupported checkpoint version — ignoring"
        );
        return None;
    }

    // ── 5. Verify CRC ─────────────────────────────────────────────────────────
    let stored_crc = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let body = &raw[HEADER_LEN..];
    let computed_crc = crc32fast::hash(body);
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

    // ── 6. Deserialise msgpack body ───────────────────────────────────────────
    let data: CheckpointData = match rmp_serde::from_slice(body) {
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

    // ── 7. Cross-check: watermark segment file must exist on disk ─────────────
    // Format: "{segment_id:06}.fbat"  (mirrors SEGMENT_EXTENSION in segment.rs)
    let seg_filename = format!(
        "{:06}.{}",
        data.watermark_segment_id,
        crate::store::segment::SEGMENT_EXTENSION
    );
    let seg_path = data_dir.join(&seg_filename);
    if !seg_path.exists() {
        tracing::warn!(
            target: "batpak::checkpoint",
            path = %path.display(),
            missing_segment = %seg_path.display(),
            "watermark segment referenced by checkpoint is missing — ignoring checkpoint"
        );
        return None;
    }

    let watermark = WatermarkInfo {
        watermark_segment_id: data.watermark_segment_id,
        watermark_offset: data.watermark_offset,
    };

    tracing::debug!(
        target: "batpak::checkpoint",
        entries = data.entries.len(),
        global_sequence = data.global_sequence,
        watermark_segment_id = data.watermark_segment_id,
        watermark_offset = data.watermark_offset,
        "checkpoint loaded successfully"
    );

    Some((data.entries, data.interner_strings, watermark))
}

// ── restore_from_checkpoint ───────────────────────────────────────────────────

/// Replay checkpoint entries into `index`, using the interner strings table
/// to resolve `entity_id` and `scope_id` back to string values.
///
/// Entries must be sorted ascending by `global_sequence` (which
/// [`write_checkpoint`] guarantees).
///
/// # Errors
///
/// Returns [`StoreError::Coordinate`] if resolved strings are empty.
/// Returns [`StoreError::Serialization`] if an InternId is out of range.
pub(crate) fn restore_from_checkpoint(
    index: &StoreIndex,
    entries: Vec<CheckpointEntry>,
    interner_strings: &[String],
) -> Result<(), StoreError> {
    for ce in entries {
        let entity_str = interner_strings
            .get(ce.entity_id as usize)
            .ok_or_else(|| StoreError::ser_msg("checkpoint entity_id out of interner range"))?;
        let scope_str = interner_strings
            .get(ce.scope_id as usize)
            .ok_or_else(|| StoreError::ser_msg("checkpoint scope_id out of interner range"))?;

        let coord = Coordinate::new(entity_str, scope_str)?;
        let entity_id = index.interner.intern(entity_str);
        let scope_id = index.interner.intern(scope_str);

        let entry = IndexEntry {
            event_id: ce.event_id,
            correlation_id: ce.correlation_id,
            causation_id: ce.causation_id,
            coord,
            entity_id,
            scope_id,
            kind: ce.kind,
            wall_ms: ce.wall_ms,
            clock: ce.clock,
            hash_chain: HashChain {
                prev_hash: ce.prev_hash,
                event_hash: ce.event_hash,
            },
            disk_pos: DiskPos {
                segment_id: ce.segment_id,
                offset: ce.offset,
                length: ce.length,
            },
            global_sequence: ce.global_sequence,
        };

        index.insert(entry);
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::index::StoreIndex;
    use tempfile::TempDir;

    /// Build a minimal populated StoreIndex with `n` synthetic entries.
    fn make_index(n: u64) -> StoreIndex {
        let idx = StoreIndex::new();
        for i in 0..n {
            let coord = Coordinate::new(
                format!("entity:{i}"),
                "test-scope",
            )
            .expect("valid coordinate");
            let entity_id = idx.interner.intern(coord.entity());
            let scope_id = idx.interner.intern(coord.scope());
            let entry = IndexEntry {
                event_id: (i + 1) as u128,
                correlation_id: (i + 1) as u128,
                causation_id: if i == 0 { None } else { Some(i as u128) },
                coord,
                entity_id,
                scope_id,
                kind: EventKind::custom(0x1, i as u16 & 0x0FFF),
                wall_ms: 1_700_000_000_000 + i * 1000,
                clock: u32::try_from(i).expect("i fits u32"),
                hash_chain: HashChain::default(),
                disk_pos: DiskPos {
                    segment_id: 0,
                    offset: i * 256,
                    length: 256,
                },
                global_sequence: i,
            };
            idx.insert(entry);
        }
        idx
    }

    /// Create a dummy segment file so the watermark cross-check passes.
    fn touch_segment(dir: &Path, segment_id: u64) {
        let name = format!("{segment_id:06}.fbat");
        std::fs::write(dir.join(name), b"dummy").expect("write dummy segment");
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

        let (entries, _strings, wm) = result.expect("some");
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

        let (entries, _strings, wm) = try_load_checkpoint(dir).expect("should load");
        assert_eq!(entries.len(), 16);
        assert_eq!(wm.watermark_offset, 4096);

        // Verify sort order
        let seqs: Vec<u64> = entries.iter().map(|e| e.global_sequence).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "entries must be sorted by global_sequence");
    }

    #[test]
    fn restore_rebuilds_index() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        touch_segment(dir, 0);

        let src = make_index(8);
        write_checkpoint(&src, dir, 0, 0).expect("write");

        let (entries, interner_strings, _wm) = try_load_checkpoint(dir).expect("should load");

        let dst = StoreIndex::new();
        restore_from_checkpoint(&dst, entries, &interner_strings).expect("restore");

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
}
