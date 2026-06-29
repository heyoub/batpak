use super::{SidxEntry, ENTRY_SIZE, SIDX_MAGIC, SIDX_MAGIC_LEGACY_SDX2};
use crate::store::StoreError;
use std::io::{Read, Seek, SeekFrom};

/// Size of the fixed-layout trailer that terminates the SIDX footer:
/// `string_table_offset(8) + entry_count(4) + magic(4)` = 16 bytes.
pub(super) const TRAILER_SIZE: u64 = 16;
const TRAILER_SIZE_USIZE: usize = 16;

/// Size of the CRC32 that sits immediately before the trailer, covering the
/// contiguous `[string_table_bytes ++ entries]` region.
pub(super) const SIDX_CRC_LEN: u64 = 4;
const SIDX_CRC_LEN_USIZE: usize = 4;

pub(super) fn sidx_crc_len_usize() -> usize {
    SIDX_CRC_LEN_USIZE
}

pub(super) struct FooterLayout {
    pub(super) string_table_offset: u64,
    pub(super) string_table_len: u64,
    pub(super) entry_count: usize,
}

pub(super) fn trailer_size_usize() -> usize {
    TRAILER_SIZE_USIZE
}

pub(super) fn read_layout<R: Read + Seek>(
    reader: &mut R,
    segment_id: u64,
) -> Result<Option<FooterLayout>, StoreError> {
    let file_len = reader.seek(SeekFrom::End(0)).map_err(StoreError::Io)?;
    // STRICT `<`: a file of EXACTLY `TRAILER_SIZE` bytes still contains a full
    // 16-byte trailer and must be parsed (then rejected as corrupt because there
    // is no room for entries/CRC). The `< -> <=` mutant would short-circuit it as
    // "no footer" — see `read_layout_parses_a_trailer_only_file_then_rejects_it`.
    if file_len < TRAILER_SIZE {
        return Ok(None);
    }

    reader
        .seek(SeekFrom::End(-(TRAILER_SIZE as i64)))
        .map_err(StoreError::Io)?;

    let mut trailer = [0u8; 16];
    reader.read_exact(&mut trailer).map_err(StoreError::Io)?;

    if &trailer[12..16] != SIDX_MAGIC {
        return Ok(None);
    }

    let string_table_offset = read_trailer_u64(&trailer[0..8], segment_id)?;
    let entry_count = read_trailer_u32(&trailer[8..12], segment_id)? as usize;
    let entries_block_len = (entry_count as u64)
        .checked_mul(ENTRY_SIZE as u64)
        .ok_or_else(|| StoreError::CorruptSegment {
            segment_id,
            detail: "SIDX entry_count × ENTRY_SIZE overflows u64".into(),
        })?;

    // entries_start = file_len - TRAILER - CRC - entries_block_len. The CRC's 4
    // bytes sit at [entries_start + entries_block_len .. + 4), immediately before
    // the 16-byte trailer; subtracting it here keeps the entries/table geometry
    // byte-identical to the region write_footer hashed.
    let entries_start = file_len
        .checked_sub(TRAILER_SIZE)
        .and_then(|n| n.checked_sub(SIDX_CRC_LEN))
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

    let string_table_len = entries_start
        .checked_sub(string_table_offset)
        .ok_or_else(|| StoreError::CorruptSegment {
            segment_id,
            detail: "SIDX string table length underflows".into(),
        })?;

    // Integrity check: recompute CRC32 over the contiguous covered region
    // [string_table_offset .. entries_start + entries_block_len) and compare it to
    // the 4 stored CRC bytes that sit immediately after the entries block. A
    // mismatch (or an old SDX2-era footer that reached here, which it cannot — the
    // magic gate above already rejected it) means the footer cannot be trusted, so
    // we return Ok(None) to degrade to the CRC-verified frame-scan rebuild. Only an
    // actual IO failure surfaces as StoreError::Io.
    let covered_end = entries_start
        .checked_add(entries_block_len)
        .ok_or_else(|| StoreError::CorruptSegment {
            segment_id,
            detail: "SIDX covered region end overflows u64".into(),
        })?;
    // `covered_len` is derived from on-disk geometry. The bounds checks above
    // prove `covered_end <= file_len`, so `covered_len` cannot exceed the file
    // size — but a corrupt/adversarial trailer with `string_table_offset` near 0
    // on a large or sparse segment can still drive it to nearly the whole file.
    // Hash the covered region in fixed-size chunks so memory stays O(chunk)
    // instead of O(covered_len), keeping cold-start reopen from OOM-ing on a
    // forged footer before the CRC can reject it and trigger the frame-scan
    // fallback. The result is byte-identical to hashing the whole span at once.
    let covered_len = string_table_len
        .checked_add(entries_block_len)
        .ok_or_else(|| StoreError::CorruptSegment {
            segment_id,
            detail: "SIDX covered region length overflows u64".into(),
        })?;

    reader
        .seek(SeekFrom::Start(string_table_offset))
        .map_err(StoreError::Io)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut remaining = covered_len;
    let mut chunk = [0u8; 8192];
    while remaining > 0 {
        // `remaining` is clamped to `chunk.len()` (a usize), so the result
        // always fits in usize regardless of how large the corrupt span claims
        // to be — the `min` is computed on the usize side to make that explicit.
        let take = usize::try_from(remaining)
            .unwrap_or(chunk.len())
            .min(chunk.len());
        reader
            .read_exact(&mut chunk[..take])
            .map_err(StoreError::Io)?;
        hasher.update(&chunk[..take]);
        remaining -= take as u64;
    }

    reader
        .seek(SeekFrom::Start(covered_end))
        .map_err(StoreError::Io)?;
    let mut stored_crc = [0u8; 4];
    reader.read_exact(&mut stored_crc).map_err(StoreError::Io)?;

    if hasher.finalize() != u32::from_le_bytes(stored_crc) {
        return Ok(None);
    }

    Ok(Some(FooterLayout {
        string_table_offset,
        string_table_len,
        entry_count,
    }))
}

/// Parse the SIDX entry table from a footer WITHOUT requiring the footer CRC to
/// pass — the "cake-and-eat-it" untrusted entry-table read.
///
/// This is the manifest-recovery counterpart to [`read_layout`]: it reads the
/// fixed 16-byte trailer geometry (`string_table_offset` + `entry_count`),
/// applies the SAME bounds guards that [`read_layout`] uses (entry block must not
/// extend before the start of the file, `string_table_offset` must not be past
/// `entries_start`), then decodes `entry_count` raw [`SidxEntry`] records via
/// [`SidxEntry::decode_from`] — which is CRC-INDEPENDENT — directly from the
/// entries block. The footer CRC is NEVER verified here, so this works on a
/// CRC-failed SDX3 footer, a legacy un-CRC'd SDX2 footer, or a partially-forged
/// trailer.
///
/// EVERY entry returned is an UNTRUSTED HYPOTHESIS. The caller MUST corroborate
/// each entry against the independently CRC-verified recovered-frame set before
/// trusting any of its fields (see `segment::corroborate_untrusted_entries`). A
/// forger can fabricate arbitrary entry bytes, but cannot match a real frame's
/// content-addressed `event_hash` (blake3) — so corroboration, not this parse, is
/// the trust boundary.
///
/// On ANY geometry/parse failure (absurd `entry_count`, `entry_count × ENTRY_SIZE`
/// overflow, an entries block that runs before the start of the file, a
/// `string_table_offset` past `entries_start`, a magic that is neither `SDX3` nor
/// the legacy `SDX2`, or a short/torn read) this returns ZERO entries
/// (`Ok(Vec::new())`) so the caller cleanly falls back to tail-policy behavior.
/// Only a genuine non-EOF IO failure surfaces as [`StoreError::Io`].
///
/// Both `SDX3` and the legacy `SDX2` magic are accepted because both are
/// recognized as frame-region boundaries by `detect_sidx_boundary`, and the
/// untrusted recovery path handles both provenances identically (the entries
/// block geometry is byte-identical across the two magics).
pub(super) fn read_entries_unauthenticated<R: Read + Seek>(
    reader: &mut R,
    segment_id: u64,
) -> Result<Vec<SidxEntry>, StoreError> {
    let file_len = reader.seek(SeekFrom::End(0)).map_err(StoreError::Io)?;
    if file_len < TRAILER_SIZE {
        return Ok(Vec::new());
    }

    reader
        .seek(SeekFrom::End(-(TRAILER_SIZE as i64)))
        .map_err(StoreError::Io)?;
    let mut trailer = [0u8; 16];
    reader.read_exact(&mut trailer).map_err(StoreError::Io)?;

    // Accept BOTH the current SDX3 and legacy SDX2 magics — the boundary detector
    // recognizes both, and the manifest geometry is identical. A trailer with any
    // other magic carries no parseable entry table → zero entries.
    let magic = &trailer[12..16];
    if magic != SIDX_MAGIC && magic != SIDX_MAGIC_LEGACY_SDX2 {
        return Ok(Vec::new());
    }

    let string_table_offset = u64::from_le_bytes([
        trailer[0], trailer[1], trailer[2], trailer[3], trailer[4], trailer[5], trailer[6],
        trailer[7],
    ]);
    let entry_count = u32::from_le_bytes([trailer[8], trailer[9], trailer[10], trailer[11]]) as u64;

    // Reuse read_layout's geometry guards. ANY failure → zero entries (fall back),
    // never a hard error: a bogus entry_count, an overflowing entries block, or an
    // out-of-range offset is exactly the "no trustworthy signal" case.
    let Some(entries_block_len) = entry_count.checked_mul(ENTRY_SIZE as u64) else {
        return Ok(Vec::new());
    };
    let Some(entries_start) = file_len
        .checked_sub(TRAILER_SIZE)
        .and_then(|n| n.checked_sub(SIDX_CRC_LEN))
        .and_then(|n| n.checked_sub(entries_block_len))
    else {
        return Ok(Vec::new());
    };
    // string_table_offset must sit at/before the entries block start, same as the
    // authenticated layout invariant. A violation means the geometry is garbage.
    if string_table_offset > entries_start {
        return Ok(Vec::new());
    }

    // Decode the entries block in place. We do NOT need the string table for
    // corroboration — only (frame_offset, frame_length, event_hash) — so we skip
    // straight to entries_start and read the raw fixed-size records. entity_idx /
    // scope_idx string-table bounds are NOT validated here (corroboration ignores
    // them; only frames that match a recovered frame's content hash are trusted).
    reader
        .seek(SeekFrom::Start(entries_start))
        .map_err(StoreError::Io)?;

    let count = usize::try_from(entry_count).unwrap_or(usize::MAX);
    let mut entries = Vec::with_capacity(count.min(1024));
    let mut buf = [0u8; ENTRY_SIZE];
    for _ in 0..entry_count {
        if let Err(e) = reader.read_exact(&mut buf) {
            // A torn/short entries block → no trustworthy manifest. Return zero
            // entries so the caller falls back, rather than a partial table that
            // could falsely corroborate. A real non-EOF IO error still propagates.
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                return Ok(Vec::new());
            }
            return Err(StoreError::Io(e));
        }
        match SidxEntry::decode_from(&buf, segment_id) {
            Ok(entry) => entries.push(entry),
            // decode_from only errors on a wrong buffer length, which cannot happen
            // here (buf is exactly ENTRY_SIZE); treat defensively as "no manifest".
            Err(_) => return Ok(Vec::new()),
        }
    }

    Ok(entries)
}

fn read_trailer_u64(bytes: &[u8], segment_id: u64) -> Result<u64, StoreError> {
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| StoreError::CorruptFrame {
        segment_id,
        offset: 0,
        reason: "trailer truncated: string_table_offset bytes not readable".into(),
    })?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_trailer_u32(bytes: &[u8], segment_id: u64) -> Result<u32, StoreError> {
    let bytes: [u8; 4] = bytes.try_into().map_err(|_| StoreError::CorruptFrame {
        segment_id,
        offset: 0,
        reason: "trailer truncated: entry_count bytes not readable".into(),
    })?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{read_layout, SIDX_MAGIC};
    use std::io::Cursor;

    #[test]
    fn read_layout_parses_a_trailer_only_file_then_rejects_it() {
        // A file of EXACTLY TRAILER_SIZE (16) bytes carries a full trailer but no
        // room for the entries block + CRC, so real `read_layout` parses the
        // trailer and then fails the geometry check with CorruptSegment. The
        // `< -> <=` mutant treats `file_len == TRAILER_SIZE` as "no footer" and
        // returns Ok(None) — so asserting the result is an Err kills it.
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(&0u64.to_le_bytes()); // string_table_offset = 0
        buf.extend_from_slice(&0u32.to_le_bytes()); // entry_count = 0
        buf.extend_from_slice(SIDX_MAGIC); // valid SDX3 magic (total = 16 bytes)
        assert_eq!(
            buf.len(),
            16,
            "trailer-only fixture must be exactly TRAILER_SIZE"
        );

        let is_err = read_layout(&mut Cursor::new(buf), 0).is_err();
        assert!(
            is_err,
            "a magic-bearing file of exactly TRAILER_SIZE must be parsed and rejected as \
             corrupt (no room for entries/CRC), not short-circuited as Ok(None)"
        );
    }
}
