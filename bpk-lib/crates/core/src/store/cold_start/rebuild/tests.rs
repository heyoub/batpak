use super::topology::compaction_source_temp_path;
use super::*;
use crate::prelude::*;
use crate::store::segment;
use std::collections::BTreeMap;
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
    use crate::id::EntityIdType;
    entries
        .iter()
        .map(|entry| ScanSummaryRow {
            event_id: entry.header.event_id.as_u128(),
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
        let coord = Coordinate::new(format!("entity:{i}"), "scope:rebuild").expect("valid coord");
        let entity_id = interner.intern(coord.entity());
        let scope_id = interner.intern(coord.scope());
        entries.push(IndexEntry {
            event_id: (i + 1) as u128,
            correlation_id: (i + 1) as u128,
            causation_id: None,
            coord,
            entity_id,
            scope_id,
            kind: EventKind::custom(0x1, u16::try_from(i + 1).expect("sample type id fits u16")),
            wall_ms: 1_700_000_000_000 + i * 1000,
            clock: u32::try_from(i + 1).expect("clock fits u32"),
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos::new(segment_id, i * 256, 256),
            global_sequence: i,
            receipt_extensions: BTreeMap::new(),
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
    let active_segment = entries.last().expect("at least one segment").0;
    let sealed_segments: Vec<_> = entries
        .into_iter()
        .filter(|(segment_id, _)| *segment_id < active_segment)
        .collect();

    assert!(
        !sealed_segments.is_empty(),
        "PROPERTY: tiny segments should produce at least one sealed segment with an SIDX footer."
    );

    let reader = Reader::new(
        dir.path().to_path_buf(),
        16,
        std::sync::Arc::new(crate::store::SystemClock::new()),
    );
    let (parallel, _) = read_sealed_sidx_entries_parallel(&reader, &sealed_segments)
        .expect("parallel SIDX footer read should succeed");
    let sequential = read_sealed_sidx_entries_sequential(&reader, &sealed_segments)
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
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        std::sync::Arc::new(crate::store::SystemClock::new()),
    );
    let clock = crate::store::SystemClock::new();
    let planner = RestorePlanner {
        reader: &reader,
        data_dir: dir.path(),
        policy: ColdStartPolicy::new(false, false),
        clock: &clock,
    };
    let (entries, interner_strings) = sample_index_entries(0, 0);
    let routing = RoutingSummary::from_sorted_entries(&entries, 1);
    let expected_chunk_count = routing.chunk_count;

    let plan = planner
        .build_snapshot_plan(
            RestoreSource::Checkpoint,
            SnapshotPlanInput {
                entries,
                interner_strings,
                watermark: WatermarkInfo {
                    watermark_segment_id: 99,
                    watermark_offset: 0,
                },
                stored_allocator: 2,
                routing,
                reopen_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
                persisted_cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
                receipt_extensions_hydrated: false,
                snapshot_loads: SnapshotLoadDiagnostics::default(),
            },
        )
        .expect("build snapshot plan");

    assert_eq!(
        plan.tail_entries, 0,
        "SANITY: empty temp dir should produce no tail replay"
    );
    assert_eq!(
            plan.routing.chunk_count,
            expected_chunk_count,
            "PROPERTY: a snapshot plan with no tail entries must preserve the existing routing chunk count instead of synthesizing an extra chunk"
        );
}

#[test]
fn build_snapshot_plan_rejects_snapshot_entries_without_backing_frames() {
    let dir = TempDir::new().expect("temp dir");
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        std::sync::Arc::new(crate::store::SystemClock::new()),
    );
    let clock = crate::store::SystemClock::new();
    let planner = RestorePlanner {
        reader: &reader,
        data_dir: dir.path(),
        policy: ColdStartPolicy::new(false, false),
        clock: &clock,
    };
    let (entries, interner_strings) = sample_index_entries(1, 0);
    let routing = RoutingSummary::from_sorted_entries(&entries, 1);

    let result = planner.build_snapshot_plan(
        RestoreSource::Checkpoint,
        SnapshotPlanInput {
            entries,
            interner_strings,
            watermark: WatermarkInfo {
                watermark_segment_id: 99,
                watermark_offset: 0,
            },
            stored_allocator: 1,
            routing,
            reopen_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
            persisted_cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
            receipt_extensions_hydrated: false,
            snapshot_loads: SnapshotLoadDiagnostics::default(),
        },
    );
    assert!(
        matches!(result, Err(StoreError::Io(_))),
        "PROPERTY: snapshot entries without backing frames must fail closed with an IO error"
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

    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        std::sync::Arc::new(crate::store::SystemClock::new()),
    );
    reader.set_active_segment(active_after_tail);
    let clock = crate::store::SystemClock::new();
    let planner = RestorePlanner {
        reader: &reader,
        data_dir: dir.path(),
        policy: ColdStartPolicy::new(false, false),
        clock: &clock,
    };
    let routing = RoutingSummary::from_sorted_entries(&[], 1);

    let plan = planner
        .build_snapshot_plan(
            RestoreSource::Checkpoint,
            SnapshotPlanInput {
                entries: Vec::new(),
                interner_strings: Vec::new(),
                watermark: WatermarkInfo {
                    watermark_segment_id,
                    watermark_offset: 0,
                },
                stored_allocator: 0,
                routing,
                reopen_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
                persisted_cumulative_reserved_kind_fallbacks: ReservedKindFallbackStats::default(),
                receipt_extensions_hydrated: false,
                snapshot_loads: SnapshotLoadDiagnostics::default(),
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
            event_id: crate::id::EventId::from(1u128),
            correlation_id: crate::id::CorrelationId::from(1u128),
            causation_id: Some(crate::id::CausationId::from(0u128)),
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
        receipt_extensions: BTreeMap::new(),
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
            event_id: crate::id::EventId::from(2u128),
            correlation_id: crate::id::CorrelationId::from(1u128),
            causation_id: Some(crate::id::CausationId::from(99u128)),
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
        receipt_extensions: BTreeMap::new(),
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
        paths[0].1, merged_path,
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

    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        std::sync::Arc::new(crate::store::SystemClock::new()),
    );
    let index = StoreIndex::new();
    let report = open_index(
        &index,
        &reader,
        dir.path(),
        ColdStartPolicy::new(true, false),
        &crate::store::SystemClock::new(),
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
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        std::sync::Arc::new(crate::store::SystemClock::new()),
    );
    reader.set_active_segment(highest_segment_id + 1);
    let tail_entries = collect_tail_entries(
        &interner,
        &reader,
        dir.path(),
        &WatermarkInfo {
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
