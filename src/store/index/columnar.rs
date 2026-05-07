//! Columnar (SoA / AoSoA) secondary query index.
//!
//! This module provides the columnar overlay indexes and the [`ScanIndex`]
//! fanout struct that maintains a mandatory AoS base view plus optional
//! SoA, SoAoS, and AoSoA64 overlays. All active views are populated on
//! every insert; queries route to the most efficient view for each access
//! pattern.
//!
//! ## Memory layout quick-reference
//!
//! | Variant   | Inner representation                                      |
//! |-----------|-----------------------------------------------------------|
//! | SoA       | Three `Vec`s sorted by `(kind, global_sequence)`          |
//! | AoSoA8    | `Vec<Tile<8>>`; each tile holds ≤ 8 events of one kind    |
//! | AoSoA16   | `Vec<Tile<16>>`; test-only tile-size harness               |
//! | AoSoA64   | `Vec<Tile<64>>`; cache-line aligned; scalar scan today     |
//!
//! ## Concurrency model
//!
//! `ColumnarIndex` wraps its mutable state in a single `parking_lot::RwLock`.
//! Multiple readers may query simultaneously; the writer thread takes an
//! exclusive write lock only during `insert`.  Because the writer already
//! serialises all appends before calling `StoreIndex::insert`, write
//! contention on this lock is effectively nil.
//!
//! ## Append ordering
//!
//! Events are always appended in ascending `global_sequence` order (the writer
//! thread assigns global_sequence under its own lock).  `insert` therefore
//! pushes to the back of the SoA vecs / open tile without any reordering.
//! `query_by_kind` performs a scalar linear pass for all layouts. AoSoA64 tiles
//! are sized to align with cache lines, but the current scan path does not use
//! SIMD intrinsics; that specialization is not yet implemented.

mod aosoa;
mod aosoa64simd;
mod routing;
mod soa;
mod soaos;

use crate::event::EventKind;
use crate::store::index::{ClockKey, DiskPos, IndexEntry, QueryHit, RoutingSummary};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use aosoa::AoSoAInner;
#[cfg(test)]
use aosoa::Tile;
use aosoa64simd::AoSoA64SimdInner;
#[cfg(test)]
use routing::{ProjectionSupport, ScanCapabilities, ScanRoute};
use soa::SoAInner;
pub(crate) use soaos::CachedProjectionSlot;
use soaos::SoAoSInner;

type ProjectionCandidates = (u64, u64, Vec<(u64, DiskPos)>);

/// Reconstruct the raw `u16` wire value from an `EventKind`.
///
/// `EventKind` stores `(category << 12) | type_id` but the inner field is
/// private. Reconstructing from the public accessors gives the same bits.
#[inline]
fn event_kind_raw(kind: EventKind) -> u16 {
    ((kind.category() as u16) << 12) | kind.type_id()
}

/// Post-filter, sort, and truncate for non-SoA bounded-scan fallback.
///
/// Retains hits with `global_sequence > after_seq` (when `started`), sorts
/// ascending, and truncates to `limit`.
#[cfg(test)]
#[inline]
pub(super) fn apply_after_bounds(
    v: &mut Vec<QueryHit>,
    after_seq: u64,
    started: bool,
    limit: usize,
) {
    if started {
        v.retain(|h| h.global_sequence > after_seq);
    }
    v.sort_by_key(|h| h.global_sequence);
    v.truncate(limit);
}

#[derive(Clone, Copy, Debug)]
enum EntryQuery<'a> {
    Kind(EventKind),
    Category(u8),
    Scope(&'a str),
}

/// Concrete storage variant held inside a [`ColumnarIndex`].
///
/// Each arm holds the mutable inner state behind a `RwLock` so that
/// concurrent readers never block each other.
enum ColumnarVariant {
    /// Flat parallel arrays; best for sequential scans.
    SoA(RwLock<SoAInner>),
    #[cfg(test)]
    /// 8-element tiles; test-only tile-size regression harness, not a production path.
    AoSoA8(RwLock<AoSoAInner<8>>),
    #[cfg(test)]
    /// 16-element tiles; test-only tile-size regression harness, not a production path.
    AoSoA16(RwLock<AoSoAInner<16>>),
    /// 64-element tiles; cache-line aligned, scalar scan today.
    ///
    /// **Routing decision (2026-04-17):** AoSoA64 is safe on all corpus shapes
    /// after the kind-keyed fill fix. Benchmarked scalar path shows ~5–9% win
    /// over SoA on sorted by_kind; interleaved is now at parity. Does not yet
    /// clear the 15% routing threshold. SIMD executor (by_kind, by_category) is
    /// the next lever — implement only after benchmarking confirms the tile
    /// structure earns the route on target hardware.
    AoSoA64(RwLock<AoSoAInner<64>>),
    /// Experimental mixed-kind 64-element tiles with inline `[u16; 64]` kinds array.
    ///
    /// Unlike `AoSoA64` (kind-homogeneous tiles + tile-skip), this variant packs
    /// any kind into a tile and scans with a two-pass auto-vectorizable loop.
    /// Benchmarked head-to-head against `AoSoA64` and `SoA` on sorted and
    /// interleaved corpora. Not default-routed until it clears the 15% threshold.
    AoSoA64Simd(RwLock<AoSoA64SimdInner>),
    /// Hybrid AoS-outer (entity groups), SoA-inner (parallel arrays per entity).
    SoAoS(RwLock<SoAoSInner>),
}

// ---------------------------------------------------------------------------
// ColumnarIndex — public API
// ---------------------------------------------------------------------------

/// Cache-friendly secondary query index that supplements the `by_fact` and
/// `scope_entities` `DashMap`s with an optional columnar overlay.
///
/// ## Thread safety
///
/// All methods take `&self`; internal state is protected by a
/// `parking_lot::RwLock`.  Writers hold an exclusive lock for the duration of
/// [`insert`]; readers share a read lock.  Because the writer thread
/// serialises all appends, write contention is negligible.
///
/// [`insert`]: ColumnarIndex::insert
pub(crate) struct ColumnarIndex {
    inner: ColumnarVariant,
}

impl ColumnarIndex {
    /// Create a new flat SoA index.
    pub(crate) fn new_soa() -> Self {
        Self {
            inner: ColumnarVariant::SoA(RwLock::new(SoAInner::new())),
        }
    }

    #[cfg(test)]
    /// Create a new AoSoA index with 8-element tiles.
    pub(crate) fn new_aosoa8() -> Self {
        Self {
            inner: ColumnarVariant::AoSoA8(RwLock::new(AoSoAInner::<8>::new())),
        }
    }

    #[cfg(test)]
    /// Create a new AoSoA index with 16-element tiles.
    pub(crate) fn new_aosoa16() -> Self {
        Self {
            inner: ColumnarVariant::AoSoA16(RwLock::new(AoSoAInner::<16>::new())),
        }
    }

    /// Create a new AoSoA index with 64-element tiles.
    pub(crate) fn new_aosoa64() -> Self {
        Self {
            inner: ColumnarVariant::AoSoA64(RwLock::new(AoSoAInner::<64>::new())),
        }
    }

    /// Create a new experimental AoSoA64Simd index (mixed-kind, inline kinds array).
    pub(crate) fn new_aosoa64_simd() -> Self {
        Self {
            inner: ColumnarVariant::AoSoA64Simd(RwLock::new(AoSoA64SimdInner::new())),
        }
    }

    /// Create a new SoAoS (hybrid AoS-outer, SoA-inner) index.
    pub(crate) fn new_soaos() -> Self {
        Self {
            inner: ColumnarVariant::SoAoS(RwLock::new(SoAoSInner::new())),
        }
    }

    /// Append `entry` to the index.
    ///
    /// Events must be inserted in ascending `global_sequence` order (which is
    /// guaranteed by the single-writer architecture).  The operation is O(1)
    /// amortised for SoA and O(1) amortised for AoSoA (tile append or new tile).
    pub(crate) fn insert(&self, entry: &Arc<IndexEntry>) {
        match &self.inner {
            ColumnarVariant::SoA(lock) => lock.write().push(entry),
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => lock.write().push(entry),
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => lock.write().push(entry),
            ColumnarVariant::AoSoA64(lock) => lock.write().push(entry),
            ColumnarVariant::AoSoA64Simd(lock) => lock.write().push(entry),
            ColumnarVariant::SoAoS(lock) => lock.write().push(entry),
        }
    }

    pub(crate) fn rebuild_from_restore_base(
        &self,
        entries_by_sequence: &[Arc<IndexEntry>],
        entries_by_entity: &[Arc<IndexEntry>],
        routing: &RoutingSummary,
    ) {
        match &self.inner {
            ColumnarVariant::SoA(lock) => {
                *lock.write() = SoAInner::from_entries(entries_by_sequence)
            }
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => {
                *lock.write() = AoSoAInner::<8>::from_entries(entries_by_sequence)
            }
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => {
                *lock.write() = AoSoAInner::<16>::from_entries(entries_by_sequence)
            }
            ColumnarVariant::AoSoA64(lock) => {
                *lock.write() = AoSoAInner::<64>::from_entries(entries_by_sequence)
            }
            ColumnarVariant::AoSoA64Simd(lock) => {
                *lock.write() = AoSoA64SimdInner::from_entries(entries_by_sequence)
            }
            ColumnarVariant::SoAoS(lock) => {
                *lock.write() = SoAoSInner::from_restore_base(entries_by_entity, routing)
            }
        }
    }

    fn query_hits_sorted(&self, query: EntryQuery<'_>) -> Vec<QueryHit> {
        let mut results = match &self.inner {
            ColumnarVariant::SoA(lock) => lock.read().hits_candidates(&query),
            ColumnarVariant::AoSoA64(lock) => lock.read().hits_candidates(&query),
            ColumnarVariant::AoSoA64Simd(lock) => lock.read().hits_candidates(&query),
            ColumnarVariant::SoAoS(lock) => lock.read().hits_candidates(&query),
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => lock.read().hits_candidates(&query),
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => lock.read().hits_candidates(&query),
        };
        results.sort_by_key(|h| h.global_sequence);
        results
    }

    /// Bounded variant of `query_hits_sorted`.
    ///
    /// For the SoA layout, uses a binary search to jump past already-consumed
    /// entries and stops the forward scan once `limit` hits are collected.
    /// Output is pre-sorted (entries are in global_sequence order).
    ///
    /// For all other layouts, collects all candidates, applies the position
    /// filter, sorts, and truncates to `limit`.
    #[cfg(test)]
    fn query_hits_sorted_after(
        &self,
        query: EntryQuery<'_>,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        match &self.inner {
            ColumnarVariant::SoA(lock) => {
                // Fast path: pre-sorted, pre-limited.
                lock.read()
                    .hits_candidates_after(&query, after_seq, started, limit)
            }
            ColumnarVariant::AoSoA64(lock) => {
                let mut v = lock.read().hits_candidates(&query);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            ColumnarVariant::AoSoA64Simd(lock) => {
                let mut v = lock.read().hits_candidates(&query);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            ColumnarVariant::SoAoS(lock) => {
                let mut v = lock.read().hits_candidates(&query);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => {
                let mut v = lock.read().hits_candidates(&query);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => {
                let mut v = lock.read().hits_candidates(&query);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
        }
    }

    pub(crate) fn query_hits_by_kind(&self, target: EventKind) -> Vec<QueryHit> {
        self.query_hits_sorted(EntryQuery::Kind(target))
    }

    pub(crate) fn query_hits_by_category(&self, category: u8) -> Vec<QueryHit> {
        self.query_hits_sorted(EntryQuery::Category(category))
    }

    pub(crate) fn query_hits_by_scope(&self, scope: &str) -> Vec<QueryHit> {
        self.query_hits_sorted(EntryQuery::Scope(scope))
    }

    #[cfg(test)]
    pub(crate) fn query_hits_by_kind_after(
        &self,
        target: EventKind,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        self.query_hits_sorted_after(EntryQuery::Kind(target), after_seq, started, limit)
    }

    #[cfg(test)]
    pub(crate) fn query_hits_by_category_after(
        &self,
        category: u8,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        self.query_hits_sorted_after(EntryQuery::Category(category), after_seq, started, limit)
    }

    #[cfg(test)]
    pub(crate) fn query_hits_by_scope_after(
        &self,
        scope: &str,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        self.query_hits_sorted_after(EntryQuery::Scope(scope), after_seq, started, limit)
    }

    /// Invoke `f` with an immutable reference to the `Tile<8>` at `idx`.
    ///
    /// This callback pattern avoids exposing interior mutability outside the
    /// module and prevents callers from holding a `RwLockReadGuard` longer
    /// than necessary.
    ///
    /// # Panics
    /// Panics if `self` is not an `AoSoA8` variant, or if `idx` is out of range.
    /// Caller contract violation — not recoverable.
    /// Invoke `f` with an immutable reference to the `Tile<8>` at `idx`.
    /// Returns `None` if `self` is not an `AoSoA8` variant.
    #[cfg(test)]
    fn with_tile8<R>(&self, idx: usize, f: impl FnOnce(&Tile<8>) -> R) -> Option<R> {
        match &self.inner {
            ColumnarVariant::AoSoA8(lock) => lock.read().with_tile(idx, f),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::AoSoA64Simd(_)
            | ColumnarVariant::SoAoS(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA16(_) => None,
        }
    }

    /// Invoke `f` with an immutable reference to the `Tile<16>` at `idx`.
    /// Returns `None` if `self` is not an `AoSoA16` variant or idx is out of range.
    #[cfg(test)]
    fn with_tile16<R>(&self, idx: usize, f: impl FnOnce(&Tile<16>) -> R) -> Option<R> {
        match &self.inner {
            ColumnarVariant::AoSoA16(lock) => lock.read().with_tile(idx, f),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::AoSoA64Simd(_)
            | ColumnarVariant::SoAoS(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) => None,
        }
    }

    /// Invoke `f` with an immutable reference to the `Tile<64>` at `idx`.
    /// Returns `None` if `self` is not an `AoSoA64` variant or idx is out of range.
    #[cfg(test)]
    fn with_tile64<R>(&self, idx: usize, f: impl FnOnce(&Tile<64>) -> R) -> Option<R> {
        match &self.inner {
            ColumnarVariant::AoSoA64(lock) => lock.read().with_tile(idx, f),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA64Simd(_)
            | ColumnarVariant::SoAoS(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => None,
        }
    }

    /// Discard all entries.  Called during index rebuild (compaction / cold start).
    pub(crate) fn clear(&self) {
        match &self.inner {
            ColumnarVariant::SoA(lock) => lock.write().clear(),
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => lock.write().clear(),
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => lock.write().clear(),
            ColumnarVariant::AoSoA64(lock) => lock.write().clear(),
            ColumnarVariant::AoSoA64Simd(lock) => lock.write().clear(),
            ColumnarVariant::SoAoS(lock) => lock.write().clear(),
        }
    }

    /// Return the number of tiles for any active tiled overlay, or 0 for non-tiled layouts.
    pub(crate) fn tile_count(&self) -> usize {
        match &self.inner {
            ColumnarVariant::AoSoA64(lock) => lock.read().tiles.len(),
            ColumnarVariant::AoSoA64Simd(lock) => lock.read().tiles.len(),
            ColumnarVariant::SoA(_) | ColumnarVariant::SoAoS(_) => 0,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => 0,
        }
    }

    pub(crate) fn entity_generation(&self, entity: &str) -> Option<u64> {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => lock.read().entity_generation(entity),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::AoSoA64Simd(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => None,
        }
    }

    pub(crate) fn cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
    ) -> Option<CachedProjectionSlot> {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => lock.read().cached_projection(entity, type_id),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::AoSoA64Simd(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => None,
        }
    }

    pub(crate) fn store_cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
        bytes: Vec<u8>,
        watermark: u64,
    ) -> bool {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => lock
                .write()
                .store_cached_projection(entity, type_id, bytes, watermark),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::AoSoA64Simd(_) => false,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => false,
        }
    }

    pub(crate) fn projection_candidates(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionCandidates> {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => {
                lock.read().projection_candidates(entity, relevant_kinds)
            }
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::AoSoA64Simd(_) => None,
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) | ColumnarVariant::AoSoA16(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ScanIndex — top-level dispatcher
// ---------------------------------------------------------------------------

/// Base AoS indexes plus optional multi-view overlays.
pub(crate) struct ScanIndex {
    /// Event-kind → ordered event entries.
    by_fact: DashMap<EventKind, BTreeMap<ClockKey, Arc<IndexEntry>>>,
    /// Scope string → set of entity strings active in that scope.
    scope_entities: DashMap<Arc<str>, HashSet<Arc<str>>>,
    /// Broad-scan overlay.
    soa: Option<ColumnarIndex>,
    /// Entity-group overlay.
    entity_groups: Option<ColumnarIndex>,
    /// Tiled replay/scanning overlay (kind-homogeneous, tile-skip).
    tiles64: Option<ColumnarIndex>,
    /// Experimental tiled overlay (mixed-kind, inline kinds array, auto-vectorizable).
    tiles64_simd: Option<ColumnarIndex>,
}

impl ScanIndex {
    /// Construct base AoS maps plus the configured optional overlays.
    pub(crate) fn for_config(config: &crate::store::IndexConfig) -> Self {
        let soa = config.topology.soa_enabled();
        let entity_groups = config.topology.entity_groups_enabled();
        let tiles64 = config.topology.tiles64_enabled();
        let tiles64_simd = config.topology.tiles64_simd_enabled();

        Self {
            by_fact: DashMap::new(),
            scope_entities: DashMap::new(),
            soa: soa.then(ColumnarIndex::new_soa),
            entity_groups: entity_groups.then(ColumnarIndex::new_soaos),
            tiles64: tiles64.then(ColumnarIndex::new_aosoa64),
            tiles64_simd: tiles64_simd.then(ColumnarIndex::new_aosoa64_simd),
        }
    }

    fn insert_base(&self, entry: &Arc<IndexEntry>) {
        let key = ClockKey {
            wall_ms: entry.wall_ms,
            clock: entry.clock,
            uuid: entry.event_id,
        };
        self.by_fact
            .entry(entry.kind)
            .or_default()
            .insert(key, Arc::clone(entry));
        self.scope_entities
            .entry(entry.coord.scope_arc())
            .or_default()
            .insert(entry.coord.entity_arc());
    }

    /// Insert an entry into whichever secondary index is active.
    ///
    /// For `Maps`, this updates both `by_fact` and `scope_entities` using the
    /// same `ClockKey` ordering used by `StoreIndex::insert`.
    ///
    /// For `Columnar`, this delegates to [`ColumnarIndex::insert`].
    pub(crate) fn insert(&self, entry: &Arc<IndexEntry>) {
        self.insert_base(entry);
        if let Some(idx) = &self.soa {
            idx.insert(entry);
        }
        if let Some(idx) = &self.entity_groups {
            idx.insert(entry);
        }
        if let Some(idx) = &self.tiles64 {
            idx.insert(entry);
        }
        if let Some(idx) = &self.tiles64_simd {
            idx.insert(entry);
        }
    }

    pub(crate) fn rebuild_from_restore_base(
        &self,
        entries_by_sequence: &[Arc<IndexEntry>],
        entries_by_entity: &[Arc<IndexEntry>],
        routing: &RoutingSummary,
    ) {
        self.by_fact.clear();
        self.scope_entities.clear();

        let mut by_fact =
            std::collections::HashMap::<EventKind, BTreeMap<ClockKey, Arc<IndexEntry>>>::new();
        let mut scope_entities = std::collections::HashMap::<Arc<str>, HashSet<Arc<str>>>::new();

        for entry in entries_by_sequence {
            let key = ClockKey {
                wall_ms: entry.wall_ms,
                clock: entry.clock,
                uuid: entry.event_id,
            };
            by_fact
                .entry(entry.kind)
                .or_default()
                .insert(key, Arc::clone(entry));
            scope_entities
                .entry(entry.coord.scope_arc())
                .or_default()
                .insert(entry.coord.entity_arc());
        }

        for (kind, map) in by_fact {
            self.by_fact.insert(kind, map);
        }
        for (scope, entities) in scope_entities {
            self.scope_entities.insert(scope, entities);
        }

        if let Some(idx) = &self.soa {
            idx.rebuild_from_restore_base(entries_by_sequence, entries_by_entity, routing);
        }
        if let Some(idx) = &self.entity_groups {
            idx.rebuild_from_restore_base(entries_by_sequence, entries_by_entity, routing);
        }
        if let Some(idx) = &self.tiles64 {
            idx.rebuild_from_restore_base(entries_by_sequence, entries_by_entity, routing);
        }
        if let Some(idx) = &self.tiles64_simd {
            idx.rebuild_from_restore_base(entries_by_sequence, entries_by_entity, routing);
        }
    }

    /// Return the set of entity strings registered under `scope` (Maps variant only).
    ///
    /// Returns `None` for the Columnar variant — callers should use
    /// [`query_hits_by_scope`] instead.
    ///
    /// [`query_hits_by_scope`]: ScanIndex::query_hits_by_scope
    pub(crate) fn scope_entity_set(&self, scope: &str) -> Option<HashSet<Arc<str>>> {
        self.scope_entities.get(scope).map(|r| r.value().clone())
    }

    /// Discard all entries.  Called during index rebuild.
    pub(crate) fn clear(&self) {
        self.by_fact.clear();
        self.scope_entities.clear();
        if let Some(idx) = &self.soa {
            idx.clear();
        }
        if let Some(idx) = &self.entity_groups {
            idx.clear();
        }
        if let Some(idx) = &self.tiles64 {
            idx.clear();
        }
        if let Some(idx) = &self.tiles64_simd {
            idx.clear();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::event::{EventKind, HashChain};
    use crate::store::index::{DiskPos, IndexEntry};
    use std::sync::Arc;

    fn make_entry(kind: EventKind, seq: u64, entity: &str, scope: &str) -> Arc<IndexEntry> {
        let coord = Coordinate::new(entity, scope).expect("coord");
        Arc::new(IndexEntry {
            event_id: seq as u128,
            correlation_id: seq as u128,
            causation_id: None,
            coord,
            entity_id: crate::store::index::interner::InternId::sentinel(),
            scope_id: crate::store::index::interner::InternId::sentinel(),
            kind,
            wall_ms: seq * 1000,
            clock: u32::try_from(seq).expect("test seq fits u32"),
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos {
                segment_id: 0,
                offset: seq * 64,
                length: 64,
            },
            global_sequence: seq,
        })
    }

    const KIND_A: EventKind = EventKind::custom(0x1, 1);
    const KIND_B: EventKind = EventKind::custom(0x1, 2);

    // --- SoA ---

    #[test]
    fn soa_insert_and_query_by_kind() {
        let idx = ColumnarIndex::new_soa();
        for i in 0u64..10 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        for i in 10u64..15 {
            idx.insert(&make_entry(KIND_B, i, "e2", "s1"));
        }
        let a = idx.query_hits_by_kind(KIND_A);
        assert_eq!(a.len(), 10);
        for (i, h) in a.iter().enumerate() {
            assert_eq!(h.global_sequence, i as u64);
        }
        let b = idx.query_hits_by_kind(KIND_B);
        assert_eq!(b.len(), 5);
    }

    #[test]
    fn soa_query_by_scope() {
        let idx = ColumnarIndex::new_soa();
        for i in 0u64..6 {
            idx.insert(&make_entry(KIND_A, i, "e1", "scope-x"));
        }
        for i in 6u64..10 {
            idx.insert(&make_entry(KIND_A, i, "e2", "scope-y"));
        }
        assert_eq!(idx.query_hits_by_scope("scope-x").len(), 6);
        assert_eq!(idx.query_hits_by_scope("scope-y").len(), 4);
        assert!(idx.query_hits_by_scope("scope-z").is_empty());
    }

    #[test]
    fn soa_clear() {
        let idx = ColumnarIndex::new_soa();
        for i in 0u64..5 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        idx.clear();
        assert!(idx.query_hits_by_kind(KIND_A).is_empty());
        assert!(idx.query_hits_by_scope("s1").is_empty());
    }

    // --- AoSoA8 ---

    #[test]
    fn aosoa8_insert_spans_multiple_tiles() {
        let idx = ColumnarIndex::new_aosoa8();
        for i in 0u64..20 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let results = idx.query_hits_by_kind(KIND_A);
        assert_eq!(results.len(), 20);
        for (i, h) in results.iter().enumerate() {
            assert_eq!(h.global_sequence, i as u64, "order must be preserved");
        }
    }

    #[test]
    fn aosoa8_interleaved_kinds() {
        let idx = ColumnarIndex::new_aosoa8();
        for i in 0u64..12 {
            idx.insert(&make_entry(KIND_A, i * 2, "ea", "s1"));
            idx.insert(&make_entry(KIND_B, i * 2 + 1, "eb", "s1"));
        }
        assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 12);
        assert_eq!(idx.query_hits_by_kind(KIND_B).len(), 12);
    }

    #[test]
    fn aosoa8_query_by_scope() {
        let idx = ColumnarIndex::new_aosoa8();
        for i in 0u64..9 {
            idx.insert(&make_entry(KIND_A, i, "ent-a", "scope-alpha"));
        }
        for i in 9u64..14 {
            idx.insert(&make_entry(KIND_A, i, "ent-b", "scope-beta"));
        }
        assert_eq!(idx.query_hits_by_scope("scope-alpha").len(), 9);
        assert_eq!(idx.query_hits_by_scope("scope-beta").len(), 5);
    }

    #[test]
    fn aosoa8_with_tile_callback() {
        let idx = ColumnarIndex::new_aosoa8();
        for i in 0u64..8 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        // First tile should be full with KIND_A
        let len = idx.with_tile8(0, |t| t.len).expect("should be AoSoA8");
        assert_eq!(len, 8);
    }

    // --- AoSoA16 ---

    #[test]
    fn aosoa16_basic() {
        let idx = ColumnarIndex::new_aosoa16();
        for i in 0u64..33 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 33);
    }

    #[test]
    fn aosoa16_with_tile_callback() {
        let idx = ColumnarIndex::new_aosoa16();
        for i in 0u64..16 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let len = idx.with_tile16(0, |t| t.len).expect("should be AoSoA16");
        assert_eq!(len, 16);
    }

    // --- AoSoA64 ---

    #[test]
    fn aosoa64_basic() {
        let idx = ColumnarIndex::new_aosoa64();
        for i in 0u64..130 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 130);
    }

    #[test]
    fn aosoa64_with_tile_callback() {
        let idx = ColumnarIndex::new_aosoa64();
        for i in 0u64..64 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let len = idx.with_tile64(0, |t| t.len).expect("should be AoSoA64");
        assert_eq!(len, 64);
    }

    // --- SoAoS ---

    #[test]
    fn soaos_insert_and_query_by_kind() {
        let idx = ColumnarIndex::new_soaos();
        for i in 0u64..10 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        for i in 10u64..15 {
            idx.insert(&make_entry(KIND_B, i, "e2", "s1"));
        }
        assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 10);
        assert_eq!(idx.query_hits_by_kind(KIND_B).len(), 5);
    }

    #[test]
    fn soaos_query_by_scope() {
        let idx = ColumnarIndex::new_soaos();
        for i in 0u64..8 {
            idx.insert(&make_entry(KIND_A, i, "e1", "scope-x"));
        }
        for i in 8u64..12 {
            idx.insert(&make_entry(KIND_A, i, "e2", "scope-y"));
        }
        assert_eq!(idx.query_hits_by_scope("scope-x").len(), 8);
        assert_eq!(idx.query_hits_by_scope("scope-y").len(), 4);
    }

    #[test]
    fn soaos_clear() {
        let idx = ColumnarIndex::new_soaos();
        for i in 0u64..5 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 5);
        idx.clear();
        assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 0);
    }

    // --- ScanIndex ---

    #[test]
    fn scan_index_maps_variant_insert_and_query() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::aos(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..7 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(si.query_hits_by_kind(KIND_A).len(), 7);
    }

    #[test]
    fn scan_index_soa_variant_insert_and_query() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::scan(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..12 {
            si.insert(&make_entry(KIND_A, i, "e1", "s2"));
        }
        assert_eq!(si.query_hits_by_kind(KIND_A).len(), 12);
    }

    #[test]
    fn scan_capabilities_follow_topology_truth() {
        let cases = [
            (
                crate::store::IndexTopology::aos(),
                ScanCapabilities {
                    by_kind: ScanRoute::BaseAoS,
                    by_scope: ScanRoute::BaseAoS,
                    by_category: ScanRoute::BaseAoS,
                    projection: ProjectionSupport {
                        entity_generation_fast_path: false,
                        cached_projection: false,
                        projection_candidates: false,
                    },
                    topology_name: "aos",
                    tile_count: 0,
                },
            ),
            (
                crate::store::IndexTopology::scan(),
                ScanCapabilities {
                    by_kind: ScanRoute::SoA,
                    by_scope: ScanRoute::SoA,
                    by_category: ScanRoute::SoA,
                    projection: ProjectionSupport {
                        entity_generation_fast_path: false,
                        cached_projection: false,
                        projection_candidates: false,
                    },
                    topology_name: "scan",
                    tile_count: 0,
                },
            ),
            (
                crate::store::IndexTopology::entity_local(),
                ScanCapabilities {
                    by_kind: ScanRoute::SoAoS,
                    by_scope: ScanRoute::SoAoS,
                    by_category: ScanRoute::SoAoS,
                    projection: ProjectionSupport {
                        entity_generation_fast_path: true,
                        cached_projection: true,
                        projection_candidates: true,
                    },
                    topology_name: "entity-local",
                    tile_count: 0,
                },
            ),
            (
                crate::store::IndexTopology::tiled(),
                ScanCapabilities {
                    by_kind: ScanRoute::AoSoA64,
                    by_scope: ScanRoute::AoSoA64,
                    by_category: ScanRoute::AoSoA64,
                    projection: ProjectionSupport {
                        entity_generation_fast_path: false,
                        cached_projection: false,
                        projection_candidates: false,
                    },
                    topology_name: "tiled",
                    tile_count: 0,
                },
            ),
            (
                crate::store::IndexTopology::tiled_simd(),
                ScanCapabilities {
                    by_kind: ScanRoute::AoSoA64Simd,
                    by_scope: ScanRoute::AoSoA64Simd,
                    by_category: ScanRoute::AoSoA64Simd,
                    projection: ProjectionSupport {
                        entity_generation_fast_path: false,
                        cached_projection: false,
                        projection_candidates: false,
                    },
                    topology_name: "tiled-simd",
                    tile_count: 0,
                },
            ),
            (
                crate::store::IndexTopology::all(),
                ScanCapabilities {
                    by_kind: ScanRoute::SoA,
                    by_scope: ScanRoute::SoAoS,
                    by_category: ScanRoute::SoA,
                    projection: ProjectionSupport {
                        entity_generation_fast_path: true,
                        cached_projection: true,
                        projection_candidates: true,
                    },
                    topology_name: "all",
                    tile_count: 0,
                },
            ),
        ];

        for (topology, expected) in cases {
            let si = ScanIndex::for_config(&crate::store::IndexConfig {
                topology,
                incremental_projection: false,
                enable_checkpoint: true,
                enable_mmap_index: true,
            });
            assert_eq!(
                si.capabilities(),
                expected,
                "ScanCapabilities must be the single routing truth for `{}`",
                expected.topology_name
            );
        }
    }

    #[test]
    fn scan_index_aosoa8_variant() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::tiled(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..20 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(si.query_hits_by_kind(KIND_A).len(), 20);
    }

    #[test]
    fn scan_index_maps_scope_entity_set() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::aos(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        si.insert(&make_entry(KIND_A, 0, "ent-1", "my-scope"));
        si.insert(&make_entry(KIND_A, 1, "ent-2", "my-scope"));
        let set = si
            .scope_entity_set("my-scope")
            .expect("should be Some for Maps");
        assert!(set.contains("ent-1" as &str));
        assert!(set.contains("ent-2" as &str));
    }

    #[test]
    fn scan_index_columnar_scope_entity_set_uses_base_aos_view() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::scan(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        si.insert(&make_entry(KIND_A, 0, "ent-1", "my-scope"));
        let set = si
            .scope_entity_set("my-scope")
            .expect("base AoS scope-entity map stays active across layouts");
        assert!(set.contains("ent-1" as &str));
    }

    #[test]
    fn scan_index_clear() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::scan(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..5 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        si.clear();
        assert!(si.query_hits_by_kind(KIND_A).is_empty());
    }

    #[test]
    fn scan_index_soaos_variant() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::entity_local(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..10 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(si.query_hits_by_kind(KIND_A).len(), 10);
        assert_eq!(si.query_hits_by_scope("s1").len(), 10);
        si.clear();
        assert!(si.query_hits_by_kind(KIND_A).is_empty());
    }

    #[test]
    fn entity_local_projection_fast_paths_round_trip() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::entity_local(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        si.insert(&make_entry(KIND_A, 0, "entity:projection", "scope:test"));
        si.insert(&make_entry(KIND_A, 1, "entity:projection", "scope:test"));

        assert_eq!(
            si.entity_generation("entity:projection"),
            Some(2),
            "PROPERTY: entity-local topology must expose an entity generation fast path for projection watchers"
        );

        let type_id = std::any::TypeId::of::<u64>();
        assert!(
            si.store_cached_projection("entity:projection", type_id, b"cached".to_vec(), 1),
            "PROPERTY: storing a group-local projection for an existing entity must report success"
        );
        let slot = si
            .cached_projection("entity:projection", type_id)
            .expect("cached projection slot");
        assert_eq!(slot.bytes, b"cached");
        assert_eq!(slot.watermark, 1);
        assert_eq!(
            slot.generation, 2,
            "PROPERTY: cached projection slots must be stamped with the entity group's current generation"
        );
    }

    #[test]
    fn scan_capabilities_track_tile_count_for_tiled_views() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::tiled(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..130 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let capabilities = si.capabilities();
        assert_eq!(capabilities.topology_name, "tiled");
        assert_eq!(capabilities.by_kind, ScanRoute::AoSoA64);
        assert_eq!(capabilities.by_scope, ScanRoute::AoSoA64);
        assert_eq!(capabilities.by_category, ScanRoute::AoSoA64);
        assert_eq!(capabilities.tile_count, 3);
        assert!(!capabilities.projection.cached_projection);
        assert!(!capabilities.projection.projection_candidates);
    }

    // --- Cross-layout oracle: all layouts must agree on query results ---
    //
    // This test is the correctness contract that makes the AoSoA64 SIMD
    // specialization (Step 4) safe to add: any specialized executor must
    // produce the same output as SoA on the same corpus.

    const KIND_C: EventKind = EventKind::custom(0x2, 1); // different category from KIND_A/KIND_B

    fn build_oracle_corpus() -> Vec<Arc<IndexEntry>> {
        // 20 KIND_A across two entities + 10 KIND_B + 5 KIND_C, two scopes.
        // Interleaved insertion to stress tile bucketing in AoSoA.
        let mut entries = Vec::new();
        let mut seq = 0u64;
        for _ in 0..10 {
            entries.push(make_entry(KIND_A, seq, "entity-alpha", "scope-one"));
            seq += 1;
            entries.push(make_entry(KIND_B, seq, "entity-beta", "scope-one"));
            seq += 1;
        }
        for _ in 0..10 {
            entries.push(make_entry(KIND_A, seq, "entity-gamma", "scope-two"));
            seq += 1;
            entries.push(make_entry(KIND_C, seq, "entity-gamma", "scope-two"));
            seq += 1;
        }
        entries
    }

    fn seq_ids(v: &[QueryHit]) -> Vec<u64> {
        v.iter().map(|h| h.global_sequence).collect()
    }

    #[test]
    fn all_layouts_agree_on_by_kind() {
        let corpus = build_oracle_corpus();
        let soa = ColumnarIndex::new_soa();
        let aosoa64 = ColumnarIndex::new_aosoa64();
        let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
        let soaos = ColumnarIndex::new_soaos();
        for entry in &corpus {
            soa.insert(entry);
            aosoa64.insert(entry);
            aosoa64_simd.insert(entry);
            soaos.insert(entry);
        }
        for kind in [KIND_A, KIND_B, KIND_C] {
            let reference = seq_ids(&soa.query_hits_by_kind(kind));
            assert_eq!(
                seq_ids(&aosoa64.query_hits_by_kind(kind)),
                reference,
                "AoSoA64 by_kind({kind:?}) must match SoA"
            );
            assert_eq!(
                seq_ids(&aosoa64_simd.query_hits_by_kind(kind)),
                reference,
                "AoSoA64Simd by_kind({kind:?}) must match SoA"
            );
            assert_eq!(
                seq_ids(&soaos.query_hits_by_kind(kind)),
                reference,
                "SoAoS by_kind({kind:?}) must match SoA"
            );
        }
    }

    #[test]
    fn all_layouts_agree_on_by_category() {
        let corpus = build_oracle_corpus();
        let soa = ColumnarIndex::new_soa();
        let aosoa64 = ColumnarIndex::new_aosoa64();
        let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
        let soaos = ColumnarIndex::new_soaos();
        for entry in &corpus {
            soa.insert(entry);
            aosoa64.insert(entry);
            aosoa64_simd.insert(entry);
            soaos.insert(entry);
        }
        for category in [0x1u8, 0x2u8] {
            let reference = seq_ids(&soa.query_hits_by_category(category));
            assert_eq!(
                seq_ids(&aosoa64.query_hits_by_category(category)),
                reference,
                "AoSoA64 by_category(0x{category:x}) must match SoA"
            );
            assert_eq!(
                seq_ids(&aosoa64_simd.query_hits_by_category(category)),
                reference,
                "AoSoA64Simd by_category(0x{category:x}) must match SoA"
            );
            assert_eq!(
                seq_ids(&soaos.query_hits_by_category(category)),
                reference,
                "SoAoS by_category(0x{category:x}) must match SoA"
            );
        }
    }

    #[test]
    fn all_layouts_agree_on_by_kind_after() {
        let corpus = build_oracle_corpus();
        let soa = ColumnarIndex::new_soa();
        let aosoa64 = ColumnarIndex::new_aosoa64();
        let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
        let soaos = ColumnarIndex::new_soaos();
        for entry in &corpus {
            soa.insert(entry);
            aosoa64.insert(entry);
            aosoa64_simd.insert(entry);
            soaos.insert(entry);
        }
        for kind in [KIND_A, KIND_B, KIND_C] {
            let reference = seq_ids(&soa.query_hits_by_kind_after(kind, 7, true, 5));
            assert_eq!(
                seq_ids(&aosoa64.query_hits_by_kind_after(kind, 7, true, 5)),
                reference,
                "AoSoA64 by_kind_after({kind:?}) must match SoA"
            );
            assert_eq!(
                seq_ids(&aosoa64_simd.query_hits_by_kind_after(kind, 7, true, 5)),
                reference,
                "AoSoA64Simd by_kind_after({kind:?}) must match SoA"
            );
            assert_eq!(
                seq_ids(&soaos.query_hits_by_kind_after(kind, 7, true, 5)),
                reference,
                "SoAoS by_kind_after({kind:?}) must match SoA"
            );
        }
    }

    #[test]
    fn all_layouts_agree_on_by_category_after() {
        let corpus = build_oracle_corpus();
        let soa = ColumnarIndex::new_soa();
        let aosoa64 = ColumnarIndex::new_aosoa64();
        let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
        let soaos = ColumnarIndex::new_soaos();
        for entry in &corpus {
            soa.insert(entry);
            aosoa64.insert(entry);
            aosoa64_simd.insert(entry);
            soaos.insert(entry);
        }
        for category in [0x1u8, 0x2u8] {
            let reference = seq_ids(&soa.query_hits_by_category_after(category, 7, true, 5));
            assert_eq!(
                seq_ids(&aosoa64.query_hits_by_category_after(category, 7, true, 5)),
                reference,
                "AoSoA64 by_category_after(0x{category:x}) must match SoA"
            );
            assert_eq!(
                seq_ids(&aosoa64_simd.query_hits_by_category_after(category, 7, true, 5)),
                reference,
                "AoSoA64Simd by_category_after(0x{category:x}) must match SoA"
            );
            assert_eq!(
                seq_ids(&soaos.query_hits_by_category_after(category, 7, true, 5)),
                reference,
                "SoAoS by_category_after(0x{category:x}) must match SoA"
            );
        }
    }

    // --- B2 contract: overlay scope queries are a subset of ground truth ---
    //
    // Every overlay's `query_hits_by_scope` output must be a subset of the
    // ground-truth "entries whose coord.scope == scope" set computed from the
    // raw corpus. Overlays may return fewer results (the shared filter
    // pipeline in StoreIndex::query_hits re-validates) but must never leak
    // events from other scopes.
    fn ground_truth_by_scope(corpus: &[Arc<IndexEntry>], scope: &str) -> Vec<u64> {
        let mut v: Vec<u64> = corpus
            .iter()
            .filter(|e| e.coord.scope() == scope)
            .map(|e| e.global_sequence)
            .collect();
        v.sort_unstable();
        v
    }

    fn is_subset_of_truth(overlay: &[QueryHit], truth: &[u64]) -> bool {
        let truth_set: std::collections::HashSet<u64> = truth.iter().copied().collect();
        overlay
            .iter()
            .all(|h| truth_set.contains(&h.global_sequence))
    }

    #[test]
    fn overlay_scope_queries_are_subset_of_ground_truth() {
        let corpus = build_oracle_corpus();
        let soa = ColumnarIndex::new_soa();
        let aosoa64 = ColumnarIndex::new_aosoa64();
        let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
        let soaos = ColumnarIndex::new_soaos();
        for entry in &corpus {
            soa.insert(entry);
            aosoa64.insert(entry);
            aosoa64_simd.insert(entry);
            soaos.insert(entry);
        }
        for scope in ["scope-one", "scope-two", "scope-missing"] {
            let truth = ground_truth_by_scope(&corpus, scope);
            for (name, overlay_hits) in [
                ("SoA", soa.query_hits_by_scope(scope)),
                ("AoSoA64", aosoa64.query_hits_by_scope(scope)),
                ("AoSoA64Simd", aosoa64_simd.query_hits_by_scope(scope)),
                ("SoAoS", soaos.query_hits_by_scope(scope)),
            ] {
                assert!(
                    is_subset_of_truth(&overlay_hits, &truth),
                    "{name} overlay leaked events outside scope {scope:?}: hits={:?} truth={:?}",
                    overlay_hits
                        .iter()
                        .map(|h| h.global_sequence)
                        .collect::<Vec<_>>(),
                    truth,
                );
            }
        }
    }

    #[test]
    fn overlay_scope_queries_after_respect_limit_and_subset() {
        let corpus = build_oracle_corpus();
        let soa = ColumnarIndex::new_soa();
        let aosoa64 = ColumnarIndex::new_aosoa64();
        let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
        let soaos = ColumnarIndex::new_soaos();
        for entry in &corpus {
            soa.insert(entry);
            aosoa64.insert(entry);
            aosoa64_simd.insert(entry);
            soaos.insert(entry);
        }
        for scope in ["scope-one", "scope-two"] {
            let truth = ground_truth_by_scope(&corpus, scope);
            for limit in [1usize, 3, 10, usize::MAX] {
                for (name, overlay_hits) in [
                    ("SoA", soa.query_hits_by_scope_after(scope, 0, false, limit)),
                    (
                        "AoSoA64",
                        aosoa64.query_hits_by_scope_after(scope, 0, false, limit),
                    ),
                    (
                        "AoSoA64Simd",
                        aosoa64_simd.query_hits_by_scope_after(scope, 0, false, limit),
                    ),
                    (
                        "SoAoS",
                        soaos.query_hits_by_scope_after(scope, 0, false, limit),
                    ),
                ] {
                    assert!(
                        overlay_hits.len() <= limit,
                        "{name} scope-after limit honoured: got {} > {}",
                        overlay_hits.len(),
                        limit
                    );
                    assert!(
                        is_subset_of_truth(&overlay_hits, &truth),
                        "{name} scope-after overlay leaked events outside scope {scope:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scan_index_after_queries_honor_kind_category_and_scope() {
        let corpus = build_oracle_corpus();
        let scan = ScanIndex::for_config(&crate::store::IndexConfig {
            topology: crate::store::IndexTopology::all(),
            ..crate::store::IndexConfig::default()
        });
        let soa = ColumnarIndex::new_soa();
        for entry in &corpus {
            scan.insert(entry);
            soa.insert(entry);
        }

        let by_kind = seq_ids(&scan.query_hits_by_kind_after(KIND_A, 7, true, 5));
        assert_eq!(
            by_kind,
            seq_ids(&soa.query_hits_by_kind_after(KIND_A, 7, true, 5)),
            "scan by_kind_after should stay wired through the overlay route"
        );

        let by_category = seq_ids(&scan.query_hits_by_category_after(0x1, 7, true, 5));
        assert_eq!(
            by_category,
            seq_ids(&soa.query_hits_by_category_after(0x1, 7, true, 5)),
            "scan by_category_after should stay wired through the overlay route"
        );

        let by_scope = seq_ids(&scan.query_hits_by_scope_after("scope-two", 7, true, 5));
        assert_eq!(
            by_scope,
            seq_ids(&soa.query_hits_by_scope_after("scope-two", 7, true, 5)),
            "scan by_scope_after should stay wired through the overlay route"
        );
    }

    #[test]
    fn all_layouts_agree_on_by_scope() {
        let corpus = build_oracle_corpus();
        let soa = ColumnarIndex::new_soa();
        let aosoa64 = ColumnarIndex::new_aosoa64();
        let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
        let soaos = ColumnarIndex::new_soaos();
        for entry in &corpus {
            soa.insert(entry);
            aosoa64.insert(entry);
            aosoa64_simd.insert(entry);
            soaos.insert(entry);
        }
        for scope in ["scope-one", "scope-two", "scope-missing"] {
            let reference = seq_ids(&soa.query_hits_by_scope(scope));
            assert_eq!(
                seq_ids(&aosoa64.query_hits_by_scope(scope)),
                reference,
                "AoSoA64 by_scope({scope:?}) must match SoA"
            );
            assert_eq!(
                seq_ids(&aosoa64_simd.query_hits_by_scope(scope)),
                reference,
                "AoSoA64Simd by_scope({scope:?}) must match SoA"
            );
            assert_eq!(
                seq_ids(&soaos.query_hits_by_scope(scope)),
                reference,
                "SoAoS by_scope({scope:?}) must match SoA"
            );
        }
    }
}
