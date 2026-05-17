pub(crate) mod columnar;
pub(crate) mod interner;
mod projection_bridge;
mod query;
mod restore;
mod visibility;

use self::columnar::ScanIndex;
use self::interner::StringInterner;
pub(crate) use self::projection_bridge::{projection_kind_matches, ProjectionReplayPlan};
use self::restore::RestoreBase;
pub(crate) use self::restore::{
    recommended_restore_chunk_count, restore_chunk_ranges, RoutingSummary,
};
use self::visibility::SequenceGate;
use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::config::IndexConfig;
use crate::store::{EncodedBytes, ExtensionKey};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// StoreIndex: in-memory 2D index + auxiliaries. Not persisted; rebuilt from segments on cold start.
/// [DEP:dashmap::DashMap] — see DEPENDENCY SURFACE for deadlock warnings
///
/// # F6 / FREEZE-4 compact swap-point
///
/// Compaction must never expose a cleared or partially rebuilt index to
/// readers. The protocol enforced here is:
///
/// 1. **Off-side build.** `compact()` allocates a *fresh* [`StoreIndex`],
///    populates it from segments via `rebuild_from_segments`, and
///    only then hands it to [`StoreIndex::replace_contents_from_fresh`].
///    While the fresh index is being built the live index is not touched,
///    so readers keep serving the pre-compact state unchanged.
/// 2. **Single swap point.** The critical section inside
///    `replace_contents_from_fresh` takes the write guard on `swap_gate`
///    and publishes the new contents. That guard is the commit point.
/// 3. **Failure safety.** If the off-side rebuild errors before the swap,
///    the live index is still valid and readable; the caller observes
///    [`crate::store::segment::CompactionOutcome::Failed`] rather than a
///    silently clobbered store.
/// 4. **Reader observation.** Reader-facing methods take the read guard on
///    `swap_gate` at entry. Concurrent readers complete on their snapshot;
///    a concurrent compact-swap waits for them to release before taking
///    the write guard. Either the old index or the new one is fully
///    observable — never a partially rebuilt view.
/// 5. **Segment cleanup AFTER swap.** Callers (see
///    `src/store/lifecycle.rs::compact`) delete the old segment files
///    only after `replace_contents_from_fresh` returns. If the process
///    crashes between swap and cleanup, cold-start reconciliation removes
///    the orphaned files.
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
    /// F6 / FREEZE-4 compact swap-point lock. Writers (compact) take the
    /// write guard for the single critical section that swaps fresh
    /// contents in; readers (queries) take the read guard at entry so they
    /// see either the old index or the new one, never a partial rebuild.
    /// The lock's inner unit value is intentionally trivial — it exists
    /// purely as a rendezvous barrier around the swap.
    swap_gate: RwLock<()>,
}

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
    pub(crate) entity_id: self::interner::InternId,
    /// Interned scope string ID for compact checkpoint serialization.
    pub(crate) scope_id: self::interner::InternId,
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
    /// Opaque receipt extensions committed with this event.
    pub receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
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
            swap_gate: RwLock::new(()),
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
    ///
    /// F6 / FREEZE-4: the insert path takes the `swap_gate` read guard so
    /// that a concurrent compact-swap (which holds the write guard) cannot
    /// race mid-clear with an in-flight writer insert. The guard is
    /// released before the method returns — writer throughput is unaffected
    /// when no compact is in flight.
    pub(crate) fn insert(&self, entry: IndexEntry) {
        let _read = self.swap_gate.read();
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
    ///
    /// F6 / FREEZE-4: takes the `swap_gate` read guard for the duration of
    /// the batch so that a concurrent compact-swap cannot race mid-clear
    /// with an in-flight writer batch insert.
    pub(crate) fn insert_batch(&self, entries: Vec<IndexEntry>) {
        let _read = self.swap_gate.read();
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
    // justifies: src/store/index/restore.rs builds restore runs from u32-backed artifact coordinates, so these width checks are supported-target invariants.
    #[allow(clippy::expect_used)]
    fn restore_sorted_entries_impl(
        &self,
        entries: Vec<IndexEntry>,
        allocator_hint: u64,
        chunk_count: usize,
        routing_hint: Option<&RoutingSummary>,
        before_publish: impl FnOnce(&Self),
    ) -> Result<(), crate::store::StoreError> {
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
            let start = usize::try_from(run.start)
                .expect("invariant: entity run index fits usize on any supported target");
            let len = usize::try_from(run.len)
                .expect("invariant: entity run length fits usize on any supported target");
            let end = start
                .checked_add(len)
                .expect("invariant: entity run start+len fits usize on supported targets");
            let slice = &restored.entries_by_entity[start..end];
            if slice.is_empty() {
                continue;
            }
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
            latest.insert(Arc::clone(&entity), Arc::clone(&slice[slice.len() - 1]));
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
        self.publish(next_sequence, "restore_sorted_entries")?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn restore_sorted_entries(
        &self,
        entries: Vec<IndexEntry>,
        allocator_hint: u64,
    ) -> Result<(), crate::store::StoreError> {
        self.restore_sorted_entries_impl(entries, allocator_hint, 1, None, |_| {})
    }

    pub(crate) fn restore_sorted_entries_with_routing(
        &self,
        entries: Vec<IndexEntry>,
        allocator_hint: u64,
        routing: &RoutingSummary,
    ) -> Result<(), crate::store::StoreError> {
        let chunk_count = usize::try_from(routing.chunk_count).unwrap_or(1).max(1);
        self.restore_sorted_entries_impl(
            entries,
            allocator_hint,
            chunk_count,
            Some(routing),
            |_| {},
        )
    }

    #[cfg(test)]
    pub(crate) fn restore_sorted_entries_with_before_publish(
        &self,
        entries: Vec<IndexEntry>,
        allocator_hint: u64,
        before_publish: impl FnOnce(&Self),
    ) -> Result<(), crate::store::StoreError> {
        self.restore_sorted_entries_impl(entries, allocator_hint, 1, None, before_publish)
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

    pub(crate) fn hlc_for_global_sequence(
        &self,
        global_sequence: u64,
    ) -> Option<crate::store::stats::HlcPoint> {
        self.by_id
            .iter()
            .find(|entry| entry.value().global_sequence == global_sequence)
            .map(|entry| crate::store::stats::HlcPoint {
                wall_ms: entry.value().wall_ms,
                global_sequence,
            })
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
    pub(crate) fn publish(
        &self,
        up_to: u64,
        operation: &'static str,
    ) -> Result<(), crate::store::StoreError> {
        self.sequence.publish(up_to, operation)
    }

    pub(crate) fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }

    /// F6 / FREEZE-4 compact swap-point.
    ///
    /// Adopt the contents of a freshly-built sibling [`StoreIndex`] as the
    /// live contents of `self`. Called exclusively by
    /// `src/store/lifecycle.rs::compact` after the segment merge has
    /// produced the new on-disk layout and `rebuild_from_segments` has
    /// populated the fresh index off-side.
    ///
    /// Protocol:
    ///
    /// * The **exclusive** write guard on `swap_gate` is held for the
    ///   duration of the transfer. Reader-facing methods on [`StoreIndex`]
    ///   acquire the read guard at entry, so no reader observes the
    ///   intermediate cleared state.
    /// * The interner on `self` is mutated in-place via
    ///   [`StringInterner::replace_from_full_snapshot`] so that external
    ///   references to `index.interner` (cold-start, writer staging) remain
    ///   valid across the swap.
    /// * `fresh` is consumed; its per-field contents are transferred into
    ///   `self` by draining `fresh` and re-inserting. This is an `O(n)`
    ///   operation but it is performed on already-populated in-memory data
    ///   (no disk I/O), which is significantly faster than the previous
    ///   protocol of clearing the live index and replaying segments under
    ///   reader visibility.
    // justifies: src/store/index/restore.rs builds the fresh index from u32-backed routing runs, so these width checks are supported-target invariants.
    #[allow(clippy::expect_used)]
    pub(crate) fn replace_contents_from_fresh(
        &self,
        fresh: StoreIndex,
    ) -> Result<(), crate::store::StoreError> {
        let _write = self.swap_gate.write();

        // Reset live fields. `scan.clear()` also drops any overlay slots.
        self.streams.clear();
        self.scan.clear();
        self.by_id.clear();
        self.latest.clear();
        self.sequence.clear();
        self.len.store(0, Ordering::Relaxed);

        // Transfer the fresh interner strings in-place so that external
        // handles to `self.interner` (cold-start checkpoint writer, writer
        // staging) continue to see a populated interner after the swap.
        // `replace_from_full_snapshot` expects `[sentinel, ...strings]`;
        // `to_snapshot` returns only the non-sentinel strings, so we
        // re-prepend the sentinel here (same shape cold-start uses).
        let mut interner_full = vec![String::new()];
        interner_full.extend(fresh.interner.to_snapshot());
        self.interner.replace_from_full_snapshot(&interner_full);

        // Streams: drain fresh, insert into self.
        for (entity, stream) in fresh.streams.into_iter() {
            self.streams.insert(entity, stream);
        }
        for (id, entry) in fresh.by_id.into_iter() {
            self.by_id.insert(id, entry);
        }
        for (entity, latest) in fresh.latest.into_iter() {
            self.latest.insert(entity, latest);
        }

        // Scan overlays: rebuild from the entries we just populated so the
        // overlay topology matches the live configuration. We walk `by_id`
        // once because it is the only map that already contains every
        // entry exactly once.
        for entry in self.by_id.iter() {
            self.scan.insert(entry.value());
        }

        // Sequence gate: restore allocator + visibility + cancelled ranges
        // from the fresh gate. The fresh gate was driven by
        // `rebuild_from_segments` which publishes to the correct watermark.
        let fresh_allocated = fresh.sequence.allocated();
        let fresh_visible = fresh.sequence.visible();
        let fresh_cancelled = fresh.sequence.cancelled_ranges_snapshot();
        self.sequence.restore_allocator(fresh_allocated);
        self.sequence
            .publish(fresh_visible, "replace_contents_from_fresh")?;
        self.sequence.restore_cancelled_ranges(fresh_cancelled);

        // Restore len. `fresh.len` is a snapshot of how many entries were
        // populated off-side — reading it after draining `fresh.by_id`
        // would be inconsistent, so we use `self.by_id.len()` which is the
        // live truth at this point.
        self.len.store(self.by_id.len(), Ordering::Relaxed);
        Ok(())
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
            entity_id: self::interner::InternId::sentinel(),
            scope_id: self::interner::InternId::sentinel(),
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
            receipt_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn clock_key_orders_by_wall_then_clock_then_uuid() {
        let mut keys = [
            ClockKey {
                wall_ms: 10,
                clock: 3,
                uuid: 9,
            },
            ClockKey {
                wall_ms: 9,
                clock: 99,
                uuid: 1,
            },
            ClockKey {
                wall_ms: 10,
                clock: 2,
                uuid: 99,
            },
            ClockKey {
                wall_ms: 10,
                clock: 3,
                uuid: 4,
            },
        ];

        keys.sort();

        assert_eq!(
            keys,
            [
                ClockKey {
                    wall_ms: 9,
                    clock: 99,
                    uuid: 1,
                },
                ClockKey {
                    wall_ms: 10,
                    clock: 2,
                    uuid: 99,
                },
                ClockKey {
                    wall_ms: 10,
                    clock: 3,
                    uuid: 4,
                },
                ClockKey {
                    wall_ms: 10,
                    clock: 3,
                    uuid: 9,
                },
            ],
            "PROPERTY: ClockKey ordering must be wall_ms first, then clock, then uuid as the deterministic tiebreaker"
        );
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

        index
            .restore_sorted_entries_with_before_publish(entries, 3, |index| {
                assert_eq!(
                    index.visible_sequence(),
                    0,
                    "visibility watermark must not advance until every view is rebuilt"
                );
                assert!(
                    index.query(&Region::all()).is_empty(),
                    "PROPERTY: reads must observe neither base maps nor overlays before publish"
                );
            })
            .expect("bulk restore publish must succeed");

        assert_eq!(index.query(&Region::all()).len(), 3);
        assert_eq!(index.visible_sequence(), 3);
    }

    #[test]
    fn upgrade_with_visibility_snapshot_rejects_cancelled_ranges() {
        let index = StoreIndex::new();
        let entity_id = index.interner.intern("entity:visibility");
        let scope_id = index.interner.intern("scope:visibility");
        for seq in 0..3 {
            let mut entry = make_entry(seq, "entity:visibility", "scope:visibility");
            entry.entity_id = entity_id;
            entry.scope_id = scope_id;
            index.insert(entry);
        }
        index
            .publish(3, "test-publish")
            .expect("publish test entries");
        index.restore_cancelled_visibility_ranges(vec![(1, 2)]);

        let hidden = QueryHit {
            event_id: 2,
            global_sequence: 1,
            disk_pos: DiskPos::new(0, 16, 16),
            kind: EventKind::custom(0xF, 1),
            clock: 1,
        };
        let (hits, visibility) = index.query_hits_with_snapshot(&Region::all());

        assert_eq!(
            hits.iter()
                .map(|hit| hit.global_sequence)
                .collect::<Vec<_>>(),
            vec![0, 2],
            "PROPERTY: query-hit collection must skip cancelled hidden ranges below the visible watermark"
        );
        assert!(
            index
                .upgrade_hit_with_visibility(hidden, &visibility)
                .is_none(),
            "PROPERTY: hit upgrade must use the same hidden-range visibility predicate as query collection"
        );
    }
}
