use super::EntryQuery;
use crate::event::EventKind;
use crate::store::index::{IndexEntry, QueryHit};
use std::collections::HashSet;
use std::sync::Arc;

/// Internal state for the flat SoA (Structure-of-Arrays) layout.
///
/// Events are stored in insertion order (== ascending `global_sequence`).
/// `query_by_kind` iterates linearly; because the `kinds` array is a compact
/// `Vec<u16>` (EventKind is a newtype over `u16`) the loop fits in L1 cache
/// for tens of thousands of events.
pub(super) struct SoAInner {
    kinds: Vec<EventKind>,
    entries: Vec<Arc<IndexEntry>>,
    // scope membership is correct-by-construction because `coord.scope` is
    // immutable post-construction; debug_assertions verifies invariant at
    // insert time.
    /// scope → set of entity strings that have emitted at least one event in
    /// that scope. Mirrors the role of `StoreIndex::scope_entities`.
    scope_entities: std::collections::HashMap<Arc<str>, HashSet<Arc<str>>>,
}

impl SoAInner {
    pub(super) fn new() -> Self {
        Self {
            kinds: Vec::new(),
            entries: Vec::new(),
            scope_entities: std::collections::HashMap::new(),
        }
    }

    pub(super) fn from_entries(entries: &[Arc<IndexEntry>]) -> Self {
        let mut kinds = Vec::with_capacity(entries.len());
        let mut built_entries = Vec::with_capacity(entries.len());
        let mut scope_entities = std::collections::HashMap::<Arc<str>, HashSet<Arc<str>>>::new();

        for entry in entries {
            let scope = entry.coord.scope_arc();
            let entity = entry.coord.entity_arc();
            kinds.push(entry.kind);
            built_entries.push(Arc::clone(entry));
            scope_entities.entry(scope).or_default().insert(entity);
        }

        Self {
            kinds,
            entries: built_entries,
            scope_entities,
        }
    }

    /// Append one event. O(1) amortised.
    pub(super) fn push(&mut self, entry: &Arc<IndexEntry>) {
        let scope: Arc<str> = entry.coord.scope_arc();
        let entity: Arc<str> = entry.coord.entity_arc();
        debug_assert_eq!(
            scope.as_ref(),
            entry.coord.scope(),
            "scope_entities bucket must match entry.coord.scope()"
        );
        self.kinds.push(entry.kind);
        self.entries.push(Arc::clone(entry));
        self.scope_entities.entry(scope).or_default().insert(entity);
    }

    fn query_hits_entries(&self, mut matches: impl FnMut(EventKind) -> bool) -> Vec<QueryHit> {
        self.kinds
            .iter()
            .zip(self.entries.iter())
            .filter(|(kind, _)| matches(**kind))
            .map(|(_, entry)| QueryHit::from_entry(entry))
            .collect()
    }

    pub(super) fn query_hits_by_kind(&self, target: EventKind) -> Vec<QueryHit> {
        self.query_hits_entries(|kind| kind == target)
    }

    pub(super) fn query_hits_by_category(&self, category: u8) -> Vec<QueryHit> {
        self.query_hits_entries(|kind| kind.category() == category)
    }

    pub(super) fn query_hits_by_scope(&self, scope: &str) -> Vec<QueryHit> {
        let Some(entities) = self.scope_entities.get(scope) else {
            return Vec::new();
        };
        self.entries
            .iter()
            .filter(|entry| entities.contains(entry.coord.entity_arc().as_ref()))
            .map(|entry| QueryHit::from_entry(entry))
            .collect()
    }

    pub(super) fn hits_candidates(&self, spec: &EntryQuery<'_>) -> Vec<QueryHit> {
        match spec {
            EntryQuery::Kind(kind) => self.query_hits_by_kind(*kind),
            EntryQuery::Category(category) => self.query_hits_by_category(*category),
            EntryQuery::Scope(scope) => self.query_hits_by_scope(scope),
        }
    }

    /// Bounded scan: binary-search past already-consumed entries, then scan
    /// forward collecting up to `limit` hits. Output is in ascending
    /// `global_sequence` order (no sort needed — `entries` are in insertion
    /// order which equals ascending global_sequence).
    #[cfg(test)]
    pub(super) fn hits_candidates_after(
        &self,
        spec: &EntryQuery<'_>,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        let start = if started {
            self.entries
                .partition_point(|entry| entry.global_sequence <= after_seq)
        } else {
            0
        };
        let remaining_kinds = &self.kinds[start..];
        let remaining_entries = &self.entries[start..];
        let mut out = Vec::new();

        match spec {
            EntryQuery::Kind(target) => {
                for (kind, entry) in remaining_kinds.iter().zip(remaining_entries.iter()) {
                    if kind == target {
                        out.push(QueryHit::from_entry(entry));
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
            }
            EntryQuery::Category(category) => {
                for (kind, entry) in remaining_kinds.iter().zip(remaining_entries.iter()) {
                    if kind.category() == *category {
                        out.push(QueryHit::from_entry(entry));
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
            }
            EntryQuery::Scope(scope) => {
                let Some(entities) = self.scope_entities.get(*scope) else {
                    return Vec::new();
                };
                for entry in remaining_entries {
                    if entities.contains(entry.coord.entity_arc().as_ref()) {
                        out.push(QueryHit::from_entry(entry));
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        out
    }

    pub(super) fn clear(&mut self) {
        self.kinds.clear();
        self.entries.clear();
        self.scope_entities.clear();
    }
}
