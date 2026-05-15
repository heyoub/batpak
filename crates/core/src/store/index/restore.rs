use super::IndexEntry;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One contiguous run of entries for the same entity inside the
/// restore-time entity-partitioned ordering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EntityRun {
    pub(crate) entity: String,
    pub(crate) start: u64,
    pub(crate) len: u64,
    pub(crate) first_sequence: u64,
    pub(crate) last_sequence: u64,
}

/// One contiguous chunk of restore-time sequence-sorted entries.
///
/// Chunks are persisted into snapshot artifacts so decode work can be split
/// deterministically without re-deriving ranges from scratch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestoreChunkSummary {
    pub(crate) start: u64,
    pub(crate) len: u64,
    pub(crate) first_sequence: u64,
    pub(crate) last_sequence: u64,
}

/// Restore-time routing summary shared across planner, rebuild, and
/// view materialization. This is intentionally cheap and serializable so the
/// same summary shape can later cross process boundaries without redesign.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RoutingSummary {
    pub(crate) entry_count: u64,
    pub(crate) chunk_count: u64,
    pub(crate) chunks: Vec<RestoreChunkSummary>,
    pub(crate) entity_runs: Vec<EntityRun>,
}

pub(super) struct RestoreBase {
    pub(super) entries_by_sequence: Vec<Arc<IndexEntry>>,
    pub(super) entries_by_entity: Vec<Arc<IndexEntry>>,
    pub(super) routing: RoutingSummary,
}

impl RestoreBase {
    pub(super) fn from_sorted_entries(
        entries: Vec<IndexEntry>,
        chunk_count: usize,
        routing_hint: Option<&RoutingSummary>,
    ) -> Self {
        let entries_by_sequence: Vec<Arc<IndexEntry>> = entries.into_iter().map(Arc::new).collect();
        let mut entries_by_entity = entries_by_sequence.clone();
        entries_by_entity.sort_by(|left, right| {
            left.coord
                .entity()
                .cmp(right.coord.entity())
                .then(left.wall_ms.cmp(&right.wall_ms))
                .then(left.clock.cmp(&right.clock))
                .then(left.event_id.cmp(&right.event_id))
        });

        Self {
            routing: routing_hint
                .filter(|routing| routing.validate(&entries_by_sequence, &entries_by_entity))
                .cloned()
                .unwrap_or_else(|| {
                    RoutingSummary::from_entries(
                        &entries_by_sequence,
                        &entries_by_entity,
                        chunk_count,
                    )
                }),
            entries_by_sequence,
            entries_by_entity,
        }
    }
}

impl RoutingSummary {
    pub(crate) fn from_sorted_entries(entries: &[IndexEntry], chunk_count: usize) -> Self {
        let arcs: Vec<Arc<IndexEntry>> = entries.iter().cloned().map(Arc::new).collect();
        let mut entity_sorted = arcs;
        entity_sorted.sort_by(|left, right| {
            left.coord
                .entity()
                .cmp(right.coord.entity())
                .then(left.wall_ms.cmp(&right.wall_ms))
                .then(left.clock.cmp(&right.clock))
                .then(left.event_id.cmp(&right.event_id))
        });
        Self::from_entries(
            &entries.iter().cloned().map(Arc::new).collect::<Vec<_>>(),
            &entity_sorted,
            chunk_count,
        )
    }

    pub(super) fn from_entries(
        entries_by_sequence: &[Arc<IndexEntry>],
        entries_by_entity: &[Arc<IndexEntry>],
        chunk_count: usize,
    ) -> Self {
        let chunk_count = chunk_count.max(1);
        let mut entity_runs = Vec::new();
        let mut cursor = 0usize;
        while cursor < entries_by_entity.len() {
            let entity = entries_by_entity[cursor].coord.entity().to_owned();
            let start = cursor;
            let first_sequence = entries_by_entity[cursor].global_sequence;
            while cursor < entries_by_entity.len()
                && entries_by_entity[cursor].coord.entity() == entity.as_str()
            {
                let previous = cursor;
                cursor += 1;
                debug_assert!(
                    cursor > previous,
                    "restore routing entity-run scan must advance cursor"
                );
            }
            let last_sequence = entries_by_entity[cursor - 1].global_sequence;
            entity_runs.push(EntityRun {
                entity,
                start: start as u64,
                len: (cursor - start) as u64,
                first_sequence,
                last_sequence,
            });
        }

        let mut chunks = Vec::new();
        if !entries_by_sequence.is_empty() {
            let base = entries_by_sequence.len() / chunk_count;
            let remainder = entries_by_sequence.len() % chunk_count;
            let mut start = 0usize;
            for chunk_index in 0..chunk_count {
                let len = base + usize::from(chunk_index < remainder);
                if len == 0 {
                    continue;
                }
                let end = start + len;
                let first_sequence = entries_by_sequence[start].global_sequence;
                let last_sequence = entries_by_sequence[end - 1].global_sequence;
                chunks.push(RestoreChunkSummary {
                    start: start as u64,
                    len: len as u64,
                    first_sequence,
                    last_sequence,
                });
                start = end;
            }
        }

        Self {
            entry_count: entries_by_entity.len() as u64,
            chunk_count: chunks.len() as u64,
            chunks,
            entity_runs,
        }
    }

    pub(crate) fn validate(
        &self,
        entries_by_sequence: &[Arc<IndexEntry>],
        entries_by_entity: &[Arc<IndexEntry>],
    ) -> bool {
        if self.entry_count != entries_by_sequence.len() as u64
            || self.entry_count != entries_by_entity.len() as u64
        {
            return false;
        }

        let mut chunk_total = 0usize;
        for chunk in &self.chunks {
            let start = match usize::try_from(chunk.start) {
                Ok(start) => start,
                Err(_) => return false,
            };
            let len = match usize::try_from(chunk.len) {
                Ok(len) => len,
                Err(_) => return false,
            };
            let end = match start.checked_add(len) {
                Some(end) => end,
                None => return false,
            };
            if len == 0 || end > entries_by_sequence.len() {
                return false;
            }
            if entries_by_sequence[start].global_sequence != chunk.first_sequence
                || entries_by_sequence[end - 1].global_sequence != chunk.last_sequence
            {
                return false;
            }
            chunk_total += len;
        }
        if chunk_total != entries_by_sequence.len() {
            return false;
        }

        let mut run_total = 0usize;
        for run in &self.entity_runs {
            let start = match usize::try_from(run.start) {
                Ok(start) => start,
                Err(_) => return false,
            };
            let len = match usize::try_from(run.len) {
                Ok(len) => len,
                Err(_) => return false,
            };
            let end = match start.checked_add(len) {
                Some(end) => end,
                None => return false,
            };
            if len == 0 || end > entries_by_entity.len() {
                return false;
            }
            let slice = &entries_by_entity[start..end];
            if slice[0].coord.entity() != run.entity
                || slice[end - start - 1].coord.entity() != run.entity
                || slice[0].global_sequence != run.first_sequence
                || slice[end - start - 1].global_sequence != run.last_sequence
                || slice.iter().any(|entry| entry.coord.entity() != run.entity)
            {
                return false;
            }
            run_total += len;
        }

        run_total == entries_by_entity.len()
    }
}

pub(crate) fn recommended_restore_chunk_count(entry_count: usize) -> usize {
    let chunks = entry_count.div_ceil(65_536);
    chunks.clamp(1, 32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::event::{EventKind, HashChain};
    use crate::store::index::{interner::InternId, DiskPos};
    use std::collections::BTreeMap;
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Duration;

    fn entry(seq: u64, entity: &str) -> IndexEntry {
        IndexEntry {
            event_id: u128::from(seq),
            correlation_id: u128::from(seq),
            causation_id: None,
            coord: Coordinate::new(entity, "scope").expect("coordinate"),
            entity_id: InternId::sentinel(),
            scope_id: InternId::sentinel(),
            kind: EventKind::custom(0x1, 1),
            wall_ms: seq,
            clock: u32::try_from(seq).expect("test sequence fits u32"),
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos::new(0, seq * 64, 64),
            global_sequence: seq,
            receipt_extensions: BTreeMap::new(),
        }
    }

    fn sorted_arcs(entries: Vec<IndexEntry>) -> (Vec<Arc<IndexEntry>>, Vec<Arc<IndexEntry>>) {
        let entries_by_sequence: Vec<_> = entries.into_iter().map(Arc::new).collect();
        let mut entries_by_entity = entries_by_sequence.clone();
        entries_by_entity.sort_by(|a, b| {
            a.coord
                .entity()
                .cmp(b.coord.entity())
                .then(a.global_sequence.cmp(&b.global_sequence))
        });
        (entries_by_sequence, entries_by_entity)
    }

    #[test]
    fn routing_summary_entity_run_scan_makes_forward_progress() {
        let entries = vec![
            entry(0, "alpha"),
            entry(1, "alpha"),
            entry(2, "beta"),
            entry(3, "beta"),
        ];
        let (tx, rx) = mpsc::channel();

        thread::Builder::new()
            .name("routing-summary-progress-regression".to_owned())
            .spawn(move || {
                let summary = RoutingSummary::from_sorted_entries(&entries, 2);
                tx.send(summary.entity_runs)
                    .expect("routing summary receiver is alive");
            })
            .expect("spawn routing summary progress regression thread");

        let runs = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("PROPERTY: routing summary entity scan must not stall");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].entity, "alpha");
        assert_eq!(runs[0].len, 2);
        assert_eq!(runs[1].entity, "beta");
        assert_eq!(runs[1].len, 2);
    }

    #[test]
    fn routing_summary_validate_accepts_in_bounds_entity_runs() {
        let entries = vec![
            entry(0, "alpha"),
            entry(1, "alpha"),
            entry(2, "beta"),
            entry(3, "beta"),
        ];
        let summary = RoutingSummary::from_sorted_entries(&entries, 2);
        let (entries_by_sequence, entries_by_entity) = sorted_arcs(entries);

        assert!(
            summary.validate(&entries_by_sequence, &entries_by_entity),
            "PROPERTY: valid in-bounds entity runs must validate; a run ending before the full entity array length is still valid when its own slice is correct"
        );
    }

    #[test]
    fn routing_summary_validate_rejects_chunk_boundary_mismatches_independently() {
        let entries = vec![
            entry(0, "alpha"),
            entry(1, "alpha"),
            entry(2, "beta"),
            entry(3, "beta"),
        ];
        let summary = RoutingSummary::from_sorted_entries(&entries, 2);
        let (entries_by_sequence, entries_by_entity) = sorted_arcs(entries);

        let mut wrong_first = summary.clone();
        wrong_first.chunks[0].first_sequence += 100;
        assert!(
            !wrong_first.validate(&entries_by_sequence, &entries_by_entity),
            "PROPERTY: chunk validation must reject a mismatched first sequence even when the last sequence still matches"
        );

        let mut wrong_last = summary;
        wrong_last.chunks[0].last_sequence += 100;
        assert!(
            !wrong_last.validate(&entries_by_sequence, &entries_by_entity),
            "PROPERTY: chunk validation must reject a mismatched last sequence even when the first sequence still matches"
        );
    }

    #[test]
    fn routing_summary_validate_rejects_empty_or_out_of_bounds_entity_runs() {
        let entries = vec![
            entry(0, "alpha"),
            entry(1, "alpha"),
            entry(2, "beta"),
            entry(3, "beta"),
        ];
        let summary = RoutingSummary::from_sorted_entries(&entries, 2);
        let (entries_by_sequence, entries_by_entity) = sorted_arcs(entries);

        let mut empty_run = summary.clone();
        empty_run.entity_runs[0].len = 0;
        assert!(
            !empty_run.validate(&entries_by_sequence, &entries_by_entity),
            "PROPERTY: zero-length entity runs are invalid and must not be accepted as harmless no-ops"
        );

        let mut out_of_bounds_run = summary;
        out_of_bounds_run.entity_runs[0].start = entries_by_entity.len() as u64;
        out_of_bounds_run.entity_runs[0].len = 1;
        assert!(
            !out_of_bounds_run.validate(&entries_by_sequence, &entries_by_entity),
            "PROPERTY: entity runs whose end exceeds the entity-sorted table are invalid"
        );
    }
}
