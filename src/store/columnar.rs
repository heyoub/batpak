//! Columnar (SoA / AoSoA) secondary query index.
//!
//! This module provides the columnar overlay indexes and the [`ScanIndex`]
//! fanout struct that maintains a mandatory AoS base view plus optional
//! SoA, SoAoS, and AoSoA64 overlays.  All active views are populated on
//! every insert; queries route to the most efficient view for each access
//! pattern.
//!
//! ## Memory layout quick-reference
//!
//! | Variant   | Inner representation                                      |
//! |-----------|-----------------------------------------------------------|
//! | SoA       | Three `Vec`s sorted by `(kind, global_sequence)`          |
//! | AoSoA8    | `Vec<Tile<8>>`; each tile holds ≤ 8 events of one kind    |
//! | AoSoA16   | `Vec<Tile<16>>`; fits AVX-512 / Apple M-series cache line |
//! | AoSoA64   | `Vec<Tile<64>>`; fills one full x86 cache line of u64s    |
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
//! `query_by_kind` performs a linear pass, which is cache-optimal for the SoA
//! layout and fully vectorisable for AoSoA once LLVM sees the uniform stride.

use crate::event::EventKind;
use crate::store::index::{ClockKey, DiskPos, IndexEntry};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

type ProjectionCandidates = (u64, u64, Vec<(u64, DiskPos)>);

// ---------------------------------------------------------------------------
// Tile — AoSoA building block
// ---------------------------------------------------------------------------

/// A cache-line-aligned tile that holds up to `N` events of the **same** kind.
///
/// The struct is `repr(C, align(64))` so that the first field begins on a
/// 64-byte cache-line boundary.  The inner `Vec`s are pre-allocated to
/// capacity `N` on construction (see [`Tile::new`]), so no heap reallocation
/// occurs during a tile's lifetime.
///
/// ### Why `Vec` instead of `[T; N]`?
///
/// Const-generic arrays of non-`Copy` types (e.g. `[Arc<IndexEntry>; N]`)
/// require `T: Default`, which `Arc<IndexEntry>` does not implement.  Using
/// `Vec` with a reserved capacity of `N` gives identical runtime behaviour
/// (no extra alloc, pointer locality preserved) while keeping the code
/// straightforward.
#[repr(C, align(64))]
pub struct Tile<const N: usize> {
    /// Event kinds stored in this tile; all entries have the same kind.
    pub kinds: Vec<EventKind>,
    /// `global_sequence` values parallel to `kinds` and `entries`.
    pub sequences: Vec<u64>,
    /// Full index entries parallel to `kinds` and `sequences`.
    pub entries: Vec<Arc<IndexEntry>>,
    /// Number of valid elements currently stored in the tile.
    pub len: usize,
}

impl<const N: usize> Tile<N> {
    /// Create an empty tile pre-allocated to capacity `N`.
    pub(crate) fn new() -> Self {
        Self {
            kinds: Vec::with_capacity(N),
            sequences: Vec::with_capacity(N),
            entries: Vec::with_capacity(N),
            len: 0,
        }
    }

    /// Returns `true` when the tile has no room for another entry.
    #[inline]
    pub(crate) fn is_full(&self) -> bool {
        self.len >= N
    }

    /// Append an entry.  Panics (debug only) if the tile is already full.
    pub(crate) fn push(&mut self, kind: EventKind, sequence: u64, entry: Arc<IndexEntry>) {
        debug_assert!(!self.is_full(), "Tile<{N}>::push called on a full tile");
        self.kinds.push(kind);
        self.sequences.push(sequence);
        self.entries.push(entry);
        self.len += 1;
    }
}

// ---------------------------------------------------------------------------
// SoAInner — the raw parallel-array state
// ---------------------------------------------------------------------------

/// Internal state for the flat SoA (Structure-of-Arrays) layout.
///
/// Events are stored in insertion order (== ascending `global_sequence`).
/// `query_by_kind` iterates linearly; because the `kinds` array is a compact
/// `Vec<u16>` (EventKind is a newtype over `u16`) the loop fits in L1 cache
/// for tens of thousands of events.
struct SoAInner {
    kinds: Vec<EventKind>,
    sequences: Vec<u64>,
    entries: Vec<Arc<IndexEntry>>,
    /// scope → set of entity strings that have emitted at least one event in
    /// that scope.  Mirrors the role of `StoreIndex::scope_entities`.
    scope_entities: std::collections::HashMap<Arc<str>, HashSet<Arc<str>>>,
}

impl SoAInner {
    fn new() -> Self {
        Self {
            kinds: Vec::new(),
            sequences: Vec::new(),
            entries: Vec::new(),
            scope_entities: std::collections::HashMap::new(),
        }
    }

    fn from_entries(entries: &[Arc<IndexEntry>]) -> Self {
        let mut kinds = Vec::with_capacity(entries.len());
        let mut sequences = Vec::with_capacity(entries.len());
        let mut built_entries = Vec::with_capacity(entries.len());
        let mut scope_entities = std::collections::HashMap::<Arc<str>, HashSet<Arc<str>>>::new();

        for entry in entries {
            let scope = entry.coord.scope_arc();
            let entity = entry.coord.entity_arc();
            kinds.push(entry.kind);
            sequences.push(entry.global_sequence);
            built_entries.push(Arc::clone(entry));
            scope_entities.entry(scope).or_default().insert(entity);
        }

        Self {
            kinds,
            sequences,
            entries: built_entries,
            scope_entities,
        }
    }

    /// Append one event.  O(1) amortised.
    fn push(&mut self, entry: &Arc<IndexEntry>) {
        let scope: Arc<str> = entry.coord.scope_arc();
        let entity: Arc<str> = entry.coord.entity_arc();
        self.kinds.push(entry.kind);
        self.sequences.push(entry.global_sequence);
        self.entries.push(Arc::clone(entry));
        self.scope_entities.entry(scope).or_default().insert(entity);
    }

    /// Return all entries whose `kind == target`.  Linear scan; cache-friendly
    /// because `kinds` is a packed `Vec<EventKind>` (2 bytes per element).
    fn query_by_kind(&self, target: EventKind) -> Vec<Arc<IndexEntry>> {
        self.kinds
            .iter()
            .zip(self.entries.iter())
            .filter(|(k, _)| **k == target)
            .map(|(_, e)| Arc::clone(e))
            .collect()
    }

    /// Return all entries whose kind falls in `category` (upper 4 bits).
    fn query_by_category(&self, category: u8) -> Vec<Arc<IndexEntry>> {
        self.kinds
            .iter()
            .zip(self.entries.iter())
            .filter(|(k, _)| k.category() == category)
            .map(|(_, e)| Arc::clone(e))
            .collect()
    }

    /// Return all entries belonging to entities registered under `scope`.
    fn query_by_scope(&self, scope: &str) -> Vec<Arc<IndexEntry>> {
        let Some(entities) = self.scope_entities.get(scope) else {
            return Vec::new();
        };
        self.entries
            .iter()
            .filter(|e| entities.contains(e.coord.entity_arc().as_ref()))
            .map(Arc::clone)
            .collect()
    }

    fn clear(&mut self) {
        self.kinds.clear();
        self.sequences.clear();
        self.entries.clear();
        self.scope_entities.clear();
    }
}

// ---------------------------------------------------------------------------
// AoSoAInner — tiled parallel-array state (generic over tile width N)
// ---------------------------------------------------------------------------

/// Internal state for tiled AoSoA layouts.
///
/// Events are bucketed into tiles by kind: every tile contains entries of a
/// single `EventKind` (matching `kinds[0]` for any non-empty tile).  When the
/// current open tile for a kind is full a new tile is started.
///
/// The outer `Vec` of `Tile`s is unsorted; `query_by_kind` iterates all tiles
/// and collects matching entries.  For workloads with few kinds this is very
/// fast because each tile fits in one or two cache lines.
struct AoSoAInner<const N: usize> {
    tiles: Vec<Tile<N>>,
    /// scope → entity set, same role as in SoAInner.
    scope_entities: std::collections::HashMap<Arc<str>, HashSet<Arc<str>>>,
}

impl<const N: usize> AoSoAInner<N> {
    fn new() -> Self {
        Self {
            tiles: Vec::new(),
            scope_entities: std::collections::HashMap::new(),
        }
    }

    fn from_entries(entries: &[Arc<IndexEntry>]) -> Self {
        let mut built = Self::new();
        for entry in entries {
            built.push(entry);
        }
        built
    }

    /// Append one event into the appropriate tile.
    fn push(&mut self, entry: &Arc<IndexEntry>) {
        let scope: Arc<str> = entry.coord.scope_arc();
        let entity: Arc<str> = entry.coord.entity_arc();
        let kind = entry.kind;
        let seq = entry.global_sequence;

        // Determine whether the last tile can accept this entry: same kind and not full.
        let can_append_to_last = self
            .tiles
            .last()
            .is_some_and(|t| !t.is_full() && t.kinds.first().copied() == Some(kind));

        if can_append_to_last {
            let t = self
                .tiles
                .last_mut()
                .expect("checked above that last() is Some"); // safe: is_some_and confirmed
            t.push(kind, seq, Arc::clone(entry));
        } else {
            let mut tile = Tile::new();
            tile.push(kind, seq, Arc::clone(entry));
            self.tiles.push(tile);
        }

        self.scope_entities.entry(scope).or_default().insert(entity);
    }

    /// Iterate every tile and collect entries whose kind matches `target`.
    fn query_by_kind(&self, target: EventKind) -> Vec<Arc<IndexEntry>> {
        let mut out = Vec::new();
        for tile in &self.tiles {
            // All elements in a tile share the same kind; skip non-matching tiles fast.
            if tile.kinds.first().copied() != Some(target) {
                continue;
            }
            for e in tile.entries.iter().take(tile.len) {
                out.push(Arc::clone(e));
            }
        }
        out
    }

    /// Return all entries whose kind falls in `category` (upper 4 bits).
    /// Skips entire tiles whose kind doesn't match the category.
    fn query_by_category(&self, category: u8) -> Vec<Arc<IndexEntry>> {
        let mut out = Vec::new();
        for tile in &self.tiles {
            if tile.kinds.first().is_none_or(|k| k.category() != category) {
                continue;
            }
            for e in tile.entries.iter().take(tile.len) {
                out.push(Arc::clone(e));
            }
        }
        out
    }

    /// Collect entries belonging to entities in `scope`.
    fn query_by_scope(&self, scope: &str) -> Vec<Arc<IndexEntry>> {
        let Some(entities) = self.scope_entities.get(scope) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for tile in &self.tiles {
            for e in tile.entries.iter().take(tile.len) {
                if entities.contains(e.coord.entity_arc().as_ref()) {
                    out.push(Arc::clone(e));
                }
            }
        }
        out
    }

    /// Execute `f` on the tile at position `idx`.
    ///
    /// Returns `None` if `idx` is out of range.
    pub(crate) fn with_tile<R>(&self, idx: usize, f: impl FnOnce(&Tile<N>) -> R) -> Option<R> {
        self.tiles.get(idx).map(f)
    }

    fn clear(&mut self) {
        self.tiles.clear();
        self.scope_entities.clear();
    }
}

// ---------------------------------------------------------------------------
// ColumnarVariant — erases the const-generic parameter at the enum level
// ── SoAoS: hybrid AoS-outer, SoA-inner ──────────────────────────────────────

/// One entity's events stored as parallel arrays (SoA within an entity group).
#[derive(Clone)]
pub(crate) struct CachedProjectionSlot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) watermark: u64,
    pub(crate) generation: u64,
    pub(crate) cached_at_us: i64,
}

struct EntityGroup {
    kinds: Vec<EventKind>,
    sequences: Vec<u64>,
    entries: Vec<Arc<IndexEntry>>,
    generation: u64,
    cached_projections: std::collections::HashMap<TypeId, CachedProjectionSlot>,
}

/// Hybrid layout: entities looked up by HashMap (AoS outer), events within each
/// entity stored as parallel arrays (SoA inner). Matches the ECS archetype pattern.
struct SoAoSInner {
    groups: std::collections::HashMap<Arc<str>, EntityGroup>,
    scope_entities: std::collections::HashMap<Arc<str>, std::collections::HashSet<Arc<str>>>,
}

impl SoAoSInner {
    fn new() -> Self {
        Self {
            groups: std::collections::HashMap::new(),
            scope_entities: std::collections::HashMap::new(),
        }
    }

    fn from_entries(entries: &[Arc<IndexEntry>]) -> Self {
        let mut built = Self::new();
        for entry in entries {
            built.push(entry);
        }
        built
    }

    fn push(&mut self, entry: &Arc<IndexEntry>) {
        let entity = entry.coord.entity_arc();
        let scope = entry.coord.scope_arc();
        let group = self
            .groups
            .entry(Arc::clone(&entity))
            .or_insert_with(|| EntityGroup {
                kinds: Vec::new(),
                sequences: Vec::new(),
                entries: Vec::new(),
                generation: 0,
                cached_projections: std::collections::HashMap::new(),
            });
        group.kinds.push(entry.kind);
        group.sequences.push(entry.global_sequence);
        group.entries.push(Arc::clone(entry));
        group.generation = group.generation.saturating_add(1);
        self.scope_entities.entry(scope).or_default().insert(entity);
    }

    fn query_by_kind(&self, target: EventKind) -> Vec<Arc<IndexEntry>> {
        let mut out = Vec::new();
        for group in self.groups.values() {
            for (i, &kind) in group.kinds.iter().enumerate() {
                if kind == target {
                    out.push(Arc::clone(&group.entries[i]));
                }
            }
        }
        out
    }

    fn query_by_category(&self, category: u8) -> Vec<Arc<IndexEntry>> {
        let mut out = Vec::new();
        for group in self.groups.values() {
            for (i, &kind) in group.kinds.iter().enumerate() {
                if kind.category() == category {
                    out.push(Arc::clone(&group.entries[i]));
                }
            }
        }
        out
    }

    fn query_by_scope(&self, scope: &str) -> Vec<Arc<IndexEntry>> {
        let mut out = Vec::new();
        if let Some(entities) = self.scope_entities.get(scope) {
            for entity in entities {
                if let Some(group) = self.groups.get(entity.as_ref()) {
                    out.extend(group.entries.iter().map(Arc::clone));
                }
            }
        }
        out
    }

    fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.groups.get(entity).map(|group| group.generation)
    }

    fn projection_candidates(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionCandidates> {
        let group = self.groups.get(entity)?;
        let match_all = relevant_kinds.is_empty();
        let mut candidates = Vec::new();
        let mut watermark = None;

        for ((&kind, &sequence), entry) in group
            .kinds
            .iter()
            .zip(group.sequences.iter())
            .zip(group.entries.iter())
        {
            if !match_all && !relevant_kinds.contains(&kind) {
                continue;
            }
            watermark = Some(sequence);
            candidates.push((sequence, entry.disk_pos));
        }

        Some((watermark?, group.generation, candidates))
    }

    fn cached_projection(&self, entity: &str, type_id: TypeId) -> Option<CachedProjectionSlot> {
        self.groups
            .get(entity)
            .and_then(|group| group.cached_projections.get(&type_id).cloned())
    }

    fn store_cached_projection(
        &mut self,
        entity: &str,
        type_id: TypeId,
        bytes: Vec<u8>,
        watermark: u64,
        cached_at_us: i64,
    ) -> bool {
        let Some(group) = self.groups.get_mut(entity) else {
            return false;
        };
        group.cached_projections.insert(
            type_id,
            CachedProjectionSlot {
                bytes,
                watermark,
                generation: group.generation,
                cached_at_us,
            },
        );
        true
    }

    fn clear(&mut self) {
        self.groups.clear();
        self.scope_entities.clear();
    }
}

// ---------------------------------------------------------------------------

/// Concrete storage variant held inside a [`ColumnarIndex`].
///
/// Each arm holds the mutable inner state behind a `RwLock` so that
/// concurrent readers never block each other.
enum ColumnarVariant {
    /// Flat parallel arrays; best for sequential scans.
    SoA(RwLock<SoAInner>),
    #[cfg(test)]
    /// 8-element tiles; each tile fills one AVX register (256-bit).
    AoSoA8(RwLock<AoSoAInner<8>>),
    #[cfg(test)]
    /// 16-element tiles; fits AVX-512 or Apple M-series 128-byte cache line.
    AoSoA16(RwLock<AoSoAInner<16>>),
    /// 64-element tiles; fills a full x86 cache line of `u64`s.
    AoSoA64(RwLock<AoSoAInner<64>>),
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
            ColumnarVariant::SoAoS(lock) => lock.write().push(entry),
        }
    }

    pub(crate) fn rebuild_from_entries(&self, entries: &[Arc<IndexEntry>]) {
        match &self.inner {
            ColumnarVariant::SoA(lock) => *lock.write() = SoAInner::from_entries(entries),
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => *lock.write() = AoSoAInner::<8>::from_entries(entries),
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => {
                *lock.write() = AoSoAInner::<16>::from_entries(entries)
            }
            ColumnarVariant::AoSoA64(lock) => {
                *lock.write() = AoSoAInner::<64>::from_entries(entries)
            }
            ColumnarVariant::SoAoS(lock) => *lock.write() = SoAoSInner::from_entries(entries),
        }
    }

    /// Return all entries whose `kind` exactly matches `target`, sorted by
    /// `global_sequence` (ascending).
    ///
    /// For SoA the result is already in insertion order (= ascending
    /// `global_sequence`).  For AoSoA tile order is also insertion order, but
    /// we sort the collected results to guarantee stable output regardless of
    /// tile interleaving between different kinds.
    pub(crate) fn query_by_kind(&self, target: EventKind) -> Vec<Arc<IndexEntry>> {
        let mut results = match &self.inner {
            ColumnarVariant::SoA(lock) => lock.read().query_by_kind(target),
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => lock.read().query_by_kind(target),
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => lock.read().query_by_kind(target),
            ColumnarVariant::AoSoA64(lock) => lock.read().query_by_kind(target),
            ColumnarVariant::SoAoS(lock) => lock.read().query_by_kind(target),
        };
        results.sort_by_key(|e| e.global_sequence);
        results
    }

    /// Return all entries whose kind falls in `category` (upper 4 bits),
    /// sorted by `global_sequence` (ascending).
    pub(crate) fn query_by_category(&self, category: u8) -> Vec<Arc<IndexEntry>> {
        let mut results = match &self.inner {
            ColumnarVariant::SoA(lock) => lock.read().query_by_category(category),
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => lock.read().query_by_category(category),
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => lock.read().query_by_category(category),
            ColumnarVariant::AoSoA64(lock) => lock.read().query_by_category(category),
            ColumnarVariant::SoAoS(lock) => lock.read().query_by_category(category),
        };
        results.sort_by_key(|e| e.global_sequence);
        results
    }

    /// Return all entries whose coordinate scope matches `scope`, sorted by
    /// `global_sequence` (ascending).
    pub(crate) fn query_by_scope(&self, scope: &str) -> Vec<Arc<IndexEntry>> {
        let mut results = match &self.inner {
            ColumnarVariant::SoA(lock) => lock.read().query_by_scope(scope),
            #[cfg(test)]
            ColumnarVariant::AoSoA8(lock) => lock.read().query_by_scope(scope),
            #[cfg(test)]
            ColumnarVariant::AoSoA16(lock) => lock.read().query_by_scope(scope),
            ColumnarVariant::AoSoA64(lock) => lock.read().query_by_scope(scope),
            ColumnarVariant::SoAoS(lock) => lock.read().query_by_scope(scope),
        };
        results.sort_by_key(|e| e.global_sequence);
        results
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
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) | ColumnarVariant::SoAoS(_) => {
                None
            }
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
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) | ColumnarVariant::SoAoS(_) => {
                None
            }
            #[cfg(test)]
            ColumnarVariant::AoSoA8(_) => None,
        }
    }

    /// Invoke `f` with an immutable reference to the `Tile<64>` at `idx`.
    /// Returns `None` if `self` is not an `AoSoA64` variant or idx is out of range.
    fn with_tile64<R>(&self, idx: usize, f: impl FnOnce(&Tile<64>) -> R) -> Option<R> {
        match &self.inner {
            ColumnarVariant::AoSoA64(lock) => lock.read().with_tile(idx, f),
            ColumnarVariant::SoA(_) | ColumnarVariant::SoAoS(_) => None,
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
            ColumnarVariant::SoAoS(lock) => lock.write().clear(),
        }
    }

    /// Return the number of tiles for the production tiled overlay, or 0 for
    /// non-tiled layouts.
    pub(crate) fn tile_count(&self) -> usize {
        if self.with_tile64(0, |_| ()).is_some() {
            if let ColumnarVariant::AoSoA64(lock) = &self.inner {
                return lock.read().tiles.len();
            }
        }
        0
    }

    pub(crate) fn entity_generation(&self, entity: &str) -> Option<u64> {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => lock.read().entity_generation(entity),
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => None,
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
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => None,
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
        cached_at_us: i64,
    ) -> bool {
        match &self.inner {
            ColumnarVariant::SoAoS(lock) => lock.write().store_cached_projection(
                entity,
                type_id,
                bytes,
                watermark,
                cached_at_us,
            ),
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => false,
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
            ColumnarVariant::SoA(_) | ColumnarVariant::AoSoA64(_) => None,
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
    /// Tiled replay/scanning overlay.
    tiles64: Option<ColumnarIndex>,
}

impl ScanIndex {
    /// Construct base AoS maps plus the configured optional overlays.
    pub(crate) fn for_config(config: &crate::store::IndexConfig) -> Self {
        use crate::store::IndexLayout;
        let mut soa = config.views.soa;
        let mut entity_groups = config.views.entity_groups;
        let mut tiles64 = config.views.tiles64;

        match config.layout {
            IndexLayout::AoS => {}
            IndexLayout::SoA => soa = true,
            IndexLayout::AoSoA8 | IndexLayout::AoSoA16 | IndexLayout::AoSoA64 => tiles64 = true,
            IndexLayout::SoAoS => entity_groups = true,
        }

        Self {
            by_fact: DashMap::new(),
            scope_entities: DashMap::new(),
            soa: soa.then(ColumnarIndex::new_soa),
            entity_groups: entity_groups.then(ColumnarIndex::new_soaos),
            tiles64: tiles64.then(ColumnarIndex::new_aosoa64),
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

    fn query_base_by_kind(&self, kind: EventKind) -> Vec<Arc<IndexEntry>> {
        let mut results: Vec<Arc<IndexEntry>> = self
            .by_fact
            .get(&kind)
            .map(|r| r.value().values().map(Arc::clone).collect())
            .unwrap_or_default();
        results.sort_by_key(|e| e.global_sequence);
        results
    }

    fn query_base_by_category(&self, category: u8) -> Vec<Arc<IndexEntry>> {
        let mut results: Vec<Arc<IndexEntry>> = self
            .by_fact
            .iter()
            .filter(|r| r.key().category() == category)
            .flat_map(|r| r.value().values().map(Arc::clone).collect::<Vec<_>>())
            .collect();
        results.sort_by_key(|e| e.global_sequence);
        results
    }

    pub(crate) fn layout_name(&self) -> &'static str {
        if self.soa.is_none() && self.entity_groups.is_none() && self.tiles64.is_none() {
            "AoS"
        } else {
            "MultiView"
        }
    }

    pub(crate) fn tile_count(&self) -> usize {
        self.tiles64.as_ref().map_or(0, ColumnarIndex::tile_count)
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
    }

    pub(crate) fn rebuild_from_entries(&self, entries: &[Arc<IndexEntry>]) {
        self.by_fact.clear();
        self.scope_entities.clear();

        let mut by_fact =
            std::collections::HashMap::<EventKind, BTreeMap<ClockKey, Arc<IndexEntry>>>::new();
        let mut scope_entities = std::collections::HashMap::<Arc<str>, HashSet<Arc<str>>>::new();

        for entry in entries {
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
            idx.rebuild_from_entries(entries);
        }
        if let Some(idx) = &self.entity_groups {
            idx.rebuild_from_entries(entries);
        }
        if let Some(idx) = &self.tiles64 {
            idx.rebuild_from_entries(entries);
        }
    }

    /// Return all entries matching `kind`, sorted by `global_sequence`.
    ///
    /// For `Maps`, this clones values out of the `BTreeMap` (ordered by
    /// `ClockKey`, which is equivalent to `global_sequence` order for events
    /// that belong to the same entity stream; a final sort ensures correctness
    /// across entities).
    ///
    /// For `Columnar`, delegates to [`ColumnarIndex::query_by_kind`].
    pub(crate) fn query_by_kind(&self, kind: EventKind) -> Vec<Arc<IndexEntry>> {
        if let Some(idx) = &self.soa {
            return idx.query_by_kind(kind);
        }
        if let Some(idx) = &self.tiles64 {
            return idx.query_by_kind(kind);
        }
        if let Some(idx) = &self.entity_groups {
            return idx.query_by_kind(kind);
        }
        self.query_base_by_kind(kind)
    }

    /// Return all entries whose coordinate scope matches `scope`, sorted by
    /// `global_sequence`.
    ///
    /// For `Maps`, resolves entity names through `scope_entities` and then
    /// falls back to callers re-filtering the stream index (this variant is
    /// intended for use by `StoreIndex::query` which performs that join).
    /// When called standalone it returns the entity set so the caller can join.
    ///
    /// For `Columnar`, delegates to [`ColumnarIndex::query_by_scope`].
    pub(crate) fn query_by_scope(&self, scope: &str) -> Vec<Arc<IndexEntry>> {
        if let Some(idx) = &self.entity_groups {
            return idx.query_by_scope(scope);
        }
        if let Some(idx) = &self.soa {
            return idx.query_by_scope(scope);
        }
        if let Some(idx) = &self.tiles64 {
            return idx.query_by_scope(scope);
        }
        Vec::new()
    }

    /// Return all entries whose kind falls in `category` (upper 4 bits),
    /// sorted by `global_sequence`.
    ///
    /// For `Maps`, iterates all kinds in `by_fact` and collects those matching
    /// the category. For `Columnar`, delegates to
    /// [`ColumnarIndex::query_by_category`].
    pub(crate) fn query_by_category(&self, category: u8) -> Vec<Arc<IndexEntry>> {
        if let Some(idx) = &self.soa {
            return idx.query_by_category(category);
        }
        if let Some(idx) = &self.tiles64 {
            return idx.query_by_category(category);
        }
        if let Some(idx) = &self.entity_groups {
            return idx.query_by_category(category);
        }
        self.query_base_by_category(category)
    }

    /// Return the set of entity strings registered under `scope` (Maps variant only).
    ///
    /// Returns `None` for the Columnar variant — callers should use
    /// [`query_by_scope`] instead.
    ///
    /// [`query_by_scope`]: ScanIndex::query_by_scope
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
    }

    pub(crate) fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.entity_groups
            .as_ref()
            .and_then(|idx| idx.entity_generation(entity))
    }

    pub(crate) fn cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
    ) -> Option<CachedProjectionSlot> {
        self.entity_groups
            .as_ref()
            .and_then(|idx| idx.cached_projection(entity, type_id))
    }

    pub(crate) fn store_cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
        bytes: Vec<u8>,
        watermark: u64,
        cached_at_us: i64,
    ) -> bool {
        self.entity_groups.as_ref().is_some_and(|idx| {
            idx.store_cached_projection(entity, type_id, bytes, watermark, cached_at_us)
        })
    }

    pub(crate) fn projection_candidates(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionCandidates> {
        self.entity_groups
            .as_ref()
            .and_then(|idx| idx.projection_candidates(entity, relevant_kinds))
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
            entity_id: crate::store::interner::InternId::sentinel(),
            scope_id: crate::store::interner::InternId::sentinel(),
            kind,
            wall_ms: seq * 1000,
            clock: u32::try_from(seq).expect("test seq fits u32"),
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
        let a = idx.query_by_kind(KIND_A);
        assert_eq!(a.len(), 10);
        // sorted by global_sequence
        for (i, e) in a.iter().enumerate() {
            assert_eq!(e.global_sequence, i as u64);
        }
        let b = idx.query_by_kind(KIND_B);
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
        let x = idx.query_by_scope("scope-x");
        assert_eq!(x.len(), 6);
        let y = idx.query_by_scope("scope-y");
        assert_eq!(y.len(), 4);
        let z = idx.query_by_scope("scope-z");
        assert!(z.is_empty());
    }

    #[test]
    fn soa_clear() {
        let idx = ColumnarIndex::new_soa();
        for i in 0u64..5 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        idx.clear();
        assert!(idx.query_by_kind(KIND_A).is_empty());
        assert!(idx.query_by_scope("s1").is_empty());
    }

    // --- AoSoA8 ---

    #[test]
    fn aosoa8_insert_spans_multiple_tiles() {
        let idx = ColumnarIndex::new_aosoa8();
        // 20 events of KIND_A → should fill 2 complete tiles + 1 partial (3 total)
        for i in 0u64..20 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let results = idx.query_by_kind(KIND_A);
        assert_eq!(results.len(), 20);
        for (i, e) in results.iter().enumerate() {
            assert_eq!(e.global_sequence, i as u64, "order must be preserved");
        }
    }

    #[test]
    fn aosoa8_interleaved_kinds() {
        let idx = ColumnarIndex::new_aosoa8();
        // Interleaved: push both kinds so tiles can't be pre-filled
        for i in 0u64..12 {
            idx.insert(&make_entry(KIND_A, i * 2, "ea", "s1"));
            idx.insert(&make_entry(KIND_B, i * 2 + 1, "eb", "s1"));
        }
        let a = idx.query_by_kind(KIND_A);
        let b = idx.query_by_kind(KIND_B);
        assert_eq!(a.len(), 12);
        assert_eq!(b.len(), 12);
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
        let alpha = idx.query_by_scope("scope-alpha");
        assert_eq!(alpha.len(), 9);
        let beta = idx.query_by_scope("scope-beta");
        assert_eq!(beta.len(), 5);
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
        assert_eq!(idx.query_by_kind(KIND_A).len(), 33);
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
        assert_eq!(idx.query_by_kind(KIND_A).len(), 130);
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
        assert_eq!(idx.query_by_kind(KIND_A).len(), 10);
        assert_eq!(idx.query_by_kind(KIND_B).len(), 5);
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
        let x = idx.query_by_scope("scope-x");
        assert_eq!(x.len(), 8);
        let y = idx.query_by_scope("scope-y");
        assert_eq!(y.len(), 4);
    }

    #[test]
    fn soaos_clear() {
        let idx = ColumnarIndex::new_soaos();
        for i in 0u64..5 {
            idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(idx.query_by_kind(KIND_A).len(), 5);
        idx.clear();
        assert_eq!(idx.query_by_kind(KIND_A).len(), 0);
    }

    // --- ScanIndex ---

    #[test]
    fn scan_index_maps_variant_insert_and_query() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            layout: crate::store::IndexLayout::AoS,
            views: crate::store::ViewConfig::none(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..7 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let results = si.query_by_kind(KIND_A);
        assert_eq!(results.len(), 7);
    }

    #[test]
    fn scan_index_soa_variant_insert_and_query() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            layout: crate::store::IndexLayout::SoA,
            views: crate::store::ViewConfig::none(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..12 {
            si.insert(&make_entry(KIND_A, i, "e1", "s2"));
        }
        let results = si.query_by_kind(KIND_A);
        assert_eq!(results.len(), 12);
    }

    #[test]
    fn scan_index_aosoa8_variant() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            layout: crate::store::IndexLayout::AoSoA8,
            views: crate::store::ViewConfig::none(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..20 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let results = si.query_by_kind(KIND_A);
        assert_eq!(results.len(), 20);
    }

    #[test]
    fn scan_index_maps_scope_entity_set() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            layout: crate::store::IndexLayout::AoS,
            views: crate::store::ViewConfig::none(),
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
            layout: crate::store::IndexLayout::SoA,
            views: crate::store::ViewConfig::none(),
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
            layout: crate::store::IndexLayout::SoA,
            views: crate::store::ViewConfig::none(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..5 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        si.clear();
        assert!(si.query_by_kind(KIND_A).is_empty());
    }

    #[test]
    fn scan_index_soaos_variant() {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            layout: crate::store::IndexLayout::SoAoS,
            views: crate::store::ViewConfig::none(),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        for i in 0u64..10 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(si.query_by_kind(KIND_A).len(), 10);
        assert_eq!(si.query_by_scope("s1").len(), 10);
        si.clear();
        assert!(si.query_by_kind(KIND_A).is_empty());
    }
}
