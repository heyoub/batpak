use super::visibility::{extend_visible_entries, VisibilitySnapshot};
use super::{IndexEntry, LaneHeadKey, QueryHit, StoreIndex};
use crate::coordinate::{KindFilter, Region};

impl StoreIndex {
    pub(crate) fn get_by_id(&self, event_id: u128) -> Option<IndexEntry> {
        let _read_guard = self.swap_gate.read();
        let visibility = self.sequence.snapshot();
        self.by_id
            .get(&event_id)
            .map(|r| r.value().as_ref().clone())
            .filter(|e| visibility.is_visible(e.global_sequence))
    }

    /// Upgrade a `QueryHit` to a full `IndexEntry` via the `by_id` DashMap.
    ///
    /// Missing backing entries are treated as fail-closed index corruption:
    /// the hit is dropped and the caller continues with the remaining visible
    /// entries rather than aborting the process.
    pub(crate) fn upgrade_hit(&self, hit: QueryHit) -> Option<IndexEntry> {
        let visibility = self.sequence.snapshot();
        self.upgrade_hit_with_visibility(hit, &visibility)
    }

    pub(crate) fn upgrade_hit_with_visibility(
        &self,
        hit: QueryHit,
        visibility: &VisibilitySnapshot,
    ) -> Option<IndexEntry> {
        let upgraded = self
            .by_id
            .get(&hit.event_id)
            .map(|entry| entry.value().as_ref().clone());
        if upgraded.is_none() {
            tracing::error!(
                target: "batpak::index",
                event_id = hit.event_id,
                global_sequence = hit.global_sequence,
                "dropping query hit with no backing by_id entry"
            );
        }
        upgraded.filter(|entry| visibility.is_visible(entry.global_sequence))
    }

    pub(crate) fn upgrade_hit_visible_on_lane(
        &self,
        hit: QueryHit,
        visibility: &VisibilitySnapshot,
    ) -> Option<IndexEntry> {
        self.upgrade_hit_with_visibility(hit, visibility)
            .filter(|entry| visibility.is_visible_on_lane(entry.global_sequence, entry.dag_lane))
    }

    /// Return all entries matching `region` as lightweight `QueryHit` values.
    ///
    /// Candidate selection picks the cheapest overlay for the primary axis,
    /// then every candidate passes through the shared
    /// [`StoreIndex::filter_region_hits`] pipeline: visibility → scope
    /// revalidation → kind/fact → clock range. Output is sorted by
    /// `global_sequence`.
    pub(crate) fn query_hits_with_snapshot(
        &self,
        region: &Region,
    ) -> (Vec<QueryHit>, VisibilitySnapshot) {
        let _read_guard = self.swap_gate.read();
        let visibility = self.sequence.snapshot();
        let hits = self.query_hits_with_visibility(region, &visibility);
        (hits, visibility)
    }

    fn query_hits_with_visibility(
        &self,
        region: &Region,
        visibility: &VisibilitySnapshot,
    ) -> Vec<QueryHit> {
        let mut hits = self.query_candidate_hits(region, |_| true);

        self.filter_region_hits(&mut hits, region, visibility);
        hits.sort_by_key(|h| h.global_sequence);
        hits
    }

    /// Produce the unfiltered candidate set for the primary query axis.
    ///
    /// The caller supplies `include_stream_entry` for stream-backed candidates
    /// only. Overlay-backed candidates keep their historical behavior and are
    /// narrowed by the shared filter pipeline after selection.
    fn query_candidate_hits<F>(&self, region: &Region, mut include_stream_entry: F) -> Vec<QueryHit>
    where
        F: FnMut(&IndexEntry) -> bool,
    {
        if region.entity_prefix.is_some() {
            return self.stream_hits_matching(region, |entry| include_stream_entry(entry));
        }

        if let Some(ref scope) = region.scope {
            let scan_hits = self.scan.query_hits_by_scope(scope.as_ref());
            if !scan_hits.is_empty() {
                return scan_hits;
            }
            return self
                .scan
                .scope_entity_set(scope.as_ref())
                .map(|entities| {
                    let mut candidates = Vec::new();
                    for entity in &entities {
                        if let Some(stream) = self.streams.get(entity.as_ref()) {
                            for entry in stream.value().values() {
                                if include_stream_entry(entry) {
                                    candidates.push(QueryHit::from_entry(entry));
                                }
                            }
                        }
                    }
                    candidates
                })
                .unwrap_or_default();
        }

        if let Some(ref fact) = region.fact {
            return match fact {
                KindFilter::Exact(k) => self.scan.query_hits_by_kind(*k),
                KindFilter::Category(c) => self.scan.query_hits_by_category(*c),
                KindFilter::Any => self.all_stream_hits_where(include_stream_entry),
            };
        }

        self.all_stream_hits_where(include_stream_entry)
    }

    fn stream_hits_matching<F>(&self, region: &Region, mut include_entry: F) -> Vec<QueryHit>
    where
        F: FnMut(&IndexEntry) -> bool,
    {
        let mut candidates = Vec::new();
        for stream in self
            .streams
            .iter()
            .filter(|r| region.matches_entity(r.key().as_ref()))
        {
            for entry in stream.value().values() {
                if include_entry(entry) {
                    candidates.push(QueryHit::from_entry(entry));
                }
            }
        }
        candidates
    }

    fn all_stream_hits_where<F>(&self, mut include_entry: F) -> Vec<QueryHit>
    where
        F: FnMut(&IndexEntry) -> bool,
    {
        let mut candidates = Vec::new();
        for stream in self.streams.iter() {
            for entry in stream.value().values() {
                if include_entry(entry) {
                    candidates.push(QueryHit::from_entry(entry));
                }
            }
        }
        candidates
    }

    /// Apply every non-candidate Region filter in a single pass:
    /// visibility, scope revalidation (when scope is the requested axis),
    /// kind/fact, and clock range. Does not sort, does not truncate.
    ///
    /// Scope revalidation is the B2 correctness check: overlays return
    /// candidates on a best-effort basis (BaseAoS returns `Vec::new` for
    /// scope queries and the stream fallback does not re-check the entry's
    /// own scope). The re-check here defends the scope-request contract
    /// regardless of which overlay produced the candidate.
    fn filter_region_hits(
        &self,
        hits: &mut Vec<QueryHit>,
        region: &Region,
        visibility: &VisibilitySnapshot,
    ) {
        let requested_scope = region.scope.as_deref();
        let fact_filter = region.fact.as_ref();
        let clock_range = region.clock_range.map(|range| range.as_tuple());
        let lane = region.lane;

        hits.retain(|h| {
            if let Some(lane) = lane {
                if h.dag_lane != lane || !visibility.is_visible_on_lane(h.global_sequence, lane) {
                    return false;
                }
            } else if !visibility.is_visible(h.global_sequence) {
                return false;
            }
            if let Some(scope) = requested_scope {
                match self.by_id.get(&h.event_id) {
                    Some(entry) => {
                        if entry.value().coord.scope() != scope {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            if let Some(fact) = fact_filter {
                let kind_ok = match fact {
                    KindFilter::Exact(k) => h.kind == *k,
                    KindFilter::Category(c) => h.kind.category() == *c,
                    KindFilter::Any => true,
                };
                if !kind_ok {
                    return false;
                }
            }
            if let Some((min, max)) = clock_range {
                if h.clock < min || h.clock > max {
                    return false;
                }
            }
            true
        });
    }

    /// Bounded variant of [`StoreIndex::query_hits`].
    pub(crate) fn query_hits_after(
        &self,
        region: &Region,
        after_seq: u64,
        started: bool,
        limit: usize,
    ) -> Vec<QueryHit> {
        let _read_guard = self.swap_gate.read();
        let visibility = self.sequence.snapshot();
        let seq_ok = |seq: u64| !started || seq > after_seq;

        if region.entity_prefix.is_none()
            && region.scope.is_none()
            && matches!(region.fact, Some(KindFilter::Any))
        {
            return self.query_any_hits_after(region, &visibility, seq_ok, limit);
        }

        let mut hits = self.query_candidate_hits(region, |entry| seq_ok(entry.global_sequence));

        self.filter_region_hits(&mut hits, region, &visibility);
        if started {
            hits.retain(|h| h.global_sequence > after_seq);
        }

        hits.sort_by_key(|h| h.global_sequence);
        hits.truncate(limit);
        hits
    }

    fn query_any_hits_after<F>(
        &self,
        region: &Region,
        visibility: &VisibilitySnapshot,
        mut seq_ok: F,
        limit: usize,
    ) -> Vec<QueryHit>
    where
        F: FnMut(u64) -> bool,
    {
        let clock_range = region.clock_range.map(|range| range.as_tuple());
        let lane = region.lane;
        let trim_threshold = limit
            .saturating_mul(2)
            .max(limit.saturating_add(1))
            .min(1 << 20);
        let initial_cap = limit.min(1 << 20);
        let mut buf: Vec<QueryHit> = Vec::with_capacity(initial_cap);
        let trim = |buf: &mut Vec<QueryHit>, limit: usize| {
            buf.sort_by_key(|h| h.global_sequence);
            buf.truncate(limit);
        };
        for stream in self.streams.iter() {
            for entry in stream.value().values() {
                if !seq_ok(entry.global_sequence) {
                    continue;
                }
                if let Some(lane) = lane {
                    if entry.dag_lane != lane
                        || !visibility.is_visible_on_lane(entry.global_sequence, lane)
                    {
                        continue;
                    }
                } else if !visibility.is_visible(entry.global_sequence) {
                    continue;
                }
                if let Some((min, max)) = clock_range {
                    if entry.clock < min || entry.clock > max {
                        continue;
                    }
                }
                buf.push(QueryHit::from_entry(entry));
                if buf.len() >= trim_threshold {
                    trim(&mut buf, limit);
                }
            }
        }
        trim(&mut buf, limit);
        buf
    }

    /// Returns the latest entry for `entity`, filtered by visibility.
    pub(crate) fn get_latest(&self, entity: &str, lane: u32) -> Option<IndexEntry> {
        let _read_guard = self.swap_gate.read();
        let visibility = self.sequence.snapshot();
        let entity_id = self.interner.get(entity)?;
        self.latest
            .get(&LaneHeadKey::new(entity_id, lane))
            .map(|r| r.value().as_ref().clone())
            .filter(|e| visibility.is_visible_on_lane(e.global_sequence, lane))
    }

    /// Returns the latest committed entry for writer chain construction.
    ///
    /// Unlike [`StoreIndex::get_latest`], this intentionally ignores reader
    /// visibility so multiple writes staged under the same visibility fence
    /// still form a single per-lane hash/clock chain before publication.
    pub(crate) fn get_latest_committed(&self, entity: &str, lane: u32) -> Option<IndexEntry> {
        let _read_guard = self.swap_gate.read();
        let entity_id = self.interner.get(entity)?;
        self.latest
            .get(&LaneHeadKey::new(entity_id, lane))
            .map(|r| r.value().as_ref().clone())
    }

    pub(crate) fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        let _read_guard = self.swap_gate.read();
        let visibility = self.sequence.snapshot();
        self.streams
            .get(entity)
            .map(|r| {
                let mut entries = Vec::with_capacity(r.value().len());
                extend_visible_entries(&mut entries, r.value().values(), &visibility);
                entries
            })
            .unwrap_or_default()
    }

    pub(crate) fn stream_lane(&self, entity: &str, lane: u32) -> Vec<IndexEntry> {
        let _read_guard = self.swap_gate.read();
        let visibility = self.sequence.snapshot();
        self.streams
            .get(entity)
            .map(|r| {
                r.value()
                    .values()
                    .filter(|entry| {
                        entry.dag_lane == lane
                            && visibility.is_visible_on_lane(entry.global_sequence, lane)
                    })
                    .map(|entry| entry.as_ref().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn query(&self, region: &Region) -> Vec<IndexEntry> {
        let (hits, visibility) = self.query_hits_with_snapshot(region);
        hits.into_iter()
            .filter_map(|hit| self.upgrade_hit_with_visibility(hit, &visibility))
            .collect()
    }
}
