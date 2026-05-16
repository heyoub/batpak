use super::ProjectionCandidates;
use crate::event::EventKind;
use crate::store::index::{projection_kind_matches, IndexEntry, QueryHit, RoutingSummary};
use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// One entity's events stored as parallel arrays (SoA within an entity group).
#[derive(Clone, Debug)]
pub(crate) struct CachedProjectionSlot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) watermark: u64,
    pub(crate) generation: u64,
}

struct EntityGroup {
    kinds: Vec<EventKind>,
    entries: Vec<Arc<IndexEntry>>,
    generation: u64,
    cached_projections: HashMap<TypeId, CachedProjectionSlot>,
}

/// Hybrid layout: entities looked up by HashMap (AoS outer), events within each
/// entity stored as parallel arrays (SoA inner). Matches the ECS archetype pattern.
pub(super) struct SoAoSInner {
    groups: HashMap<Arc<str>, EntityGroup>,
    // scope membership is correct-by-construction because `coord.scope` is
    // immutable post-construction; debug_assertions verifies invariant at
    // insert time.
    scope_entities: HashMap<Arc<str>, HashSet<Arc<str>>>,
}

impl SoAoSInner {
    pub(super) fn new() -> Self {
        Self {
            groups: HashMap::new(),
            scope_entities: HashMap::new(),
        }
    }

    // justifies: src/store/index/restore.rs and src/store/index/columnar/soaos.rs keep routing runs u32-backed; supported targets index them losslessly as usize.
    #[allow(clippy::expect_used)]
    pub(super) fn from_restore_base(
        entries_by_entity: &[Arc<IndexEntry>],
        routing: &RoutingSummary,
    ) -> Self {
        let mut groups = HashMap::with_capacity(routing.entity_runs.len());
        let mut scope_entities = HashMap::<Arc<str>, HashSet<Arc<str>>>::new();

        for run in &routing.entity_runs {
            let start = usize::try_from(run.start)
                .expect("invariant: entity run index fits usize on any supported target");
            let len = usize::try_from(run.len)
                .expect("invariant: entity run length fits usize on any supported target");
            let end = start
                .checked_add(len)
                .expect("invariant: entity run start+len fits usize on supported targets");
            let slice = &entries_by_entity[start..end];
            if slice.is_empty() {
                continue;
            }
            let entity = slice[0].coord.entity_arc();
            let mut group = EntityGroup {
                kinds: Vec::with_capacity(slice.len()),
                entries: Vec::with_capacity(slice.len()),
                generation: slice.len() as u64,
                cached_projections: HashMap::new(),
            };
            for entry in slice {
                group.kinds.push(entry.kind);
                group.entries.push(Arc::clone(entry));
                scope_entities
                    .entry(entry.coord.scope_arc())
                    .or_default()
                    .insert(Arc::clone(&entity));
            }
            groups.insert(entity, group);
        }

        Self {
            groups,
            scope_entities,
        }
    }

    pub(super) fn push(&mut self, entry: &Arc<IndexEntry>) {
        let entity = entry.coord.entity_arc();
        let scope = entry.coord.scope_arc();
        debug_assert_eq!(
            scope.as_ref(),
            entry.coord.scope(),
            "scope_entities bucket must match entry.coord.scope()"
        );
        let group = self
            .groups
            .entry(Arc::clone(&entity))
            .or_insert_with(|| EntityGroup {
                kinds: Vec::new(),
                entries: Vec::new(),
                generation: 0,
                cached_projections: HashMap::new(),
            });
        group.kinds.push(entry.kind);
        group.entries.push(Arc::clone(entry));
        group.generation = group.generation.saturating_add(1);
        self.scope_entities.entry(scope).or_default().insert(entity);
    }

    fn query_hits_entries(&self, mut matches: impl FnMut(EventKind) -> bool) -> Vec<QueryHit> {
        let mut out = Vec::new();
        for group in self.groups.values() {
            for (i, &kind) in group.kinds.iter().enumerate() {
                if matches(kind) {
                    out.push(QueryHit::from_entry(&group.entries[i]));
                }
            }
        }
        out
    }

    pub(super) fn query_hits_by_kind(&self, target: EventKind) -> Vec<QueryHit> {
        self.query_hits_entries(|kind| kind == target)
    }

    pub(super) fn query_hits_by_category(&self, category: u8) -> Vec<QueryHit> {
        self.query_hits_entries(|kind| kind.category() == category)
    }

    pub(super) fn query_hits_by_scope(&self, scope: &str) -> Vec<QueryHit> {
        let mut out = Vec::new();
        if let Some(entities) = self.scope_entities.get(scope) {
            for entity in entities {
                if let Some(group) = self.groups.get(entity.as_ref()) {
                    for entry in &group.entries {
                        out.push(QueryHit::from_entry(entry));
                    }
                }
            }
        }
        out
    }

    pub(super) fn hits_candidates(&self, spec: &super::EntryQuery<'_>) -> Vec<QueryHit> {
        match spec {
            super::EntryQuery::Kind(k) => self.query_hits_by_kind(*k),
            super::EntryQuery::Category(c) => self.query_hits_by_category(*c),
            super::EntryQuery::Scope(s) => self.query_hits_by_scope(s),
        }
    }

    pub(super) fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.groups.get(entity).map(|group| group.generation)
    }

    pub(super) fn projection_candidates(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionCandidates> {
        let group = self.groups.get(entity)?;
        let mut candidates = Vec::new();
        let mut watermark = None;

        for (&kind, entry) in group.kinds.iter().zip(group.entries.iter()) {
            if !projection_kind_matches(relevant_kinds, kind) {
                continue;
            }
            let sequence = entry.global_sequence;
            watermark = Some(sequence);
            candidates.push((sequence, entry.disk_pos));
        }

        Some((watermark?, group.generation, candidates))
    }

    pub(super) fn cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
    ) -> Option<CachedProjectionSlot> {
        self.groups
            .get(entity)
            .and_then(|group| group.cached_projections.get(&type_id).cloned())
    }

    pub(super) fn store_cached_projection(
        &mut self,
        entity: &str,
        type_id: TypeId,
        bytes: Vec<u8>,
        watermark: u64,
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
            },
        );
        true
    }

    pub(super) fn clear(&mut self) {
        self.groups.clear();
        self.scope_entities.clear();
    }
}
