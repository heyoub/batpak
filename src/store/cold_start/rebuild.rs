use crate::coordinate::Coordinate;
use crate::store::cold_start::ColdStartPolicy;
use crate::store::config::duration_micros;
use crate::store::index::interner::StringInterner;
use crate::store::index::{DiskPos, IndexEntry, RoutingSummary, StoreIndex};
use crate::store::segment;
use crate::store::segment::scan::{Reader, ScannedIndexEntry};
use crate::store::segment::sidx::ReservedKindFallbackStats;
use crate::store::StoreError;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) const COMPACTION_MARKER_FILENAME: &str = "compaction.pending.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingCompaction {
    pub merged_id: u64,
    pub source_segment_ids: Vec<u64>,
}

/// Which cold-start restore strategy was actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OpenIndexPath {
    /// Restored from the mmap snapshot (`index.fbati`) plus tail replay.
    Mmap,
    /// Restored from the checkpoint (`index.ckpt`) plus tail replay.
    Checkpoint,
    /// Full rebuild from segment files (parallel SIDX + sequential active).
    Rebuild,
}

/// Diagnostic output from `open_index()`. Hard truth, not logs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct OpenIndexReport {
    /// Which restore strategy was selected and completed.
    pub path: OpenIndexPath,
    /// Number of entries restored from the snapshot (mmap or checkpoint body).
    pub restored_entries: usize,
    /// Number of entries replayed from the tail after the snapshot watermark.
    pub tail_entries: usize,
    /// Wall-clock microseconds for the entire open_index() call.
    pub elapsed_us: u64,
    /// Number of unknown reserved system-kind SIDX/mmap values that fell back
    /// to `DATA` during reopen.
    pub unknown_reserved_system_kind_fallbacks: usize,
    /// Histogram of raw reserved system-kind values encountered during this reopen.
    pub unknown_reserved_system_kind_histogram: BTreeMap<u16, usize>,
    /// Number of unknown reserved effect-kind SIDX/mmap values that fell back
    /// to `EFFECT_ERROR` during reopen.
    pub unknown_reserved_effect_kind_fallbacks: usize,
    /// Histogram of raw reserved effect-kind values encountered during this reopen.
    pub unknown_reserved_effect_kind_histogram: BTreeMap<u16, usize>,
    /// Cumulative number of unknown reserved system-kind fallbacks persisted
    /// through this store's cold-start artifacts, including this reopen.
    pub cumulative_unknown_reserved_system_kind_fallbacks: usize,
    /// Cumulative histogram of raw reserved system-kind values persisted through
    /// this store's cold-start artifacts, including this reopen.
    pub cumulative_unknown_reserved_system_kind_histogram: BTreeMap<u16, usize>,
    /// Cumulative number of unknown reserved effect-kind fallbacks persisted
    /// through this store's cold-start artifacts, including this reopen.
    pub cumulative_unknown_reserved_effect_kind_fallbacks: usize,
    /// Cumulative histogram of raw reserved effect-kind values persisted
    /// through this store's cold-start artifacts, including this reopen.
    pub cumulative_unknown_reserved_effect_kind_histogram: BTreeMap<u16, usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenIndexOutcome {
    pub(crate) report: OpenIndexReport,
    pub(crate) cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreSource {
    Mmap,
    Checkpoint,
    SealedSidxRebuild,
    FrameScanFallback,
}

struct RestorePlan {
    source: RestoreSource,
    entries: Vec<IndexEntry>,
    interner_strings: Vec<String>,
    allocator_hint: u64,
    routing: RoutingSummary,
    restored_entries: usize,
    tail_entries: usize,
    reopen_reserved_kind_fallbacks: ReservedKindFallbackStats,
    persisted_cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

struct SnapshotPlanInput {
    entries: Vec<IndexEntry>,
    interner_strings: Vec<String>,
    watermark: super::checkpoint::WatermarkInfo,
    stored_allocator: u64,
    routing: RoutingSummary,
    reopen_reserved_kind_fallbacks: ReservedKindFallbackStats,
    persisted_cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
}

struct RestorePlanner<'a> {
    reader: &'a Reader,
    data_dir: &'a Path,
    policy: ColdStartPolicy,
}

impl<'a> RestorePlanner<'a> {
    fn build(&self) -> Result<RestorePlan, StoreError> {
        // A pending compaction marker means the on-disk segment set still
        // needs marker-aware reconciliation. Skip stale mmap/checkpoint
        // fast paths until that reconciliation has run; otherwise reopen can
        // resurrect pre-compaction truth from an artifact written before the
        // marker.
        let has_pending_compaction = load_pending_compaction(self.data_dir)?.is_some();

        if !has_pending_compaction && self.policy.try_mmap_index() {
            if let Some(snapshot) = super::mmap::try_load_mmap_snapshot(self.data_dir) {
                return self.build_snapshot_plan(
                    RestoreSource::Mmap,
                    SnapshotPlanInput {
                        entries: snapshot.entries,
                        interner_strings: snapshot.interner_strings,
                        watermark: snapshot.watermark,
                        stored_allocator: snapshot.stored_allocator,
                        routing: snapshot.routing,
                        reopen_reserved_kind_fallbacks: snapshot.reopen_reserved_kind_fallbacks,
                        persisted_cumulative_reserved_kind_fallbacks: snapshot
                            .cumulative_reserved_kind_fallbacks,
                    },
                );
            }
        }

        if !has_pending_compaction && self.policy.try_checkpoint() {
            if let Some(snapshot) = super::checkpoint::try_load_checkpoint_snapshot(self.data_dir) {
                return self.build_snapshot_plan(
                    RestoreSource::Checkpoint,
                    SnapshotPlanInput {
                        entries: snapshot.entries,
                        interner_strings: snapshot.interner_strings,
                        watermark: snapshot.watermark,
                        stored_allocator: snapshot.stored_allocator,
                        routing: snapshot.routing,
                        reopen_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
                        persisted_cumulative_reserved_kind_fallbacks: snapshot
                            .cumulative_reserved_kind_fallbacks,
                    },
                );
            }
        }

        let (
            source,
            entries,
            interner_strings,
            allocator_hint,
            chunk_count,
            reopen_reserved_kind_fallbacks,
        ) = collect_rebuild_entries(self.reader, self.data_dir)?;
        let routing = RoutingSummary::from_sorted_entries(&entries, chunk_count.max(1));
        Ok(RestorePlan {
            source,
            restored_entries: entries.len(),
            tail_entries: 0,
            allocator_hint,
            interner_strings,
            routing,
            entries,
            reopen_reserved_kind_fallbacks,
            persisted_cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
        })
    }

    // justifies: ADR-0008; planner in src/store/cold_start/rebuild.rs takes ownership of snapshot data — clippy's suggestion would force a re-clone on the caller side.
    #[allow(clippy::needless_pass_by_value)]
    fn build_snapshot_plan(
        &self,
        source: RestoreSource,
        mut snapshot: SnapshotPlanInput,
    ) -> Result<RestorePlan, StoreError> {
        let interner = StringInterner::new();
        interner.replace_from_full_snapshot(&snapshot.interner_strings);
        let tail_entries = collect_tail_entries(
            &interner,
            self.reader,
            self.data_dir,
            &snapshot.watermark,
            snapshot.stored_allocator,
        )?;
        let restored_entries = snapshot.entries.len();
        let tail_count = tail_entries.len();
        snapshot.entries.extend(tail_entries);
        snapshot.entries.sort_by_key(|entry| entry.global_sequence);
        let chunk_count = usize::try_from(snapshot.routing.chunk_count)
            .unwrap_or(1)
            .max(1)
            + usize::from(tail_count > 0);
        let routing = RoutingSummary::from_sorted_entries(&snapshot.entries, chunk_count);

        Ok(RestorePlan {
            source,
            allocator_hint: snapshot.stored_allocator.max(
                snapshot
                    .entries
                    .last()
                    .map(|entry| entry.global_sequence.saturating_add(1))
                    .unwrap_or(0),
            ),
            interner_strings: full_interner_snapshot(&interner),
            routing,
            entries: snapshot.entries,
            restored_entries,
            tail_entries: tail_count,
            reopen_reserved_kind_fallbacks: snapshot.reopen_reserved_kind_fallbacks,
            persisted_cumulative_reserved_kind_fallbacks: snapshot
                .persisted_cumulative_reserved_kind_fallbacks,
        })
    }
}

/// Open the index using the fastest available path:
/// 1. Try mmap snapshot (`index.fbati`) → if valid, restore + replay tail.
/// 2. Try checkpoint (`index.ckpt`) → if valid, restore + replay tail.
/// 3. Fall back to full segment rebuild (parallel SIDX on sealed + sequential active).
pub(crate) fn open_index(
    index: &StoreIndex,
    reader: &Reader,
    data_dir: &Path,
    policy: ColdStartPolicy,
) -> Result<OpenIndexOutcome, StoreError> {
    let t0 = std::time::Instant::now();
    let planner = RestorePlanner {
        reader,
        data_dir,
        policy,
    };
    let plan = planner.build()?;

    index
        .interner
        .replace_from_full_snapshot(&plan.interner_strings);
    index.restore_sorted_entries_with_routing(plan.entries, plan.allocator_hint, &plan.routing)?;
    // G2: cold-start fails closed on corrupt hidden-ranges metadata.
    // A missing file is OK (first open); any other read/parse failure is
    // surfaced so callers cannot silently resurrect cancelled events.
    if let Some(ranges) = crate::store::hidden_ranges::load_cancelled_ranges(data_dir)? {
        index.restore_cancelled_visibility_ranges(ranges);
    }

    let cumulative_reserved_kind_fallbacks = plan
        .persisted_cumulative_reserved_kind_fallbacks
        .add(&plan.reopen_reserved_kind_fallbacks);

    Ok(OpenIndexOutcome {
        report: OpenIndexReport {
            path: match plan.source {
                RestoreSource::Mmap => OpenIndexPath::Mmap,
                RestoreSource::Checkpoint => OpenIndexPath::Checkpoint,
                RestoreSource::SealedSidxRebuild | RestoreSource::FrameScanFallback => {
                    OpenIndexPath::Rebuild
                }
            },
            restored_entries: plan.restored_entries,
            tail_entries: plan.tail_entries,
            elapsed_us: duration_micros(t0.elapsed()),
            unknown_reserved_system_kind_fallbacks: plan.reopen_reserved_kind_fallbacks.system,
            unknown_reserved_system_kind_histogram: plan
                .reopen_reserved_kind_fallbacks
                .system_histogram
                .clone(),
            unknown_reserved_effect_kind_fallbacks: plan.reopen_reserved_kind_fallbacks.effect,
            unknown_reserved_effect_kind_histogram: plan
                .reopen_reserved_kind_fallbacks
                .effect_histogram
                .clone(),
            cumulative_unknown_reserved_system_kind_fallbacks: cumulative_reserved_kind_fallbacks
                .system,
            cumulative_unknown_reserved_system_kind_histogram: cumulative_reserved_kind_fallbacks
                .system_histogram
                .clone(),
            cumulative_unknown_reserved_effect_kind_fallbacks: cumulative_reserved_kind_fallbacks
                .effect,
            cumulative_unknown_reserved_effect_kind_histogram: cumulative_reserved_kind_fallbacks
                .effect_histogram
                .clone(),
        },
        cumulative_reserved_kind_fallbacks,
    })
}

fn pending_compaction_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(COMPACTION_MARKER_FILENAME)
}

fn compaction_source_temp_path(data_dir: &Path, merged_id: u64) -> std::path::PathBuf {
    data_dir.join(format!(
        "{merged_id:06}.{}.compact-src",
        segment::SEGMENT_EXTENSION
    ))
}

fn load_pending_compaction(data_dir: &Path) -> Result<Option<PendingCompaction>, StoreError> {
    let path = pending_compaction_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(StoreError::Io)?;
    let marker = serde_json::from_slice::<PendingCompaction>(&bytes)
        .map_err(|_| StoreError::DataDirMalformed { path: path.clone() })?;
    Ok(Some(marker))
}

pub(crate) fn write_pending_compaction(
    data_dir: &Path,
    merged_id: u64,
    source_segment_ids: &[u64],
) -> Result<(), StoreError> {
    let marker = PendingCompaction {
        merged_id,
        source_segment_ids: source_segment_ids.to_vec(),
    };
    let final_path = pending_compaction_path(data_dir);
    crate::store::platform::fs::write_file_atomically(
        data_dir,
        &final_path,
        "compaction marker",
        |file| {
            serde_json::to_writer(file, &marker).map_err(|e| StoreError::Serialization(Box::new(e)))
        },
    )
}

pub(crate) fn clear_pending_compaction(data_dir: &Path) -> Result<(), StoreError> {
    let path = pending_compaction_path(data_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            crate::store::platform::sync::sync_parent_dir(&path)?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StoreError::Io(err)),
    }
}

fn segment_paths(data_dir: &Path) -> Result<Vec<(u64, std::path::PathBuf)>, StoreError> {
    let mut entries: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(data_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let is_segment = path
                .extension()
                .map(|ext| ext == segment::SEGMENT_EXTENSION)
                .unwrap_or(false);
            if !is_segment {
                return None;
            }
            let segment_id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u64>().ok())?;
            Some((segment_id, path))
        })
        .collect();
    if let Some(marker) = load_pending_compaction(data_dir)? {
        let merged_present = entries
            .iter()
            .any(|(segment_id, _)| *segment_id == marker.merged_id);
        let temp_source_path = compaction_source_temp_path(data_dir, marker.merged_id);
        let temp_source_exists = temp_source_path.exists();
        let stale_finalized_marker = merged_present
            && !temp_source_exists
            && marker
                .source_segment_ids
                .iter()
                .filter(|&&segment_id| segment_id != marker.merged_id)
                .all(|segment_id| !entries.iter().any(|(id, _)| id == segment_id));

        if !stale_finalized_marker {
            if merged_present {
                entries.retain(|(segment_id, _)| {
                    *segment_id == marker.merged_id
                        || !marker
                            .source_segment_ids
                            .iter()
                            .any(|source_id| source_id == segment_id)
                });
            } else {
                if temp_source_exists {
                    entries.retain(|(segment_id, _)| *segment_id != marker.merged_id);
                    entries.push((marker.merged_id, temp_source_path));
                }
                for source_id in marker
                    .source_segment_ids
                    .iter()
                    .copied()
                    .filter(|source_id| *source_id != marker.merged_id)
                {
                    if !entries
                        .iter()
                        .any(|(segment_id, _)| *segment_id == source_id)
                    {
                        return Err(StoreError::DataDirMalformed {
                            path: pending_compaction_path(data_dir),
                        });
                    }
                }
            }
        }
    }
    entries.sort_by_key(|(segment_id, _)| *segment_id);
    Ok(entries)
}

fn read_sealed_sidx_entries_parallel(
    sealed_segments: &[(u64, std::path::PathBuf)],
) -> Option<(Vec<ScannedIndexEntry>, ReservedKindFallbackStats)> {
    let per_segment: Result<Vec<_>, StoreError> = sealed_segments
        .par_iter()
        .map(|(segment_id, path)| scanned_entries_from_sidx_footer(*segment_id, path))
        .collect();

    match per_segment {
        Ok(mut batches) => {
            let mut flat = Vec::new();
            let mut reserved_kind_fallbacks = ReservedKindFallbackStats::default();
            for (batch, counts) in batches.drain(..) {
                flat.extend(batch);
                reserved_kind_fallbacks = reserved_kind_fallbacks.add(&counts);
            }
            flat.sort_by_key(|entry| entry.global_sequence.unwrap_or(0));
            Some((flat, reserved_kind_fallbacks))
        }
        Err(error) => {
            tracing::warn!(
                target: "batpak::rebuild",
                error = %error,
                "parallel SIDX rebuild unavailable; falling back to sequential scan"
            );
            None
        }
    }
}

fn scanned_entries_from_sidx_footer(
    segment_id: u64,
    path: &Path,
) -> Result<(Vec<ScannedIndexEntry>, ReservedKindFallbackStats), StoreError> {
    match crate::store::segment::sidx::read_footer(path) {
        Ok(Some((entries, strings))) => {
            let mut scanned = Vec::with_capacity(entries.len());
            let mut reserved_kind_fallbacks = ReservedKindFallbackStats::default();
            for entry in entries {
                let row = entry.to_cold_start_row_counted(segment_id, &mut reserved_kind_fallbacks);
                let kind = row.kind;
                if kind == crate::event::EventKind::SYSTEM_BATCH_BEGIN
                    || kind == crate::event::EventKind::SYSTEM_BATCH_COMMIT
                {
                    continue;
                }
                scanned.push(ScannedIndexEntry::from_cold_start_row(&row, &strings)?);
            }
            Ok((scanned, reserved_kind_fallbacks))
        }
        Ok(None) => Err(StoreError::ser_msg(
            "sealed segment missing SIDX footer during parallel rebuild",
        )),
        Err(error) => Err(error),
    }
}

fn full_interner_snapshot(interner: &StringInterner) -> Vec<String> {
    let mut snapshot = vec![String::new()];
    snapshot.extend(interner.to_snapshot());
    snapshot
}

#[cfg(test)]
fn read_sealed_sidx_entries_sequential(
    sealed_segments: &[(u64, std::path::PathBuf)],
) -> Result<Vec<ScannedIndexEntry>, StoreError> {
    let mut flat = Vec::new();
    for (segment_id, path) in sealed_segments {
        flat.extend(scanned_entries_from_sidx_footer(*segment_id, path)?.0);
    }
    flat.sort_by_key(|entry| entry.global_sequence.unwrap_or(0));
    Ok(flat)
}

#[derive(Default)]
struct SequenceTracker {
    max_seen: u64,
    inserted_any: bool,
}

impl SequenceTracker {
    fn synthesize_next(&self) -> u64 {
        if self.inserted_any {
            self.max_seen.saturating_add(1)
        } else {
            0
        }
    }

    fn note_seen(&mut self, global_sequence: u64) {
        self.max_seen = self.max_seen.max(global_sequence);
        self.inserted_any = true;
    }
}

/// Build an `IndexEntry` from a `ScannedIndexEntry` and the chosen
/// `global_sequence`.
fn entry_from_scan(
    interner: &StringInterner,
    se: ScannedIndexEntry,
    global_sequence: u64,
) -> Result<IndexEntry, StoreError> {
    let coord = Coordinate::new(&se.entity, &se.scope)?;
    let entity_id = interner.intern(&se.entity);
    let scope_id = interner.intern(&se.scope);
    let clock = se.header.position.sequence;
    Ok(IndexEntry {
        event_id: se.header.event_id,
        correlation_id: se.header.correlation_id,
        causation_id: se.header.causation_id.filter(|&id| id != 0),
        coord,
        entity_id,
        scope_id,
        kind: se.header.event_kind,
        wall_ms: se.header.position.wall_ms,
        clock,
        dag_lane: se.header.position.lane,
        dag_depth: se.header.position.depth,
        hash_chain: se.hash_chain,
        disk_pos: DiskPos::new(se.segment_id, se.offset, se.length),
        global_sequence,
    })
}

fn collect_tail_entries(
    interner: &StringInterner,
    reader: &Reader,
    data_dir: &Path,
    watermark: &super::checkpoint::WatermarkInfo,
    allocator_floor: u64,
) -> Result<Vec<IndexEntry>, StoreError> {
    let entries = segment_paths(data_dir)?;
    let mut batch_state = crate::store::segment::scan::BatchRecoveryState::default();
    let mut tracker = SequenceTracker {
        max_seen: allocator_floor.saturating_sub(1),
        inserted_any: allocator_floor > 0,
    };
    let mut rebuilt_entries = Vec::new();

    for (seg_id, path) in &entries {
        if *seg_id < watermark.watermark_segment_id {
            continue;
        }

        reader.scan_segment_index_into(path, Some(&mut batch_state), |se| {
            if *seg_id == watermark.watermark_segment_id && se.offset < watermark.watermark_offset {
                return Ok(());
            }
            let global_sequence = se
                .global_sequence
                .unwrap_or_else(|| tracker.synthesize_next());
            let entry = entry_from_scan(interner, se, global_sequence)?;
            tracker.note_seen(global_sequence);
            rebuilt_entries.push(entry);
            Ok(())
        })?;
    }

    Ok(rebuilt_entries)
}

type RebuildResult = (
    RestoreSource,
    Vec<IndexEntry>,
    Vec<String>,
    u64,
    usize,
    ReservedKindFallbackStats,
);

fn collect_rebuild_entries(reader: &Reader, data_dir: &Path) -> Result<RebuildResult, StoreError> {
    let entries = segment_paths(data_dir)?;
    let configured_active_segment = reader.active_segment_id();
    let active_segment_id = (configured_active_segment != 0).then_some(configured_active_segment);
    let interner = StringInterner::new();
    let mut rebuilt_entries = Vec::new();
    let mut tracker = SequenceTracker::default();

    let sealed_segments: Vec<_> = entries
        .iter()
        .filter(|(segment_id, _)| active_segment_id.is_none_or(|active| *segment_id < active))
        .cloned()
        .collect();

    let mut source = RestoreSource::SealedSidxRebuild;
    let mut chunk_count = sealed_segments.len().max(1);

    let mut reserved_kind_fallbacks = ReservedKindFallbackStats::default();

    if !sealed_segments.is_empty() {
        if let Some((scanned, counts)) = read_sealed_sidx_entries_parallel(&sealed_segments) {
            reserved_kind_fallbacks = reserved_kind_fallbacks.add(&counts);
            for se in scanned {
                let global_sequence = se
                    .global_sequence
                    .unwrap_or_else(|| tracker.synthesize_next());
                let entry = entry_from_scan(&interner, se, global_sequence)?;
                tracker.note_seen(global_sequence);
                rebuilt_entries.push(entry);
            }
        } else {
            source = RestoreSource::FrameScanFallback;
            chunk_count = 1;
            let mut batch_state = crate::store::segment::scan::BatchRecoveryState::default();
            for (_, path) in &sealed_segments {
                reader.scan_segment_index_into(path, Some(&mut batch_state), |se| {
                    let global_sequence = se
                        .global_sequence
                        .unwrap_or_else(|| tracker.synthesize_next());
                    let entry = entry_from_scan(&interner, se, global_sequence)?;
                    tracker.note_seen(global_sequence);
                    rebuilt_entries.push(entry);
                    Ok(())
                })?;
            }
        }
    } else {
        source = RestoreSource::FrameScanFallback;
    }

    let mut batch_state = crate::store::segment::scan::BatchRecoveryState::default();
    for (segment_id, path) in &entries {
        if Some(*segment_id) != active_segment_id {
            continue;
        }
        reader.scan_segment_index_into(path, Some(&mut batch_state), |se| {
            let global_sequence = se
                .global_sequence
                .unwrap_or_else(|| tracker.synthesize_next());
            let entry = entry_from_scan(&interner, se, global_sequence)?;
            tracker.note_seen(global_sequence);
            rebuilt_entries.push(entry);
            Ok(())
        })?;
    }

    rebuilt_entries.sort_by_key(|entry| entry.global_sequence);
    let allocator_hint = if tracker.inserted_any {
        tracker.max_seen.saturating_add(1)
    } else {
        0
    };

    Ok((
        source,
        rebuilt_entries,
        full_interner_snapshot(&interner),
        allocator_hint,
        chunk_count,
        reserved_kind_fallbacks,
    ))
}

/// Scan all segment files in `data_dir`, rebuild the in-memory index from their contents.
/// Used by both cold-start (`Store::open_with_cache`) and post-compaction index rebuild.
/// Handles cross-segment batch recovery using BatchRecoveryState.
pub(crate) fn rebuild_from_segments(
    index: &StoreIndex,
    reader: &Reader,
    data_dir: &Path,
) -> Result<(), StoreError> {
    let (_, entries, interner_strings, allocator_hint, chunk_count, _) =
        collect_rebuild_entries(reader, data_dir)?;
    index.interner.replace_from_full_snapshot(&interner_strings);
    let routing = RoutingSummary::from_sorted_entries(&entries, chunk_count.max(1));
    index.restore_sorted_entries_with_routing(entries, allocator_hint, &routing)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use tempfile::TempDir;

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ScanSummaryRow {
        event_id: u128,
        entity: String,
        scope: String,
        category: u8,
        type_id: u16,
        global_sequence: u64,
        offset: u64,
        length: u32,
    }

    fn rotating_store_config(dir: &TempDir) -> StoreConfig {
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
    }

    fn scanned_summary(
        entries: &[crate::store::segment::scan::ScannedIndexEntry],
    ) -> Vec<ScanSummaryRow> {
        entries
            .iter()
            .map(|entry| ScanSummaryRow {
                event_id: entry.header.event_id,
                entity: entry.entity.clone(),
                scope: entry.scope.clone(),
                category: entry.header.event_kind.category(),
                type_id: entry.header.event_kind.type_id(),
                global_sequence: entry.global_sequence.unwrap_or(0),
                offset: entry.offset,
                length: entry.length,
            })
            .collect()
    }

    fn sample_index_entries(count: u64, segment_id: u64) -> (Vec<IndexEntry>, Vec<String>) {
        let interner = StringInterner::new();
        let mut entries = Vec::new();
        for i in 0..count {
            let coord =
                Coordinate::new(format!("entity:{i}"), "scope:rebuild").expect("valid coord");
            let entity_id = interner.intern(coord.entity());
            let scope_id = interner.intern(coord.scope());
            entries.push(IndexEntry {
                event_id: (i + 1) as u128,
                correlation_id: (i + 1) as u128,
                causation_id: None,
                coord,
                entity_id,
                scope_id,
                kind: EventKind::custom(
                    0x1,
                    u16::try_from(i + 1).expect("sample type id fits u16"),
                ),
                wall_ms: 1_700_000_000_000 + i * 1000,
                clock: u32::try_from(i + 1).expect("clock fits u32"),
                dag_lane: 0,
                dag_depth: 0,
                hash_chain: HashChain::default(),
                disk_pos: DiskPos::new(segment_id, i * 256, 256),
                global_sequence: i,
            });
        }
        let interner_strings = full_interner_snapshot(&interner);
        (entries, interner_strings)
    }

    #[test]
    fn parallel_sidx_footer_read_matches_sequential_footer_read() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(rotating_store_config(&dir)).expect("open store");
        let coord = Coordinate::new("entity:sidx", "scope:rebuild").expect("coord");
        let kind = EventKind::custom(0xF, 9);
        let payload = serde_json::json!({
            "blob": "payload that forces rapid segment rotation and sealed footer generation"
        });

        for n in 0..64u32 {
            store
                .append(
                    &coord,
                    kind,
                    &serde_json::json!({"n": n, "payload": payload}),
                )
                .expect("append");
        }
        store.close().expect("close store");

        let entries = segment_paths(dir.path()).expect("segment paths");
        let active_segment = entries
            .iter()
            .map(|(segment_id, _)| *segment_id)
            .max()
            .expect("at least one segment");
        let sealed_segments: Vec<_> = entries
            .into_iter()
            .filter(|(segment_id, _)| *segment_id < active_segment)
            .collect();

        assert!(
            !sealed_segments.is_empty(),
            "PROPERTY: tiny segments should produce at least one sealed segment with an SIDX footer."
        );

        let (parallel, _) = read_sealed_sidx_entries_parallel(&sealed_segments)
            .expect("parallel SIDX footer read should succeed");
        let sequential = read_sealed_sidx_entries_sequential(&sealed_segments)
            .expect("sequential SIDX footer read should succeed");

        assert_eq!(
            scanned_summary(&parallel),
            scanned_summary(&sequential),
            "PROPERTY: parallel SIDX footer rebuild must match sequential footer semantics exactly."
        );
    }

    #[test]
    fn build_snapshot_plan_keeps_chunk_count_when_tail_is_empty() {
        let dir = TempDir::new().expect("temp dir");
        let reader = Reader::new(dir.path().to_path_buf(), 4);
        let planner = RestorePlanner {
            reader: &reader,
            data_dir: dir.path(),
            policy: ColdStartPolicy::new(false, false),
        };
        let (entries, interner_strings) = sample_index_entries(2, 0);
        let routing = RoutingSummary::from_sorted_entries(&entries, 1);

        let plan = planner
            .build_snapshot_plan(
                RestoreSource::Checkpoint,
                SnapshotPlanInput {
                    entries,
                    interner_strings,
                    watermark: crate::store::cold_start::checkpoint::WatermarkInfo {
                        watermark_segment_id: 99,
                        watermark_offset: 0,
                    },
                    stored_allocator: 2,
                    routing,
                    reopen_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
                    persisted_cumulative_reserved_kind_fallbacks:
                        ReservedKindFallbackStats::default(),
                },
            )
            .expect("build snapshot plan");

        assert_eq!(
            plan.tail_entries, 0,
            "SANITY: empty temp dir should produce no tail replay"
        );
        assert_eq!(
            plan.routing.chunk_count,
            1,
            "PROPERTY: a snapshot plan with no tail entries must preserve the existing routing chunk count instead of synthesizing an extra chunk"
        );
    }

    #[test]
    fn build_snapshot_plan_adds_chunk_when_tail_is_present() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(rotating_store_config(&dir)).expect("open store");
        let coord = Coordinate::new("entity:tail-plan", "scope:rebuild").expect("coord");
        let kind = EventKind::custom(0xE, 8);
        for n in 0..16u32 {
            store
                .append(&coord, kind, &serde_json::json!({ "n": n }))
                .expect("append tail event");
        }
        store.close().expect("close store");

        let entries = segment_paths(dir.path()).expect("segment paths");
        let watermark_segment_id = entries
            .first()
            .map(|(segment_id, _)| *segment_id)
            .expect("watermark segment id");
        let active_after_tail = entries
            .last()
            .map(|(segment_id, _)| segment_id.saturating_add(1))
            .expect("active segment id");

        let reader = Reader::new(dir.path().to_path_buf(), 4);
        reader.set_active_segment(active_after_tail);
        let planner = RestorePlanner {
            reader: &reader,
            data_dir: dir.path(),
            policy: ColdStartPolicy::new(false, false),
        };
        let routing = RoutingSummary::from_sorted_entries(&[], 1);

        let plan = planner
            .build_snapshot_plan(
                RestoreSource::Checkpoint,
                SnapshotPlanInput {
                    entries: Vec::new(),
                    interner_strings: Vec::new(),
                    watermark: crate::store::cold_start::checkpoint::WatermarkInfo {
                        watermark_segment_id,
                        watermark_offset: 0,
                    },
                    stored_allocator: 0,
                    routing,
                    reopen_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
                    persisted_cumulative_reserved_kind_fallbacks:
                        ReservedKindFallbackStats::default(),
                },
            )
            .expect("build snapshot plan with tail");

        assert!(
            plan.tail_entries > 0,
            "SANITY: fixture should collect tail entries from the watermark segment onward"
        );
        assert_eq!(
            plan.routing.chunk_count,
            2,
            "PROPERTY: snapshot restore must add exactly one routing chunk when tail replay contributes entries"
        );
    }

    #[test]
    fn entry_from_scan_normalizes_zero_causation() {
        use crate::coordinate::DagPosition;
        use crate::event::{EventHeader, EventKind, HashChain};
        use crate::store::segment::scan::ScannedIndexEntry;

        let interner = StringInterner::new();
        let se = ScannedIndexEntry {
            header: EventHeader {
                event_id: 1,
                correlation_id: 1,
                causation_id: Some(0),
                timestamp_us: 0,
                position: DagPosition::new(0, 0, 1),
                payload_size: 0,
                event_kind: EventKind::custom(0x1, 1),
                flags: 0,
                content_hash: [0u8; 32],
            },
            entity: "entity:test".to_string(),
            scope: "scope:test".to_string(),
            hash_chain: HashChain::default(),
            segment_id: 0,
            offset: 0,
            length: 64,
            global_sequence: Some(0),
        };
        let entry = entry_from_scan(&interner, se, 0).expect("entry_from_scan");
        assert_eq!(
            entry.causation_id, None,
            "INVARIANT: Some(0) causation_id from scan must normalize to None"
        );
    }

    #[test]
    fn entry_from_scan_preserves_nonzero_causation() {
        use crate::coordinate::DagPosition;
        use crate::event::{EventHeader, EventKind, HashChain};
        use crate::store::segment::scan::ScannedIndexEntry;

        let interner = StringInterner::new();
        let se = ScannedIndexEntry {
            header: EventHeader {
                event_id: 2,
                correlation_id: 1,
                causation_id: Some(99),
                timestamp_us: 0,
                position: DagPosition::new(0, 0, 1),
                payload_size: 0,
                event_kind: EventKind::custom(0x1, 1),
                flags: 0,
                content_hash: [0u8; 32],
            },
            entity: "entity:test".to_string(),
            scope: "scope:test".to_string(),
            hash_chain: HashChain::default(),
            segment_id: 0,
            offset: 0,
            length: 64,
            global_sequence: Some(1),
        };
        let entry = entry_from_scan(&interner, se, 1).expect("entry_from_scan");
        assert_eq!(entry.causation_id, Some(99));
    }

    #[test]
    fn segment_paths_ignore_superseded_sources_when_merge_is_present() {
        let dir = TempDir::new().expect("temp dir");
        let merged_path = dir.path().join(segment::segment_filename(1));
        let superseded_path = dir.path().join(segment::segment_filename(2));
        let untouched_path = dir.path().join(segment::segment_filename(3));
        let temp_source_path = compaction_source_temp_path(dir.path(), 1);

        std::fs::write(&merged_path, []).expect("write merged");
        std::fs::write(&superseded_path, []).expect("write superseded");
        std::fs::write(&untouched_path, []).expect("write untouched");
        std::fs::write(&temp_source_path, []).expect("write temp source");
        write_pending_compaction(dir.path(), 1, &[1, 2]).expect("write marker");

        let paths = segment_paths(dir.path()).expect("segment paths");
        let ids: Vec<_> = paths.iter().map(|(segment_id, _)| *segment_id).collect();

        assert_eq!(
            ids,
            vec![1, 3],
            "PROPERTY: when the merged segment is published, cold-start must ignore superseded compacted sources."
        );
        assert_eq!(
            paths[0].1,
            merged_path,
            "PROPERTY: cold-start must prefer the published merged segment, not the compact-src temp."
        );
    }

    #[test]
    fn segment_paths_restore_temp_source_when_merge_not_published() {
        let dir = TempDir::new().expect("temp dir");
        let temp_source_path = compaction_source_temp_path(dir.path(), 1);
        let source_path = dir.path().join(segment::segment_filename(2));
        let untouched_path = dir.path().join(segment::segment_filename(3));

        std::fs::write(&temp_source_path, []).expect("write temp source");
        std::fs::write(&source_path, []).expect("write source");
        std::fs::write(&untouched_path, []).expect("write untouched");
        write_pending_compaction(dir.path(), 1, &[1, 2]).expect("write marker");

        let paths = segment_paths(dir.path()).expect("segment paths");
        let ids: Vec<_> = paths.iter().map(|(segment_id, _)| *segment_id).collect();

        assert_eq!(
            ids,
            vec![1, 2, 3],
            "PROPERTY: if compaction crashes before publishing the merged segment, cold-start must reconstruct the pre-compact segment set."
        );
        assert_eq!(
            paths[0].1,
            temp_source_path,
            "PROPERTY: cold-start must substitute the compact-src temp for the renamed merged-id source."
        );
    }

    #[test]
    fn segment_paths_reject_missing_sources_even_if_unrelated_segments_exist() {
        let dir = TempDir::new().expect("temp dir");
        let unrelated_path = dir.path().join(segment::segment_filename(99));

        std::fs::write(&unrelated_path, []).expect("write unrelated segment");
        write_pending_compaction(dir.path(), 1, &[1, 2]).expect("write marker");

        let err = segment_paths(dir.path()).expect_err(
            "PROPERTY: pending compaction must fail when a declared source segment is missing",
        );

        assert!(
            matches!(err, StoreError::DataDirMalformed { .. }),
            "PROPERTY: unrelated segments must not satisfy the pending-compaction source presence check"
        );
    }

    #[test]
    fn clear_pending_compaction_is_idempotent_when_marker_is_absent() {
        let dir = TempDir::new().expect("temp dir");

        clear_pending_compaction(dir.path())
            .expect("PROPERTY: clearing an absent pending-compaction marker must be idempotent");
    }

    #[test]
    fn open_index_skips_fast_paths_when_pending_compaction_marker_exists() {
        let dir = TempDir::new().expect("temp dir");
        let config = crate::store::StoreConfig::new(dir.path())
            .with_enable_checkpoint(true)
            .with_enable_mmap_index(false)
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1);
        let store = crate::store::Store::open(config).expect("open");
        let coord = crate::coordinate::Coordinate::new("entity:pending-fast-path", "scope:test")
            .expect("coord");
        let kind = crate::event::EventKind::custom(0xE, 1);
        for i in 0..20u32 {
            store
                .append(&coord, kind, &serde_json::json!({ "i": i }))
                .expect("append");
        }
        store.close().expect("close");

        let existing = segment_paths(dir.path()).expect("segment paths");
        let merged_id = existing.first().expect("segment id").0;
        write_pending_compaction(dir.path(), merged_id, &[merged_id]).expect("write marker");

        let reader = Reader::new(dir.path().to_path_buf(), 4);
        let index = StoreIndex::new();
        let report = open_index(
            &index,
            &reader,
            dir.path(),
            ColdStartPolicy::new(true, false),
        )
        .expect("open index with pending compaction");

        assert_eq!(
            report.report.path,
            OpenIndexPath::Rebuild,
            "PROPERTY: pending compaction must force a marker-aware rebuild instead of trusting checkpoint fast paths."
        );
    }

    #[test]
    fn collect_tail_entries_keeps_events_from_the_watermark_segment() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(rotating_store_config(&dir)).expect("open store");
        let coord = Coordinate::new("entity:tail", "scope:watermark").expect("coord");
        let kind = EventKind::custom(0xE, 7);

        for n in 0..64u32 {
            store
                .append(&coord, kind, &serde_json::json!({ "n": n }))
                .expect("append");
        }
        store.close().expect("close");

        let entries = segment_paths(dir.path()).expect("segment paths");
        assert!(
            entries.len() >= 2,
            "SANITY: rotating config should create multiple segments for watermark-tail testing"
        );
        let watermark_segment_id = entries
            .first()
            .map(|(segment_id, _)| *segment_id)
            .expect("watermark segment id");
        let highest_segment_id = entries
            .last()
            .map(|(segment_id, _)| *segment_id)
            .expect("highest segment id");

        let interner = StringInterner::new();
        let reader = Reader::new(dir.path().to_path_buf(), 4);
        reader.set_active_segment(highest_segment_id + 1);
        let tail_entries = collect_tail_entries(
            &interner,
            &reader,
            dir.path(),
            &crate::store::cold_start::checkpoint::WatermarkInfo {
                watermark_segment_id,
                watermark_offset: 0,
            },
            0,
        )
        .expect("collect tail entries");

        assert!(
            tail_entries
                .iter()
                .any(|entry| entry.disk_pos.segment_id == watermark_segment_id),
            "PROPERTY: replay tail must include events from the watermark segment itself when the watermark offset is at the segment start"
        );
    }
}
