use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::columnar::ScanIndex;
use crate::store::config::IndexLayout;
use crate::store::interner::StringInterner;
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Gated publish boundary for reader visibility.
///
/// `allocated` advances when sequences are reserved (writer-only).
/// `visible` is the exclusive upper bound readers filter against:
/// an entry is visible iff `entry.global_sequence < visible`.
///
/// Invariant: `visible <= allocated` (enforced by `debug_assert` in `publish`).
pub(crate) struct SequenceGate {
    /// Next sequence to be assigned. Only the writer thread advances this.
    allocated: AtomicU64,
    /// Exclusive upper bound for reader visibility. Entries with
    /// `global_sequence < visible` are returned by read methods.
    visible: AtomicU64,
}

impl SequenceGate {
    pub(crate) fn new() -> Self {
        Self {
            allocated: AtomicU64::new(0),
            visible: AtomicU64::new(0),
        }
    }

    /// Reserve `n` sequences. Returns first in `[first, first + n)`.
    pub(crate) fn reserve(&self, n: u64) -> u64 {
        self.allocated.fetch_add(n, Ordering::AcqRel)
    }

    /// Advance visibility so readers see entries with `global_sequence < up_to`.
    ///
    /// # Panics (debug)
    ///
    /// Panics if `up_to` exceeds the allocated counter or regresses below
    /// the current visible watermark.
    #[allow(clippy::panic)] // correctness invariant, not a recoverable error
    pub(crate) fn publish(&self, up_to: u64) {
        assert!(
            up_to <= self.allocated.load(Ordering::Acquire),
            "publish({up_to}) exceeds allocated({})",
            self.allocated.load(Ordering::Acquire),
        );
        assert!(
            up_to >= self.visible.load(Ordering::Acquire),
            "publish({up_to}) regresses below visible({})",
            self.visible.load(Ordering::Acquire),
        );
        self.visible.store(up_to, Ordering::Release);
    }

    /// Current visibility watermark (exclusive upper bound).
    pub(crate) fn visible(&self) -> u64 {
        self.visible.load(Ordering::Acquire)
    }

    /// Current allocator position (next sequence to be assigned).
    pub(crate) fn allocated(&self) -> u64 {
        self.allocated.load(Ordering::Acquire)
    }

    /// Advance allocator by 1. Used by `insert()` for the single-event path.
    pub(crate) fn advance(&self) {
        self.allocated.fetch_add(1, Ordering::Release);
    }

    /// Set the allocator to a specific value during checkpoint restore.
    ///
    /// Checkpoint stores the allocator position at write time (which may
    /// be higher than `entry_count` due to burned batch slots). On restore,
    /// `insert()` calls `advance()` per entry, but the allocator must end
    /// at the checkpointed value — not at the entry count.
    pub(crate) fn restore_allocator(&self, value: u64) {
        self.allocated.store(value, Ordering::Release);
    }

    /// Reset both counters to 0 (used by `clear()` during rebuild/compaction).
    pub(crate) fn clear(&self) {
        self.allocated.store(0, Ordering::Release);
        self.visible.store(0, Ordering::Release);
    }
}

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
    /// Gated sequence counter: allocator + visibility watermark.
    /// Replaces the former bare `global_sequence: AtomicU64`.
    pub(crate) sequence: SequenceGate,
    /// Total event count.
    len: AtomicUsize,
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
            sequence: SequenceGate::new(),
            len: AtomicUsize::new(0),
            interner: Arc::new(StringInterner::new()),
        }
    }

    /// Reserve N global sequences for batch staging.
    /// Returns the first sequence number; caller allocates `[first, first + n)`.
    /// Used by writer to pre-assign sequences before writing to disk.
    pub(crate) fn reserve_sequences(&self, n: u64) -> u64 {
        self.sequence.reserve(n)
    }

    /// Called by writer step 9. Inserts into ALL indexes atomically.
    /// Caller must be the single writer thread; this is the only writer of
    /// the index, so no per-entity lock is needed.
    /// Advances the allocator by one — used by the live single-event append path.
    pub(crate) fn insert(&self, entry: IndexEntry) {
        self.insert_inner(entry);
        // Advance allocator (visibility is advanced separately by publish()).
        self.sequence.advance();
    }

    /// Replay-only insert. Identical to `insert()` except the allocator is
    /// **not** advanced. The caller (a [`ReplayCursor`]) is responsible for
    /// restoring the allocator to the correct value once all replay entries
    /// have been inserted, so sparse `global_sequence` values from disk are
    /// preserved verbatim.
    pub(crate) fn insert_replay(&self, entry: IndexEntry) {
        self.insert_inner(entry);
        // Allocator advance intentionally omitted — see ReplayCursor::commit.
    }

    fn insert_inner(&self, entry: IndexEntry) {
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

            // Global sequence already reserved during batch staging via reserve_sequences()
            self.len.fetch_add(1, Ordering::Relaxed);
        }

        // Global sequence already reserved during batch staging via reserve_sequences()
        // No additional fetch_add needed.
    }

    pub(crate) fn get_by_id(&self, event_id: u128) -> Option<IndexEntry> {
        let vis = self.sequence.visible();
        self.by_id
            .get(&event_id)
            .map(|r| r.value().as_ref().clone())
            .filter(|e| e.global_sequence < vis)
    }

    /// Returns the latest entry for `entity`, filtered by visibility.
    ///
    /// **Transient behavior during batch insert:** Between `insert_batch()`
    /// and `publish()`, the `latest` map may contain an entry whose sequence
    /// exceeds the visibility watermark. This method filters it out, which
    /// can transiently return `None` even when visible entries exist in
    /// `streams`. The window is sub-microsecond (single writer, publish is
    /// the next instruction). The writer calls this only BEFORE `insert_batch()`,
    /// so it always sees previously-published state.
    pub(crate) fn get_latest(&self, entity: &str) -> Option<IndexEntry> {
        let vis = self.sequence.visible();
        self.latest
            .get(entity)
            .map(|r| r.value().as_ref().clone())
            .filter(|e| e.global_sequence < vis)
    }

    pub(crate) fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        let vis = self.sequence.visible();
        self.streams
            .get(entity)
            .map(|r| {
                r.value()
                    .values()
                    .filter(|arc| arc.global_sequence < vis)
                    .map(|arc| arc.as_ref().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn query(&self, region: &crate::coordinate::Region) -> Vec<IndexEntry> {
        // Region query strategy:
        // 1. Determine candidate set based on most selective filter
        // 2. Apply remaining filters to narrow results
        // 3. Filter by visibility watermark
        // 4. Apply clock_range last (it's per-entity, cheap)
        use crate::coordinate::KindFilter;
        let vis = self.sequence.visible();

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

        // Visibility watermark: exclude entries not yet published.
        candidates.retain(|e| e.global_sequence < vis);

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

    /// Current allocator position (next sequence to be assigned).
    /// Used by checkpoint, rebuild, writer, and stats/diagnostics.
    pub(crate) fn global_sequence(&self) -> u64 {
        self.sequence.allocated()
    }

    /// Current visibility watermark (exclusive upper bound).
    /// Entries with `global_sequence < visible_sequence()` are returned by read methods.
    pub(crate) fn visible_sequence(&self) -> u64 {
        self.sequence.visible()
    }

    /// Advance the visibility watermark so readers can observe entries
    /// with `global_sequence < up_to`.
    ///
    /// Called by the writer after `insert()` or `insert_batch()`, and by
    /// checkpoint restore / index rebuild after all entries are loaded.
    pub(crate) fn publish(&self, up_to: u64) {
        self.sequence.publish(up_to);
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
        self.sequence.clear();
        self.len.store(0, Ordering::Relaxed);
    }

    /// Begin a replay session against this index. Use this for checkpoint
    /// restore and segment rebuild paths to preserve sparse `global_sequence`
    /// values from durable sources (SIDX footers / checkpoint payload) while
    /// synthesizing sequences for entries with no durable source.
    ///
    /// The returned [`ReplayCursor`] holds an exclusive borrow of the index
    /// and **must** be `commit()`-ed to publish entries and restore the
    /// allocator. Forgetting to commit leaves the index unpublished — the
    /// `Drop` impl emits a debug-mode panic to catch this in tests.
    pub(crate) fn begin_replay(&self) -> ReplayCursor<'_> {
        ReplayCursor {
            index: self,
            max_seen: 0,
            committed: false,
        }
    }
}

/// Type-safe replay session over a [`StoreIndex`].
///
/// `ReplayCursor` exists so the borrow checker enforces three things at the
/// type level:
///
/// 1. Replay entries cannot escape the lifetime of the index they target
///    (the cursor borrows `&'a StoreIndex`).
/// 2. Sequence assignment, allocator restoration, and visibility publish
///    are coupled — `commit()` consumes the cursor and performs all three
///    in one shot, so callers cannot publish without restoring the allocator
///    or vice versa.
/// 3. Forgetting to call `commit()` is detected at debug time via `Drop`,
///    which prevents silently leaving the index in an unpublished state.
///
/// **Sequence preservation policy:**
/// - If `entry.global_sequence` is provided by the caller (e.g. from a SIDX
///   footer or a checkpoint blob), it is preserved verbatim. The cursor
///   tracks the maximum seen value.
/// - The caller of [`Self::insert`] is responsible for setting
///   `entry.global_sequence` before calling. For sources without a durable
///   sequence, [`Self::synthesize_next`] returns the next free slot above
///   the running max.
pub(crate) struct ReplayCursor<'a> {
    index: &'a StoreIndex,
    /// Highest `global_sequence` observed so far across all inserted entries.
    /// After `commit`, the allocator is set to `max_seen + 1` (or higher if
    /// the caller passes a hint via [`Self::commit_with_allocator_hint`]).
    max_seen: u64,
    committed: bool,
}

impl<'a> ReplayCursor<'a> {
    /// Insert a fully-built entry whose `global_sequence` has already been
    /// chosen by the caller (e.g. read from a SIDX footer or checkpoint blob).
    ///
    /// The cursor records the sequence in its running maximum so the
    /// allocator can be restored correctly at commit time.
    pub(crate) fn insert(&mut self, entry: IndexEntry) {
        self.max_seen = self.max_seen.max(entry.global_sequence);
        self.index.insert_replay(entry);
    }

    /// Allocate the next sequence above the cursor's running maximum.
    /// Used for replay sources that have no durable `global_sequence`
    /// (e.g. slow-path scans of an active or footerless segment).
    ///
    /// Calling this method does not insert anything; the caller is expected
    /// to use the returned value to populate `IndexEntry::global_sequence`
    /// and then call [`Self::insert`] with the populated entry.
    pub(crate) fn synthesize_next(&mut self) -> u64 {
        // Don't update max_seen here — insert() will, once the entry is built.
        self.max_seen.saturating_add(1)
    }

    /// Finish the replay session.
    ///
    /// Restores the allocator to `max_seen + 1` (or `hint` if higher), then
    /// publishes that value as the visibility watermark so all replayed
    /// entries become visible to readers atomically.
    ///
    /// `hint` is used by checkpoint restore to preserve burned-slot
    /// allocator positions: the checkpoint stores the original allocator
    /// value, which may be greater than `max(entry.global_sequence)` because
    /// some sequence slots were reserved for batches that later failed.
    /// Pass `0` if there is no hint (i.e. segment rebuild).
    pub(crate) fn commit(mut self, hint: u64) {
        let next = self.max_seen.saturating_add(1).max(hint);
        self.index.sequence.restore_allocator(next);
        self.index.publish(next);
        self.committed = true;
    }

    /// Abandon the replay session without publishing.
    ///
    /// Use this on error paths where partial replay state should not become
    /// visible to readers. The allocator and visibility watermark are left
    /// untouched. Any entries already inserted via [`Self::insert`] remain
    /// in the underlying index maps but are unreachable until a later
    /// successful replay publishes a watermark covering them.
    ///
    /// This is the explicit "I'm bailing out" signal that suppresses the
    /// `Drop` debug-assertion designed to catch forgotten `commit()` calls.
    pub(crate) fn abort(mut self) {
        self.committed = true;
    }
}

impl<'a> Drop for ReplayCursor<'a> {
    fn drop(&mut self) {
        // In debug builds, catch programmer errors where the cursor is
        // dropped without commit() being called. In release builds the
        // index is left unpublished, which is the safest possible state
        // (readers see nothing).
        debug_assert!(
            self.committed,
            "ReplayCursor dropped without calling commit() — index is unpublished",
        );
    }
}
