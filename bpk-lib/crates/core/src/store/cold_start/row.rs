use crate::coordinate::Coordinate;
use crate::event::{EventHeader, EventKind, HashChain};
use crate::store::index::interner::InternId;
use crate::store::index::{DiskPos, IndexEntry};
use crate::store::StoreError;
use std::collections::BTreeMap;
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColdStartSource {
    Checkpoint,
    MmapIndex,
    Sidx,
}

impl ColdStartSource {
    fn label(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::MmapIndex => "mmap index",
            Self::Sidx => "SIDX",
        }
    }
}

/// Watermark and global-sequence information returned by cold-start artifacts.
///
/// The caller uses these values to know how far the durable log extends without
/// reading every segment file.
pub(crate) struct WatermarkInfo {
    /// Segment ID of the highest durably-written event.
    pub watermark_segment_id: u64,
    /// Byte offset within the watermark segment.
    pub watermark_offset: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReservedKindFallbackStats {
    pub(crate) system: usize,
    pub(crate) effect: usize,
    #[serde(default)]
    pub(crate) system_histogram: BTreeMap<u16, usize>,
    #[serde(default)]
    pub(crate) effect_histogram: BTreeMap<u16, usize>,
}

impl ReservedKindFallbackStats {
    pub(crate) fn record_system(&mut self, raw: u16) {
        self.system += 1;
        *self.system_histogram.entry(raw).or_insert(0) += 1;
    }

    pub(crate) fn record_effect(&mut self, raw: u16) {
        self.effect += 1;
        *self.effect_histogram.entry(raw).or_insert(0) += 1;
    }

    pub(crate) fn merge_from(&mut self, other: &Self) {
        self.system += other.system;
        self.effect += other.effect;
        for (&raw, &count) in &other.system_histogram {
            *self.system_histogram.entry(raw).or_insert(0) += count;
        }
        for (&raw, &count) in &other.effect_histogram {
            *self.effect_histogram.entry(raw).or_insert(0) += count;
        }
    }

    pub(crate) fn add(mut self, other: &Self) -> Self {
        self.merge_from(other);
        self
    }
}

/// Convert an [`EventKind`] to the raw `u16` used in cold-start index artifacts.
///
/// Delegates to [`EventKind::as_raw_u16`], the canonical
/// `(category << 12) | type_id` encoding shared by signing covers, projection
/// cache keys, SIDX footers, mmap rows, and writer notifications.
#[inline]
pub(crate) fn kind_to_raw(kind: EventKind) -> u16 {
    kind.as_raw_u16()
}

/// Reconstruct an [`EventKind`] from its raw `u16` disk representation.
///
/// `EventKind::custom()` rejects the reserved categories `0x0` (system) and `0xD`
/// (effect) with a panic, so those are matched directly against the known library
/// constants. Any unrecognised value in a reserved range falls back to the closest
/// documented constant (system or effect root) so the index can still be rebuilt.
fn raw_to_kind_impl(raw: u16, counts: Option<&mut ReservedKindFallbackStats>) -> EventKind {
    let category = (raw >> 12) as u8;
    match category {
        // Reserved system category (0x0) - match known constants by full value.
        0x0 => match raw {
            0x0001 => EventKind::SYSTEM_INIT,
            0x0002 => EventKind::SYSTEM_SHUTDOWN,
            0x0003 => EventKind::SYSTEM_HEARTBEAT,
            0x0004 => EventKind::SYSTEM_CONFIG_CHANGE,
            0x0005 => EventKind::SYSTEM_CHECKPOINT,
            0x0006 => EventKind::SYSTEM_BATCH_BEGIN,
            0x0007 => EventKind::SYSTEM_BATCH_COMMIT,
            0x0008 => EventKind::SYSTEM_OPEN_COMPLETED,
            0x0009 => EventKind::SYSTEM_CLOSE_COMPLETED,
            0x000F => EventKind::SYSTEM_DENIAL,
            0x0FFE => EventKind::TOMBSTONE,
            0x0000 => EventKind::DATA,
            _ => {
                if let Some(counts) = counts {
                    counts.record_system(raw);
                }
                warn!(
                    raw,
                    "unrecognized reserved system kind in SIDX footer; falling back to DATA"
                );
                EventKind::DATA
            }
        },
        // Reserved effect category (0xD) - match known constants.
        0xD => match raw {
            0xD001 => EventKind::EFFECT_ERROR,
            0xD002 => EventKind::EFFECT_RETRY,
            0xD004 => EventKind::EFFECT_ACK,
            0xD005 => EventKind::EFFECT_BACKPRESSURE,
            0xD006 => EventKind::EFFECT_CANCEL,
            0xD007 => EventKind::EFFECT_CONFLICT,
            _ => {
                if let Some(counts) = counts {
                    counts.record_effect(raw);
                }
                warn!(
                    raw,
                    "unrecognized reserved effect kind in SIDX footer; falling back to EFFECT_ERROR"
                );
                EventKind::EFFECT_ERROR
            }
        },
        // All other categories (0x1-0xC, 0xE-0xF) are open for product use.
        other => EventKind::custom(other, raw & 0x0FFF),
    }
}

#[cfg(test)]
pub(crate) fn raw_to_kind(raw: u16) -> EventKind {
    raw_to_kind_impl(raw, None)
}

pub(crate) fn raw_to_kind_counted(raw: u16, counts: &mut ReservedKindFallbackStats) -> EventKind {
    raw_to_kind_impl(raw, Some(counts))
}

/// Canonical persisted-index row shared by cold-start artifact readers.
///
/// This is intentionally narrower than `EventHeader`: it carries only the
/// persisted facts shared across checkpoint, mmap, and SIDX restore paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColdStartIndexRow {
    pub(crate) source: ColdStartSource,
    pub(crate) event_id: u128,
    pub(crate) correlation_id: u128,
    pub(crate) causation_id: Option<u128>,
    pub(crate) entity_id: InternId,
    pub(crate) scope_id: InternId,
    pub(crate) kind: EventKind,
    pub(crate) wall_ms: u64,
    pub(crate) clock: u32,
    pub(crate) dag_lane: u32,
    pub(crate) dag_depth: u32,
    pub(crate) hash_chain: HashChain,
    pub(crate) disk_pos: DiskPos,
    pub(crate) global_sequence: u64,
}

impl ColdStartIndexRow {
    fn resolve_part<'a>(
        &self,
        interner_strings: &'a [String],
        id: InternId,
        field: &str,
    ) -> Result<&'a str, StoreError> {
        interner_strings
            .get(id.to_usize())
            .map(String::as_str)
            .ok_or_else(|| {
                StoreError::ser_msg(&format!(
                    "{} {} is out of interner range",
                    self.source.label(),
                    field
                ))
            })
    }

    pub(crate) fn resolve_strings(
        &self,
        interner_strings: &[String],
    ) -> Result<(String, String), StoreError> {
        Ok((
            self.resolve_part(interner_strings, self.entity_id, "entity_id")?
                .to_owned(),
            self.resolve_part(interner_strings, self.scope_id, "scope_id")?
                .to_owned(),
        ))
    }

    pub(crate) fn to_index_entry(
        &self,
        interner_strings: &[String],
    ) -> Result<IndexEntry, StoreError> {
        let entity = self.resolve_part(interner_strings, self.entity_id, "entity_id")?;
        let scope = self.resolve_part(interner_strings, self.scope_id, "scope_id")?;
        let coord = Coordinate::new(entity, scope)?;
        Ok(IndexEntry {
            event_id: self.event_id,
            correlation_id: self.correlation_id,
            causation_id: self.causation_id,
            coord,
            entity_id: self.entity_id,
            scope_id: self.scope_id,
            kind: self.kind,
            wall_ms: self.wall_ms,
            clock: self.clock,
            dag_lane: self.dag_lane,
            dag_depth: self.dag_depth,
            hash_chain: self.hash_chain.clone(),
            disk_pos: self.disk_pos,
            global_sequence: self.global_sequence,
            receipt_extensions: BTreeMap::new(),
        })
    }

    pub(crate) fn to_event_header(&self) -> EventHeader {
        EventHeader::new(
            self.event_id,
            self.correlation_id,
            self.causation_id,
            (self.wall_ms * 1000) as i64,
            crate::coordinate::DagPosition::with_hlc(
                self.wall_ms,
                0,
                self.dag_depth,
                self.dag_lane,
                self.clock,
            ),
            0,
            self.kind,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ColdStartIndexRow, ColdStartSource};
    use crate::event::{EventKind, HashChain};
    use crate::store::index::interner::InternId;
    use crate::store::index::DiskPos;

    #[test]
    fn cold_start_row_to_event_header_preserves_lane_depth_and_ids() {
        let row = ColdStartIndexRow {
            source: ColdStartSource::Sidx,
            event_id: 1,
            correlation_id: 2,
            causation_id: Some(3),
            entity_id: InternId(1),
            scope_id: InternId(2),
            kind: EventKind::DATA,
            wall_ms: 1_700_000_000_000,
            clock: 9,
            dag_lane: 4,
            dag_depth: 2,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos::new(7, 64, 32),
            global_sequence: 11,
        };

        let header = row.to_event_header();

        assert_eq!(header.event_id, crate::id::EventId::from(1u128));
        assert_eq!(header.correlation_id, crate::id::CorrelationId::from(2u128));
        assert_eq!(header.causation_id, Some(crate::id::CausationId::from(3u128)));
        assert_eq!(header.timestamp_us, 1_700_000_000_000_000);
        assert_eq!(header.position.wall_ms, 1_700_000_000_000);
        assert_eq!(header.position.sequence, 9);
        assert_eq!(header.position.lane, 4);
        assert_eq!(header.position.depth, 2);
        assert_eq!(header.event_kind, EventKind::DATA);
        assert_eq!(header.payload_size, 0);
        assert_eq!(header.flags, 0);
        assert_eq!(header.content_hash, [0u8; 32]);
    }
}
