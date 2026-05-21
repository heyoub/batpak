use crate::event::HashChain;
use crate::store::cold_start::{
    kind_to_raw, raw_to_kind_counted, ColdStartIndexRow, ColdStartSource, ReservedKindFallbackStats,
};
use crate::store::index::interner::InternId;
use crate::store::index::{DiskPos, IndexEntry, RoutingSummary};
use crate::store::StoreError;

pub(super) const MMAP_INDEX_MAGIC: &[u8; 6] = b"FBATIX";
pub(super) const MMAP_INDEX_VERSION: u16 = 5;
pub(crate) const MMAP_INDEX_FILENAME: &str = "index.fbati";

pub(super) const PREFIX_LEN: usize = 6 + 2 + 4;
const HEADER_TAIL_LEN_V1: usize = 8 + 8 + 8 + 4 + 8 + 8;
const HEADER_TAIL_LEN_V2: usize = HEADER_TAIL_LEN_V1 + 8;
const HEADER_TAIL_LEN_V3: usize = HEADER_TAIL_LEN_V2 + 8;
const HEADER_LEN_V1: usize = PREFIX_LEN + HEADER_TAIL_LEN_V1;
const HEADER_LEN_V2: usize = PREFIX_LEN + HEADER_TAIL_LEN_V2;
const HEADER_LEN_V3: usize = PREFIX_LEN + HEADER_TAIL_LEN_V3;
const MMAP_ENTRY_SIZE_V2: usize = 162;
const MMAP_ENTRY_SIZE_V3: usize = 170;
pub(super) const MMAP_ENTRY_SIZE_V5: usize = MMAP_ENTRY_SIZE_V3 + 8 + 8 + 32;

pub(super) fn header_tail_len(version: u16) -> usize {
    if version == 1 {
        HEADER_TAIL_LEN_V1
    } else if version >= 5 {
        HEADER_TAIL_LEN_V3
    } else {
        HEADER_TAIL_LEN_V2
    }
}

pub(super) fn header_len(version: u16) -> usize {
    if version == 1 {
        HEADER_LEN_V1
    } else if version >= 5 {
        HEADER_LEN_V3
    } else {
        HEADER_LEN_V2
    }
}

pub(super) fn entry_size(version: u16) -> usize {
    if version >= 5 {
        MMAP_ENTRY_SIZE_V5
    } else if version >= 3 {
        MMAP_ENTRY_SIZE_V3
    } else {
        MMAP_ENTRY_SIZE_V2
    }
}

pub(super) fn read_le_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

pub(super) fn read_le_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

pub(super) fn read_le_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct MmapSummaryDataV2 {
    pub(super) routing: RoutingSummary,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct MmapSummaryDataV4 {
    pub(super) routing: RoutingSummary,
    #[serde(default)]
    pub(super) reserved_kind_fallbacks: ReservedKindFallbackStats,
}

pub(super) struct MmapIndexEntry {
    pub(super) event_id: u128,
    pub(super) entity_idx: u32,
    pub(super) scope_idx: u32,
    pub(super) kind: u16,
    pub(super) wall_ms: u64,
    pub(super) clock: u32,
    pub(super) dag_lane: u32,
    pub(super) dag_depth: u32,
    pub(super) prev_hash: [u8; 32],
    pub(super) event_hash: [u8; 32],
    pub(super) segment_id: u64,
    pub(super) frame_offset: u64,
    pub(super) frame_length: u32,
    pub(super) global_sequence: u64,
    pub(super) correlation_id: u128,
    pub(super) causation_id: u128,
    pub(super) extension_offset: u64,
    pub(super) extension_len: u64,
    pub(super) extension_hash: [u8; 32],
}

impl MmapIndexEntry {
    pub(super) fn from_index_entry(entry: &IndexEntry) -> Self {
        Self {
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

    fn to_disk_pos(&self) -> DiskPos {
        DiskPos::new(self.segment_id, self.frame_offset, self.frame_length)
    }

    pub(super) fn to_cold_start_row_counted(
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

    pub(super) fn encode_into_v5(&self, buf: &mut [u8]) {
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

    pub(super) fn decode_from(buf: &[u8], version: u16) -> Result<Self, StoreError> {
        let expected_size = entry_size(version);
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
