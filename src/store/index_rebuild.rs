use crate::coordinate::Coordinate;
use crate::store::index::{DiskPos, IndexEntry, RoutingSummary, StoreIndex};
use crate::store::interner::StringInterner;
use crate::store::reader::Reader;
use crate::store::segment;
use crate::store::StoreError;
use rayon::prelude::*;
use std::path::Path;

/// Which cold-start restore strategy was actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenIndexPath {
    /// Restored from the mmap snapshot (`index.fbati`) plus tail replay.
    Mmap,
    /// Restored from the checkpoint (`index.ckpt`) plus tail replay.
    Checkpoint,
    /// Full rebuild from segment files (parallel SIDX + sequential active).
    Rebuild,
}

/// Diagnostic output from `open_index()`. Hard truth, not logs.
#[derive(Debug, Clone)]
pub struct OpenIndexReport {
    /// Which restore strategy was selected and completed.
    pub path: OpenIndexPath,
    /// Number of entries restored from the snapshot (mmap or checkpoint body).
    pub restored_entries: usize,
    /// Number of entries replayed from the tail after the snapshot watermark.
    pub tail_entries: usize,
    /// Wall-clock microseconds for the entire open_index() call.
    pub elapsed_us: u64,
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
}

struct RestorePlanner<'a> {
    reader: &'a Reader,
    data_dir: &'a Path,
    enable_checkpoint: bool,
    enable_mmap_index: bool,
}

impl<'a> RestorePlanner<'a> {
    fn build(&self) -> Result<RestorePlan, StoreError> {
        if self.enable_mmap_index {
            if let Some(snapshot) = crate::store::mmap_index::try_load_mmap_snapshot(self.data_dir)
            {
                return self.build_snapshot_plan(
                    RestoreSource::Mmap,
                    snapshot.entries,
                    snapshot.interner_strings,
                    snapshot.watermark,
                    snapshot.stored_allocator,
                    snapshot.routing,
                );
            }
        }

        if self.enable_checkpoint {
            if let Some(snapshot) =
                crate::store::checkpoint::try_load_checkpoint_snapshot(self.data_dir)
            {
                return self.build_snapshot_plan(
                    RestoreSource::Checkpoint,
                    snapshot.entries,
                    snapshot.interner_strings,
                    snapshot.watermark,
                    snapshot.stored_allocator,
                    snapshot.routing,
                );
            }
        }

        let (source, entries, interner_strings, allocator_hint, chunk_count) =
            collect_rebuild_entries(self.reader, self.data_dir)?;
        let routing = RoutingSummary::from_sorted_entries(&entries, chunk_count.max(1));
        Ok(RestorePlan {
            source,
            restored_entries: entries.len(),
            tail_entries: 0,
            allocator_hint,
            interner_strings,
            routing,
            entries,
        })
    }

    #[allow(clippy::needless_pass_by_value)] // planner takes ownership of snapshot data
    fn build_snapshot_plan(
        &self,
        source: RestoreSource,
        mut snapshot_entries: Vec<IndexEntry>,
        interner_strings: Vec<String>,
        watermark: crate::store::checkpoint::WatermarkInfo,
        stored_allocator: u64,
        snapshot_routing: RoutingSummary,
    ) -> Result<RestorePlan, StoreError> {
        let interner = StringInterner::new();
        interner.replace_from_full_snapshot(&interner_strings);
        let tail_entries = collect_tail_entries(
            &interner,
            self.reader,
            self.data_dir,
            &watermark,
            stored_allocator,
        )?;
        let restored_entries = snapshot_entries.len();
        let tail_count = tail_entries.len();
        snapshot_entries.extend(tail_entries);
        snapshot_entries.sort_by_key(|entry| entry.global_sequence);
        let chunk_count = usize::try_from(snapshot_routing.chunk_count)
            .unwrap_or(1)
            .max(1)
            + usize::from(tail_count > 0);
        let routing = RoutingSummary::from_sorted_entries(&snapshot_entries, chunk_count);

        Ok(RestorePlan {
            source,
            allocator_hint: stored_allocator.max(
                snapshot_entries
                    .last()
                    .map(|entry| entry.global_sequence.saturating_add(1))
                    .unwrap_or(0),
            ),
            interner_strings: full_interner_snapshot(&interner),
            routing,
            entries: snapshot_entries,
            restored_entries,
            tail_entries: tail_count,
        })
    }
}

/// Open the index using the fastest available path:
/// 1. Try mmap snapshot (`index.fbati`) → if valid, restore + replay tail.
/// 2. Try checkpoint (`index.ckpt`) → if valid, restore + replay tail.
/// 3. Fall back to full segment rebuild (parallel SIDX on sealed + sequential active).
#[allow(clippy::cast_possible_truncation)] // as_micros() -> u64: overflow at ~584,942 years
pub(crate) fn open_index(
    index: &StoreIndex,
    reader: &Reader,
    data_dir: &Path,
    enable_checkpoint: bool,
    enable_mmap_index: bool,
) -> Result<OpenIndexReport, StoreError> {
    let t0 = std::time::Instant::now();
    let planner = RestorePlanner {
        reader,
        data_dir,
        enable_checkpoint,
        enable_mmap_index,
    };
    let plan = planner.build()?;

    index
        .interner
        .replace_from_full_snapshot(&plan.interner_strings);
    index.restore_sorted_entries_with_routing(plan.entries, plan.allocator_hint, &plan.routing);
    restore_cancelled_visibility_ranges(index, data_dir);

    Ok(OpenIndexReport {
        path: match plan.source {
            RestoreSource::Mmap => OpenIndexPath::Mmap,
            RestoreSource::Checkpoint => OpenIndexPath::Checkpoint,
            RestoreSource::SealedSidxRebuild | RestoreSource::FrameScanFallback => {
                OpenIndexPath::Rebuild
            }
        },
        restored_entries: plan.restored_entries,
        tail_entries: plan.tail_entries,
        elapsed_us: t0.elapsed().as_micros() as u64,
    })
}

pub(crate) fn restore_cancelled_visibility_ranges(index: &StoreIndex, data_dir: &Path) {
    if let Some(ranges) = crate::store::visibility_ranges::try_load_cancelled_ranges(data_dir) {
        index.restore_cancelled_visibility_ranges(ranges);
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
    entries.sort_by_key(|(segment_id, _)| *segment_id);
    Ok(entries)
}

fn read_sealed_sidx_entries_parallel(
    sealed_segments: &[(u64, std::path::PathBuf)],
) -> Option<Vec<crate::store::reader::ScannedIndexEntry>> {
    let per_segment: Result<Vec<_>, StoreError> = sealed_segments
        .par_iter()
        .map(|(segment_id, path)| scanned_entries_from_sidx_footer(*segment_id, path))
        .collect();

    match per_segment {
        Ok(mut batches) => {
            let mut flat = Vec::new();
            for batch in batches.drain(..) {
                flat.extend(batch);
            }
            flat.sort_by_key(|entry| entry.global_sequence.unwrap_or(0));
            Some(flat)
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
) -> Result<Vec<crate::store::reader::ScannedIndexEntry>, StoreError> {
    match crate::store::sidx::read_footer(path) {
        Ok(Some((entries, strings))) => {
            let mut scanned = Vec::with_capacity(entries.len());
            for entry in entries {
                let kind = crate::store::sidx::raw_to_kind(entry.kind);
                if kind == crate::event::EventKind::SYSTEM_BATCH_BEGIN
                    || kind == crate::event::EventKind::SYSTEM_BATCH_COMMIT
                {
                    continue;
                }
                let entity = strings
                    .get(entry.entity_idx as usize)
                    .cloned()
                    .ok_or_else(|| StoreError::ser_msg("SIDX entity_idx out of range"))?;
                let scope = strings
                    .get(entry.scope_idx as usize)
                    .cloned()
                    .ok_or_else(|| StoreError::ser_msg("SIDX scope_idx out of range"))?;
                scanned.push(crate::store::reader::ScannedIndexEntry {
                    header: crate::event::EventHeader::from_sidx(
                        entry.event_id,
                        entry.correlation_id,
                        (entry.causation_id != 0).then_some(entry.causation_id),
                        entry.wall_ms,
                        entry.clock,
                        entry.dag_lane,
                        entry.dag_depth,
                        kind,
                    ),
                    entity,
                    scope,
                    hash_chain: crate::event::HashChain {
                        prev_hash: entry.prev_hash,
                        event_hash: entry.event_hash,
                    },
                    segment_id,
                    offset: entry.frame_offset,
                    length: entry.frame_length,
                    global_sequence: Some(entry.global_sequence),
                });
            }
            Ok(scanned)
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
) -> Result<Vec<crate::store::reader::ScannedIndexEntry>, StoreError> {
    let mut flat = Vec::new();
    for (segment_id, path) in sealed_segments {
        flat.extend(scanned_entries_from_sidx_footer(*segment_id, path)?);
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
    se: crate::store::reader::ScannedIndexEntry,
    global_sequence: u64,
) -> Result<IndexEntry, StoreError> {
    let coord = Coordinate::new(&se.entity, &se.scope)?;
    let entity_id = interner.intern(&se.entity);
    let scope_id = interner.intern(&se.scope);
    let clock = se.header.position.sequence;
    Ok(IndexEntry {
        event_id: se.header.event_id,
        correlation_id: se.header.correlation_id,
        causation_id: se.header.causation_id,
        coord,
        entity_id,
        scope_id,
        kind: se.header.event_kind,
        wall_ms: se.header.position.wall_ms,
        clock,
        dag_lane: se.header.position.lane,
        dag_depth: se.header.position.depth,
        hash_chain: se.hash_chain,
        disk_pos: DiskPos {
            segment_id: se.segment_id,
            offset: se.offset,
            length: se.length,
        },
        global_sequence,
    })
}

fn collect_tail_entries(
    interner: &StringInterner,
    reader: &Reader,
    data_dir: &Path,
    watermark: &crate::store::checkpoint::WatermarkInfo,
    allocator_floor: u64,
) -> Result<Vec<IndexEntry>, StoreError> {
    let entries = segment_paths(data_dir)?;
    let mut batch_state = crate::store::reader::BatchRecoveryState::default();
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

// Complex return type justified: internal planner helper, not public API.
#[allow(clippy::type_complexity)]
fn collect_rebuild_entries(
    reader: &Reader,
    data_dir: &Path,
) -> Result<(RestoreSource, Vec<IndexEntry>, Vec<String>, u64, usize), StoreError> {
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

    if !sealed_segments.is_empty() {
        if let Some(scanned) = read_sealed_sidx_entries_parallel(&sealed_segments) {
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
            let mut batch_state = crate::store::reader::BatchRecoveryState::default();
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

    let mut batch_state = crate::store::reader::BatchRecoveryState::default();
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
    let (_, entries, interner_strings, allocator_hint, chunk_count) =
        collect_rebuild_entries(reader, data_dir)?;
    index.interner.replace_from_full_snapshot(&interner_strings);
    let routing = RoutingSummary::from_sorted_entries(&entries, chunk_count.max(1));
    index.restore_sorted_entries_with_routing(entries, allocator_hint, &routing);
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

    fn scanned_summary(entries: &[crate::store::reader::ScannedIndexEntry]) -> Vec<ScanSummaryRow> {
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

        let parallel = read_sealed_sidx_entries_parallel(&sealed_segments)
            .expect("parallel SIDX footer read should succeed");
        let sequential = read_sealed_sidx_entries_sequential(&sealed_segments)
            .expect("sequential SIDX footer read should succeed");

        assert_eq!(
            scanned_summary(&parallel),
            scanned_summary(&sequential),
            "PROPERTY: parallel SIDX footer rebuild must match sequential footer semantics exactly."
        );
    }
}
