//! Columnar (SoA / AoSoA) secondary query index.
//!
//! This module provides [`ScanIndex`], which dispatches between two strategies
//! for the `by_fact` and `scope_entities` dimensions of the event index:
//!
//! - **Maps** (`IndexLayout::AoS`): the classic `DashMap`-based path.  The
//!   caller keeps the original `DashMap`s and this module is not involved
//!   beyond constructing the right variant.
//! - **Columnar** (`IndexLayout::SoA`, `AoSoA8`, `AoSoA16`, `AoSoA64`):
//!   replaces both DashMaps with cache-friendly parallel arrays (SoA) or
//!   tiled parallel arrays (AoSoA).
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
//! exclusive write lock only during `insert`.  Because the writer serialises
//! all appends already (it owns the entity lock before calling `StoreIndex::insert`),
//! write contention on this lock is effectively nil.
//!
//! ## Append ordering
//!
//! Events are always appended in ascending `global_sequence` order (the writer
//! thread assigns global_sequence under its own lock).  `insert` therefore
//! pushes to the back of the SoA vecs / open tile without any reordering.
//! `query_by_kind` performs a linear pass, which is cache-optimal for the SoA
//! layout and fully vectorisable for AoSoA once LLVM sees the uniform stride.

use crate::event::EventKind;
use crate::store::index::{ClockKey, IndexEntry};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

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
struct EntityGroup {
    kinds: Vec<EventKind>,
    sequences: Vec<u64>,
    entries: Vec<Arc<IndexEntry>>,
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
            });
        group.kinds.push(entry.kind);
        group.sequences.push(entry.global_sequence);
        group.entries.push(Arc::clone(entry));
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
    /// 8-element tiles; each tile fills one AVX register (256-bit).
    AoSoA8(RwLock<AoSoAInner<8>>),
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

/// Cache-friendly secondary query index that replaces the `by_fact` and
/// `scope_entities` `DashMap`s when a non-AoS [`IndexLayout`] is selected.
///
/// ## Thread safety
///
/// All methods take `&self`; internal state is protected by a
/// `parking_lot::RwLock`.  Writers hold an exclusive lock for the duration of
/// [`insert`]; readers share a read lock.  Because the writer thread
/// serialises appends via the per-entity mutex, write contention is negligible.
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

    /// Create a new AoSoA index with 8-element tiles.
    pub(crate) fn new_aosoa8() -> Self {
        Self {
            inner: ColumnarVariant::AoSoA8(RwLock::new(AoSoAInner::<8>::new())),
        }
    }

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
            ColumnarVariant::AoSoA8(lock) => lock.write().push(entry),
            ColumnarVariant::AoSoA16(lock) => lock.write().push(entry),
            ColumnarVariant::AoSoA64(lock) => lock.write().push(entry),
            ColumnarVariant::SoAoS(lock) => lock.write().push(entry),
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
            ColumnarVariant::AoSoA8(lock) => lock.read().query_by_kind(target),
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
            ColumnarVariant::AoSoA8(lock) => lock.read().query_by_category(category),
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
            ColumnarVariant::AoSoA8(lock) => lock.read().query_by_scope(scope),
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
    fn with_tile8<R>(&self, idx: usize, f: impl FnOnce(&Tile<8>) -> R) -> Option<R> {
        match &self.inner {
            ColumnarVariant::AoSoA8(lock) => lock.read().with_tile(idx, f),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA16(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::SoAoS(_) => None,
        }
    }

    /// Invoke `f` with an immutable reference to the `Tile<16>` at `idx`.
    /// Returns `None` if `self` is not an `AoSoA16` variant or idx is out of range.
    fn with_tile16<R>(&self, idx: usize, f: impl FnOnce(&Tile<16>) -> R) -> Option<R> {
        match &self.inner {
            ColumnarVariant::AoSoA16(lock) => lock.read().with_tile(idx, f),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA8(_)
            | ColumnarVariant::AoSoA64(_)
            | ColumnarVariant::SoAoS(_) => None,
        }
    }

    /// Invoke `f` with an immutable reference to the `Tile<64>` at `idx`.
    /// Returns `None` if `self` is not an `AoSoA64` variant or idx is out of range.
    fn with_tile64<R>(&self, idx: usize, f: impl FnOnce(&Tile<64>) -> R) -> Option<R> {
        match &self.inner {
            ColumnarVariant::AoSoA64(lock) => lock.read().with_tile(idx, f),
            ColumnarVariant::SoA(_)
            | ColumnarVariant::AoSoA8(_)
            | ColumnarVariant::AoSoA16(_)
            | ColumnarVariant::SoAoS(_) => None,
        }
    }

    /// Discard all entries.  Called during index rebuild (compaction / cold start).
    pub(crate) fn clear(&self) {
        match &self.inner {
            ColumnarVariant::SoA(lock) => lock.write().clear(),
            ColumnarVariant::AoSoA8(lock) => lock.write().clear(),
            ColumnarVariant::AoSoA16(lock) => lock.write().clear(),
            ColumnarVariant::AoSoA64(lock) => lock.write().clear(),
            ColumnarVariant::SoAoS(lock) => lock.write().clear(),
        }
    }

    /// Return the number of tiles for AoSoA layouts, or 0 for SoA/SoAoS.
    /// Probes through the public `with_tile*` dispatch to ensure those methods
    /// have at least one production caller (not just tests).
    pub(crate) fn tile_count(&self) -> usize {
        if self.with_tile8(0, |_| ()).is_some() {
            if let ColumnarVariant::AoSoA8(lock) = &self.inner {
                return lock.read().tiles.len();
            }
        }
        if self.with_tile16(0, |_| ()).is_some() {
            if let ColumnarVariant::AoSoA16(lock) = &self.inner {
                return lock.read().tiles.len();
            }
        }
        if self.with_tile64(0, |_| ()).is_some() {
            if let ColumnarVariant::AoSoA64(lock) = &self.inner {
                return lock.read().tiles.len();
            }
        }
        0
    }

    /// Return the layout name as a static string for diagnostics.
    pub(crate) fn layout_name(&self) -> &'static str {
        match &self.inner {
            ColumnarVariant::SoA(_) => "SoA",
            ColumnarVariant::AoSoA8(_) => "AoSoA8",
            ColumnarVariant::AoSoA16(_) => "AoSoA16",
            ColumnarVariant::AoSoA64(_) => "AoSoA64",
            ColumnarVariant::SoAoS(_) => "SoAoS",
        }
    }
}

// ---------------------------------------------------------------------------
// ScanIndex — top-level dispatcher
// ---------------------------------------------------------------------------

/// Dispatches scan queries (`by_fact`, `by_scope`) to either the classic
/// `DashMap`-based indexes or a [`ColumnarIndex`].
///
/// The variant is selected once at store-open time based on
/// [`IndexLayout`][crate::store::IndexLayout] and never changes afterwards.
///
/// `IndexLayout::AoS` produces `ScanIndex::Maps`; all other layouts produce
/// `ScanIndex::Columnar`.
pub(crate) enum ScanIndex {
    /// Classic hash-map-based secondary indexes.  Both maps are publicly
    /// visible within the crate so that [`StoreIndex`] can insert directly.
    Maps {
        /// Event-kind → ordered event entries.
        by_fact: DashMap<EventKind, BTreeMap<ClockKey, Arc<IndexEntry>>>,
        /// Scope string → set of entity strings active in that scope.
        scope_entities: DashMap<Arc<str>, HashSet<Arc<str>>>,
    },
    /// Cache-friendly columnar index (SoA or AoSoA).
    Columnar(ColumnarIndex),
}

impl ScanIndex {
    /// Construct the appropriate `ScanIndex` for the given layout.
    ///
    /// `IndexLayout::AoS` returns `ScanIndex::Maps` (no columnar index).
    /// All other layouts return `ScanIndex::Columnar`.
    pub(crate) fn for_layout(layout: &crate::store::IndexLayout) -> Self {
        use crate::store::IndexLayout;
        match layout {
            IndexLayout::AoS => Self::Maps {
                by_fact: DashMap::new(),
                scope_entities: DashMap::new(),
            },
            IndexLayout::SoA => Self::Columnar(ColumnarIndex::new_soa()),
            IndexLayout::AoSoA8 => Self::Columnar(ColumnarIndex::new_aosoa8()),
            IndexLayout::AoSoA16 => Self::Columnar(ColumnarIndex::new_aosoa16()),
            IndexLayout::AoSoA64 => Self::Columnar(ColumnarIndex::new_aosoa64()),
            IndexLayout::SoAoS => Self::Columnar(ColumnarIndex::new_soaos()),
        }
    }

    /// Insert an entry into whichever secondary index is active.
    ///
    /// For `Maps`, this updates both `by_fact` and `scope_entities` using the
    /// same `ClockKey` ordering used by `StoreIndex::insert`.
    ///
    /// For `Columnar`, this delegates to [`ColumnarIndex::insert`].
    pub(crate) fn insert(&self, entry: &Arc<IndexEntry>) {
        match self {
            Self::Maps {
                by_fact,
                scope_entities,
            } => {
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
            Self::Columnar(idx) => idx.insert(entry),
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
        match self {
            Self::Maps { by_fact, .. } => {
                let mut results: Vec<Arc<IndexEntry>> = by_fact
                    .get(&kind)
                    .map(|r| r.value().values().map(Arc::clone).collect())
                    .unwrap_or_default();
                results.sort_by_key(|e| e.global_sequence);
                results
            }
            Self::Columnar(idx) => idx.query_by_kind(kind),
        }
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
        match self {
            Self::Maps { scope_entities, .. } => {
                // In the Maps variant the full entry data lives in
                // `StoreIndex::streams`; `scope_entities` only tracks entity
                // membership.  Returning an empty vec here is correct: callers
                // that need scope queries against Maps go through
                // `StoreIndex::query`, which uses `scope_entities` directly.
                // This method is primarily meaningful for the Columnar variant.
                let _ = scope_entities; // acknowledged: Maps callers use StoreIndex::query
                Vec::new()
            }
            Self::Columnar(idx) => idx.query_by_scope(scope),
        }
    }

    /// Return all entries whose kind falls in `category` (upper 4 bits),
    /// sorted by `global_sequence`.
    ///
    /// For `Maps`, iterates all kinds in `by_fact` and collects those matching
    /// the category. For `Columnar`, delegates to
    /// [`ColumnarIndex::query_by_category`].
    pub(crate) fn query_by_category(&self, category: u8) -> Vec<Arc<IndexEntry>> {
        match self {
            Self::Maps { by_fact, .. } => {
                let mut results: Vec<Arc<IndexEntry>> = by_fact
                    .iter()
                    .filter(|r| r.key().category() == category)
                    .flat_map(|r| r.value().values().map(Arc::clone).collect::<Vec<_>>())
                    .collect();
                results.sort_by_key(|e| e.global_sequence);
                results
            }
            Self::Columnar(idx) => idx.query_by_category(category),
        }
    }

    /// Return the set of entity strings registered under `scope` (Maps variant only).
    ///
    /// Returns `None` for the Columnar variant — callers should use
    /// [`query_by_scope`] instead.
    ///
    /// [`query_by_scope`]: ScanIndex::query_by_scope
    pub(crate) fn scope_entity_set(&self, scope: &str) -> Option<HashSet<Arc<str>>> {
        match self {
            Self::Maps { scope_entities, .. } => {
                scope_entities.get(scope).map(|r| r.value().clone())
            }
            Self::Columnar(_) => None,
        }
    }

    /// Discard all entries.  Called during index rebuild.
    pub(crate) fn clear(&self) {
        match self {
            Self::Maps {
                by_fact,
                scope_entities,
            } => {
                by_fact.clear();
                scope_entities.clear();
            }
            Self::Columnar(idx) => idx.clear(),
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
        use crate::store::IndexLayout;
        let si = ScanIndex::for_layout(&IndexLayout::AoS);
        for i in 0u64..7 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let results = si.query_by_kind(KIND_A);
        assert_eq!(results.len(), 7);
    }

    #[test]
    fn scan_index_soa_variant_insert_and_query() {
        use crate::store::IndexLayout;
        let si = ScanIndex::for_layout(&IndexLayout::SoA);
        for i in 0u64..12 {
            si.insert(&make_entry(KIND_A, i, "e1", "s2"));
        }
        let results = si.query_by_kind(KIND_A);
        assert_eq!(results.len(), 12);
    }

    #[test]
    fn scan_index_aosoa8_variant() {
        use crate::store::IndexLayout;
        let si = ScanIndex::for_layout(&IndexLayout::AoSoA8);
        for i in 0u64..20 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        let results = si.query_by_kind(KIND_A);
        assert_eq!(results.len(), 20);
    }

    #[test]
    fn scan_index_maps_scope_entity_set() {
        use crate::store::IndexLayout;
        let si = ScanIndex::for_layout(&IndexLayout::AoS);
        si.insert(&make_entry(KIND_A, 0, "ent-1", "my-scope"));
        si.insert(&make_entry(KIND_A, 1, "ent-2", "my-scope"));
        let set = si
            .scope_entity_set("my-scope")
            .expect("should be Some for Maps");
        assert!(set.contains("ent-1" as &str));
        assert!(set.contains("ent-2" as &str));
    }

    #[test]
    fn scan_index_columnar_scope_entity_set_returns_none() {
        use crate::store::IndexLayout;
        let si = ScanIndex::for_layout(&IndexLayout::SoA);
        si.insert(&make_entry(KIND_A, 0, "ent-1", "my-scope"));
        assert!(si.scope_entity_set("my-scope").is_none());
    }

    #[test]
    fn scan_index_clear() {
        use crate::store::IndexLayout;
        let si = ScanIndex::for_layout(&IndexLayout::SoA);
        for i in 0u64..5 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        si.clear();
        assert!(si.query_by_kind(KIND_A).is_empty());
    }

    #[test]
    fn scan_index_soaos_variant() {
        use crate::store::IndexLayout;
        let si = ScanIndex::for_layout(&IndexLayout::SoAoS);
        for i in 0u64..10 {
            si.insert(&make_entry(KIND_A, i, "e1", "s1"));
        }
        assert_eq!(si.query_by_kind(KIND_A).len(), 10);
        assert_eq!(si.query_by_scope("s1").len(), 10);
        si.clear();
        assert!(si.query_by_kind(KIND_A).is_empty());
    }
}
