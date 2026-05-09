//! PROVES: read-walk evidence is opt-in, deterministic, visibility-bounded, and
//! keeps request intent separate from report identity.
//! CATCHES: selector/request serialization laundering, unbounded proof refs,
//! incorrect limit/drop counts, unsorted findings, and body-hash drift.
//! SEEDED: deterministic / no randomness.

use batpak::prelude::*;
use batpak::store::{
    ReadWalkDroppedCount, ReadWalkEvidenceReport, ReadWalkFinding, ReadWalkFreshnessIntent,
    ReadWalkFrontierKind, ReadWalkHash, ReadWalkInputFrontier, ReadWalkProofRef, ReadWalkProofRefs,
    ReadWalkReplayMode, ReadWalkReportBody, ReadWalkReportError, ReadWalkRequest,
    ReadWalkSourceRef, READ_WALK_REPORT_SCHEMA_VERSION,
};
use std::error::Error;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult = Result<(), Box<dyn Error>>;

fn append_events(store: &Store<Open>, entity: &str, scope: &str, count: u64) -> TestResult {
    let coord = Coordinate::new(entity, scope)?;
    let kind = EventKind::custom(0xE, 0x41);
    for n in 0..count {
        store.append(&coord, kind, &serde_json::json!({ "n": n }))?;
    }
    Ok(())
}

#[test]
fn read_walk_report_body_hash_is_deterministic() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    append_events(&store, "entity:readwalk:det", "scope:det", 3)?;
    let mut request = ReadWalkRequest::full(Region::scope("scope:det"));
    request.include_proof_refs = true;

    let first = store.query_with_read_walk_evidence(&request)?;
    let second = store.query_with_read_walk_evidence(&request)?;

    assert_eq!(first.1.body_hash, second.1.body_hash);
    assert_eq!(first.1.body, second.1.body);
    assert_eq!(first.1.body.schema_version, READ_WALK_REPORT_SCHEMA_VERSION);
    assert_eq!(first.1.body.replay_mode, ReadWalkReplayMode::Current);
    Ok(())
}

#[test]
fn read_walk_limit_reports_known_dropped_count() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    append_events(&store, "entity:readwalk:limit", "scope:limit", 5)?;
    let request = ReadWalkRequest {
        region: Region::scope("scope:limit"),
        limit: Some(2),
        include_proof_refs: false,
        freshness_intent: Freshness::Consistent,
    };

    let (entries, report) = store.query_with_read_walk_evidence(&request)?;
    assert_eq!(entries.len(), 2);
    assert_eq!(report.body.matched_count, 5);
    assert_eq!(report.body.returned_count, 2);
    assert_eq!(
        report.body.dropped_limited_count,
        ReadWalkDroppedCount::Known(3)
    );
    assert!(
        report
            .body
            .findings
            .iter()
            .any(|f| matches!(f, ReadWalkFinding::LimitedResults { dropped_count: 3 })),
        "PROPERTY: limited read walk must emit deterministic LimitedResults finding"
    );
    Ok(())
}

#[test]
fn read_walk_proof_refs_known_when_requested() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    append_events(&store, "entity:readwalk:proof", "scope:proof", 2)?;
    let request = ReadWalkRequest {
        region: Region::scope("scope:proof"),
        limit: None,
        include_proof_refs: true,
        freshness_intent: Freshness::MaybeStale { max_stale_ms: 10 },
    };
    let (entries, report) = store.query_with_read_walk_evidence(&request)?;

    assert_eq!(
        report.body.freshness_intent,
        ReadWalkFreshnessIntent::MaybeStale { max_stale_ms: 10 }
    );
    assert!(matches!(
        report.body.input_frontier,
        Some(frontier) if frontier.kind == ReadWalkFrontierKind::Visible
    ));
    let ReadWalkProofRefs::Known(refs) = &report.body.proof_refs else {
        return Err(
            std::io::Error::other("PROPERTY: proof refs must be known when requested").into(),
        );
    };
    assert_eq!(refs.len(), entries.len());
    assert!(!refs.is_empty());
    Ok(())
}

#[test]
fn read_walk_findings_are_sorted_deterministically() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    append_events(&store, "entity:readwalk:sort", "scope:sort", 4)?;
    let request = ReadWalkRequest {
        region: Region::scope("scope:sort"),
        limit: Some(1),
        include_proof_refs: false,
        freshness_intent: Freshness::Consistent,
    };
    let (entries, report) = store.query_with_read_walk_evidence(&request)?;
    assert_eq!(entries.len(), 1);
    let mut sorted = report.body.findings.clone();
    sorted.sort();
    assert_eq!(
        report.body.findings, sorted,
        "PROPERTY: read walk findings must be emitted in deterministic sorted order"
    );

    let source_ref = ReadWalkSourceRef::Scope {
        scope: "scope:sort".to_owned(),
    };
    assert!(matches!(source_ref, ReadWalkSourceRef::Scope { .. }));
    let report_hash: ReadWalkHash = report.body_hash;
    assert_ne!(report_hash, [0_u8; 32]);
    let input_frontier: Option<ReadWalkInputFrontier> = report.body.input_frontier;
    assert!(input_frontier.is_some());
    let proof_ref = ReadWalkProofRef {
        event_id: 0,
        global_sequence: 0,
        event_hash: [0_u8; 32],
    };
    assert_eq!(proof_ref.global_sequence, 0);
    let synthetic_error = ReadWalkReportError::BodyEncoding {
        message: "synthetic".to_owned(),
    };
    assert!(synthetic_error.to_string().contains("synthetic"));
    let body: ReadWalkReportBody = report.body.clone();
    assert_eq!(body.schema_version, READ_WALK_REPORT_SCHEMA_VERSION);
    let envelope: ReadWalkEvidenceReport = report;
    assert_eq!(
        envelope.body.schema_version,
        READ_WALK_REPORT_SCHEMA_VERSION
    );
    Ok(())
}

#[test]
fn read_walk_report_round_trips_through_canonical_encoding() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    append_events(
        &store,
        "entity:readwalk:report-serde",
        "scope:report-serde",
        2,
    )?;
    let request = ReadWalkRequest {
        region: Region::entity("entity:readwalk:report-serde")
            .with_scope("scope:report-serde")
            .with_fact(batpak::coordinate::KindFilter::Exact(EventKind::custom(
                0xE, 0x41,
            )))
            .with_clock_range((0, 9)),
        limit: Some(3),
        include_proof_refs: true,
        freshness_intent: Freshness::MaybeStale { max_stale_ms: 25 },
    };

    let (_, report) = store.query_with_read_walk_evidence(&request)?;
    let bytes = batpak::canonical::to_bytes(&report)?;
    let decoded: ReadWalkEvidenceReport = batpak::canonical::from_bytes(&bytes)?;

    assert_eq!(decoded, report);
    Ok(())
}
