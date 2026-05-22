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
    sort_entries_by_entity(&mut entries_by_entity);
    (entries_by_sequence, entries_by_entity)
}

#[test]
fn restore_chunk_ranges_uses_valid_persisted_chunks() {
    let entries = vec![
        entry(0, "alpha"),
        entry(1, "alpha"),
        entry(2, "beta"),
        entry(3, "beta"),
    ];
    let routing = RoutingSummary::from_sorted_entries(&entries, 2);

    assert_eq!(
        restore_chunk_ranges(entries.len(), &routing),
        vec![(0, 2), (2, 2)]
    );
}

#[test]
fn restore_chunk_ranges_falls_back_for_malformed_chunks() {
    let entries = vec![
        entry(0, "alpha"),
        entry(1, "alpha"),
        entry(2, "beta"),
        entry(3, "beta"),
    ];
    let mut routing = RoutingSummary::from_sorted_entries(&entries, 2);
    routing.chunks[1].start = 3;

    assert_eq!(restore_chunk_ranges(entries.len(), &routing), vec![(0, 4)]);
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
    assert_eq!(
        summary.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Valid
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
    assert_eq!(
        wrong_first.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::ChunkFirstSequenceMismatch)
    );
    assert!(
        !wrong_first.validate(&entries_by_sequence, &entries_by_entity),
        "PROPERTY: chunk validation must reject a mismatched first sequence even when the last sequence still matches"
    );

    let mut wrong_last = summary;
    wrong_last.chunks[0].last_sequence += 100;
    assert_eq!(
        wrong_last.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::ChunkLastSequenceMismatch)
    );
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
    assert_eq!(
        empty_run.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::EntityRunLenZero)
    );
    assert!(
        !empty_run.validate(&entries_by_sequence, &entries_by_entity),
        "PROPERTY: zero-length entity runs are invalid and must not be accepted as harmless no-ops"
    );

    let mut out_of_bounds_run = summary;
    out_of_bounds_run.entity_runs[0].start = entries_by_entity.len() as u64;
    out_of_bounds_run.entity_runs[0].len = 1;
    assert_eq!(
        out_of_bounds_run.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::EntityRunEndOutOfBounds)
    );
    assert!(
        !out_of_bounds_run.validate(&entries_by_sequence, &entries_by_entity),
        "PROPERTY: entity runs whose end exceeds the entity-sorted table are invalid"
    );
}

#[test]
fn routing_summary_validate_detailed_reports_count_and_total_mismatches() {
    let entries = vec![
        entry(0, "alpha"),
        entry(1, "alpha"),
        entry(2, "beta"),
        entry(3, "beta"),
    ];
    let summary = RoutingSummary::from_sorted_entries(&entries, 2);
    let (entries_by_sequence, entries_by_entity) = sorted_arcs(entries);

    let mut wrong_entry_count = summary.clone();
    wrong_entry_count.entry_count += 1;
    assert_eq!(
        wrong_entry_count.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::EntryCountMismatch)
    );

    let mut wrong_chunk_count = summary.clone();
    wrong_chunk_count.chunk_count += 1;
    assert_eq!(
        wrong_chunk_count.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::ChunkCountMismatch)
    );

    let mut missing_chunk = summary.clone();
    missing_chunk.chunks.pop();
    missing_chunk.chunk_count = missing_chunk.chunks.len() as u64;
    assert_eq!(
        missing_chunk.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::ChunkTotalMismatch)
    );

    let mut missing_run = summary;
    missing_run.entity_runs.pop();
    assert_eq!(
        missing_run.validate_detailed(&entries_by_sequence, &entries_by_entity),
        RoutingValidation::Invalid(RoutingValidationError::EntityRunTotalMismatch)
    );
}
