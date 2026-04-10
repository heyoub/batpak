use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::columnar::ScanIndex;
use crate::store::config::IndexLayout;
use crate::store::interner::StringInterner;
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// StoreIndex: in-memory 2D index + auxiliaries. NOT persisted — rebuilt from segments on cold start.
/// [SPEC:src/store/index.rs]
/// [DEP:dashmap::DashMap] — see DEPENDENCY SURFACE for deadlock warnings
pub(crate) struct StoreIndex {
    /// Primary: entity -> ordered events. [DEP:dashmap::DashMap::get_mut] for insert.
    streams: DashMap<Arc<str>, BTreeMap<ClockKey, Arc<IndexEntry>>>,
    /// Scan index: either DashMap-based (AoS) or columnar (SoA/AoSoA).
    /// Handles by_fact and scope queries. When columnar, the DashMaps inside
    /// ScanIndex::Maps are replaced by contiguous arrays.
    pub(crate) scan: ScanIndex,
    /// Point lookup: event_id -> entry. O(1) get by ID.
    by_id: DashMap<u128, Arc<IndexEntry>>,
    /// Chain head: entity -> latest IndexEntry. For prev_hash in writer step 2.
    latest: DashMap<Arc<str>, Arc<IndexEntry>>,
    /// Monotonic counter. Foundation for cursors, checkpoints, exactly-once.
    global_sequence: AtomicU64,
    /// Total event count.
    len: AtomicUsize,
    /// Per-entity write locks. Writer step 1 acquires these.
    /// [SPEC:IMPLEMENTATION NOTES item 5 — DashMap guard lifetimes]
    pub(crate) entity_locks: DashMap<Arc<str>, Arc<parking_lot::Mutex<()>>>,
    /// String interner for compact index keys and checkpoint serialization.
    /// Entity and scope strings are interned on insert; IDs are used by
    /// checkpoint and (future) InternId-based IndexEntry fields.
    pub(crate) interner: Arc<StringInterner>,
}

/// ClockKey: BTreeMap key. Ord: wall_ms-first, then clock, then uuid tiebreak.
/// wall_ms enables global causal ordering across entities (HLC layer 1).
/// [SPEC:IMPLEMENTATION NOTES item 1]

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockKey {
    /// HLC wall clock milliseconds — global ordering across entities.
    pub wall_ms: u64,
    /// Per-entity monotonic sequence number used as the HLC logical counter.
    pub clock: u32,
    /// Event UUID tiebreaker for deterministic ordering within the same clock tick.
    pub uuid: u128,
}

/// IndexEntry: everything needed for index queries without disk reads.
/// Shared via `Arc` across all index maps — one allocation per event.
#[derive(Clone, Debug)]
pub struct IndexEntry {
    /// Unique ID of the event.
    pub event_id: u128,
    /// Correlation ID linking related events in a causal chain.
    pub correlation_id: u128,
    /// ID of the event that caused this one; `None` for root-cause events.
    pub causation_id: Option<u128>,
    /// Entity and scope coordinates for this event.
    pub coord: Coordinate,
    /// Interned entity string ID for compact checkpoint serialization.
    pub(crate) entity_id: crate::store::interner::InternId,
    /// Interned scope string ID for compact checkpoint serialization.
    pub(crate) scope_id: crate::store::interner::InternId,
    /// Event kind (type discriminant).
    pub kind: EventKind,
    /// HLC wall clock milliseconds — for global causal ordering.
    pub wall_ms: u64,
    /// Per-entity monotonic sequence number.
    pub clock: u32,
    /// Blake3 hash chain linking this event to its predecessor.
    pub hash_chain: HashChain,
    /// Location of the event frame on disk.
    pub disk_pos: DiskPos,
    /// Globally monotonic sequence number assigned at commit time.
    pub global_sequence: u64,
}

/// DiskPos: where to find this event on disk.
#[derive(Clone, Copy, Debug)]
pub struct DiskPos {
    /// Numeric identifier of the segment file containing this event.
    pub segment_id: u64,
    /// Byte offset of the frame within the segment file.
    pub offset: u64,
    /// Total byte length of the encoded frame.
    pub length: u32,
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
    /// Returns `true` if this event is part of a causal chain (its correlation ID differs from its event ID).
    pub fn is_correlated(&self) -> bool {
        self.event_id != self.correlation_id
    }

    /// Returns `true` if this event was directly caused by the given event ID.
    pub fn is_caused_by(&self, event_id: u128) -> bool {
        self.causation_id == Some(event_id)
    }

    /// Returns `true` if this event has no causation ID (it is a root-cause event).
    pub fn is_root_cause(&self) -> bool {
        self.causation_id.is_none()
    }
}

impl StoreIndex {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_layout(&IndexLayout::default())
    }

    /// Create a StoreIndex with the specified scan index layout.
    pub(crate) fn with_layout(layout: &IndexLayout) -> Self {
        Self {
            streams: DashMap::new(),
            scan: ScanIndex::for_layout(layout),
            by_id: DashMap::new(),
            latest: DashMap::new(),
            global_sequence: AtomicU64::new(0),
            len: AtomicUsize::new(0),
            entity_locks: DashMap::new(),
            interner: Arc::new(StringInterner::new()),
        }
    }

    /// Called by writer step 9. Inserts into ALL indexes atomically.
    /// CONSTRAINT: caller MUST hold the entity lock before calling this.
    /// [SPEC:IMPLEMENTATION NOTES item 5]
    pub(crate) fn insert(&self, entry: IndexEntry) {
        let entity = entry.coord.entity_arc();

        // Intern entity and scope strings. IDs stored in IndexEntry for
        // compact checkpoint serialization and future InternId-only index.
        debug_assert_eq!(entry.entity_id, self.interner.intern(entry.coord.entity()));
        debug_assert_eq!(entry.scope_id, self.interner.intern(entry.coord.scope()));

        let key = ClockKey {
            wall_ms: entry.wall_ms,
            clock: entry.clock,
            uuid: entry.event_id,
        };

        // Arc: one allocation, shared across all maps.
        let arc_entry = Arc::new(entry);

        // Primary index: entity -> BTreeMap
        // [DEP:dashmap::DashMap::entry] — holds write lock, release fast
        self.streams
            .entry(Arc::clone(&entity))
            .or_default()
            .insert(key, Arc::clone(&arc_entry));

        // Scan index: by_fact + scope (DashMap or columnar depending on layout)
        self.scan.insert(&arc_entry);

        // Point lookup
        self.by_id
            .insert(arc_entry.event_id, Arc::clone(&arc_entry));

        // Chain head
        self.latest.insert(entity, arc_entry);

        // Counters — Release ordering sufficient for single-writer
        self.global_sequence.fetch_add(1, Ordering::Release);
        self.len.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomic batch insert: all entries become visible together.
    /// [SPEC:src/store/index.rs — insert_batch]
    pub(crate) fn insert_batch(&self, entries: Vec<IndexEntry>) {
        if entries.is_empty() {
            return;
        }

        // Pre-allocate Arcs to minimize work under locks.
        let arc_entries: Vec<Arc<IndexEntry>> = entries.into_iter().map(Arc::new).collect();

        // Insert all entries. Since we have a single writer thread,
        // no other inserts can interleave. Readers will see all or none
        // depending on when they query relative to this loop.
        for arc_entry in &arc_entries {
            let entity = arc_entry.coord.entity_arc();
            let key = ClockKey {
                wall_ms: arc_entry.wall_ms,
                clock: arc_entry.clock,
                uuid: arc_entry.event_id,
            };

            // Primary index: entity -> BTreeMap
            self.streams
                .entry(Arc::clone(&entity))
                .or_default()
                .insert(key, Arc::clone(arc_entry));

            // Scan index
            self.scan.insert(arc_entry);

            // Point lookup
            self.by_id.insert(arc_entry.event_id, Arc::clone(arc_entry));

            // Chain head
            self.latest.insert(entity, Arc::clone(arc_entry));

            // Global sequence already reserved during batch staging
            self.len.fetch_add(1, Ordering::Relaxed);
        }

        // Single global_sequence bump for the batch (already reserved per-entry during staging)
        self.global_sequence
            .fetch_add(arc_entries.len() as u64, Ordering::Release);
    }

    pub(crate) fn get_by_id(&self, event_id: u128) -> Option<IndexEntry> {
        self.by_id
            .get(&event_id)
            .map(|r| r.value().as_ref().clone())
    }

    pub(crate) fn get_latest(&self, entity: &str) -> Option<IndexEntry> {
        self.latest.get(entity).map(|r| r.value().as_ref().clone())
    }

    pub(crate) fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        self.streams
            .get(entity)
            .map(|r| r.value().values().map(|arc| arc.as_ref().clone()).collect())
            .unwrap_or_default()
    }

    pub(crate) fn query(&self, region: &crate::coordinate::Region) -> Vec<IndexEntry> {
        // Region query strategy:
        // 1. Determine candidate set based on most selective filter
        // 2. Apply remaining filters to narrow results
        // 3. Apply clock_range last (it's per-entity, cheap)
        use crate::coordinate::KindFilter;

        let mut candidates: Vec<IndexEntry> = if let Some(ref prefix) = region.entity_prefix {
            // Entity prefix → scan streams map for matching keys
            self.streams
                .iter()
                .filter(|r| r.key().as_ref().starts_with(prefix.as_ref()))
                .flat_map(|r| {
                    r.value()
                        .values()
                        .map(|arc| arc.as_ref().clone())
                        .collect::<Vec<_>>()
                })
                .collect()
        } else if let Some(ref scope) = region.scope {
            // Scope → delegate to scan index
            let scope_entries = self.scan.query_by_scope(scope.as_ref());
            if !scope_entries.is_empty() {
                scope_entries
                    .into_iter()
                    .map(|arc| arc.as_ref().clone())
                    .collect()
            } else {
                // Fallback for Maps mode: look up entities in scope, collect their streams
                if let Some(entities) = self.scan.scope_entity_set(scope.as_ref()) {
                    entities
                        .iter()
                        .flat_map(|entity| {
                            self.streams
                                .get(entity.as_ref())
                                .map(|r| {
                                    r.value()
                                        .values()
                                        .map(|arc| arc.as_ref().clone())
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
        } else if let Some(ref fact) = region.fact {
            // Fact filter → delegate to scan index for Exact kind
            match fact {
                KindFilter::Exact(k) => {
                    let results = self.scan.query_by_kind(*k);
                    if !results.is_empty() {
                        results
                            .into_iter()
                            .map(|arc| arc.as_ref().clone())
                            .collect()
                    } else {
                        // Empty could mean AoS mode with no events of this kind — that's correct
                        Vec::new()
                    }
                }
                KindFilter::Category(c) => {
                    let results = self.scan.query_by_category(*c);
                    results
                        .into_iter()
                        .map(|arc| arc.as_ref().clone())
                        .collect()
                }
                KindFilter::Any => self
                    .streams
                    .iter()
                    .flat_map(|r| {
                        r.value()
                            .values()
                            .map(|arc| arc.as_ref().clone())
                            .collect::<Vec<_>>()
                    })
                    .collect(),
            }
        } else {
            // Region::all() with no filters — return everything
            self.streams
                .iter()
                .flat_map(|r| {
                    r.value()
                        .values()
                        .map(|arc| arc.as_ref().clone())
                        .collect::<Vec<_>>()
                })
                .collect()
        };

        // Apply remaining filters that weren't used for the initial candidate set.

        // Scope filter (if entity_prefix was the primary selector)
        if region.entity_prefix.is_some() {
            if let Some(ref scope) = region.scope {
                candidates.retain(|e| e.coord.scope() == scope.as_ref());
            }
        }

        // Fact filter (if not already applied)
        if region.entity_prefix.is_some() || region.scope.is_some() {
            if let Some(ref fact) = region.fact {
                candidates.retain(|e| match fact {
                    KindFilter::Exact(k) => e.kind == *k,
                    KindFilter::Category(c) => e.kind.category() == *c,
                    KindFilter::Any => true,
                });
            }
        }

        // Clock range filter (always per-entity clock, not global_sequence)
        if let Some((min, max)) = region.clock_range {
            candidates.retain(|e| e.clock >= min && e.clock <= max);
        }

        // Sort by global_sequence for consistent ordering
        candidates.sort_by_key(|e| e.global_sequence);
        candidates
    }

    /// Return a snapshot of all entries in the index, collected from `by_id`.
    ///
    /// Used by `checkpoint::write_checkpoint` to serialise the full index.
    /// DashMap iteration is not a linearisable snapshot, but that is acceptable
    /// because checkpoints are always written from a quiesced write path.
    pub(crate) fn all_entries(&self) -> Vec<IndexEntry> {
        self.by_id
            .iter()
            .map(|r| r.value().as_ref().clone())
            .collect()
    }

    pub(crate) fn global_sequence(&self) -> u64 {
        self.global_sequence.load(Ordering::Acquire)
    }

    pub(crate) fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }

    /// Clear all indexes for a full rebuild (e.g. after compaction).
    pub(crate) fn clear(&self) {
        self.streams.clear();
        self.scan.clear();
        self.by_id.clear();
        self.latest.clear();
        self.global_sequence.store(0, Ordering::Release);
        self.len.store(0, Ordering::Relaxed);
        // entity_locks intentionally NOT cleared — writer may hold references
    }
}
