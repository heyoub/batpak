//! SIDX — Segment InDeX footer for fast cold-start index rebuild.
//!
//! A SIDX footer is appended to a **sealed** segment file immediately after all event
//! frames have been written. On the next cold start, the store can seek to the last 16
//! bytes of each segment, detect the `b"SDX3"` magic, and reconstruct the in-memory
//! index without re-deserialising every MessagePack frame.
//!
//! # On-disk layout (end of segment file)
//!
//! ```text
//! [...frames...]
//! [string_table_bytes]           — msgpack-encoded Vec<String> (entity + scope names)
//! [entries: N × ENTRY_SIZE]      — raw little-endian binary, no framing
//! [crc32: u32 LE]                — CRC32 over [string_table_bytes ++ entries], integrity
//! [string_table_offset: u64 LE]  — byte offset from segment start where the table begins
//! [entry_count: u32 LE]          — number of SidxEntry records
//! [magic: b"SDX3"]               — 4 bytes; last bytes of the file
//! ```
//!
//! To read: seek to `EOF - 16`, read the fixed trailer in on-disk order
//! `string_table_offset(8) + entry_count(4) + magic(4)`; the trailing `magic` must equal
//! `SDX3` before the other fields are trusted (matching `footer::read_layout`). The 4-byte
//! `crc32` sits immediately before the trailer, covering `[string_table_bytes ++ entries]`.
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

mod footer;

#[cfg(test)]
use crate::event::EventKind;
use crate::event::HashChain;
#[cfg(test)]
pub(crate) use crate::store::cold_start::raw_to_kind;
pub(crate) use crate::store::cold_start::{
    kind_to_raw, raw_to_kind_counted, ReservedKindFallbackStats,
};
use crate::store::cold_start::{ColdStartIndexRow, ColdStartSource};
use crate::store::index::interner::InternId;
use crate::store::StoreError;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

// ── constants ─────────────────────────────────────────────────────────────────

/// Four-byte magic that identifies a SIDX footer at the tail of a segment file.
///
/// `SDX3` (the `SDX2`→`SDX3` bump) gates the CRC32-bearing footer: pre-0.8.3 `SDX2`
/// footers carried no integrity check, so they read as `Ok(None)` and fall back to the
/// CRC-verified frame-scan rebuild rather than trusting un-CRC'd bytes.
pub(crate) const SIDX_MAGIC: &[u8; 4] = b"SDX3";

/// Legacy pre-0.8.3 SIDX magic. A footer tagged `SDX2` carries no CRC32, so its
/// *content* must never be trusted on read — `read_footer`/`read_layout` keep
/// rejecting it by matching only [`SIDX_MAGIC`], which degrades to the
/// CRC-verified frame-scan rebuild.
///
/// Its *boundary*, however, must still be honored: the 16-byte trailer geometry
/// (`string_table_offset(8) + entry_count(4) + magic(4)`) is identical across
/// `SDX2`/`SDX3`, so `string_table_offset` reliably marks where the frame region
/// ends. [`crate::store::segment::detect_sidx_boundary`] recognizes BOTH magics so
/// the frame-scan fallback stops at the true end of frames instead of over-running
/// into the old un-CRC'd footer bytes (which a frame scan would mis-read as a
/// corrupt/oversized frame, hard-failing cold start for any non-tail segment).
pub(crate) const SIDX_MAGIC_LEGACY_SDX2: &[u8; 4] = b"SDX2";

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
    pub(crate) fn to_disk_pos(&self, segment_id: u64) -> crate::store::index::DiskPos {
        crate::store::index::DiskPos::new(segment_id, self.frame_offset, self.frame_length)
    }

    pub(crate) fn to_cold_start_row(&self, segment_id: u64) -> ColdStartIndexRow {
        self.to_cold_start_row_counted(segment_id, &mut ReservedKindFallbackStats::default())
    }

    pub(crate) fn to_cold_start_row_counted(
        &self,
        segment_id: u64,
        counts: &mut ReservedKindFallbackStats,
    ) -> ColdStartIndexRow {
        ColdStartIndexRow {
            source: ColdStartSource::Sidx,
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
            disk_pos: self.to_disk_pos(segment_id),
            global_sequence: self.global_sequence,
        }
    }

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
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InternerExhausted`] if interning `entity` or
    /// `scope` would overflow the `u32` string-table index domain.
    pub(crate) fn record(
        &mut self,
        mut entry: SidxEntry,
        entity: &str,
        scope: &str,
    ) -> Result<(), StoreError> {
        entry.entity_idx = self.intern(entity)?;
        entry.scope_idx = self.intern(scope)?;
        self.entries.push(entry);
        Ok(())
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
    /// `writer` must implement both [`Write`] and [`Seek`]. `segment_id` is
    /// used only to stamp structural errors (e.g. too many entries).
    ///
    /// # Footer layout written
    ///
    /// ```text
    /// [string_table_bytes]          — msgpack-encoded Vec<String>
    /// [entries: N × ENTRY_SIZE]     — raw little-endian binary
    /// [crc32: u32 LE]               — CRC32 over [string_table_bytes ++ entries]
    /// [string_table_offset: u64 LE] — byte offset where string_table_bytes starts
    /// [entry_count: u32 LE]
    /// [magic: b"SDX3"]
    /// ```
    ///
    /// The body is assembled in a single `Vec<u8>` and written in one
    /// `write_all` call so a partial-write torn state cannot leave the
    /// footer half-formed: either the entire footer is on disk or none of
    /// it is. This matters for crash recovery — a partially-written
    /// footer would cause `read_footer` to either mis-parse or (worse)
    /// silently fall back to the slow frame-scan path.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Serialization`] if the string table cannot be encoded to msgpack.
    /// Returns [`StoreError::SegmentTooManyEntries`] if the entry count exceeds `u32::MAX`.
    /// Returns [`StoreError::Io`] if the write fails.
    pub(crate) fn write_footer<W: Write + Seek>(
        &self,
        writer: &mut W,
        segment_id: u64,
    ) -> Result<(), StoreError> {
        // 1. Encode string table to msgpack.
        let string_table_bytes = crate::encoding::to_bytes(&self.strings)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;

        // 2. Record the file position where the string table will start.
        let string_table_offset = writer.stream_position().map_err(StoreError::Io)?;

        // 3. Validate entry count fits in u32 before building the footer.
        // A segment with > u32::MAX entries is structurally invalid — the
        // SIDX trailer cannot represent it, and saturating silently would
        // ship a lie on disk. Surface this as a real error.
        let entry_count =
            u32::try_from(self.entries.len()).map_err(|_| StoreError::SegmentTooManyEntries {
                segment_id,
                count: self.entries.len() as u64,
            })?;

        // 4. Build the full footer in one contiguous buffer so the write
        // is atomic (single write_all) — no partial-write torn state.
        let mut footer = Vec::with_capacity(
            string_table_bytes.len()
                + self.entries.len() * ENTRY_SIZE
                + footer::sidx_crc_len_usize()
                + footer::trailer_size_usize(),
        );

        footer.extend_from_slice(&string_table_bytes);

        let mut buf = [0u8; ENTRY_SIZE];
        for entry in &self.entries {
            entry.encode_into(&mut buf);
            footer.extend_from_slice(&buf);
        }

        // 4b. Append a CRC32 over the contiguous [string_table_bytes ++ entries]
        // region that has accumulated in `footer` so far. This is the integrity
        // check `footer::read_layout` recomputes and verifies on read; placing it
        // immediately before the fixed 16-byte trailer keeps the trailer geometry
        // (offset/count/magic) unchanged for the fixed-offset trailer readers.
        let crc = crc32fast::hash(&footer);
        footer.extend_from_slice(&crc.to_le_bytes());

        footer.extend_from_slice(&string_table_offset.to_le_bytes());
        footer.extend_from_slice(&entry_count.to_le_bytes());
        footer.extend_from_slice(SIDX_MAGIC);

        writer.write_all(&footer).map_err(StoreError::Io)?;

        Ok(())
    }

    /// Intern `s` and return its index in the string table.
    ///
    /// If `s` already exists in the table, returns the existing index.
    /// Otherwise appends it and returns the new index.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InternerExhausted`] if the string table has filled
    /// the `u32` index domain (~4 billion distinct strings). This is unreachable
    /// under the segment-size ceiling but is surfaced as a typed error rather
    /// than truncating the index and shipping an aliased slot on disk.
    fn intern(&mut self, s: &str) -> Result<u32, StoreError> {
        if let Some(&idx) = self.string_map.get(s) {
            return Ok(idx);
        }
        let idx = u32::try_from(self.strings.len()).map_err(|_| StoreError::InternerExhausted {
            count: u64::try_from(self.strings.len()).unwrap_or(u64::MAX),
        })?;
        self.strings.push(s.to_owned());
        self.string_map.insert(s.to_owned(), idx);
        Ok(idx)
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

/// Return the `string_table_offset` from a footer **only if its CRC
/// authenticates** the SDX3 footer.
///
/// This is the trust oracle for the frame-region boundary: it runs the exact
/// same CRC verification as [`read_footer`] (via `footer::read_layout`) over an
/// already-open reader, but discards the parsed entries — the caller only needs
/// to know whether the trailer's `string_table_offset` came from an
/// authenticated footer.
///
/// Returns `Ok(Some(offset))` when the footer is a CRC-valid SDX3 footer (the
/// offset is authoritative). Returns `Ok(None)` when there is no footer, the
/// CRC mismatches, or the footer is a legacy un-CRC'd SDX2 footer (the offset
/// is, at best, an unauthenticated hint). Only an actual IO failure surfaces as
/// [`StoreError::Io`].
pub(crate) fn authenticated_string_table_offset<R: Read + Seek>(
    reader: &mut R,
    segment_id: u64,
) -> Result<Option<u64>, StoreError> {
    match footer::read_layout(reader, segment_id) {
        Ok(Some(layout)) => Ok(Some(layout.string_table_offset)),
        // No SDX3 magic / CRC mismatch → no authenticated footer.
        Ok(None) => Ok(None),
        // A structurally-inconsistent footer (e.g. a forged trailer whose
        // entry_count/offset geometry is impossible) is NOT trusted — it cannot
        // authenticate the offset. This is a trust oracle, not the rebuild path,
        // so we degrade to "untrusted" rather than surfacing the structural error;
        // the frame-scan recovery is the authority. Only a real IO failure must
        // still propagate so it is not silently swallowed as untrust.
        Err(StoreError::Io(e)) => Err(StoreError::Io(e)),
        Err(_) => Ok(None),
    }
}

/// Parse the SIDX entry table WITHOUT requiring the footer CRC to authenticate.
///
/// This is the untrusted manifest-recovery read used by the segment frame-scan
/// fallback (the "cake-and-eat-it" resolution): when the footer boundary is
/// UNTRUSTED (CRC-failed SDX3, legacy SDX2, or a forged trailer), the entries are
/// still parseable because [`SidxEntry::decode_from`] is CRC-independent. Every
/// returned entry is an UNTRUSTED HYPOTHESIS the caller MUST corroborate against
/// the independently CRC-verified recovered frames before trusting it.
///
/// Returns ZERO entries (`Ok(Vec::new())`) on any geometry/parse failure so the
/// caller falls back to the existing tail-policy behavior. Only a real IO failure
/// surfaces as [`StoreError::Io`]. See [`footer::read_entries_unauthenticated`].
pub(crate) fn read_entries_unauthenticated<R: Read + Seek>(
    reader: &mut R,
    segment_id: u64,
) -> Result<Vec<SidxEntry>, StoreError> {
    footer::read_entries_unauthenticated(reader, segment_id)
}

pub(crate) fn read_footer(path: &Path) -> Result<Option<SidxFooterData>, StoreError> {
    // Derive a segment_id for error messages from the filename ("000042.fbat" → 42).
    // Falls back to 0 if the filename is malformed, but surfaces the parse failure
    // so a corrupt name on disk is not invisible.
    let segment_id = match crate::store::segment::SegmentId::from_filename(path) {
        Ok(parsed) => parsed.as_u64(),
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "skipping malformed segment filename"
            );
            0
        }
    };

    let mut file = crate::store::platform::fs::open_file(path).map_err(StoreError::Io)?;

    let Some(layout) = footer::read_layout(&mut file, segment_id)? else {
        return Ok(None);
    };

    // ── 4. Read and decode string table ───────────────────────────────────────
    file.seek(SeekFrom::Start(layout.string_table_offset))
        .map_err(StoreError::Io)?;

    let table_len_usize =
        usize::try_from(layout.string_table_len).map_err(|_| StoreError::CorruptSegment {
            segment_id,
            detail: format!(
                "SIDX string table length {} exceeds usize::MAX",
                layout.string_table_len
            ),
        })?;
    let mut string_table_buf = vec![0u8; table_len_usize];
    file.read_exact(&mut string_table_buf)
        .map_err(StoreError::Io)?;

    let strings: Vec<String> = crate::encoding::from_bytes(&string_table_buf)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;

    // ── 5. Read and decode entries ─────────────────────────────────────────────
    // After reading the string table we are positioned at entries_start.
    let mut entries = Vec::with_capacity(layout.entry_count);
    let mut entry_buf = [0u8; ENTRY_SIZE];

    for i in 0..layout.entry_count {
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

#[cfg(test)]
mod tests;
