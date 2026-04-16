//! SIDX — Segment InDeX footer for fast cold-start index rebuild.
//!
//! A SIDX footer is appended to a **sealed** segment file immediately after all event
//! frames have been written. On the next cold start, the store can seek to the last 16
//! bytes of each segment, detect the `b"SDX2"` magic, and reconstruct the in-memory
//! index without re-deserialising every MessagePack frame.
//!
//! # On-disk layout (end of segment file)
//!
//! ```text
//! [...frames...]
//! [string_table_bytes]           — msgpack-encoded Vec<String> (entity + scope names)
//! [entries: N × ENTRY_SIZE]      — raw little-endian binary, no framing, no CRC
//! [string_table_offset: u64 LE]  — byte offset from segment start where the table begins
//! [entry_count: u32 LE]          — number of SidxEntry records
//! [magic: b"SDX2"]               — 4 bytes; last bytes of the file
//! ```
//!
//! To read: seek to `EOF - 16`, read `magic(4) + entry_count(4) + string_table_offset(8)`.
//! Then seek to `string_table_offset` and read the string table, then the entry block.
//!
//! # Entry binary layout (162 bytes per entry, little-endian)
//!
//! | Field           | Bytes | Notes                               |
//! |-----------------|-------|-------------------------------------|
//! | event_id        | 16    | u128 LE                             |
//! | entity_idx      | 4     | u32 LE — index into string table    |
//! | scope_idx       | 4     | u32 LE — index into string table    |
//! | kind            | 2     | u16 LE — EventKind raw value        |
//! | wall_ms         | 8     | u64 LE                              |
//! | clock           | 4     | u32 LE                              |
//! | dag_lane        | 4     | u32 LE                              |
//! | dag_depth       | 4     | u32 LE                              |
//! | prev_hash       | 32    | as-is bytes                         |
//! | event_hash      | 32    | as-is bytes                         |
//! | frame_offset    | 8     | u64 LE                              |
//! | frame_length    | 4     | u32 LE                              |
//! | global_sequence | 8     | u64 LE                              |
//! | correlation_id  | 16    | u128 LE                             |
//! | causation_id    | 16    | u128 LE; 0 = no causation           |
//! | **Total**       | **162** |                                   |

use crate::event::EventKind;
use crate::store::StoreError;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

// ── constants ─────────────────────────────────────────────────────────────────

/// Four-byte magic that identifies a SIDX footer at the tail of a segment file.
pub(crate) const SIDX_MAGIC: &[u8; 4] = b"SDX2";

/// Size of the fixed-layout trailer that terminates the SIDX footer:
/// `string_table_offset(8) + entry_count(4) + magic(4)` = 16 bytes.
const TRAILER_SIZE: u64 = 16;

/// Fixed byte size of one serialised [`SidxEntry`] on disk.
///
/// Breakdown:
/// - event_id(16) + entity_idx(4) + scope_idx(4) + kind(2) = 26
/// - wall_ms(8) + clock(4) + dag_lane(4) + dag_depth(4) = 20 → 46
/// - prev_hash(32) + event_hash(32) = 64 → 110
/// - frame_offset(8) + frame_length(4) + global_sequence(8) = 20 → 130
/// - correlation_id(16) + causation_id(16) = 32 → **162**
pub(crate) const ENTRY_SIZE: usize = 162;

const _ASSERT_ENTRY_SIZE: () = {
    // Compile-time sanity: update this constant whenever SidxEntry fields change.
    assert!(
        ENTRY_SIZE == 162,
        "ENTRY_SIZE must equal 162 — update when SidxEntry layout changes"
    );
};

// ── EventKind helpers ─────────────────────────────────────────────────────────

/// Convert an [`EventKind`] to the raw `u16` used in the on-disk SIDX entry.
///
/// Reconstructs the packed value from the two public bit-field accessors,
/// mirroring `EventKind`'s internal `(category << 12) | type_id` encoding.
#[inline]
pub(crate) fn kind_to_raw(kind: EventKind) -> u16 {
    (u16::from(kind.category()) << 12) | kind.type_id()
}

/// Reconstruct an [`EventKind`] from its raw `u16` disk representation.
///
/// `EventKind::custom()` rejects the reserved categories `0x0` (system) and `0xD`
/// (effect) with a panic, so those are matched directly against the known library
/// constants. Any unrecognised value in a reserved range falls back to the closest
/// documented constant (system or effect root) so the index can still be rebuilt.
pub(crate) fn raw_to_kind(raw: u16) -> EventKind {
    let category = (raw >> 12) as u8;
    match category {
        // Reserved system category (0x0) — match known constants by full value.
        0x0 => match raw {
            0x0001 => EventKind::SYSTEM_INIT,
            0x0002 => EventKind::SYSTEM_SHUTDOWN,
            0x0003 => EventKind::SYSTEM_HEARTBEAT,
            0x0004 => EventKind::SYSTEM_CONFIG_CHANGE,
            0x0005 => EventKind::SYSTEM_CHECKPOINT,
            0x0FFE => EventKind::TOMBSTONE,
            // DATA (0x0000) and any unrecognised system kind → DATA
            _ => EventKind::DATA,
        },
        // Reserved effect category (0xD) — match known constants.
        0xD => match raw {
            0xD001 => EventKind::EFFECT_ERROR,
            0xD002 => EventKind::EFFECT_RETRY,
            0xD004 => EventKind::EFFECT_ACK,
            0xD005 => EventKind::EFFECT_BACKPRESSURE,
            0xD006 => EventKind::EFFECT_CANCEL,
            0xD007 => EventKind::EFFECT_CONFLICT,
            // Any unrecognised effect kind → EFFECT_ERROR (closest semantic parent)
            _ => EventKind::EFFECT_ERROR,
        },
        // All other categories (0x1–0xC, 0xE–0xF) are open for product use.
        other => EventKind::custom(other, raw & 0x0FFF),
    }
}

// ── SidxEntry ─────────────────────────────────────────────────────────────────

/// A single index record corresponding to one event in a sealed segment.
///
/// Stored as packed little-endian binary — no serde, no framing, no CRC.
/// Entity and scope strings are resolved through the companion string table
/// kept in [`SidxEntryCollector`] and written by [`SidxEntryCollector::write_footer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SidxEntry {
    /// 128-bit globally unique event identifier.
    pub event_id: u128,
    /// Index into the segment's string table for the entity name.
    pub entity_idx: u32,
    /// Index into the segment's string table for the scope name.
    pub scope_idx: u32,
    /// Raw [`EventKind`] discriminant: upper 4 bits = category, lower 12 = type id.
    /// Use [`kind_to_raw`] to produce and [`raw_to_kind`] to consume this field.
    pub kind: u16,
    /// HLC wall-clock milliseconds at commit time.
    pub wall_ms: u64,
    /// Per-entity monotonic sequence number at commit time.
    pub clock: u32,
    /// Branch lane within the logical event DAG.
    pub dag_lane: u32,
    /// Branch depth within the logical event DAG.
    pub dag_depth: u32,
    /// Blake3 hash of the immediately preceding event in this entity's chain.
    /// All-zeros signals genesis (no predecessor).
    pub prev_hash: [u8; 32],
    /// Blake3 hash of this event's serialised content bytes.
    pub event_hash: [u8; 32],
    /// Byte offset of this event's frame within the segment file.
    pub frame_offset: u64,
    /// Byte length of the encoded frame (header + CRC + msgpack).
    pub frame_length: u32,
    /// Globally monotonic sequence number assigned by the writer at commit time.
    pub global_sequence: u64,
    /// Correlation identifier grouping related events into a single causal saga.
    pub correlation_id: u128,
    /// Identifier of the event that directly caused this one; `0` means root cause.
    pub causation_id: u128,
}

impl SidxEntry {
    /// Serialise this entry into `buf`, which must be exactly [`ENTRY_SIZE`] bytes.
    ///
    /// All multi-byte integers are written in little-endian byte order.
    /// Hash fields are copied as-is (byte arrays have no endianness).
    pub(crate) fn encode_into(&self, buf: &mut [u8]) {
        debug_assert_eq!(
            buf.len(),
            ENTRY_SIZE,
            "encode_into: buf must be ENTRY_SIZE bytes"
        );

        let mut pos = 0usize;

        // Helper: copy little-endian bytes of a primitive into buf at `pos`.
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
        put_le!(self.frame_offset, 8);
        put_le!(self.frame_length, 4);
        put_le!(self.global_sequence, 8);
        put_le!(self.correlation_id, 16);
        put_le!(self.causation_id, 16);

        debug_assert_eq!(pos, ENTRY_SIZE, "encode_into: wrote wrong byte count");
    }

    /// Deserialise an entry from `buf`, which must be exactly [`ENTRY_SIZE`] bytes.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptSegment`] if `buf` is not [`ENTRY_SIZE`] bytes long.
    pub(crate) fn decode_from(buf: &[u8], segment_id: u64) -> Result<Self, StoreError> {
        if buf.len() != ENTRY_SIZE {
            return Err(StoreError::CorruptSegment {
                segment_id,
                detail: format!(
                    "SIDX entry buffer is {} bytes, expected {ENTRY_SIZE}",
                    buf.len()
                ),
            });
        }

        let mut pos = 0usize;

        macro_rules! get_le {
            ($t:ty, $n:expr) => {{
                let arr: [u8; $n] = buf[pos..pos + $n]
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
        let dag_lane = get_le!(u32, 4);
        let dag_depth = get_le!(u32, 4);
        let prev_hash = get_hash!();
        let event_hash = get_hash!();
        let frame_offset = get_le!(u64, 8);
        let frame_length = get_le!(u32, 4);
        let global_sequence = get_le!(u64, 8);
        let correlation_id = get_le!(u128, 16);
        let causation_id = get_le!(u128, 16);

        debug_assert_eq!(pos, ENTRY_SIZE, "decode_from: consumed wrong byte count");

        Ok(Self {
            event_id,
            entity_idx,
            scope_idx,
            kind,
            wall_ms,
            clock,
            dag_lane,
            dag_depth,
            prev_hash,
            event_hash,
            frame_offset,
            frame_length,
            global_sequence,
            correlation_id,
            causation_id,
        })
    }

    /// Reconstruct the [`EventKind`] from the raw `kind` field stored in this entry.
    #[cfg(test)]
    pub(crate) fn event_kind(&self) -> EventKind {
        raw_to_kind(self.kind)
    }
}

// ── SidxEntryCollector ────────────────────────────────────────────────────────

/// Accumulates [`SidxEntry`] records and their associated entity/scope strings
/// during a segment write, then serialises the complete SIDX footer in one pass
/// when the segment is sealed.
///
/// Entity and scope strings are **interned**: each unique string is stored once
/// in the string table and referenced by index from every entry. This keeps the
/// footer compact even when many events share the same entity or scope.
pub(crate) struct SidxEntryCollector {
    /// Accumulated index entries in append order.
    entries: Vec<SidxEntry>,
    /// Deduplicated list of all entity and scope strings. Indices are stable after insertion.
    strings: Vec<String>,
    /// Reverse map from string content to its position in `strings`.
    string_map: HashMap<String, u32>,
}

impl SidxEntryCollector {
    /// Create an empty collector ready to accept entries.
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            strings: Vec::new(),
            string_map: HashMap::new(),
        }
    }

    /// Record one event's index data.
    ///
    /// The `entity_idx` and `scope_idx` fields of `entry` are overwritten with
    /// the interned indices for `entity` and `scope`. All other fields are
    /// copied verbatim from `entry`.
    pub(crate) fn record(&mut self, mut entry: SidxEntry, entity: &str, scope: &str) {
        entry.entity_idx = self.intern(entity);
        entry.scope_idx = self.intern(scope);
        self.entries.push(entry);
    }

    /// Return a shared reference to all entries collected so far.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> &[SidxEntry] {
        &self.entries
    }

    /// Return a shared reference to the interned string table.
    #[cfg(test)]
    pub(crate) fn strings(&self) -> &[String] {
        &self.strings
    }

    /// Write the SIDX footer immediately after the current write position of `writer`.
    ///
    /// The caller must ensure all event frames have been written before calling this.
    /// `writer` must implement both [`Write`] and [`Seek`].
    ///
    /// # Footer layout written
    ///
    /// ```text
    /// [string_table_bytes]          — msgpack-encoded Vec<String>
    /// [entries: N × ENTRY_SIZE]     — raw little-endian binary
    /// [string_table_offset: u64 LE] — byte offset where string_table_bytes starts
    /// [entry_count: u32 LE]
    /// [magic: b"SDX2"]
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Serialization`] if the string table cannot be encoded to msgpack.
    /// Returns [`StoreError::Io`] if any write or seek operation fails.
    pub(crate) fn write_footer<W: Write + Seek>(&self, writer: &mut W) -> Result<(), StoreError> {
        // 1. Encode string table to msgpack.
        let string_table_bytes = rmp_serde::to_vec_named(&self.strings)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;

        // 2. Record the file position where the string table will start.
        let string_table_offset = writer.stream_position().map_err(StoreError::Io)?;

        // 3. Write string table bytes.
        writer
            .write_all(&string_table_bytes)
            .map_err(StoreError::Io)?;

        // 4. Write all entries as packed little-endian binary.
        let mut buf = [0u8; ENTRY_SIZE];
        for entry in &self.entries {
            entry.encode_into(&mut buf);
            writer.write_all(&buf).map_err(StoreError::Io)?;
        }

        // 5. Write the 16-byte trailer: string_table_offset(8) + entry_count(4) + magic(4).
        writer
            .write_all(&string_table_offset.to_le_bytes())
            .map_err(StoreError::Io)?;

        // Saturate at u32::MAX — a single segment can never hold 4 billion events.
        let entry_count = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
        writer
            .write_all(&entry_count.to_le_bytes())
            .map_err(StoreError::Io)?;

        writer.write_all(SIDX_MAGIC).map_err(StoreError::Io)?;

        Ok(())
    }

    /// Intern `s` and return its index in the string table.
    ///
    /// If `s` already exists in the table, returns the existing index.
    /// Otherwise appends it and returns the new index.
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&idx) = self.string_map.get(s) {
            return idx;
        }
        // Saturating truncation: segments can never accumulate 4B unique strings.
        #[allow(clippy::cast_possible_truncation)]
        // string table bounded by segment size, always < u32::MAX
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        self.string_map.insert(s.to_owned(), idx);
        idx
    }
}

// ── read_footer ───────────────────────────────────────────────────────────────

/// Read the SIDX footer from a sealed segment file.
///
/// Returns `Ok(None)` when the file does not contain a SIDX footer — either
/// because it was written before SIDX was introduced, or because the file is
/// too small to hold the 16-byte trailer.
///
/// Returns `Ok(Some((entries, strings)))` on success. The `strings` vec is the
/// interned string table; use `strings[entry.entity_idx as usize]` and
/// `strings[entry.scope_idx as usize]` to resolve entity and scope names.
///
/// # Errors
///
/// Returns [`StoreError::Io`] if any seek or read operation fails.
/// Returns [`StoreError::Serialization`] if the msgpack string table cannot be decoded.
/// Returns [`StoreError::CorruptSegment`] if structural invariants are violated (e.g.
/// out-of-range offsets or string-table indices).
/// Parsed SIDX footer: entries + string table.
pub(crate) type SidxFooterData = (Vec<SidxEntry>, Vec<String>);

pub(crate) fn read_footer(path: &Path) -> Result<Option<SidxFooterData>, StoreError> {
    // Derive a segment_id for error messages from the filename ("000042.fbat" → 42).
    let segment_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let mut file = std::fs::File::open(path).map_err(StoreError::Io)?;

    // ── 1. Guard: file must be at least TRAILER_SIZE bytes ────────────────────
    let file_len = file.seek(SeekFrom::End(0)).map_err(StoreError::Io)?;
    if file_len < TRAILER_SIZE {
        return Ok(None);
    }

    // ── 2. Read the 16-byte trailer ───────────────────────────────────────────
    file.seek(SeekFrom::End(-(TRAILER_SIZE as i64)))
        .map_err(StoreError::Io)?;

    let mut trailer = [0u8; 16];
    file.read_exact(&mut trailer).map_err(StoreError::Io)?;

    // Last 4 bytes must be the SIDX magic; if not, this is a non-SIDX segment.
    if &trailer[12..16] != SIDX_MAGIC {
        return Ok(None);
    }

    let string_table_offset =
        u64::from_le_bytes(trailer[0..8].try_into().expect("slice is 8 bytes"));
    let entry_count =
        u32::from_le_bytes(trailer[8..12].try_into().expect("slice is 4 bytes")) as usize;

    // ── 3. Validate offsets before any further I/O ────────────────────────────
    // entries block occupies the ENTRY_SIZE × N bytes immediately before the trailer.
    let entries_block_len = (entry_count as u64)
        .checked_mul(ENTRY_SIZE as u64)
        .ok_or_else(|| StoreError::CorruptSegment {
            segment_id,
            detail: "SIDX entry_count × ENTRY_SIZE overflows u64".into(),
        })?;

    // entries_start = file_len - TRAILER_SIZE - entries_block_len
    let entries_start = file_len
        .checked_sub(TRAILER_SIZE)
        .and_then(|n| n.checked_sub(entries_block_len))
        .ok_or_else(|| StoreError::CorruptSegment {
            segment_id,
            detail: "SIDX entry block extends before the beginning of the file".into(),
        })?;

    if string_table_offset > entries_start {
        return Err(StoreError::CorruptSegment {
            segment_id,
            detail: format!(
                "SIDX string_table_offset {string_table_offset} is past entries_start {entries_start}"
            ),
        });
    }

    // string_table_len is the gap between the table start and the entry block start.
    let string_table_len = entries_start
        .checked_sub(string_table_offset)
        .ok_or_else(|| StoreError::CorruptSegment {
            segment_id,
            detail: "SIDX string table length underflows".into(),
        })?;

    // ── 4. Read and decode string table ───────────────────────────────────────
    file.seek(SeekFrom::Start(string_table_offset))
        .map_err(StoreError::Io)?;

    let table_len_usize =
        usize::try_from(string_table_len).map_err(|_| StoreError::CorruptSegment {
            segment_id,
            detail: format!("SIDX string table length {string_table_len} exceeds usize::MAX"),
        })?;
    let mut string_table_buf = vec![0u8; table_len_usize];
    file.read_exact(&mut string_table_buf)
        .map_err(StoreError::Io)?;

    let strings: Vec<String> = rmp_serde::from_slice(&string_table_buf)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;

    // ── 5. Read and decode entries ─────────────────────────────────────────────
    // After reading the string table we are positioned at entries_start.
    let mut entries = Vec::with_capacity(entry_count);
    let mut entry_buf = [0u8; ENTRY_SIZE];

    for i in 0..entry_count {
        file.read_exact(&mut entry_buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                StoreError::CorruptSegment {
                    segment_id,
                    detail: format!("SIDX: entry {i} truncated at EOF"),
                }
            } else {
                StoreError::Io(e)
            }
        })?;

        let entry = SidxEntry::decode_from(&entry_buf, segment_id)?;

        // Validate string-table index bounds.
        if entry.entity_idx as usize >= strings.len() {
            return Err(StoreError::CorruptSegment {
                segment_id,
                detail: format!(
                    "SIDX entry {i}: entity_idx {} out of range (table has {} strings)",
                    entry.entity_idx,
                    strings.len()
                ),
            });
        }
        if entry.scope_idx as usize >= strings.len() {
            return Err(StoreError::CorruptSegment {
                segment_id,
                detail: format!(
                    "SIDX entry {i}: scope_idx {} out of range (table has {} strings)",
                    entry.scope_idx,
                    strings.len()
                ),
            });
        }

        entries.push(entry);
    }

    Ok(Some((entries, strings)))
}

// ── tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::NamedTempFile;

    /// Construct a minimal [`SidxEntry`] with deterministic field values.
    /// `entity_idx` and `scope_idx` are left at 0; `record()` will overwrite them.
    fn sample_entry(n: u8) -> SidxEntry {
        SidxEntry {
            event_id: u128::from(n),
            entity_idx: 0,
            scope_idx: 0,
            kind: kind_to_raw(EventKind::custom(0x1, u16::from(n))),
            wall_ms: 1_000_000 + u64::from(n),
            clock: u32::from(n),
            dag_lane: u32::from(n % 3),
            dag_depth: u32::from(n % 5),
            prev_hash: [n; 32],
            event_hash: [n.wrapping_add(1); 32],
            frame_offset: u64::from(n) * 512,
            frame_length: 128,
            global_sequence: u64::from(n),
            correlation_id: u128::from(n),
            causation_id: 0,
        }
    }

    // The previous `entry_size_constant_matches_layout` test asserted that
    // a `Vec<u8>` you just created with length `ENTRY_SIZE` still has length
    // `ENTRY_SIZE` after `encode_into` writes in-place. That's a tautology —
    // a `Vec<u8>` cannot change length under an in-place writer. The
    // compile-time `_ASSERT_ENTRY_SIZE` const at the top of this file already
    // covers the layout invariant. Test deleted in the Tier 1 drill sweep.

    // ── encode / decode round-trip ─────────────────────────────────────────────

    #[test]
    fn encode_decode_round_trip() {
        let original = SidxEntry {
            event_id: 0xDEAD_BEEF_CAFE_1234_5678_9ABC_DEF0_1234_u128,
            entity_idx: 7,
            scope_idx: 3,
            kind: 0xF042,
            wall_ms: 1_700_000_000_000,
            clock: 99,
            dag_lane: 4,
            dag_depth: 2,
            prev_hash: [0xAB; 32],
            event_hash: [0xCD; 32],
            frame_offset: 0x0000_1234_5678_9ABC,
            frame_length: 4096,
            global_sequence: 0xFFFF_FFFF_0000_0001,
            correlation_id: 0x1111_1111_2222_2222_3333_3333_4444_4444_u128,
            causation_id: 0,
        };

        let mut buf = [0u8; ENTRY_SIZE];
        original.encode_into(&mut buf);
        let decoded = SidxEntry::decode_from(&buf, 1).expect("decode must succeed");
        assert_eq!(original, decoded, "round-trip must be lossless");
    }

    // ── kind_to_raw / raw_to_kind / event_kind round-trip ────────────────────

    #[test]
    fn kind_round_trip_product_kind() {
        let kind = EventKind::custom(0x5, 0x042);
        let raw = kind_to_raw(kind);
        let recovered = raw_to_kind(raw);
        assert_eq!(recovered.category(), kind.category());
        assert_eq!(recovered.type_id(), kind.type_id());
    }

    #[test]
    fn kind_round_trip_system_constants() {
        for &kind in &[
            EventKind::SYSTEM_INIT,
            EventKind::SYSTEM_SHUTDOWN,
            EventKind::SYSTEM_HEARTBEAT,
            EventKind::SYSTEM_CONFIG_CHANGE,
            EventKind::SYSTEM_CHECKPOINT,
            EventKind::TOMBSTONE,
            EventKind::DATA,
        ] {
            let recovered = raw_to_kind(kind_to_raw(kind));
            assert_eq!(
                kind_to_raw(recovered),
                kind_to_raw(kind),
                "system kind round-trip failed for raw value {:#06x}",
                kind_to_raw(kind)
            );
        }
    }

    #[test]
    fn kind_round_trip_effect_constants() {
        for &kind in &[
            EventKind::EFFECT_ERROR,
            EventKind::EFFECT_RETRY,
            EventKind::EFFECT_ACK,
            EventKind::EFFECT_BACKPRESSURE,
            EventKind::EFFECT_CANCEL,
            EventKind::EFFECT_CONFLICT,
        ] {
            let recovered = raw_to_kind(kind_to_raw(kind));
            assert_eq!(
                kind_to_raw(recovered),
                kind_to_raw(kind),
                "effect kind round-trip failed for raw value {:#06x}",
                kind_to_raw(kind)
            );
        }
    }

    #[test]
    fn event_kind_helper_matches_raw_to_kind() {
        let entry = SidxEntry {
            kind: kind_to_raw(EventKind::custom(0x3, 0x7)),
            ..sample_entry(0)
        };
        let via_helper = entry.event_kind();
        let via_fn = raw_to_kind(entry.kind);
        assert_eq!(kind_to_raw(via_helper), kind_to_raw(via_fn));
    }

    // ── intern deduplicates strings ───────────────────────────────────────────

    #[test]
    fn intern_deduplicates_strings() {
        let mut collector = SidxEntryCollector::new();
        let i0 = collector.intern("entity:1");
        let i1 = collector.intern("scope:default");
        let i2 = collector.intern("entity:1");
        assert_eq!(i0, i2, "same string must return the same index");
        assert_ne!(i0, i1, "different strings must get different indices");
        assert_eq!(
            collector.strings().len(),
            2,
            "only 2 unique strings expected"
        );
    }

    // ── write_footer / read_footer round-trip ─────────────────────────────────

    #[test]
    fn footer_round_trip() {
        // Simulate a segment: write dummy frame bytes, then append the SIDX footer.
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"FBAT"); // pretend segment magic
        buf.extend_from_slice(&[0u8; 60]); // pretend frames

        let mut cursor = Cursor::new(&mut buf);
        cursor.seek(SeekFrom::End(0)).expect("seek to end");

        let mut collector = SidxEntryCollector::new();
        collector.record(sample_entry(1), "user:1", "profile");
        collector.record(sample_entry(2), "user:2", "profile");

        collector
            .write_footer(&mut cursor)
            .expect("write_footer must succeed");

        // Persist to a temporary file and read back.
        let mut tmp = NamedTempFile::new().expect("create temp file");
        tmp.write_all(&buf).expect("write buf to temp file");
        tmp.flush().expect("flush temp file");

        let (entries, strings) = read_footer(tmp.path())
            .expect("read_footer must not error")
            .expect("SIDX footer must be found");

        assert_eq!(entries.len(), 2, "expected 2 entries");
        assert!(strings.contains(&"user:1".to_owned()));
        assert!(strings.contains(&"user:2".to_owned()));
        assert!(strings.contains(&"profile".to_owned()));

        let e0_entity = &strings[entries[0].entity_idx as usize];
        let e1_entity = &strings[entries[1].entity_idx as usize];
        assert_eq!(e0_entity, "user:1");
        assert_eq!(e1_entity, "user:2");

        // Both entries share the same scope string index.
        assert_eq!(
            entries[0].scope_idx, entries[1].scope_idx,
            "shared scope must use the same string table index"
        );
    }

    // ── read_footer returns None when no SIDX magic ───────────────────────────

    #[test]
    fn read_footer_returns_none_without_magic() {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        // Write enough bytes to pass the size guard but with no SIDX magic.
        tmp.write_all(b"FBAT\x00\x00\x00\x00some bytes that are not a sidx footer at all")
            .expect("write");
        tmp.flush().expect("flush");
        let result = read_footer(tmp.path()).expect("must not IO-error");
        assert!(result.is_none(), "non-SIDX file must return None");
    }

    #[test]
    fn read_footer_returns_none_for_old_sidx_magic() {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        tmp.write_all(&[0u8; 12]).expect("write prefix");
        tmp.write_all(b"SIDX").expect("write old magic");
        tmp.flush().expect("flush");

        let result = read_footer(tmp.path()).expect("must not IO-error");
        assert!(result.is_none(), "old SIDX magic must fall back cleanly");
    }

    // ── read_footer returns None for files smaller than TRAILER_SIZE ──────────

    #[test]
    fn read_footer_returns_none_for_tiny_file() {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        tmp.write_all(b"AB").expect("write");
        tmp.flush().expect("flush");
        let result = read_footer(tmp.path()).expect("must not IO-error");
        assert!(result.is_none(), "tiny file must return None");
    }

    // ── read_footer returns None for an empty file ────────────────────────────

    #[test]
    fn read_footer_returns_none_for_empty_file() {
        let tmp = NamedTempFile::new().expect("create temp file");
        let result = read_footer(tmp.path()).expect("must not IO-error");
        assert!(result.is_none(), "empty file must return None");
    }

    // ── string table interning across multiple entries ────────────────────────

    #[test]
    fn shared_string_table_is_compact() {
        let mut collector = SidxEntryCollector::new();
        // Three events in the same entity + scope → string table should have exactly 2 entries.
        for n in 0u8..3 {
            collector.record(sample_entry(n), "order:999", "payments");
        }
        assert_eq!(
            collector.strings().len(),
            2,
            "only 'order:999' and 'payments' should appear in the table"
        );
        // All entries must share the same pair of indices.
        let unique_pairs: std::collections::HashSet<(u32, u32)> = collector
            .entries()
            .iter()
            .map(|e| (e.entity_idx, e.scope_idx))
            .collect();
        assert_eq!(
            unique_pairs.len(),
            1,
            "all entries sharing entity+scope must have identical index pairs"
        );
    }

    // ── decode_from rejects a wrong-sized buffer ──────────────────────────────

    #[test]
    fn decode_from_rejects_wrong_size() {
        let short = vec![0u8; ENTRY_SIZE - 1];
        assert!(
            SidxEntry::decode_from(&short, 42).is_err(),
            "decode_from must error when buffer is too short"
        );

        let long = vec![0u8; ENTRY_SIZE + 1];
        assert!(
            SidxEntry::decode_from(&long, 42).is_err(),
            "decode_from must error when buffer is too long"
        );
    }

    // ── zero-entry footer round-trip ──────────────────────────────────────────

    #[test]
    fn footer_round_trip_zero_entries() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&[0u8; 32]); // pretend frames

        let mut cursor = Cursor::new(&mut buf);
        cursor.seek(SeekFrom::End(0)).expect("seek to end");

        let collector = SidxEntryCollector::new();
        collector
            .write_footer(&mut cursor)
            .expect("write_footer must succeed");

        let mut tmp = NamedTempFile::new().expect("create temp file");
        tmp.write_all(&buf).expect("write");
        tmp.flush().expect("flush");

        let (entries, strings) = read_footer(tmp.path())
            .expect("read_footer must not error")
            .expect("footer must be found");

        assert!(entries.is_empty(), "zero entries expected");
        assert!(
            strings.is_empty(),
            "zero strings expected for empty collector"
        );
    }
}
