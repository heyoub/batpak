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
mod projection_fast_paths;
mod routing;
mod soa;
mod soaos;

use crate::event::EventKind;
use crate::store::index::{ClockKey, DiskPos, IndexEntry, QueryHit, RoutingSummary};
use dashmap::DashMap;
use parking_lot::RwLock;
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
/// Delegates to [`EventKind::as_raw_u16`], the canonical
/// `(category << 12) | type_id` encoding.
#[inline]
fn event_kind_raw(kind: EventKind) -> u16 {
    kind.as_raw_u16()
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
mod tests;
