use super::{event_kind_raw, EntryQuery};
use crate::event::EventKind;
use crate::store::index::{IndexEntry, QueryHit};
use std::collections::HashSet;
use std::sync::Arc;

/// A fixed-width 64-slot tile that holds events of **any** kind.
///
/// Unlike [`super::Tile`], `Tile64Simd` stores kind values in an inline
/// `[u16; 64]` array rather than a heap-allocated `Vec`. This lets the compiler
/// see a contiguous, fixed-size comparison array and auto-vectorize the scan
/// loop: no heap pointer dereference, no dynamic dispatch, just 64 `u16`
/// values sitting in a cache line.
///
/// The trade-off versus the generic kind-homogeneous tiles:
/// - **No tile-skip**: tiles contain mixed kinds, so every tile must be scanned.
/// - **Vectorizable comparison**: the `kinds_raw` loop has a fixed bound and
///   may be auto-vectorized by the compiler with SIMD instructions.
/// - **Better interleaved fill**: one open tile accepts any kind, so
///   interleaved multi-kind workloads produce fully-packed tiles.
#[repr(C, align(64))]
pub(super) struct Tile64Simd {
    /// Raw `u16` kind values, inline. Slots beyond `len` are zero-padded.
    kinds_raw: [u16; 64],
    /// Full index entries parallel to `kinds_raw`.
    entries: Vec<Arc<IndexEntry>>,
    /// Number of valid elements currently stored (<= 64).
    len: usize,
}

impl Tile64Simd {
    pub(super) fn new() -> Self {
        Self {
            kinds_raw: [0u16; 64],
            entries: Vec::with_capacity(64),
            len: 0,
        }
    }

    #[inline]
    pub(super) fn is_full(&self) -> bool {
        self.len >= 64
    }

    pub(super) fn push(&mut self, kind: EventKind, entry: Arc<IndexEntry>) {
        debug_assert!(!self.is_full(), "Tile64Simd::push called on a full tile");
        self.kinds_raw[self.len] = event_kind_raw(kind);
        self.entries.push(entry);
        self.len += 1;
    }

    fn collect_hits_by_kind(&self, target_raw: u16, out: &mut Vec<QueryHit>) {
        let n = self.len;
        let mut hits = [0u8; 64];
        for (hit, &kind_raw) in hits[..n].iter_mut().zip(&self.kinds_raw[..n]) {
            *hit = (kind_raw == target_raw) as u8;
        }
        for (hit, entry) in hits[..n].iter().zip(&self.entries[..n]) {
            if *hit != 0 {
                out.push(QueryHit::from_entry(entry));
            }
        }
    }

    fn collect_hits_by_category(&self, category: u8, out: &mut Vec<QueryHit>) {
        let n = self.len;
        let mut hits = [0u8; 64];
        for (hit, &kind_raw) in hits[..n].iter_mut().zip(&self.kinds_raw[..n]) {
            *hit = ((kind_raw >> 12) as u8 == category) as u8;
        }
        for (hit, entry) in hits[..n].iter().zip(&self.entries[..n]) {
            if *hit != 0 {
                out.push(QueryHit::from_entry(entry));
            }
        }
    }
}

/// Internal state for the experimental mixed-kind AoSoA64Simd layout.
///
/// Fill strategy: one open tile at a time, any kind accepted. When the open
/// tile fills (64 entries), a new tile is allocated. This produces fully-packed
/// tiles regardless of insertion order, at the cost of no tile-skip.
///
/// Query path: every tile is scanned via the two-pass `collect_hits_by_kind`
/// / `collect_hits_by_category` methods on [`Tile64Simd`], which are designed
/// to be auto-vectorized by the compiler.
pub(super) struct AoSoA64SimdInner {
    pub(super) tiles: Vec<Tile64Simd>,
    /// Index of the current open (not yet full) tile, or `None` if all tiles
    /// are full or no tiles have been allocated yet.
    open_tile: Option<usize>,
    // scope membership is correct-by-construction because `coord.scope` is
    // immutable post-construction; debug_assertions verifies invariant at
    // insert time.
    scope_entities: std::collections::HashMap<Arc<str>, HashSet<Arc<str>>>,
}

impl AoSoA64SimdInner {
    pub(super) fn new() -> Self {
        Self {
            tiles: Vec::new(),
            open_tile: None,
            scope_entities: std::collections::HashMap::new(),
        }
    }

    pub(super) fn from_entries(entries: &[Arc<IndexEntry>]) -> Self {
        let mut built = Self::new();
        for entry in entries {
            built.push(entry);
        }
        built
    }

    pub(super) fn push(&mut self, entry: &Arc<IndexEntry>) {
        let scope: Arc<str> = entry.coord.scope_arc();
        let entity: Arc<str> = entry.coord.entity_arc();
        let kind = entry.kind;
        debug_assert_eq!(
            scope.as_ref(),
            entry.coord.scope(),
            "scope_entities bucket must match entry.coord.scope()"
        );

        match self.open_tile {
            Some(idx) => {
                self.tiles[idx].push(kind, Arc::clone(entry));
                if self.tiles[idx].is_full() {
                    self.open_tile = None;
                }
            }
            None => {
                let new_idx = self.tiles.len();
                let mut tile = Tile64Simd::new();
                tile.push(kind, Arc::clone(entry));
                let is_full = tile.is_full();
                self.tiles.push(tile);
                if !is_full {
                    self.open_tile = Some(new_idx);
                }
            }
        }

        self.scope_entities.entry(scope).or_default().insert(entity);
    }

    pub(super) fn query_hits_by_kind(&self, target: EventKind) -> Vec<QueryHit> {
        let target_raw = event_kind_raw(target);
        let mut out = Vec::new();
        for tile in &self.tiles {
            tile.collect_hits_by_kind(target_raw, &mut out);
        }
        out
    }

    pub(super) fn query_hits_by_category(&self, category: u8) -> Vec<QueryHit> {
        let mut out = Vec::new();
        for tile in &self.tiles {
            tile.collect_hits_by_category(category, &mut out);
        }
        out
    }

    pub(super) fn query_hits_by_scope(&self, scope: &str) -> Vec<QueryHit> {
        let Some(entities) = self.scope_entities.get(scope) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for tile in &self.tiles {
            for entry in tile.entries.iter().take(tile.len) {
                if entities.contains(entry.coord.entity_arc().as_ref()) {
                    out.push(QueryHit::from_entry(entry));
                }
            }
        }
        out
    }

    pub(super) fn hits_candidates(&self, spec: &EntryQuery<'_>) -> Vec<QueryHit> {
        match spec {
            EntryQuery::Kind(kind) => self.query_hits_by_kind(*kind),
            EntryQuery::Category(category) => self.query_hits_by_category(*category),
            EntryQuery::Scope(scope) => self.query_hits_by_scope(scope),
        }
    }

    pub(super) fn clear(&mut self) {
        self.tiles.clear();
        self.open_tile = None;
        self.scope_entities.clear();
    }
}
