//! Mmap-first cold-start artifact.
//!
//! `index.fbati` is a fixed-width snapshot of the in-memory index written on
//! orderly close and after compaction. On open we validate the artifact,
//! mmap it, restore the interner snapshot, replay the entry section, then
//! replay only the durable tail after the recorded watermark.

use crate::coordinate::Coordinate;
use crate::event::HashChain;
use crate::store::checkpoint::WatermarkInfo;
use crate::store::index::{DiskPos, IndexEntry, StoreIndex};
use crate::store::sidx::{kind_to_raw, raw_to_kind};
use crate::store::StoreError;
use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::NamedTempFile;

pub(crate) const MMAP_INDEX_MAGIC: &[u8; 6] = b"FBATIX";
pub(crate) const MMAP_INDEX_VERSION: u16 = 1;
pub(crate) const MMAP_INDEX_FILENAME: &str = "index.fbati";

const PREFIX_LEN: usize = 6 + 2 + 4;
const HEADER_TAIL_LEN: usize = 8 + 8 + 8 + 4 + 8 + 8;
const HEADER_LEN: usize = PREFIX_LEN + HEADER_TAIL_LEN;
const MMAP_ENTRY_SIZE: usize = 162;

struct LoadedMmapIndex {
    mmap: Mmap,
    interner_strings: Vec<String>,
    entries_offset: usize,
    entry_count: u64,
    watermark: WatermarkInfo,
    stored_allocator: u64,
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
    fn encode_into(&self, buf: &mut [u8]) {
        debug_assert_eq!(buf.len(), MMAP_ENTRY_SIZE);
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
        debug_assert_eq!(pos, MMAP_ENTRY_SIZE);
    }

    fn decode_from(buf: &[u8]) -> Result<Self, StoreError> {
        if buf.len() != MMAP_ENTRY_SIZE {
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

        let decoded = Self {
            event_id: get_le!(u128, 16),
            entity_idx: get_le!(u32, 4),
            scope_idx: get_le!(u32, 4),
            kind: get_le!(u16, 2),
            wall_ms: get_le!(u64, 8),
            clock: get_le!(u32, 4),
            prev_hash: get_hash!(),
            event_hash: get_hash!(),
            segment_id: get_le!(u64, 8),
            frame_offset: get_le!(u64, 8),
            frame_length: get_le!(u32, 4),
            global_sequence: get_le!(u64, 8),
            correlation_id: get_le!(u128, 16),
            causation_id: get_le!(u128, 16),
        };
        debug_assert_eq!(pos, MMAP_ENTRY_SIZE);
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

    let mut interner_strings = vec![String::new()];
    interner_strings.extend(index.interner.to_snapshot());
    let interner_bytes = rmp_serde::to_vec_named(&interner_strings)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;

    let interner_count = u32::try_from(interner_strings.len())
        .map_err(|_| StoreError::ser_msg("interner snapshot too large for mmap index"))?;
    let entry_count = u64::try_from(entries.len())
        .map_err(|_| StoreError::ser_msg("entry count too large for mmap index"))?;
    let interner_bytes_len = u64::try_from(interner_bytes.len())
        .map_err(|_| StoreError::ser_msg("interner payload too large for mmap index"))?;

    let mut header_tail = Vec::with_capacity(HEADER_TAIL_LEN);
    header_tail.extend_from_slice(&watermark_segment_id.to_le_bytes());
    header_tail.extend_from_slice(&watermark_offset.to_le_bytes());
    header_tail.extend_from_slice(&index.global_sequence().to_le_bytes());
    header_tail.extend_from_slice(&interner_count.to_le_bytes());
    header_tail.extend_from_slice(&entry_count.to_le_bytes());
    header_tail.extend_from_slice(&interner_bytes_len.to_le_bytes());

    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header_tail);
    hasher.update(&interner_bytes);

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

        let mut buf = [0u8; MMAP_ENTRY_SIZE];
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
    let mut header = [0u8; HEADER_LEN];
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

    if let Err(error) = file.read_exact(&mut header) {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            error = %error,
            "mmap index header is unreadable"
        );
        return None;
    }

    if &header[..6] != MMAP_INDEX_MAGIC {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            "mmap index has wrong magic"
        );
        return None;
    }

    let version = u16::from_le_bytes(header[6..8].try_into().expect("header slice length"));
    if version != MMAP_INDEX_VERSION {
        tracing::warn!(
            target: "batpak::mmap_index",
            path = %path.display(),
            version,
            expected = MMAP_INDEX_VERSION,
            "unsupported mmap index version"
        );
        return None;
    }

    let expected_crc = u32::from_le_bytes(header[8..12].try_into().expect("header slice length"));
    let mut cursor = 12usize;
    let watermark_segment_id = u64::from_le_bytes(
        header[cursor..cursor + 8]
            .try_into()
            .expect("header slice length"),
    );
    cursor += 8;
    let watermark_offset = u64::from_le_bytes(
        header[cursor..cursor + 8]
            .try_into()
            .expect("header slice length"),
    );
    cursor += 8;
    let stored_allocator = u64::from_le_bytes(
        header[cursor..cursor + 8]
            .try_into()
            .expect("header slice length"),
    );
    cursor += 8;
    let interner_count = u32::from_le_bytes(
        header[cursor..cursor + 4]
            .try_into()
            .expect("header slice length"),
    );
    cursor += 4;
    let entry_count = u64::from_le_bytes(
        header[cursor..cursor + 8]
            .try_into()
            .expect("header slice length"),
    );
    cursor += 8;
    let interner_bytes_len = u64::from_le_bytes(
        header[cursor..cursor + 8]
            .try_into()
            .expect("header slice length"),
    );

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
    let entry_bytes_len = match entry_count_usize.checked_mul(MMAP_ENTRY_SIZE) {
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
    let entries_offset = match HEADER_LEN.checked_add(interner_bytes_len) {
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

    let interner_slice = &mmap[HEADER_LEN..entries_offset];
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

    Some(LoadedMmapIndex {
        mmap,
        interner_strings,
        entries_offset,
        entry_count,
        watermark: WatermarkInfo {
            watermark_segment_id,
            watermark_offset,
        },
        stored_allocator,
    })
}

/// Restore the index from the mmap-first artifact. Returns the watermark and
/// allocator position if the artifact was present and valid.
pub(crate) fn try_restore_mmap_index(
    index: &StoreIndex,
    data_dir: &Path,
) -> Option<(WatermarkInfo, u64)> {
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
    let entries_end = loaded.entries_offset + (entry_count * MMAP_ENTRY_SIZE);
    let entries_slice = &loaded.mmap[loaded.entries_offset..entries_end];
    index.clear();
    index
        .interner
        .replace_from_full_snapshot(&loaded.interner_strings);
    let mut rebuilt_entries = Vec::with_capacity(entry_count);

    for chunk in entries_slice.chunks_exact(MMAP_ENTRY_SIZE) {
        let entry = match MmapIndexEntry::decode_from(chunk) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    target: "batpak::mmap_index",
                    error = %error,
                    "mmap index restore failed while decoding an entry"
                );
                return None;
            }
        };
        let entity = match loaded.interner_strings.get(entry.entity_idx as usize) {
            Some(entity) => entity,
            None => {
                tracing::warn!(
                    target: "batpak::mmap_index",
                    entity_idx = entry.entity_idx,
                    "mmap index entity_idx is out of range"
                );
                return None;
            }
        };
        let scope = match loaded.interner_strings.get(entry.scope_idx as usize) {
            Some(scope) => scope,
            None => {
                tracing::warn!(
                    target: "batpak::mmap_index",
                    scope_idx = entry.scope_idx,
                    "mmap index scope_idx is out of range"
                );
                return None;
            }
        };

        let coord = match Coordinate::new(entity, scope) {
            Ok(coord) => coord,
            Err(error) => {
                tracing::warn!(
                    target: "batpak::mmap_index",
                    error = %error,
                    "mmap index restore failed while rebuilding a coordinate"
                );
                return None;
            }
        };
        rebuilt_entries.push(IndexEntry {
            event_id: entry.event_id,
            correlation_id: entry.correlation_id,
            causation_id: (entry.causation_id != 0).then_some(entry.causation_id),
            coord,
            entity_id: crate::store::interner::InternId(entry.entity_idx),
            scope_id: crate::store::interner::InternId(entry.scope_idx),
            kind: raw_to_kind(entry.kind),
            wall_ms: entry.wall_ms,
            clock: entry.clock,
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

    index.restore_sorted_entries(rebuilt_entries, loaded.stored_allocator);
    Some((loaded.watermark, loaded.stored_allocator))
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
}
