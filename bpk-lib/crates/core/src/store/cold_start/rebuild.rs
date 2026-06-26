use crate::coordinate::Coordinate;
use crate::store::cold_start::{ColdStartPolicy, ReservedKindFallbackStats, WatermarkInfo};
use crate::store::index::interner::StringInterner;
use crate::store::index::{DiskPos, IndexEntry, RoutingSummary, StoreIndex};
use crate::store::segment::scan::{FrameScanTailPolicy, Reader, ScannedIndexEntry};
use crate::store::StoreError;
use rayon::prelude::*;
use std::path::Path;

/// Reference to the optional fault injector threaded through the cold-start
/// recovery path.
///
/// With `dangerous-test-hooks` ON this is `&Option<Arc<dyn FaultInjector>>`
/// (always `None` unless a test installs one via `with_fault_injector`, so
/// threading it is behavior-preserving). With the feature OFF the `fault`
/// module does not exist, so the alias collapses to `&()` and every recovery
/// fn carries an inert reference the compiler elides — keeping the default
/// build byte-for-byte behavior-identical.
#[cfg(feature = "dangerous-test-hooks")]
type FaultInjectorRef<'a> = &'a Option<std::sync::Arc<dyn crate::store::fault::FaultInjector>>;
#[cfg(not(feature = "dangerous-test-hooks"))]
type FaultInjectorRef<'a> = &'a ();

/// The inert "no fault injector" value matching [`FaultInjectorRef`] for the
/// active feature configuration. Used by the post-compaction rebuild path
/// (which is never fault-injected), the sequential SIDX helper, and internal
/// unit tests so they thread a no-op injector regardless of whether
/// `dangerous-test-hooks` is enabled.
const NO_FAULT_INJECTOR: FaultInjectorRef<'static> = {
    #[cfg(feature = "dangerous-test-hooks")]
    {
        &None
    }
    #[cfg(not(feature = "dangerous-test-hooks"))]
    {
        &()
    }
};

#[cfg(not(feature = "dangerous-test-hooks"))]
#[inline]
fn disabled_fault_injection_input<T>(_value: T) {}

mod load_status;
mod report;
mod topology;

pub use load_status::OpenIndexLoadStatus;
use load_status::SnapshotLoadDiagnostics;
pub use report::{OpenIndexPath, OpenIndexReport};
pub(crate) use topology::{
    clear_pending_compaction, write_pending_compaction, COMPACTION_MARKER_FILENAME,
};
use topology::{load_pending_compaction, segment_paths};

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
    snapshot_loads: SnapshotLoadDiagnostics,
}

struct SnapshotPlanInput {
    entries: Vec<IndexEntry>,
    interner_strings: Vec<String>,
    watermark: WatermarkInfo,
    stored_allocator: u64,
    routing: RoutingSummary,
    reopen_reserved_kind_fallbacks: ReservedKindFallbackStats,
    persisted_cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats,
    receipt_extensions_hydrated: bool,
    snapshot_loads: SnapshotLoadDiagnostics,
}

struct RestorePlanner<'a> {
    reader: &'a Reader,
    data_dir: &'a Path,
    policy: ColdStartPolicy,
    clock: &'a dyn crate::store::Clock,
    fault_injector: FaultInjectorRef<'a>,
}

impl<'a> RestorePlanner<'a> {
    fn build(&self) -> Result<RestorePlan, StoreError> {
        // Pending compaction requires marker-aware segment reconciliation
        // before stale mmap/checkpoint artifacts can be trusted.
        let has_pending_compaction = load_pending_compaction(self.data_dir)?.is_some();
        let mut snapshot_loads = SnapshotLoadDiagnostics::default();

        if !has_pending_compaction && self.policy.try_mmap_index() {
            #[cfg(feature = "dangerous-test-hooks")]
            crate::store::fault::maybe_inject(
                crate::store::fault::InjectionPoint::MmapIndexLoad,
                self.fault_injector,
            )?;
            let mmap_load = super::mmap::load_mmap_snapshot(self.data_dir, self.clock);
            snapshot_loads.record_mmap(&mmap_load);
            // A future-version mmap artifact is a CANONICAL TYPED REFUSAL, NOT a
            // silent rebuild-from-scan: a future writer may have written data
            // this reader cannot interpret, so degrading to a scan would risk a
            // silent downgrade instead of a legally reachable state. Corrupt or
            // older artifacts (`Invalid`) still fall through to the safe rebuild
            // below. justifies: INV-MMAP-SEALED-READS
            if let super::FileLoad::FutureVersion { found, supported } = mmap_load {
                return Err(StoreError::MmapFutureVersion { found, supported });
            }
            if let super::FileLoad::Loaded(snapshot) = mmap_load {
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
                        receipt_extensions_hydrated: snapshot.receipt_extensions_hydrated,
                        snapshot_loads,
                    },
                );
            }
        }

        if !has_pending_compaction && self.policy.try_checkpoint() {
            #[cfg(feature = "dangerous-test-hooks")]
            crate::store::fault::maybe_inject(
                crate::store::fault::InjectionPoint::CheckpointDecode,
                self.fault_injector,
            )?;
            let checkpoint_load = super::checkpoint::load_checkpoint_snapshot(self.data_dir);
            snapshot_loads.record_checkpoint(&checkpoint_load);
            // A future-version checkpoint is a CANONICAL TYPED REFUSAL, NOT a
            // silent rebuild-from-scan — same stance as the mmap path above. A
            // corrupt or older checkpoint (`Invalid`) still falls through to the
            // safe rebuild below. justifies: INV-ONDISK-FORWARD-COMPAT-CANONICAL
            if let super::FileLoad::FutureVersion { found, supported } = checkpoint_load {
                return Err(StoreError::CheckpointFutureVersion { found, supported });
            }
            if let super::FileLoad::Loaded(snapshot) = checkpoint_load {
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
                        receipt_extensions_hydrated: snapshot.receipt_extensions_hydrated,
                        snapshot_loads,
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
        ) = collect_rebuild_entries(self.reader, self.data_dir, self.fault_injector)?;
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
            snapshot_loads,
        })
    }

    fn build_snapshot_plan(
        &self,
        source: RestoreSource,
        snapshot: SnapshotPlanInput,
    ) -> Result<RestorePlan, StoreError> {
        // Destructure up front so the move-out of each owned field is explicit;
        // the planner consumes the snapshot rather than borrowing it.
        let SnapshotPlanInput {
            mut entries,
            interner_strings,
            watermark,
            stored_allocator,
            routing: input_routing,
            reopen_reserved_kind_fallbacks,
            persisted_cumulative_reserved_kind_fallbacks,
            receipt_extensions_hydrated,
            snapshot_loads,
        } = snapshot;

        if !receipt_extensions_hydrated {
            hydrate_receipt_extensions(self.reader, &mut entries)?;
        }
        let interner = StringInterner::new();
        interner.replace_from_full_snapshot(&interner_strings)?;
        let tail_entries = collect_tail_entries(
            &interner,
            self.reader,
            self.data_dir,
            &watermark,
            stored_allocator,
            self.fault_injector,
        )?;
        let restored_entries = entries.len();
        let tail_count = tail_entries.len();
        entries.extend(tail_entries);
        entries.sort_by_key(|entry| entry.global_sequence);
        let chunk_count = usize::try_from(input_routing.chunk_count)
            .unwrap_or(1)
            .max(1)
            + usize::from(tail_count > 0);
        let routing = RoutingSummary::from_sorted_entries(&entries, chunk_count);

        Ok(RestorePlan {
            source,
            allocator_hint: stored_allocator.max(
                entries
                    .last()
                    .map(|entry| entry.global_sequence.saturating_add(1))
                    .unwrap_or(0),
            ),
            interner_strings: full_interner_snapshot(&interner),
            routing,
            entries,
            restored_entries,
            tail_entries: tail_count,
            reopen_reserved_kind_fallbacks,
            persisted_cumulative_reserved_kind_fallbacks,
            snapshot_loads,
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
    clock: &dyn crate::store::Clock,
    fault_injector: FaultInjectorRef<'_>,
) -> Result<OpenIndexOutcome, StoreError> {
    let t0 = clock.now_mono_ns();
    let planner = RestorePlanner {
        reader,
        data_dir,
        policy,
        clock,
        fault_injector,
    };
    let t_plan = clock.now_mono_ns();
    let plan = planner.build()?;
    let phase_plan_build_us = elapsed_us(clock, t_plan);

    let t_interner = clock.now_mono_ns();
    index
        .interner
        .replace_from_full_snapshot(&plan.interner_strings)?;
    let phase_interner_us = elapsed_us(clock, t_interner);

    let t_restore = clock.now_mono_ns();
    index.restore_sorted_entries_with_routing(plan.entries, plan.allocator_hint, &plan.routing)?;
    let phase_restore_index_us = elapsed_us(clock, t_restore);

    // Restore the durable idempotency store UNCONDITIONALLY and early. It is an
    // AUTHORITY: it is NEVER reconstructed from a segment scan (segments may
    // have evicted the events) and the index rebuild above must NOT overwrite
    // it. A missing/corrupt file degrades to empty (logged loudly); a
    // future-version file is a hard error. justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
    match crate::store::index::idemp::read_idemp_file(data_dir)? {
        crate::store::index::idemp::IdempLoad::Loaded(entries) => {
            index.idemp.restore(entries);
        }
        crate::store::index::idemp::IdempLoad::Missing => {}
        crate::store::index::idemp::IdempLoad::Invalid { reason } => {
            tracing::warn!(
                target: "batpak::idemp",
                reason = %reason,
                "durable idempotency store unreadable on open; continuing with empty durable \
                 dedup history (store remains correct, loses cross-compaction dedup memory)"
            );
        }
    }

    // G2: cold-start fails closed on corrupt hidden-ranges metadata.
    // A missing file is OK (first open); any other read/parse failure is
    // surfaced so callers cannot silently resurrect cancelled events.
    let t_hidden = clock.now_mono_ns();
    #[cfg(feature = "dangerous-test-hooks")]
    crate::store::fault::maybe_inject(
        crate::store::fault::InjectionPoint::HiddenRangesLoad,
        fault_injector,
    )?;
    if let Some(ranges) = crate::store::hidden_ranges::load_cancelled_ranges(data_dir)? {
        index.restore_cancelled_visibility_ranges(ranges);
    }
    let phase_hidden_ranges_us = elapsed_us(clock, t_hidden);

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
            elapsed_us: elapsed_us(clock, t0),
            phase_plan_build_us,
            phase_interner_us,
            phase_restore_index_us,
            phase_hidden_ranges_us,
            mmap_load_status: plan.snapshot_loads.mmap_status,
            mmap_invalid_reason: plan.snapshot_loads.mmap_invalid_reason.clone(),
            checkpoint_load_status: plan.snapshot_loads.checkpoint_status,
            checkpoint_invalid_reason: plan.snapshot_loads.checkpoint_invalid_reason.clone(),
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

fn elapsed_us(clock: &dyn crate::store::Clock, start_ns: i64) -> u64 {
    u64::try_from(clock.now_mono_ns().saturating_sub(start_ns).max(0) / 1_000).unwrap_or(u64::MAX)
}

fn read_sealed_sidx_entries_parallel(
    reader: &Reader,
    sealed_segments: &[(u64, std::path::PathBuf)],
    fault_injector: FaultInjectorRef<'_>,
) -> Option<(Vec<ScannedIndexEntry>, ReservedKindFallbackStats)> {
    let per_segment: Result<Vec<_>, StoreError> = sealed_segments
        .par_iter()
        .map(|(segment_id, path)| {
            scanned_entries_from_sidx_footer(reader, *segment_id, path, fault_injector)
        })
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
    reader: &Reader,
    segment_id: u64,
    path: &Path,
    fault_injector: FaultInjectorRef<'_>,
) -> Result<(Vec<ScannedIndexEntry>, ReservedKindFallbackStats), StoreError> {
    #[cfg(feature = "dangerous-test-hooks")]
    crate::store::fault::maybe_inject(
        crate::store::fault::InjectionPoint::IndexFooterDecode { segment_id },
        fault_injector,
    )?;
    #[cfg(not(feature = "dangerous-test-hooks"))]
    disabled_fault_injection_input(fault_injector);
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
                let mut scanned_entry = ScannedIndexEntry::from_cold_start_row(&row, &strings)?;
                scanned_entry.receipt_extensions = reader.read_receipt_extensions(&row.disk_pos)?;
                scanned.push(scanned_entry);
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
    reader: &Reader,
    sealed_segments: &[(u64, std::path::PathBuf)],
) -> Result<Vec<ScannedIndexEntry>, StoreError> {
    let mut flat = Vec::new();
    for (segment_id, path) in sealed_segments {
        flat.extend(
            scanned_entries_from_sidx_footer(reader, *segment_id, path, NO_FAULT_INJECTOR)?.0,
        );
    }
    flat.sort_by_key(|entry| entry.global_sequence.unwrap_or(0));
    Ok(flat)
}

fn hydrate_receipt_extensions(
    reader: &Reader,
    entries: &mut [IndexEntry],
) -> Result<(), StoreError> {
    for entry in entries {
        entry.receipt_extensions = reader.read_receipt_extensions(&entry.disk_pos)?;
    }
    Ok(())
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
    let entity_id = interner.intern(&se.entity)?;
    let scope_id = interner.intern(&se.scope)?;
    let clock = se.header.position.sequence;
    use crate::id::EntityIdType;
    Ok(IndexEntry {
        event_id: se.header.event_id.as_u128(),
        correlation_id: se.header.correlation_id.as_u128(),
        causation_id: se
            .header
            .causation_id
            .map(|id| id.as_u128())
            .filter(|&id| id != 0),
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
        receipt_extensions: se.receipt_extensions,
    })
}

fn collect_tail_entries(
    interner: &StringInterner,
    reader: &Reader,
    data_dir: &Path,
    watermark: &WatermarkInfo,
    allocator_floor: u64,
    fault_injector: FaultInjectorRef<'_>,
) -> Result<Vec<IndexEntry>, StoreError> {
    let entries = segment_paths(data_dir)?;
    let recoverable_tail_segment_id = entries.last().map(|(segment_id, _)| *segment_id);
    let mut batch_state = crate::store::segment::scan::BatchRecoveryState::default();
    let mut tracker = SequenceTracker {
        max_seen: allocator_floor.saturating_sub(1),
        inserted_any: allocator_floor > 0,
    };
    let mut rebuilt_entries = Vec::new();
    let mut frame_index: usize = 0;

    for (seg_id, path) in &entries {
        if *seg_id < watermark.watermark_segment_id {
            continue;
        }

        let tail_policy = if Some(*seg_id) == recoverable_tail_segment_id {
            FrameScanTailPolicy::RecoverTornTail
        } else {
            FrameScanTailPolicy::FailClosed
        };

        reader.scan_segment_index_into_with_tail_policy(
            path,
            Some(&mut batch_state),
            tail_policy,
            |se| {
                inject_scan_frame(fault_injector, *seg_id, se.offset, &mut frame_index)?;
                if *seg_id == watermark.watermark_segment_id
                    && se.offset < watermark.watermark_offset
                {
                    return Ok(());
                }
                let global_sequence = se
                    .global_sequence
                    .unwrap_or_else(|| tracker.synthesize_next());
                let entry = entry_from_scan(interner, se, global_sequence)?;
                tracker.note_seen(global_sequence);
                rebuilt_entries.push(entry);
                Ok(())
            },
        )?;
    }

    Ok(rebuilt_entries)
}

/// Per-frame cold-start scan injection helper.
///
/// Fires `ColdStartScanFrame` (and a paired `ReadAt`) for the frame currently
/// being scanned, then advances `frame_index`. Compiles to a no-op (only the
/// counter bump) when `dangerous-test-hooks` is off, keeping the default build
/// behavior-identical.
#[inline]
fn inject_scan_frame(
    fault_injector: FaultInjectorRef<'_>,
    segment_id: u64,
    offset: u64,
    frame_index: &mut usize,
) -> Result<(), StoreError> {
    let this_frame = *frame_index;
    *frame_index = frame_index.saturating_add(1);
    #[cfg(feature = "dangerous-test-hooks")]
    {
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::ReadAt { offset, len: 0 },
            fault_injector,
        )?;
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::ColdStartScanFrame {
                segment_id,
                frame_index: this_frame,
            },
            fault_injector,
        )?;
    }
    #[cfg(not(feature = "dangerous-test-hooks"))]
    {
        disabled_fault_injection_input((fault_injector, segment_id, offset, this_frame));
    }
    Ok(())
}

type RebuildResult = (
    RestoreSource,
    Vec<IndexEntry>,
    Vec<String>,
    u64,
    usize,
    ReservedKindFallbackStats,
);

fn collect_rebuild_entries(
    reader: &Reader,
    data_dir: &Path,
    fault_injector: FaultInjectorRef<'_>,
) -> Result<RebuildResult, StoreError> {
    let entries = segment_paths(data_dir)?;
    let recoverable_tail_segment_id = entries.last().map(|(segment_id, _)| *segment_id);
    let configured_active_segment = reader.active_segment_id();
    let active_segment_id = (configured_active_segment != 0).then_some(configured_active_segment);
    let interner = StringInterner::new();
    let mut rebuilt_entries = Vec::new();
    let mut tracker = SequenceTracker::default();
    let mut frame_index: usize = 0;

    let sealed_segments: Vec<_> = entries
        .iter()
        .filter(|(segment_id, _)| active_segment_id.is_none_or(|active| *segment_id < active))
        .cloned()
        .collect();

    let mut source = RestoreSource::SealedSidxRebuild;
    let mut chunk_count = sealed_segments.len().max(1);

    let mut reserved_kind_fallbacks = ReservedKindFallbackStats::default();

    if !sealed_segments.is_empty() {
        if let Some((scanned, counts)) =
            read_sealed_sidx_entries_parallel(reader, &sealed_segments, fault_injector)
        {
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
            for (segment_id, path) in &sealed_segments {
                let tail_policy = if Some(*segment_id) == recoverable_tail_segment_id {
                    FrameScanTailPolicy::RecoverTornTail
                } else {
                    FrameScanTailPolicy::FailClosed
                };
                reader.scan_segment_index_into_with_tail_policy(
                    path,
                    Some(&mut batch_state),
                    tail_policy,
                    |se| {
                        inject_scan_frame(
                            fault_injector,
                            *segment_id,
                            se.offset,
                            &mut frame_index,
                        )?;
                        let global_sequence = se
                            .global_sequence
                            .unwrap_or_else(|| tracker.synthesize_next());
                        let entry = entry_from_scan(&interner, se, global_sequence)?;
                        tracker.note_seen(global_sequence);
                        rebuilt_entries.push(entry);
                        Ok(())
                    },
                )?;
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
        reader.scan_segment_index_into_with_tail_policy(
            path,
            Some(&mut batch_state),
            FrameScanTailPolicy::RecoverTornTail,
            |se| {
                inject_scan_frame(fault_injector, *segment_id, se.offset, &mut frame_index)?;
                let global_sequence = se
                    .global_sequence
                    .unwrap_or_else(|| tracker.synthesize_next());
                let entry = entry_from_scan(&interner, se, global_sequence)?;
                tracker.note_seen(global_sequence);
                rebuilt_entries.push(entry);
                Ok(())
            },
        )?;
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
        collect_rebuild_entries(reader, data_dir, NO_FAULT_INJECTOR)?;
    index
        .interner
        .replace_from_full_snapshot(&interner_strings)?;
    let routing = RoutingSummary::from_sorted_entries(&entries, chunk_count.max(1));
    index.restore_sorted_entries_with_routing(entries, allocator_hint, &routing)?;
    Ok(())
}

#[cfg(test)]
mod tests;
