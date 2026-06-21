use super::interner::InternId;
use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::{EncodedBytes, ExtensionKey};
use std::collections::BTreeMap;

/// ClockKey: BTreeMap key. Ord: wall_ms-first, then clock, then uuid tiebreak.
/// `wall_ms` enables global causal ordering across entities (HLC layer 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClockKey {
    /// HLC wall clock milliseconds — global ordering across entities.
    pub(crate) wall_ms: u64,
    /// Per-entity monotonic sequence number used as the HLC logical counter.
    pub(crate) clock: u32,
    /// Event UUID tiebreaker for deterministic ordering within the same clock tick.
    pub(crate) uuid: u128,
}

/// IndexEntry: everything needed for index queries without disk reads.
/// Shared via `Arc` across all index maps — one allocation per event.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IndexEntry {
    /// Unique ID of the event.
    pub(crate) event_id: u128,
    /// Correlation ID linking related events in a causal chain.
    pub(crate) correlation_id: u128,
    /// ID of the event that caused this one; `None` for root-cause events.
    pub(crate) causation_id: Option<u128>,
    /// Entity and scope coordinates for this event.
    pub(crate) coord: Coordinate,
    /// Interned entity string ID for compact checkpoint serialization.
    pub(crate) entity_id: InternId,
    /// Interned scope string ID for compact checkpoint serialization.
    pub(crate) scope_id: InternId,
    /// Event kind (type discriminant).
    pub(crate) kind: EventKind,
    /// HLC wall clock milliseconds — for global causal ordering.
    pub(crate) wall_ms: u64,
    /// Per-entity monotonic sequence number.
    pub(crate) clock: u32,
    /// Branch lane within the logical event DAG.
    pub(crate) dag_lane: u32,
    /// Branch depth within the logical event DAG.
    pub(crate) dag_depth: u32,
    /// Blake3 hash chain linking this event to its predecessor.
    pub(crate) hash_chain: HashChain,
    /// Location of the event frame on disk.
    pub(crate) disk_pos: DiskPos,
    /// Globally monotonic sequence number assigned at commit time.
    pub(crate) global_sequence: u64,
    /// Opaque receipt extensions committed with this event.
    pub(crate) receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

/// DiskPos: where to find this event on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskPos {
    /// Numeric identifier of the segment file containing this event.
    pub(crate) segment_id: u64,
    /// Byte offset of the frame within the segment file.
    pub(crate) offset: u64,
    /// Total byte length of the encoded frame.
    pub(crate) length: u32,
}

impl DiskPos {
    /// Construct a new persisted frame location.
    pub const fn new(segment_id: u64, offset: u64, length: u32) -> Self {
        Self {
            segment_id,
            offset,
            length,
        }
    }

    /// Numeric identifier of the segment file containing this event.
    pub const fn segment_id(self) -> u64 {
        self.segment_id
    }

    /// Byte offset of the frame within the segment file.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Total byte length of the encoded frame.
    pub const fn length(self) -> u32 {
        self.length
    }
}

/// Minimal result for columnar scan hot paths.
///
/// Scan methods return `Vec<QueryHit>` to avoid two per-result costs that
/// existed in the `Vec<Arc<IndexEntry>>` path:
///  1. `Arc::clone` (atomic ref-count increment) inside the scan loop.
///  2. Immediate `as_ref().clone()` (full `IndexEntry` memcpy) at the
///     `StoreIndex` boundary.
///
/// Callers that need the full entry call `StoreIndex::upgrade_hit`, which does
/// a single `by_id` DashMap lookup and one `IndexEntry` clone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QueryHit {
    /// Event identity — used by `upgrade_hit` for the `by_id` lookup.
    pub(crate) event_id: u128,
    /// Globally monotonic commit order. Used for cursor position, visibility
    /// filtering, sort, and clock-range comparisons.
    pub(crate) global_sequence: u64,
    /// On-disk frame location. Sufficient for projection replay without a full
    /// `IndexEntry` clone.
    pub(crate) disk_pos: DiskPos,
    /// Event kind. Needed for secondary fact filter and projection kind match.
    pub(crate) kind: EventKind,
    /// Per-entity HLC clock. Needed for `Region::clock_range` filtering.
    pub(crate) clock: u32,
}

impl QueryHit {
    pub(crate) fn from_entry(entry: &IndexEntry) -> Self {
        Self {
            event_id: entry.event_id,
            global_sequence: entry.global_sequence,
            disk_pos: entry.disk_pos,
            kind: entry.kind,
            clock: entry.clock,
        }
    }
}

impl Ord for ClockKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.clock.cmp(&other.clock))
            .then(self.uuid.cmp(&other.uuid))
    }
}

impl PartialOrd for ClockKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl IndexEntry {
    /// Unique ID of the event.
    #[must_use]
    pub const fn event_id(&self) -> u128 {
        self.event_id
    }

    /// Correlation ID linking related events in a causal chain.
    #[must_use]
    pub const fn correlation_id(&self) -> u128 {
        self.correlation_id
    }

    /// ID of the event that caused this one; `None` for root-cause events.
    #[must_use]
    pub const fn causation_id(&self) -> Option<u128> {
        self.causation_id
    }

    /// Entity and scope coordinates for this event.
    #[must_use]
    pub const fn coord(&self) -> &Coordinate {
        &self.coord
    }

    /// Event kind (type discriminant).
    #[must_use]
    pub const fn event_kind(&self) -> EventKind {
        self.kind
    }

    /// HLC wall clock milliseconds for global causal ordering.
    #[must_use]
    pub const fn wall_ms(&self) -> u64 {
        self.wall_ms
    }

    /// Per-entity monotonic sequence number.
    #[must_use]
    pub const fn clock(&self) -> u32 {
        self.clock
    }

    /// Branch lane within the logical event DAG.
    #[must_use]
    pub const fn dag_lane(&self) -> u32 {
        self.dag_lane
    }

    /// Branch depth within the logical event DAG.
    #[must_use]
    pub const fn dag_depth(&self) -> u32 {
        self.dag_depth
    }

    /// Blake3 hash chain linking this event to its predecessor.
    #[must_use]
    pub const fn hash_chain(&self) -> &HashChain {
        &self.hash_chain
    }

    /// Location of the event frame on disk.
    #[must_use]
    pub const fn disk_pos(&self) -> DiskPos {
        self.disk_pos
    }

    /// Globally monotonic sequence number assigned at commit time.
    #[must_use]
    pub const fn global_sequence(&self) -> u64 {
        self.global_sequence
    }

    /// Opaque receipt extensions committed with this event.
    #[must_use]
    pub const fn receipt_extensions(&self) -> &BTreeMap<ExtensionKey, EncodedBytes> {
        &self.receipt_extensions
    }

    /// Returns `true` if this event is part of a causal chain (its correlation ID differs from its event ID).
    pub fn is_correlated(&self) -> bool {
        self.event_id != self.correlation_id
    }

    /// Returns `true` if this event was directly caused by the given event ID.
    pub fn is_caused_by(&self, event_id: crate::id::EventId) -> bool {
        use crate::id::EntityIdType;
        self.causation_id == Some(event_id.as_u128())
    }

    /// Returns `true` if this event has no causation ID (it is a root-cause event).
    pub fn is_root_cause(&self) -> bool {
        self.causation_id.is_none()
    }
}

#[cfg(test)]
mod entry_behavior_tests {
    use super::super::interner::InternId;
    use super::*;

    fn make_entry(event_id: u128, correlation_id: u128, causation_id: Option<u128>) -> IndexEntry {
        IndexEntry {
            event_id,
            correlation_id,
            causation_id,
            coord: Coordinate::new("entity:e", "scope:s").expect("coord"),
            entity_id: InternId::sentinel(),
            scope_id: InternId::sentinel(),
            kind: EventKind::custom(0xA, 7),
            wall_ms: 42,
            clock: 3,
            dag_lane: 1,
            dag_depth: 2,
            hash_chain: HashChain {
                prev_hash: [1u8; 32],
                event_hash: [2u8; 32],
            },
            disk_pos: DiskPos::new(11, 64, 128),
            global_sequence: 9,
            receipt_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn disk_pos_accessors_return_the_constructed_fields() {
        // Pins each accessor to its own field; swapping segment_id<->offset or
        // returning a constant would flip exactly one of these.
        let pos = DiskPos::new(7, 4096, 256);
        assert_eq!(pos.segment_id(), 7, "segment_id accessor");
        assert_eq!(pos.offset(), 4096, "offset accessor");
        assert_eq!(pos.length(), 256, "length accessor");
    }

    #[test]
    fn query_hit_from_entry_copies_the_correct_five_fields() {
        let entry = make_entry(0xDEAD, 0xBEEF, Some(0xCAFE));
        let hit = QueryHit::from_entry(&entry);
        assert_eq!(hit.event_id, 0xDEAD, "event_id must come from entry");
        assert_eq!(
            hit.global_sequence, 9,
            "global_sequence must come from entry"
        );
        assert_eq!(
            hit.disk_pos,
            DiskPos::new(11, 64, 128),
            "disk_pos preserved"
        );
        assert_eq!(hit.kind, EventKind::custom(0xA, 7), "kind preserved");
        assert_eq!(hit.clock, 3, "clock must come from entry, not wall_ms");
        // clock and wall_ms differ in the fixture, so a clock<->wall_ms swap fails.
        assert_ne!(u64::from(hit.clock), entry.wall_ms);
    }

    #[test]
    fn is_correlated_is_true_only_when_correlation_differs_from_event_id() {
        // Self-correlated root: event_id == correlation_id.
        let root = make_entry(100, 100, None);
        assert!(
            !root.is_correlated(),
            "a self-correlated root must NOT be reported as correlated"
        );
        // Reaction: correlation_id != event_id.
        let reaction = make_entry(101, 100, Some(100));
        assert!(
            reaction.is_correlated(),
            "an event whose correlation differs from its id IS correlated"
        );
    }

    #[test]
    fn is_root_cause_polarity_matches_causation_presence() {
        assert!(
            make_entry(1, 1, None).is_root_cause(),
            "no causation -> root cause"
        );
        assert!(
            !make_entry(2, 1, Some(1)).is_root_cause(),
            "present causation -> NOT root cause"
        );
    }

    #[test]
    fn is_caused_by_is_an_exact_match_not_a_has_any_cause_predicate() {
        let entry = make_entry(2, 1, Some(0x1111));
        assert!(
            entry.is_caused_by(crate::id::EventId::from(0x1111u128)),
            "exact causation id must match"
        );
        assert!(
            !entry.is_caused_by(crate::id::EventId::from(0x2222u128)),
            "a different id must NOT match (exactness)"
        );
        // A root-cause event is caused by nothing.
        assert!(!make_entry(1, 1, None).is_caused_by(crate::id::EventId::from(0u128)));
    }

    #[test]
    fn clock_key_tiebreaks_by_uuid_when_wall_and_clock_match() {
        // Equal wall_ms and clock: the uuid tiebreak decides ordering.
        let lo = ClockKey {
            wall_ms: 5,
            clock: 5,
            uuid: 1,
        };
        let hi = ClockKey {
            wall_ms: 5,
            clock: 5,
            uuid: 2,
        };
        assert!(lo < hi, "uuid tiebreak must order lower uuid first");
        // clock dominates uuid.
        let earlier_clock = ClockKey {
            wall_ms: 5,
            clock: 4,
            uuid: 9999,
        };
        assert!(
            earlier_clock < lo,
            "a smaller clock must sort before a larger clock regardless of uuid"
        );
    }
}
