use super::IndexEntry;
use crate::store::StoreError;
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

impl EntityRun {
    /// Convert this persisted `[start, start+len)` run into a `usize` slice
    /// range, surfacing a typed corruption error rather than panicking.
    ///
    /// `start`/`len` are read back from a persisted [`RoutingSummary`]; on an
    /// untrusted artifact they may not fit `usize` or may overflow on
    /// `start + len`. Both conditions are corruption, so they map to a
    /// [`StoreError::CorruptSegment`] (the logical routing artifact has no
    /// segment id, so `0` is used) instead of an unchecked cast or index.
    pub(crate) fn usize_range(&self) -> Result<std::ops::Range<usize>, StoreError> {
        let start = usize::try_from(self.start).map_err(|_| {
            StoreError::corrupt_segment_with_detail(0, "routing entity-run start exceeds usize")
        })?;
        let len = usize::try_from(self.len).map_err(|_| {
            StoreError::corrupt_segment_with_detail(0, "routing entity-run len exceeds usize")
        })?;
        let end = start.checked_add(len).ok_or_else(|| {
            StoreError::corrupt_segment_with_detail(
                0,
                "routing entity-run start+len overflows usize",
            )
        })?;
        Ok(start..end)
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoutingValidation {
    Valid,
    Invalid(RoutingValidationError),
}

impl RoutingValidation {
    pub(crate) fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoutingValidationError {
    EntryCountMismatch,
    ChunkCountMismatch,
    ChunkStartOverflow,
    ChunkLenOverflow,
    ChunkLenZero,
    ChunkEndOverflow,
    ChunkEndOutOfBounds,
    ChunkFirstSequenceMismatch,
    ChunkLastSequenceMismatch,
    ChunkTotalMismatch,
    EntityRunStartOverflow,
    EntityRunLenOverflow,
    EntityRunLenZero,
    EntityRunEndOverflow,
    EntityRunEndOutOfBounds,
    EntityRunEntityMismatch,
    EntityRunFirstSequenceMismatch,
    EntityRunLastSequenceMismatch,
    EntityRunInternalEntityMismatch,
    EntityRunTotalMismatch,
}

impl RestoreBase {
    pub(super) fn from_sorted_entries(
        entries: Vec<IndexEntry>,
        chunk_count: usize,
        routing_hint: Option<&RoutingSummary>,
    ) -> Self {
        let entries_by_sequence: Vec<Arc<IndexEntry>> = entries.into_iter().map(Arc::new).collect();
        let mut entries_by_entity = entries_by_sequence.clone();
        sort_entries_by_entity(&mut entries_by_entity);

        Self {
            routing: routing_hint
                .and_then(|routing| {
                    let validation =
                        routing.validate_detailed(&entries_by_sequence, &entries_by_entity);
                    debug_assert_eq!(
                        routing.validate(&entries_by_sequence, &entries_by_entity),
                        validation.is_valid(),
                        "restore routing bool validation must stay a projection of detailed validation"
                    );
                    match validation {
                        RoutingValidation::Valid => Some(routing.clone()),
                        RoutingValidation::Invalid(error) => {
                            tracing::debug!(
                                ?error,
                                "ignored stale restore routing hint and rebuilt routing summary"
                            );
                            None
                        }
                    }
                })
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
        let entries_by_sequence: Vec<Arc<IndexEntry>> =
            entries.iter().cloned().map(Arc::new).collect();
        let mut entries_by_entity = entries_by_sequence.clone();
        sort_entries_by_entity(&mut entries_by_entity);
        Self::from_entries(&entries_by_sequence, &entries_by_entity, chunk_count)
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
        self.validate_detailed(entries_by_sequence, entries_by_entity)
            .is_valid()
    }

    pub(crate) fn validate_detailed(
        &self,
        entries_by_sequence: &[Arc<IndexEntry>],
        entries_by_entity: &[Arc<IndexEntry>],
    ) -> RoutingValidation {
        if self.entry_count != entries_by_sequence.len() as u64
            || self.entry_count != entries_by_entity.len() as u64
        {
            return RoutingValidation::Invalid(RoutingValidationError::EntryCountMismatch);
        }
        if self.chunk_count != self.chunks.len() as u64 {
            return RoutingValidation::Invalid(RoutingValidationError::ChunkCountMismatch);
        }

        let mut chunk_total = 0usize;
        for chunk in &self.chunks {
            let start = match usize::try_from(chunk.start) {
                Ok(start) => start,
                Err(_) => {
                    return RoutingValidation::Invalid(RoutingValidationError::ChunkStartOverflow);
                }
            };
            let len = match usize::try_from(chunk.len) {
                Ok(len) => len,
                Err(_) => {
                    return RoutingValidation::Invalid(RoutingValidationError::ChunkLenOverflow);
                }
            };
            let end = match start.checked_add(len) {
                Some(end) => end,
                None => {
                    return RoutingValidation::Invalid(RoutingValidationError::ChunkEndOverflow);
                }
            };
            if len == 0 {
                return RoutingValidation::Invalid(RoutingValidationError::ChunkLenZero);
            }
            if end > entries_by_sequence.len() {
                return RoutingValidation::Invalid(RoutingValidationError::ChunkEndOutOfBounds);
            }
            if entries_by_sequence[start].global_sequence != chunk.first_sequence {
                return RoutingValidation::Invalid(
                    RoutingValidationError::ChunkFirstSequenceMismatch,
                );
            }
            if entries_by_sequence[end - 1].global_sequence != chunk.last_sequence {
                return RoutingValidation::Invalid(
                    RoutingValidationError::ChunkLastSequenceMismatch,
                );
            }
            chunk_total += len;
        }
        if chunk_total != entries_by_sequence.len() {
            return RoutingValidation::Invalid(RoutingValidationError::ChunkTotalMismatch);
        }

        let mut run_total = 0usize;
        for run in &self.entity_runs {
            let start = match usize::try_from(run.start) {
                Ok(start) => start,
                Err(_) => {
                    return RoutingValidation::Invalid(
                        RoutingValidationError::EntityRunStartOverflow,
                    );
                }
            };
            let len = match usize::try_from(run.len) {
                Ok(len) => len,
                Err(_) => {
                    return RoutingValidation::Invalid(
                        RoutingValidationError::EntityRunLenOverflow,
                    );
                }
            };
            let end = match start.checked_add(len) {
                Some(end) => end,
                None => {
                    return RoutingValidation::Invalid(
                        RoutingValidationError::EntityRunEndOverflow,
                    );
                }
            };
            if len == 0 {
                return RoutingValidation::Invalid(RoutingValidationError::EntityRunLenZero);
            }
            if end > entries_by_entity.len() {
                return RoutingValidation::Invalid(RoutingValidationError::EntityRunEndOutOfBounds);
            }
            let slice = &entries_by_entity[start..end];
            if slice[0].coord.entity() != run.entity
                || slice[end - start - 1].coord.entity() != run.entity
            {
                return RoutingValidation::Invalid(RoutingValidationError::EntityRunEntityMismatch);
            }
            if slice[0].global_sequence != run.first_sequence {
                return RoutingValidation::Invalid(
                    RoutingValidationError::EntityRunFirstSequenceMismatch,
                );
            }
            if slice[end - start - 1].global_sequence != run.last_sequence {
                return RoutingValidation::Invalid(
                    RoutingValidationError::EntityRunLastSequenceMismatch,
                );
            }
            if slice.iter().any(|entry| entry.coord.entity() != run.entity) {
                return RoutingValidation::Invalid(
                    RoutingValidationError::EntityRunInternalEntityMismatch,
                );
            }
            run_total += len;
        }

        if run_total == entries_by_entity.len() {
            RoutingValidation::Valid
        } else {
            RoutingValidation::Invalid(RoutingValidationError::EntityRunTotalMismatch)
        }
    }
}

fn sort_entries_by_entity(entries: &mut [Arc<IndexEntry>]) {
    entries.sort_by(|left, right| {
        left.coord
            .entity()
            .cmp(right.coord.entity())
            .then(left.wall_ms.cmp(&right.wall_ms))
            .then(left.clock.cmp(&right.clock))
            .then(left.event_id.cmp(&right.event_id))
    });
}

pub(crate) fn recommended_restore_chunk_count(entry_count: usize) -> usize {
    let chunks = entry_count.div_ceil(65_536);
    chunks.clamp(1, 32)
}

pub(crate) fn restore_chunk_ranges(
    entry_count: usize,
    routing: &RoutingSummary,
) -> Vec<(usize, usize)> {
    fn even_ranges(entry_count: usize) -> Vec<(usize, usize)> {
        let chunk_count = recommended_restore_chunk_count(entry_count);
        let base = entry_count / chunk_count;
        let remainder = entry_count % chunk_count;
        let mut start = 0usize;
        let mut ranges = Vec::new();
        for chunk_index in 0..chunk_count {
            let len = base + usize::from(chunk_index < remainder);
            if len == 0 {
                continue;
            }
            ranges.push((start, len));
            start += len;
        }
        ranges
    }

    if routing.chunks.is_empty() {
        return even_ranges(entry_count);
    }

    let mut ranges = Vec::with_capacity(routing.chunks.len());
    let mut expected_start = 0usize;
    for chunk in &routing.chunks {
        let Ok(start) = usize::try_from(chunk.start) else {
            return even_ranges(entry_count);
        };
        let Ok(len) = usize::try_from(chunk.len) else {
            return even_ranges(entry_count);
        };
        let Some(end) = start.checked_add(len) else {
            return even_ranges(entry_count);
        };
        if len == 0 || start != expected_start || end > entry_count {
            return even_ranges(entry_count);
        }
        ranges.push((start, len));
        expected_start = end;
    }

    if expected_start == entry_count {
        ranges
    } else {
        even_ranges(entry_count)
    }
}

#[cfg(test)]
mod tests;
