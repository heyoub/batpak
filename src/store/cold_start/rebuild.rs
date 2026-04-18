use crate::coordinate::Coordinate;
use crate::store::cold_start::ColdStartPolicy;
use crate::store::config::duration_micros;
use crate::store::index::interner::StringInterner;
use crate::store::index::{DiskPos, IndexEntry, RoutingSummary, StoreIndex};
use crate::store::segment;
use crate::store::segment::scan::{Reader, ScannedIndexEntry};
use crate::store::StoreError;
use rayon::prelude::*;
use std::path::Path;

pub(crate) const COMPACTION_MARKER_FILENAME: &str = "compaction.pending.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingCompaction {
    pub merged_id: u64,
    pub source_segment_ids: Vec<u64>,
}

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
                    snapshot.entries,
                    snapshot.interner_strings,
                    snapshot.watermark,
                    snapshot.stored_allocator,
                    snapshot.routing,
                );
            }
        }

        if !has_pending_compaction && self.policy.try_checkpoint() {
            if let Some(snapshot) = super::checkpoint::try_load_checkpoint_snapshot(self.data_dir) {
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

    // justifies: ADR-0008; planner in src/store/cold_start/rebuild.rs takes ownership of snapshot data — clippy's suggestion would force a re-clone on the caller side.
    #[allow(clippy::needless_pass_by_value)]
    fn build_snapshot_plan(
        &self,
        source: RestoreSource,
        mut snapshot_entries: Vec<IndexEntry>,
        interner_strings: Vec<String>,
        watermark: super::checkpoint::WatermarkInfo,
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
pub(crate) fn open_index(
    index: &StoreIndex,
    reader: &Reader,
    data_dir: &Path,
    policy: ColdStartPolicy,
) -> Result<OpenIndexReport, StoreError> {
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
    index.restore_sorted_entries_with_routing(plan.entries, plan.allocator_hint, &plan.routing);
    // G2: cold-start fails closed on corrupt hidden-ranges metadata.
    // A missing file is OK (first open); any other read/parse failure is
    // surfaced so callers cannot silently resurrect cancelled events.
    if let Some(ranges) = crate::store::hidden_ranges::load_cancelled_ranges(data_dir)? {
        index.restore_cancelled_visibility_ranges(ranges);
    }

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
        elapsed_us: duration_micros(t0.elapsed()),
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
    super::write_artifact_atomically(data_dir, &final_path, "compaction marker", |file| {
        serde_json::to_writer(file, &marker).map_err(|e| StoreError::Serialization(Box::new(e)))
    })
}

pub(crate) fn clear_pending_compaction(data_dir: &Path) -> Result<(), StoreError> {
    let path = pending_compaction_path(data_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            #[cfg(unix)]
            {
                if let Some(parent) = path.parent() {
                    std::fs::File::open(parent)
                        .and_then(|dir| dir.sync_all())
                        .map_err(StoreError::Io)?;
                }
            }
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
) -> Option<Vec<ScannedIndexEntry>> {
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
) -> Result<Vec<ScannedIndexEntry>, StoreError> {
    match crate::store::segment::sidx::read_footer(path) {
        Ok(Some((entries, strings))) => {
            let mut scanned = Vec::with_capacity(entries.len());
            for entry in entries {
                let row = entry.to_cold_start_row(segment_id);
                let kind = row.kind;
                if kind == crate::event::EventKind::SYSTEM_BATCH_BEGIN
                    || kind == crate::event::EventKind::SYSTEM_BATCH_COMMIT
                {
                    continue;
                }
                scanned.push(ScannedIndexEntry::from_cold_start_row(&row, &strings)?);
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
) -> Result<Vec<ScannedIndexEntry>, StoreError> {
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

type RebuildResult = (RestoreSource, Vec<IndexEntry>, Vec<String>, u64, usize);

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
            report.path,
            OpenIndexPath::Rebuild,
            "PROPERTY: pending compaction must force a marker-aware rebuild instead of trusting checkpoint fast paths."
        );
    }
}
