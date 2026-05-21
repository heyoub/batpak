#[cfg(test)]
use super::apply_after_bounds;
use super::{CachedProjectionSlot, EntryQuery, ProjectionCandidates, ScanIndex};
use crate::event::EventKind;
use crate::store::index::{ProjectionCacheStoreStatus, QueryHit};
use std::any::TypeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScanRoute {
    BaseAoS,
    SoA,
    SoAoS,
    AoSoA64,
    AoSoA64Simd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectionSupport {
    pub(super) entity_generation_fast_path: bool,
    pub(super) cached_projection: bool,
    pub(super) projection_candidates: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ScanCapabilities {
    pub(super) by_kind: ScanRoute,
    pub(super) by_scope: ScanRoute,
    pub(super) by_category: ScanRoute,
    pub(super) projection: ProjectionSupport,
    pub(super) topology_name: &'static str,
    pub(super) tile_count: usize,
}

impl ScanIndex {
    pub(super) fn capabilities(&self) -> ScanCapabilities {
        let (has_soa, has_entity_groups, has_tiles64, has_tiles64_simd) = (
            self.soa.is_some(),
            self.entity_groups.is_some(),
            self.tiles64.is_some(),
            self.tiles64_simd.is_some(),
        );

        let topology_name = match (has_soa, has_entity_groups, has_tiles64, has_tiles64_simd) {
            (false, false, false, false) => "aos",
            (true, false, false, false) => "scan",
            (false, true, false, false) => "entity-local",
            (false, false, true, false) => "tiled",
            (false, false, false, true) => "tiled-simd",
            (true, true, true, false) => "all",
            _ => "hybrid",
        };

        ScanCapabilities {
            by_kind: if has_soa {
                ScanRoute::SoA
            } else if has_tiles64 {
                ScanRoute::AoSoA64
            } else if has_tiles64_simd {
                ScanRoute::AoSoA64Simd
            } else if has_entity_groups {
                ScanRoute::SoAoS
            } else {
                ScanRoute::BaseAoS
            },
            by_scope: if has_entity_groups {
                ScanRoute::SoAoS
            } else if has_soa {
                ScanRoute::SoA
            } else if has_tiles64 {
                ScanRoute::AoSoA64
            } else if has_tiles64_simd {
                ScanRoute::AoSoA64Simd
            } else {
                ScanRoute::BaseAoS
            },
            by_category: if has_soa {
                ScanRoute::SoA
            } else if has_tiles64 {
                ScanRoute::AoSoA64
            } else if has_tiles64_simd {
                ScanRoute::AoSoA64Simd
            } else if has_entity_groups {
                ScanRoute::SoAoS
            } else {
                ScanRoute::BaseAoS
            },
            projection: ProjectionSupport {
                entity_generation_fast_path: has_entity_groups,
                cached_projection: has_entity_groups,
                projection_candidates: has_entity_groups,
            },
            topology_name,
            tile_count: self
                .tiles64
                .as_ref()
                .or(self.tiles64_simd.as_ref())
                .map_or(0, super::ColumnarIndex::tile_count),
        }
    }

    fn query_base_hits_by_kind(&self, kind: EventKind) -> Vec<QueryHit> {
        let mut results: Vec<QueryHit> = self
            .by_fact
            .get(&kind)
            .map(|r| {
                r.value()
                    .values()
                    .map(|entry| QueryHit::from_entry(entry))
                    .collect()
            })
            .unwrap_or_default();
        results.sort_by_key(|h| h.global_sequence);
        results
    }

    fn query_base_hits_by_category(&self, category: u8) -> Vec<QueryHit> {
        let mut results = Vec::new();
        for entries in self
            .by_fact
            .iter()
            .filter(|r| r.key().category() == category)
        {
            results.extend(
                entries
                    .value()
                    .values()
                    .map(|entry| QueryHit::from_entry(entry)),
            );
        }
        results.sort_by_key(|h| h.global_sequence);
        results
    }

    fn query_hits_route(&self, route: ScanRoute, query: EntryQuery<'_>) -> Vec<QueryHit> {
        let fallback = || match query {
            EntryQuery::Kind(kind) => self.query_base_hits_by_kind(kind),
            EntryQuery::Category(category) => self.query_base_hits_by_category(category),
            EntryQuery::Scope(_) => Vec::new(),
        };
        match (route, query) {
            (ScanRoute::BaseAoS, EntryQuery::Kind(kind)) => self.query_base_hits_by_kind(kind),
            (ScanRoute::BaseAoS, EntryQuery::Category(category)) => {
                self.query_base_hits_by_category(category)
            }
            (ScanRoute::BaseAoS, EntryQuery::Scope(_)) => Vec::new(),
            (ScanRoute::SoA, EntryQuery::Kind(kind)) => self
                .soa
                .as_ref()
                .map_or_else(fallback, |soa| soa.query_hits_by_kind(kind)),
            (ScanRoute::SoA, EntryQuery::Category(category)) => self
                .soa
                .as_ref()
                .map_or_else(fallback, |soa| soa.query_hits_by_category(category)),
            (ScanRoute::SoA, EntryQuery::Scope(scope)) => self
                .soa
                .as_ref()
                .map_or_else(fallback, |soa| soa.query_hits_by_scope(scope)),
            (ScanRoute::SoAoS, EntryQuery::Kind(kind)) => self
                .entity_groups
                .as_ref()
                .map_or_else(fallback, |groups| groups.query_hits_by_kind(kind)),
            (ScanRoute::SoAoS, EntryQuery::Category(category)) => self
                .entity_groups
                .as_ref()
                .map_or_else(fallback, |groups| groups.query_hits_by_category(category)),
            (ScanRoute::SoAoS, EntryQuery::Scope(scope)) => self
                .entity_groups
                .as_ref()
                .map_or_else(fallback, |groups| groups.query_hits_by_scope(scope)),
            (ScanRoute::AoSoA64, EntryQuery::Kind(kind)) => self
                .tiles64
                .as_ref()
                .map_or_else(fallback, |tiles| tiles.query_hits_by_kind(kind)),
            (ScanRoute::AoSoA64, EntryQuery::Category(category)) => self
                .tiles64
                .as_ref()
                .map_or_else(fallback, |tiles| tiles.query_hits_by_category(category)),
            (ScanRoute::AoSoA64, EntryQuery::Scope(scope)) => self
                .tiles64
                .as_ref()
                .map_or_else(fallback, |tiles| tiles.query_hits_by_scope(scope)),
            (ScanRoute::AoSoA64Simd, EntryQuery::Kind(kind)) => self
                .tiles64_simd
                .as_ref()
                .map_or_else(fallback, |tiles| tiles.query_hits_by_kind(kind)),
            (ScanRoute::AoSoA64Simd, EntryQuery::Category(category)) => self
                .tiles64_simd
                .as_ref()
                .map_or_else(fallback, |tiles| tiles.query_hits_by_category(category)),
            (ScanRoute::AoSoA64Simd, EntryQuery::Scope(scope)) => self
                .tiles64_simd
                .as_ref()
                .map_or_else(fallback, |tiles| tiles.query_hits_by_scope(scope)),
        }
    }

    pub(crate) fn query_hits_by_kind(&self, kind: EventKind) -> Vec<QueryHit> {
        self.query_hits_route(self.capabilities().by_kind, EntryQuery::Kind(kind))
    }

    pub(crate) fn query_hits_by_category(&self, category: u8) -> Vec<QueryHit> {
        self.query_hits_route(
            self.capabilities().by_category,
            EntryQuery::Category(category),
        )
    }

    pub(crate) fn query_hits_by_scope(&self, scope: &str) -> Vec<QueryHit> {
        self.query_hits_route(self.capabilities().by_scope, EntryQuery::Scope(scope))
    }

    #[cfg(test)]
    fn query_hits_route_after(
        &self,
        route: ScanRoute,
        query: EntryQuery<'_>,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        let fallback = || match query {
            EntryQuery::Kind(kind) => {
                let mut v = self.query_base_hits_by_kind(kind);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            EntryQuery::Category(cat) => {
                let mut v = self.query_base_hits_by_category(cat);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            EntryQuery::Scope(_) => Vec::new(),
        };
        match (route, query) {
            (ScanRoute::BaseAoS, EntryQuery::Kind(kind)) => {
                let mut v = self.query_base_hits_by_kind(kind);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            (ScanRoute::BaseAoS, EntryQuery::Category(cat)) => {
                let mut v = self.query_base_hits_by_category(cat);
                apply_after_bounds(&mut v, after_seq, started, limit);
                v
            }
            (ScanRoute::BaseAoS, EntryQuery::Scope(_)) => Vec::new(),
            (ScanRoute::SoA, EntryQuery::Kind(kind)) => {
                self.soa.as_ref().map_or_else(fallback, |soa| {
                    soa.query_hits_by_kind_after(kind, after_seq, started, limit)
                })
            }
            (ScanRoute::SoA, EntryQuery::Category(cat)) => {
                self.soa.as_ref().map_or_else(fallback, |soa| {
                    soa.query_hits_by_category_after(cat, after_seq, started, limit)
                })
            }
            (ScanRoute::SoA, EntryQuery::Scope(scope)) => {
                self.soa.as_ref().map_or_else(fallback, |soa| {
                    soa.query_hits_by_scope_after(scope, after_seq, started, limit)
                })
            }
            (ScanRoute::SoAoS, EntryQuery::Kind(kind)) => {
                self.entity_groups.as_ref().map_or_else(fallback, |groups| {
                    let mut v = groups.query_hits_by_kind(kind);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::SoAoS, EntryQuery::Category(cat)) => {
                self.entity_groups.as_ref().map_or_else(fallback, |groups| {
                    let mut v = groups.query_hits_by_category(cat);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::SoAoS, EntryQuery::Scope(scope)) => {
                self.entity_groups.as_ref().map_or_else(fallback, |groups| {
                    let mut v = groups.query_hits_by_scope(scope);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::AoSoA64, EntryQuery::Kind(kind)) => {
                self.tiles64.as_ref().map_or_else(fallback, |tiles| {
                    let mut v = tiles.query_hits_by_kind(kind);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::AoSoA64, EntryQuery::Category(cat)) => {
                self.tiles64.as_ref().map_or_else(fallback, |tiles| {
                    let mut v = tiles.query_hits_by_category(cat);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::AoSoA64, EntryQuery::Scope(scope)) => {
                self.tiles64.as_ref().map_or_else(fallback, |tiles| {
                    let mut v = tiles.query_hits_by_scope(scope);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::AoSoA64Simd, EntryQuery::Kind(kind)) => {
                self.tiles64_simd.as_ref().map_or_else(fallback, |tiles| {
                    let mut v = tiles.query_hits_by_kind(kind);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::AoSoA64Simd, EntryQuery::Category(cat)) => {
                self.tiles64_simd.as_ref().map_or_else(fallback, |tiles| {
                    let mut v = tiles.query_hits_by_category(cat);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
            (ScanRoute::AoSoA64Simd, EntryQuery::Scope(scope)) => {
                self.tiles64_simd.as_ref().map_or_else(fallback, |tiles| {
                    let mut v = tiles.query_hits_by_scope(scope);
                    apply_after_bounds(&mut v, after_seq, started, limit);
                    v
                })
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn query_hits_by_kind_after(
        &self,
        kind: EventKind,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        self.query_hits_route_after(
            self.capabilities().by_kind,
            EntryQuery::Kind(kind),
            after_seq,
            started,
            limit,
        )
    }

    #[cfg(test)]
    pub(crate) fn query_hits_by_category_after(
        &self,
        category: u8,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        self.query_hits_route_after(
            self.capabilities().by_category,
            EntryQuery::Category(category),
            after_seq,
            started,
            limit,
        )
    }

    #[cfg(test)]
    pub(crate) fn query_hits_by_scope_after(
        &self,
        scope: &str,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        self.query_hits_route_after(
            self.capabilities().by_scope,
            EntryQuery::Scope(scope),
            after_seq,
            started,
            limit,
        )
    }

    pub(crate) fn topology_name(&self) -> &'static str {
        self.capabilities().topology_name
    }

    pub(crate) fn tile_count(&self) -> usize {
        self.capabilities().tile_count
    }

    pub(crate) fn entity_generation(&self, entity: &str) -> Option<u64> {
        let projection = self.capabilities().projection;
        if !projection.entity_generation_fast_path {
            return None;
        }
        self.entity_groups
            .as_ref()
            .and_then(|idx| idx.entity_generation(entity))
    }

    pub(crate) fn cached_projection(
        &self,
        entity: &str,
        type_id: TypeId,
    ) -> Option<CachedProjectionSlot> {
        let projection = self.capabilities().projection;
        if !projection.cached_projection {
            return None;
        }
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
    ) -> ProjectionCacheStoreStatus {
        let projection = self.capabilities().projection;
        if !projection.cached_projection {
            return ProjectionCacheStoreStatus::UnsupportedTopology;
        }
        self.entity_groups
            .as_ref()
            .map_or(ProjectionCacheStoreStatus::UnsupportedTopology, |idx| {
                idx.store_cached_projection(entity, type_id, bytes, watermark)
            })
    }

    pub(crate) fn projection_candidates(
        &self,
        entity: &str,
        relevant_kinds: &[EventKind],
    ) -> Option<ProjectionCandidates> {
        let projection = self.capabilities().projection;
        if !projection.projection_candidates {
            return None;
        }
        self.entity_groups
            .as_ref()
            .and_then(|idx| idx.projection_candidates(entity, relevant_kinds))
    }
}
