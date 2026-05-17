//! PROVES: projection-run evidence is bound to projection flow outcome facts and
//! emits deterministic, precise freshness/cache/frontier/output states.
//! CATCHES: pre-run frontier sampling, fake unknown freshness/cache states,
//! stale body hashes, unsorted findings, and projection failures without
//! structured evidence.
//! SEEDED: deterministic / no randomness.

use batpak::prelude::*;
use batpak::store::projection::{CacheCapabilities, CacheMeta, ProjectionCache};
use batpak::store::{
    ProjectionRunCacheStatus, ProjectionRunCheckpointRef, ProjectionRunEvidenceReport,
    ProjectionRunFinding, ProjectionRunFreshnessStatus, ProjectionRunFrontierKind,
    ProjectionRunHash, ProjectionRunInputFrontier, ProjectionRunOutputHash,
    ProjectionRunReplayMode, ProjectionRunReportBody, ProjectionRunReportError,
    ProjectionRunRequestedFreshness, ProjectionSourceRef, PROJECTION_RUN_REPORT_SCHEMA_VERSION,
};
use std::error::Error;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult = Result<(), Box<dyn Error>>;

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct CounterProjection {
    count: u64,
}

impl EventSourced for CounterProjection {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len() as u64,
        })
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        std::hint::black_box(event.event_kind());
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 0x61)];
        &KINDS
    }
}

struct CacheGetError;

impl ProjectionCache for CacheGetError {
    fn capabilities(&self) -> CacheCapabilities {
        CacheCapabilities::none()
    }

    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        std::hint::black_box(key);
        Err(StoreError::CacheFailed("cache get fail".into()))
    }

    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError> {
        std::hint::black_box((key, value, meta.watermark));
        Ok(())
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError> {
        std::hint::black_box(prefix);
        Ok(0)
    }

    fn sync(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

#[test]
fn projection_run_report_is_deterministic_for_same_inputs() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:projection-run-deterministic", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x61);
    store.append(&coord, kind, &serde_json::json!({"n": 1}))?;

    let first = store.project_run_evidence::<CounterProjection>(
        "entity:projection-run-deterministic",
        &Freshness::Consistent,
    )?;
    let second = store.project_run_evidence::<CounterProjection>(
        "entity:projection-run-deterministic",
        &Freshness::Consistent,
    )?;

    assert_eq!(first.1.body_hash, second.1.body_hash);
    assert_eq!(first.1.body, second.1.body);
    assert_eq!(
        first.1.body.schema_version,
        PROJECTION_RUN_REPORT_SCHEMA_VERSION
    );
    assert_eq!(first.1.body.replay_mode, ProjectionRunReplayMode::Current);
    assert_eq!(
        first.1.body.requested_freshness,
        ProjectionRunRequestedFreshness::Consistent
    );
    Ok(())
}

#[test]
fn projection_run_success_records_known_output_hash_when_available() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:projection-run-output-hash", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x61);
    store.append(&coord, kind, &serde_json::json!({"n": 1}))?;

    let (state, report) = store.project_run_evidence::<CounterProjection>(
        "entity:projection-run-output-hash",
        &Freshness::Consistent,
    )?;

    assert_eq!(state, Some(CounterProjection { count: 1 }));
    assert!(
        matches!(report.body.output_hash, ProjectionRunOutputHash::Known(_)),
        "PROPERTY: successful projection runs with serializable output must record Known output hash"
    );
    assert!(matches!(
        report.body.input_frontier,
        Some(ProjectionRunInputFrontier {
            kind: ProjectionRunFrontierKind::Visible,
            ..
        })
    ));
    Ok(())
}

#[test]
fn projection_run_input_frontier_is_bound_to_replay_watermark() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:projection-run-frontier", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x61);
    let first = store.append(&coord, kind, &serde_json::json!({"n": 1}))?;
    let second = store.append(&coord, kind, &serde_json::json!({"n": 2}))?;

    let (state, report) = store.project_run_evidence::<CounterProjection>(
        "entity:projection-run-frontier",
        &Freshness::Consistent,
    )?;

    assert_eq!(state, Some(CounterProjection { count: 2 }));
    assert_ne!(first.sequence, second.sequence);
    assert!(matches!(
        report.body.input_frontier,
        Some(ProjectionRunInputFrontier {
            kind: ProjectionRunFrontierKind::Visible,
            global_sequence,
            ..
        }) if global_sequence == second.sequence
    ), "PROPERTY: projection run evidence input_frontier must be the replay watermark used by the outcome");
    Ok(())
}

#[test]
fn cache_status_unavailable_emits_explicit_finding() -> TestResult {
    let dir = tempfile::TempDir::new()?;
    let config = StoreConfig::new(dir.path());
    let store = Store::open_with_cache(config, Box::new(CacheGetError))?;
    let coord = Coordinate::new("entity:projection-run-cache-unknown", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x61);
    store.append(&coord, kind, &serde_json::json!({"n": 1}))?;

    let (state, report) = store.project_run_evidence::<CounterProjection>(
        "entity:projection-run-cache-unknown",
        &Freshness::Consistent,
    )?;

    assert_eq!(state, Some(CounterProjection { count: 1 }));
    assert_eq!(
        report.body.cache_status,
        ProjectionRunCacheStatus::Unavailable {
            reason: "cache_get_failed".to_owned(),
        }
    );
    assert!(
        report
            .body
            .findings
            .contains(&ProjectionRunFinding::CacheStatusUnavailable),
        "PROPERTY: unavailable cache status must produce explicit CacheStatusUnavailable finding"
    );
    Ok(())
}

#[test]
fn projection_failure_returns_structured_error_with_deterministic_finding() -> TestResult {
    let (store, dir) = small_store_support::small_segment_store()?;
    let coord = Coordinate::new("entity:projection-run-failure", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x61);
    let receipt = store.append(&coord, kind, &serde_json::json!({"n": 1}))?;

    let path = dir.path().join(batpak::store::segment::segment_filename(
        receipt.disk_pos.segment_id(),
    ));
    std::fs::remove_file(path)?;

    let result = store.project_run_evidence::<CounterProjection>(
        "entity:projection-run-failure",
        &Freshness::Consistent,
    );
    let Err(error) = result else {
        return Err(std::io::Error::other(
            "PROPERTY: projection run should fail after deleting its segment",
        )
        .into());
    };
    if let ProjectionRunReportError::ProjectionFailed { report, .. } = error {
        assert!(
            report
                .body
                .findings
                .contains(&ProjectionRunFinding::ProjectionFailed),
            "PROPERTY: projection failures must surface ProjectionFailed finding in deterministic report body"
        );
    } else {
        return Err(std::io::Error::other(
            "PROPERTY: projection failure must return ProjectionFailed report error",
        )
        .into());
    }
    Ok(())
}

#[test]
fn findings_are_sorted_deterministically() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let (state, report) = store.project_run_evidence::<CounterProjection>(
        "entity:projection-run-empty",
        &Freshness::Consistent,
    )?;
    assert_eq!(state, None);
    let mut sorted = report.body.findings.clone();
    sorted.sort();
    assert_eq!(
        report.body.findings, sorted,
        "PROPERTY: projection run findings must be emitted in deterministic sorted order",
    );

    let source_ref = ProjectionSourceRef::Entity {
        entity: "entity:projection-run-empty".to_owned(),
    };
    assert!(matches!(source_ref, ProjectionSourceRef::Entity { .. }));
    assert_eq!(
        ProjectionRunCheckpointRef::NotApplicable,
        report.body.checkpoint_ref
    );
    assert_eq!(
        report.body.observed_freshness,
        ProjectionRunFreshnessStatus::NotApplicable,
        "PROPERTY: known empty projection has no freshness uncertainty",
    );
    assert!(
        !report
            .body
            .findings
            .contains(&ProjectionRunFinding::ObservedFreshnessUnavailable),
        "PROPERTY: empty/no-input projection must not emit fake freshness uncertainty",
    );
    let report_body: ProjectionRunReportBody = report.body.clone();
    assert_eq!(
        report_body.schema_version,
        PROJECTION_RUN_REPORT_SCHEMA_VERSION
    );
    let report_hash: ProjectionRunHash = report.body_hash;
    assert_ne!(report_hash, [0_u8; 32]);
    let report_envelope: ProjectionRunEvidenceReport = report;
    assert_eq!(
        report_envelope.body.schema_version,
        PROJECTION_RUN_REPORT_SCHEMA_VERSION
    );
    Ok(())
}
