use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::columnar::{CachedProjectionSlot, ScanIndex};
use crate::store::config::IndexConfig;
use crate::store::interner::StringInterner;
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::any::TypeId;
use std::collections::{BTreeMap, HashMap};
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
    /// Currently active visibility fence token, or 0 when no fence is active.
    active_fence: AtomicU64,
    /// Lowest sequence staged into the active fence, or `u64::MAX` if the
    /// fence has not yet staged any entries.
    active_fence_start: AtomicU64,
    /// Exclusive upper bound of the highest sequence staged into the active fence.
    active_fence_end: AtomicU64,
    /// Monotonic token allocator for visibility fences.
    next_fence_token: AtomicU64,
    /// Permanently hidden fence ranges cancelled in the current runtime.
    /// Stored as an immutable `Arc` snapshot so that readers pay only a
    /// refcount bump instead of cloning the whole vec on every query.
    cancelled_ranges: RwLock<Arc<Vec<(u64, u64)>>>,
}

#[derive(Clone, Debug)]
struct VisibilitySnapshot {
    visible: u64,
    cancelled_ranges: Arc<Vec<(u64, u64)>>,
}

impl VisibilitySnapshot {
    fn is_visible(&self, sequence: u64) -> bool {
        if sequence >= self.visible {
            return false;
        }
        !self
            .cancelled_ranges
            .iter()
            .any(|(start, end)| sequence >= *start && sequence < *end)
    }
}

impl SequenceGate {
    fn insert_cancelled_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
        if start >= end {
            return;
        }
        ranges.push((start, end));
        ranges.sort_by_key(|(range_start, _)| *range_start);

        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for (range_start, range_end) in ranges.drain(..) {
            if let Some((_, merged_end)) = merged.last_mut() {
                if range_start <= *merged_end {
                    *merged_end = (*merged_end).max(range_end);
                    continue;
                }
            }
            merged.push((range_start, range_end));
        }
        *ranges = merged;
    }

    pub(crate) fn new() -> Self {
        Self {
            allocated: AtomicU64::new(0),
            visible: AtomicU64::new(0),
            active_fence: AtomicU64::new(0),
            active_fence_start: AtomicU64::new(u64::MAX),
            active_fence_end: AtomicU64::new(0),
            next_fence_token: AtomicU64::new(1),
            cancelled_ranges: RwLock::new(Arc::new(Vec::new())),
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
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        self.next_fence_token.store(1, Ordering::Release);
        *self.cancelled_ranges.write() = Arc::new(Vec::new());
    }

    pub(crate) fn begin_fence(&self) -> Result<u64, crate::store::StoreError> {
        let token = self.next_fence_token.fetch_add(1, Ordering::AcqRel);
        match self
            .active_fence
            .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                self.active_fence_start.store(u64::MAX, Ordering::Release);
                self.active_fence_end.store(0, Ordering::Release);
                Ok(token)
            }
            Err(_) => Err(crate::store::StoreError::VisibilityFenceActive),
        }
    }

    pub(crate) fn active_fence_token(&self) -> Option<u64> {
        let token = self.active_fence.load(Ordering::Acquire);
        (token != 0).then_some(token)
    }

    pub(crate) fn note_fence_progress(
        &self,
        token: u64,
        start: u64,
        end: u64,
    ) -> Result<(), crate::store::StoreError> {
        if self.active_fence.load(Ordering::Acquire) != token {
            return Err(crate::store::StoreError::VisibilityFenceNotActive);
        }
        let _ =
            self.active_fence_start
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.min(start))
                });
        let _ =
            self.active_fence_end
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.max(end))
                });
        Ok(())
    }

    pub(crate) fn finish_fence(
        &self,
        token: u64,
        publish_to: Option<u64>,
    ) -> Result<(), crate::store::StoreError> {
        if self.active_fence.load(Ordering::Acquire) != token {
            return Err(crate::store::StoreError::VisibilityFenceNotActive);
        }
        if let Some(up_to) = publish_to {
            self.publish(up_to);
        }
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        Ok(())
    }

    pub(crate) fn cancel_fence(&self, token: u64) -> Result<(), crate::store::StoreError> {
        if self.active_fence.load(Ordering::Acquire) != token {
            return Err(crate::store::StoreError::VisibilityFenceNotActive);
        }
        let start = self.active_fence_start.load(Ordering::Acquire);
        let end = self.active_fence_end.load(Ordering::Acquire);
        if start != u64::MAX && start < end {
            let mut guard = self.cancelled_ranges.write();
            let mut ranges = (**guard).clone();
            Self::insert_cancelled_range(&mut ranges, start, end);
            *guard = Arc::new(ranges);
        }
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        Ok(())
    }

    fn snapshot(&self) -> VisibilitySnapshot {
        VisibilitySnapshot {
            visible: self.visible.load(Ordering::Acquire),
            cancelled_ranges: Arc::clone(&self.cancelled_ranges.read()),
        }
    }

    pub(crate) fn cancelled_ranges_snapshot(&self) -> Vec<(u64, u64)> {
        self.cancelled_ranges.read().as_ref().clone()
    }

    pub(crate) fn restore_cancelled_ranges(&self, ranges: Vec<(u64, u64)>) {
        let mut built = Vec::new();
        for (start, end) in ranges {
            Self::insert_cancelled_range(&mut built, start, end);
        }
        *self.cancelled_ranges.write() = Arc::new(built);
    }
}

/// StoreIndex: in-memory 2D index + auxiliaries. Not persisted; rebuilt from segments on cold start.
/// [DEP:dashmap::DashMap] — see DEPENDENCY SURFACE for deadlock warnings
pub(crate) struct StoreIndex {
    /// Primary: entity -> ordered events. [DEP:dashmap::DashMap::get_mut] for insert.
    streams: DashMap<Arc<str>, BTreeMap<ClockKey, Arc<IndexEntry>>>,
    /// Base AoS scan maps plus optional overlay views.
    /// Handles by_fact and scope queries while keeping the live topology honest:
    /// the base maps always exist and configured overlays fan out in parallel.
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
/// `wall_ms` enables global causal ordering across entities (HLC layer 1).

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
    /// Branch lane within the logical event DAG.
    pub dag_lane: u32,
    /// Branch depth within the logical event DAG.
    pub dag_depth: u32,
    /// Blake3 hash chain linking this event to its predecessor.
    pub hash_chain: HashChain,
    /// Location of the event frame on disk.
    pub disk_pos: DiskPos,
    /// Globally monotonic sequence number assigned at commit time.
    pub global_sequence: u64,
}

/// DiskPos: where to find this event on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskPos {
    /// Numeric identifier of the segment file containing this event.
    pub segment_id: u64,
    /// Byte offset of the frame within the segment file.
    pub offset: u64,
    /// Total byte length of the encoded frame.
    pub length: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionReplayItem {
    pub(crate) global_sequence: u64,
    pub(crate) disk_pos: DiskPos,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionReplayPlan {
    pub(crate) watermark: u64,
    pub(crate) generation: u64,
    pub(crate) items: Vec<ProjectionReplayItem>,
}

/// One contiguous run of entries for the same entity inside the
/// restore-time entity-partitioned ordering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EntityRun {
    pub(crate) entity: String,
    pub(crate) start: u64,
    pub(crate) len: u64,
    pub(crate) first_sequence: u64,
    pub(crate) last_sequence: u64,
}

/// One contiguous chunk of restore-time sequence-sorted entries.
///
/// Chunks are persisted into snapshot artifacts so decode work can be split
/// deterministically without re-deriving ranges from scratch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestoreChunkSummary {
    pub(crate) start: u64,
    pub(crate) len: u64,
    pub(crate) first_sequence: u64,
    pub(crate) last_sequence: u64,
}

/// Restore-time routing summary shared across planner, rebuild, and
/// view materialization. This is intentionally cheap and serializable so the
/// same summary shape can later cross process boundaries without redesign.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RoutingSummary {
    pub(crate) entry_count: u64,
    pub(crate) chunk_count: u64,
    pub(crate) chunks: Vec<RestoreChunkSummary>,
    pub(crate) entity_runs: Vec<EntityRun>,
}

struct RestoreBase {
    entries_by_sequence: Vec<Arc<IndexEntry>>,
    entries_by_entity: Vec<Arc<IndexEntry>>,
    routing: RoutingSummary,
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

impl RestoreBase {
    fn from_sorted_entries(
        entries: Vec<IndexEntry>,
        chunk_count: usize,
        routing_hint: Option<&RoutingSummary>,
    ) -> Self {
        let entries_by_sequence: Vec<Arc<IndexEntry>> = entries.into_iter().map(Arc::new).collect();
        let mut entries_by_entity = entries_by_sequence.clone();
        entries_by_entity.sort_by(|left, right| {
            left.coord
                .entity()
                .cmp(right.coord.entity())
                .then(left.wall_ms.cmp(&right.wall_ms))
                .then(left.clock.cmp(&right.clock))
                .then(left.event_id.cmp(&right.event_id))
        });

        Self {
            routing: routing_hint
                .filter(|routing| routing.validate(&entries_by_sequence, &entries_by_entity))
                .cloned()
                .unwrap_or_else(|| {
                    RoutingSummary::from_entries(
                        &entries_by_sequence,
                        &entries_by_entity,
                        chunk_count,
                    )
                }),
            entries_by_sequence,
            entries_by_entity,
        }
    }
}

impl RoutingSummary {
    pub(crate) fn from_sorted_entries(entries: &[IndexEntry], chunk_count: usize) -> Self {
        let arcs: Vec<Arc<IndexEntry>> = entries.iter().cloned().map(Arc::new).collect();
        let mut entity_sorted = arcs;
        entity_sorted.sort_by(|left, right| {
            left.coord
                .entity()
                .cmp(right.coord.entity())
                .then(left.wall_ms.cmp(&right.wall_ms))
                .then(left.clock.cmp(&right.clock))
                .then(left.event_id.cmp(&right.event_id))
        });
        Self::from_entries(
            &entries.iter().cloned().map(Arc::new).collect::<Vec<_>>(),
            &entity_sorted,
            chunk_count,
        )
    }

    fn from_entries(
        entries_by_sequence: &[Arc<IndexEntry>],
        entries_by_entity: &[Arc<IndexEntry>],
        chunk_count: usize,
    ) -> Self {
        let chunk_count = chunk_count.max(1);
        let mut entity_runs = Vec::new();
        let mut cursor = 0usize;
        while cursor < entries_by_entity.len() {
            let entity = entries_by_entity[cursor].coord.entity().to_owned();
            let start = cursor;
            let first_sequence = entries_by_entity[cursor].global_sequence;
            while cursor < entries_by_entity.len()
                && entries_by_entity[cursor].coord.entity() == entity.as_str()
            {
                cursor += 1;
            }
            let last_sequence = entries_by_entity[cursor - 1].global_sequence;
            entity_runs.push(EntityRun {
                entity,
                start: start as u64,
                len: (cursor - start) as u64,
                first_sequence,
                last_sequence,
            });
        }

        let mut chunks = Vec::new();
        if !entries_by_sequence.is_empty() {
            let base = entries_by_sequence.len() / chunk_count;
            let remainder = entries_by_sequence.len() % chunk_count;
            let mut start = 0usize;
            for chunk_index in 0..chunk_count {
                let len = base + usize::from(chunk_index < remainder);
                if len == 0 {
                    continue;
                }
                let end = start + len;
                let first_sequence = entries_by_sequence[start].global_sequence;
                let last_sequence = entries_by_sequence[end - 1].global_sequence;
                chunks.push(RestoreChunkSummary {
                    start: start as u64,
                    len: len as u64,
                    first_sequence,
                    last_sequence,
                });
                start = end;
            }
        }

        Self {
            entry_count: entries_by_entity.len() as u64,
            chunk_count: chunks.len() as u64,
            chunks,
            entity_runs,
        }
    }

    pub(crate) fn validate(
        &self,
        entries_by_sequence: &[Arc<IndexEntry>],
        entries_by_entity: &[Arc<IndexEntry>],
    ) -> bool {
        if self.entry_count != entries_by_sequence.len() as u64
            || self.entry_count != entries_by_entity.len() as u64
        {
            return false;
        }

        let mut chunk_total = 0usize;
        for chunk in &self.chunks {
            let start = match usize::try_from(chunk.start) {
                Ok(start) => start,
                Err(_) => return false,
            };
            let len = match usize::try_from(chunk.len) {
                Ok(len) => len,
                Err(_) => return false,
            };
            let end = match start.checked_add(len) {
                Some(end) => end,
                None => return false,
            };
            if len == 0 || end > entries_by_sequence.len() {
                return false;
            }
            if entries_by_sequence[start].global_sequence != chunk.first_sequence
                || entries_by_sequence[end - 1].global_sequence != chunk.last_sequence
            {
                return false;
            }
            chunk_total += len;
        }
        if chunk_total != entries_by_sequence.len() {
            return false;
        }

        let mut run_total = 0usize;
        for run in &self.entity_runs {
            let start = match usize::try_from(run.start) {
                Ok(start) => start,
                Err(_) => return false,
            };
            let len = match usize::try_from(run.len) {
                Ok(len) => len,
                Err(_) => return false,
            };
            let end = match start.checked_add(len) {
                Some(end) => end,
                None => return false,
            };
            if len == 0 || end > entries_by_entity.len() {
                return false;
            }
            let slice = &entries_by_entity[start..end];
            if slice[0].coord.entity() != run.entity
                || slice[end - start - 1].coord.entity() != run.entity
                || slice[0].global_sequence != run.first_sequence
                || slice[end - start - 1].global_sequence != run.last_sequence
                || slice.iter().any(|entry| entry.coord.entity() != run.entity)
            {
                return false;
            }
            run_total += len;
        }

        run_total == entries_by_entity.len()
    }
}

pub(crate) fn recommended_restore_chunk_count(entry_count: usize) -> usize {
    let chunks = entry_count.div_ceil(65_536);
    chunks.clamp(1, 32)
}

impl StoreIndex {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_config(&IndexConfig::default())
    }

    /// Create a StoreIndex with the specified index configuration.
    pub(crate) fn with_config(config: &IndexConfig) -> Self {
        Self {
            streams: DashMap::new(),
            scan: ScanIndex::for_config(config),
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

    /// Replace the in-memory index contents from a sorted durable snapshot.
    ///
    /// `entries` must be sorted ascending by `global_sequence`. The allocator is
    /// restored to `max(last_sequence + 1, allocator_hint)` and published only
    /// after every base map and overlay view has been rebuilt.
    // Entity run indices are u64 for serialization portability; truncation is safe on 64-bit.
    #[allow(clippy::cast_possible_truncation)]
    fn restore_sorted_entries_impl(
        &self,
        entries: Vec<IndexEntry>,
        allocator_hint: u64,
        chunk_count: usize,
        routing_hint: Option<&RoutingSummary>,
        before_publish: impl FnOnce(&Self),
    ) {
        self.streams.clear();
        self.scan.clear();
        self.by_id.clear();
        self.latest.clear();
        self.sequence.clear();

        let restored = RestoreBase::from_sorted_entries(entries, chunk_count, routing_hint);
        let mut by_id =
            HashMap::<u128, Arc<IndexEntry>>::with_capacity(restored.entries_by_sequence.len());
        let mut latest =
            HashMap::<Arc<str>, Arc<IndexEntry>>::with_capacity(restored.routing.entity_runs.len());

        for run in &restored.routing.entity_runs {
            let start = run.start as usize;
            let end = start + (run.len as usize);
            let slice = &restored.entries_by_entity[start..end];
            let entity = slice[0].coord.entity_arc();
            let stream: BTreeMap<ClockKey, Arc<IndexEntry>> = slice
                .iter()
                .map(|entry| {
                    (
                        ClockKey {
                            wall_ms: entry.wall_ms,
                            clock: entry.clock,
                            uuid: entry.event_id,
                        },
                        Arc::clone(entry),
                    )
                })
                .collect();
            latest.insert(
                Arc::clone(&entity),
                Arc::clone(slice.last().expect("run is non-empty")),
            );
            self.streams.insert(entity, stream);
        }

        self.scan.rebuild_from_restore_base(
            &restored.entries_by_sequence,
            &restored.entries_by_entity,
            &restored.routing,
        );
        for entry in &restored.entries_by_sequence {
            by_id.insert(entry.event_id, Arc::clone(entry));
        }
        for (event_id, entry) in by_id {
            self.by_id.insert(event_id, entry);
        }
        for (entity, entry) in latest {
            self.latest.insert(entity, entry);
        }

        self.len
            .store(restored.entries_by_sequence.len(), Ordering::Relaxed);
        before_publish(self);

        let next_sequence = restored
            .entries_by_sequence
            .last()
            .map(|entry| entry.global_sequence.saturating_add(1))
            .unwrap_or(allocator_hint)
            .max(allocator_hint);
        self.sequence.restore_allocator(next_sequence);
        self.publish(next_sequence);
    }

    #[cfg(test)]
    pub(crate) fn restore_sorted_entries(&self, entries: Vec<IndexEntry>, allocator_hint: u64) {
        self.restore_sorted_entries_impl(entries, allocator_hint, 1, None, |_| {});
    }

    pub(crate) fn restore_sorted_entries_with_routing(
        &self,
        entries: Vec<IndexEntry>,
        allocator_hint: u64,
        routing: &RoutingSummary,
    ) {
        let chunk_count = usize::try_from(routing.chunk_count).unwrap_or(1).max(1);
        self.restore_sorted_entries_impl(
            entries,
            allocator_hint,
            chunk_count,
            Some(routing),
            |_| {},
        );
    }

    #[cfg(test)]
    pub(crate) fn restore_sorted_entries_with_before_publish(
        &self,
        entries: Vec<IndexEntry>,
        allocator_hint: u64,
        before_publish: impl FnOnce(&Self),
    ) {
        self.restore_sorted_entries_impl(entries, allocator_hint, 1, None, before_publish);
    }

    pub(crate) fn get_by_id(&self, event_id: u128) -> Option<IndexEntry> {
        let visibility = self.sequence.snapshot();
        self.by_id
            .get(&event_id)
            .map(|r| r.value().as_ref().clone())
            .filter(|e| visibility.is_visible(e.global_sequence))
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
        let visibility = self.sequence.snapshot();
        self.latest
            .get(entity)
            .map(|r| r.value().as_ref().clone())
            .filter(|e| visibility.is_visible(e.global_sequence))
    }

    pub(crate) fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        let visibility = self.sequence.snapshot();
        self.streams
            .get(entity)
            .map(|r| {
                r.value()
                    .values()
                    .filter(|arc| visibility.is_visible(arc.global_sequence))
                    .map(|arc| arc.as_ref().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn query(&self, region: &crate::coordinate::Region) -> Vec<IndexEntry> {
        let visibility = self.sequence.snapshot();
        // Region query strategy:
        // 1. Determine candidate set based on most selective filter
        // 2. Apply remaining filters to narrow results
        // 3. Filter by visibility watermark
        // 4. Apply clock_range last (it's per-entity, cheap)
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

        // Visibility watermark: exclude entries not yet published.
        candidates.retain(|e| visibility.is_visible(e.global_sequence));

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
    pub(crate) fn topology_name(&self) -> &'static str {
        self.scan.topology_name()
    }

    pub(crate) fn tile_count(&self) -> usize {
        self.scan.tile_count()
    }

    pub(crate) fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.scan.entity_generation(entity).or_else(|| {
            self.streams
                .get(entity)
                .map(|entries| entries.value().len() as u64)
        })
    }

    pub(crate) fn cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
    ) -> Option<CachedProjectionSlot> {
        self.scan.cached_projection(entity, type_id)
    }

    pub(crate) fn store_cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
        bytes: Vec<u8>,
        watermark: u64,
        cached_at_us: i64,
    ) -> bool {
        self.scan
            .store_cached_projection(entity, type_id, bytes, watermark, cached_at_us)
    }

    pub(crate) fn projection_replay_plan(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionReplayPlan> {
        if let Some((watermark, generation, items)) =
            self.scan.projection_candidates(entity, relevant_kinds)
        {
            return Some(ProjectionReplayPlan {
                watermark,
                generation,
                items: items
                    .into_iter()
                    .map(|(global_sequence, disk_pos)| ProjectionReplayItem {
                        global_sequence,
                        disk_pos,
                    })
                    .collect(),
            });
        }

        let stream = self.streams.get(entity)?;
        let match_all = relevant_kinds.is_empty();
        let mut items = Vec::new();
        let mut watermark = None;
        for entry in stream.value().values() {
            if !match_all && !relevant_kinds.contains(&entry.kind) {
                continue;
            }
            watermark = Some(entry.global_sequence);
            items.push(ProjectionReplayItem {
                global_sequence: entry.global_sequence,
                disk_pos: entry.disk_pos,
            });
        }

        Some(ProjectionReplayPlan {
            watermark: watermark?,
            generation: stream.value().len() as u64,
            items,
        })
    }

    pub(crate) fn begin_visibility_fence(&self) -> Result<u64, crate::store::StoreError> {
        self.sequence.begin_fence()
    }

    pub(crate) fn active_visibility_fence(&self) -> Option<u64> {
        self.sequence.active_fence_token()
    }

    pub(crate) fn finish_visibility_fence(
        &self,
        token: u64,
        publish_to: Option<u64>,
    ) -> Result<(), crate::store::StoreError> {
        self.sequence.finish_fence(token, publish_to)
    }

    pub(crate) fn note_visibility_fence_progress(
        &self,
        token: u64,
        start: u64,
        end: u64,
    ) -> Result<(), crate::store::StoreError> {
        self.sequence.note_fence_progress(token, start, end)
    }

    pub(crate) fn cancel_visibility_fence(
        &self,
        token: u64,
    ) -> Result<(), crate::store::StoreError> {
        self.sequence.cancel_fence(token)
    }

    pub(crate) fn cancelled_visibility_ranges(&self) -> Vec<(u64, u64)> {
        self.sequence.cancelled_ranges_snapshot()
    }

    pub(crate) fn restore_cancelled_visibility_ranges(&self, ranges: Vec<(u64, u64)>) {
        self.sequence.restore_cancelled_ranges(ranges);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate::Region;
    use crate::event::EventKind;

    fn make_entry(seq: u64, entity: &str, scope: &str) -> IndexEntry {
        let coord = Coordinate::new(entity, scope).expect("coord");
        IndexEntry {
            event_id: seq as u128 + 1,
            correlation_id: seq as u128 + 1,
            causation_id: None,
            entity_id: crate::store::interner::InternId::sentinel(),
            scope_id: crate::store::interner::InternId::sentinel(),
            coord,
            kind: EventKind::custom(0xF, 1),
            wall_ms: seq,
            clock: u32::try_from(seq).expect("small seq"),
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos {
                segment_id: 0,
                offset: seq * 16,
                length: 16,
            },
            global_sequence: seq,
        }
    }

    #[test]
    fn bulk_restore_keeps_entries_invisible_until_publish() {
        let index = StoreIndex::new();
        let entity_id = index.interner.intern("entity:bulk");
        let scope_id = index.interner.intern("scope:bulk");
        let entries = (0..3)
            .map(|seq| {
                let mut entry = make_entry(seq, "entity:bulk", "scope:bulk");
                entry.entity_id = entity_id;
                entry.scope_id = scope_id;
                entry
            })
            .collect();

        index.restore_sorted_entries_with_before_publish(entries, 3, |index| {
            assert_eq!(
                index.visible_sequence(),
                0,
                "visibility watermark must not advance until every view is rebuilt"
            );
            assert!(
                index.query(&Region::all()).is_empty(),
                "PROPERTY: reads must observe neither base maps nor overlays before publish"
            );
        });

        assert_eq!(index.query(&Region::all()).len(), 3);
        assert_eq!(index.visible_sequence(), 3);
    }
}
