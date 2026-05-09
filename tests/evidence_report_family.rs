//! Family-wide invariants for the batpak deterministic evidence-report layer.
//!
//! PROVES: evidence report bodies have deterministic canonical identity,
//! metadata-independent body hashes, sorted findings, topology-independent
//! read/projection identity, and domain-neutral public type names.
//! CATCHES: report identity drift, topology-specific evidence hashes, metadata
//! leaking into deterministic body identity, unsorted findings, and public API
//! vocabulary leaks.
//! SEEDED: deterministic / no randomness.

use batpak::prelude::*;
use batpak::schema::{
    compare_schema_snapshot, SchemaSnapshot, SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION,
};
use batpak::store::{
    ChainWalkMode, ChainWalkRequest, ChainWalkStartRef, IndexTopology, LossPrecision,
    ReadWalkRequest, SubscriberDeliveryState, SubscriberFrontierRequest, SubscriberFrontierSource,
    CHAIN_WALK_REPORT_SCHEMA_VERSION, READ_WALK_REPORT_SCHEMA_VERSION,
    SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION,
};
use serde::Serialize;
use std::error::Error;
use std::sync::Arc;
use tempfile::TempDir;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult = Result<(), Box<dyn Error>>;

fn body_hash_via_canonical(body: &(impl Serialize + Sized)) -> Result<[u8; 32], Box<dyn Error>> {
    let bytes = batpak::canonical::to_bytes(body)?;
    Ok(batpak::event::hash::compute_hash(&bytes))
}

fn sorted_eq<T: Ord + Clone + core::fmt::Debug>(slice: &[T]) {
    let mut s = slice.to_vec();
    s.sort();
    assert_eq!(
        slice,
        s.as_slice(),
        "PROPERTY: evidence findings must be in sorted structural order"
    );
}

/// Public evidence-related type names passed to subscribers must remain domain-neutral.
#[test]
fn evidence_public_types_avoid_protocol_product_vocabulary() -> TestResult {
    let blob = concat!(
        stringify!(ChainWalkEvidenceReport),
        stringify!(SubscriberFrontierEvidenceReport),
        stringify!(ProjectionRunEvidenceReport),
        stringify!(ReadWalkEvidenceReport),
        stringify!(SchemaSnapshotEvidenceReport),
        stringify!(StoreResourceEvidenceReport),
        stringify!(StoreResourceEnvelope),
    );
    let lower = blob.to_ascii_lowercase();
    const FORBIDDEN: &[&str] = &[
        "capability",
        "criticality",
        "budget",
        "portkind",
        "mcp",
        "a2a",
        "extprofile",
        "capsule",
        "websocket",
        "sandbox",
        "deployment",
    ];
    for word in FORBIDDEN {
        assert!(
            !lower.contains(word),
            "evidence-related public identifiers must remain domain-neutral: found `{word}` in `{blob}`"
        );
    }
    Ok(())
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct FamilyProjection {
    count: u64,
}

impl EventSourced for FamilyProjection {
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
        static KINDS: [EventKind; 1] = [EventKind::custom(0xE, 0x71)];
        &KINDS
    }
}

#[test]
fn schema_snapshot_evidence_family_invariants() -> TestResult {
    let expected = SchemaSnapshot::from_hashes(stable_id_fixture(), [1_u8; 32], [2_u8; 32]);
    let observed = SchemaSnapshot::from_hashes(stable_id_fixture(), [1_u8; 32], [2_u8; 32]);
    let report = compare_schema_snapshot(&expected, &observed)?;

    assert_ne!(report.body.schema_version, 0);
    assert_eq!(
        report.body.schema_version,
        SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION
    );
    sorted_eq(&report.body.findings);
    let expected_hash = body_hash_via_canonical(&report.body)?;
    assert_eq!(
        report.body_hash, expected_hash,
        "PROPERTY: body_hash equals hash(canonical(body))"
    );

    let mut with_meta = report.clone();
    with_meta.generated_at_unix_ms = Some(u64::MAX);
    with_meta.batpak_version = Some("bogus-test-version".into());
    with_meta.diagnostics = vec!["noise".into()];
    assert_eq!(with_meta.body, report.body);
    assert_eq!(with_meta.body_hash, report.body_hash);
    Ok(())
}

#[test]
fn chain_walk_evidence_family_invariants_and_no_append_side_effect() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:family-chain", "scope:fam")?;
    let kind = EventKind::custom(0xF, 0x20);
    let before = store.stats().event_count;

    let first_event_id = store
        .append(&coord, kind, &serde_json::json!({"s": 0}))?
        .event_id;
    let second_event_id = store
        .append(&coord, kind, &serde_json::json!({"s": 1}))?
        .event_id;
    let last = store
        .append(&coord, kind, &serde_json::json!({"s": 2}))?
        .event_id;

    let request = ChainWalkRequest::linear(ChainWalkStartRef::EventId(last), 32);
    let report = store.chain_walk_evidence(&request)?;

    assert_ne!(first_event_id, second_event_id);
    assert_ne!(second_event_id, last);
    assert_eq!(
        store.stats().event_count,
        before + 3,
        "chain walk must not append"
    );
    assert_eq!(report.body.schema_version, CHAIN_WALK_REPORT_SCHEMA_VERSION);
    assert_ne!(report.body.schema_version, 0);
    assert_eq!(report.body.mode, ChainWalkMode::Linear);
    sorted_eq(&report.body.findings);
    let expected_hash = body_hash_via_canonical(&report.body)?;
    assert_eq!(report.body_hash, expected_hash);

    let mut noisy = report.clone();
    noisy.generated_at_unix_ms = Some(123);
    assert_eq!(noisy.body_hash, report.body_hash);
    Ok(())
}

#[test]
fn chain_walk_three_link_chain_checked_count_stable_across_close_reopen() -> TestResult {
    let (store, dir) = small_store_support::small_segment_store()?;
    let path = dir.path().to_path_buf();
    let coord = Coordinate::new("entity:family-chain-reopen", "scope:fam")?;
    let kind = EventKind::custom(0xF, 0x21);

    let mut last = 0_u128;
    for s in 0..3 {
        last = store
            .append(&coord, kind, &serde_json::json!({"s": s}))?
            .event_id;
    }

    let request = ChainWalkRequest::linear(ChainWalkStartRef::EventId(last), 64);
    let first = store.chain_walk_evidence(&request)?;
    assert_eq!(
        first.body.checked_count, 3,
        "PROPERTY: walk from leaf of 3-append chain checks 3 entries"
    );
    store.close()?;

    let store2 = Store::open(small_store_support::small_segment_store_config(&path))?;
    let second = store2.chain_walk_evidence(&request)?;

    assert_eq!(first.body, second.body);
    assert_eq!(first.body_hash, second.body_hash);
    store2.close()?;
    Ok(())
}

#[test]
fn subscriber_frontier_exact_range_only_when_precision_demands_it() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let before = store.stats().event_count;
    let request = SubscriberFrontierRequest {
        source: SubscriberFrontierSource::LossyPush,
        consumed_frontier_sequence: Some(0),
        delivery_state: SubscriberDeliveryState::Active,
        loss_precision: LossPrecision::ExactRange,
        exact_dropped_ranges: vec![(1, 5), (10, 20)],
    };

    let report = store.subscriber_frontier_observation(&request)?;
    assert_eq!(
        store.stats().event_count,
        before,
        "frontier observation must not append"
    );
    sorted_eq(&report.body.findings);
    assert_ne!(report.body.schema_version, 0);
    assert_eq!(
        report.body.schema_version,
        SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION
    );
    assert!(report.body.findings.iter().any(|f| matches!(
        f,
        batpak::store::SubscriberFrontierFinding::ExactDroppedRange {
            start_sequence: 1,
            end_sequence: 5,
        },
    )),);
    let expected_hash = body_hash_via_canonical(&report.body)?;
    assert_eq!(report.body_hash, expected_hash);

    let mut m = report.clone();
    m.diagnostics.push("diag".into());
    assert_eq!(m.body_hash, report.body_hash);
    Ok(())
}

#[test]
fn projection_run_body_hash_changes_after_relevant_append() -> TestResult {
    let (store, dir) = small_store_support::small_segment_store()?;
    let path = dir.path().to_path_buf();
    let coord = Coordinate::new("entity:family-projection-reopen", "scope:fam")?;
    let kind = EventKind::custom(0xE, 0x71);

    store.append(&coord, kind, &serde_json::json!({"n": 0}))?;
    let r1 =
        store.project_run_evidence::<FamilyProjection>(coord.entity(), &Freshness::Consistent)?;

    store.append(&coord, kind, &serde_json::json!({"n": 1}))?;
    let r2 =
        store.project_run_evidence::<FamilyProjection>(coord.entity(), &Freshness::Consistent)?;

    assert_ne!(
        r1.1.body_hash, r2.1.body_hash,
        "PROPERTY: new relevant append must change projection run evidence identity"
    );
    assert_ne!(r1.1.body, r2.1.body);
    sorted_eq(&r1.1.body.findings);
    sorted_eq(&r2.1.body.findings);

    store.close()?;

    let store_ro = Store::open(small_store_support::small_segment_store_config(&path))?;
    let r_after = store_ro
        .project_run_evidence::<FamilyProjection>(coord.entity(), &Freshness::Consistent)?;

    assert_eq!(r_after.0, r2.0);
    assert_eq!(r_after.1.body.projection_id, r2.1.body.projection_id);
    assert_eq!(r_after.1.body.source_refs, r2.1.body.source_refs);
    assert_eq!(r_after.1.body.replay_mode, r2.1.body.replay_mode);
    assert_eq!(
        r_after.1.body.requested_freshness,
        r2.1.body.requested_freshness
    );
    assert_eq!(
        r_after.1.body.observed_freshness,
        r2.1.body.observed_freshness
    );
    assert_eq!(r_after.1.body.output_hash, r2.1.body.output_hash);
    assert_eq!(r_after.1.body.cache_status, r2.1.body.cache_status);
    assert_eq!(r_after.1.body.checkpoint_ref, r2.1.body.checkpoint_ref);
    assert_eq!(r_after.1.body.findings, r2.1.body.findings);
    let expected_reopen_hash = body_hash_via_canonical(&r_after.1.body)?;
    assert_eq!(r_after.1.body_hash, expected_reopen_hash);
    store_ro.close()?;
    Ok(())
}

#[test]
fn read_walk_evidence_matches_across_close_reopen() -> TestResult {
    let (store, dir) = small_store_support::small_segment_store()?;
    let path = dir.path().to_path_buf();
    let coord = Coordinate::new("entity:family-readwalk", "scope:fam_rw")?;
    let kind = EventKind::custom(0xE, 0x81);
    store.append(&coord, kind, &serde_json::json!({"i": 0}))?;

    let mut req = ReadWalkRequest::full(
        Region::scope(coord.scope()).with_fact(batpak::coordinate::KindFilter::Exact(kind)),
    );
    req.include_proof_refs = true;

    let before_ct = store.stats().event_count;
    let (_, rep1) = store.query_with_read_walk_evidence(&req)?;
    assert_eq!(
        store.stats().event_count,
        before_ct,
        "read walk must not append",
    );

    sorted_eq(&rep1.body.findings);
    assert_eq!(rep1.body.schema_version, READ_WALK_REPORT_SCHEMA_VERSION);

    store.close()?;

    let store2 = Store::open(small_store_support::small_segment_store_config(&path))?;
    let (_, rep2) = store2.query_with_read_walk_evidence(&req)?;

    assert_eq!(rep1.body.source_refs, rep2.body.source_refs);
    assert_eq!(rep1.body.replay_mode, rep2.body.replay_mode);
    assert_eq!(rep1.body.freshness_intent, rep2.body.freshness_intent);
    assert_eq!(rep1.body.requested_limit, rep2.body.requested_limit);
    assert_eq!(rep1.body.matched_count, rep2.body.matched_count);
    assert_eq!(rep1.body.returned_count, rep2.body.returned_count);
    assert_eq!(
        rep1.body.dropped_limited_count,
        rep2.body.dropped_limited_count
    );
    assert_eq!(rep1.body.proof_refs, rep2.body.proof_refs);
    assert_eq!(rep1.body.findings, rep2.body.findings);
    let expected_reopen_hash = body_hash_via_canonical(&rep2.body)?;
    assert_eq!(rep2.body_hash, expected_reopen_hash);
    store2.close()?;
    Ok(())
}

#[test]
fn read_walk_evidence_body_is_topology_independent() -> TestResult {
    let mut baseline = None;
    for (label, topology) in topology_cases() {
        let dir = TempDir::new()?;
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_index_topology(topology)
                .with_clock(Some(Arc::new(|| 1_000_000))),
        )?;
        let coord = Coordinate::new("entity:family-topology-read", "scope:topology-read")?;
        let kind = EventKind::custom(0xE, 0x81);
        for i in 0..5 {
            store.append(&coord, kind, &serde_json::json!({ "i": i }))?;
        }

        let request = ReadWalkRequest::full(
            Region::scope(coord.scope()).with_fact(batpak::coordinate::KindFilter::Exact(kind)),
        );
        let (_, report) = store.query_with_read_walk_evidence(&request)?;
        let expected_hash = body_hash_via_canonical(&report.body)?;
        assert_eq!(report.body_hash, expected_hash);

        if let Some((baseline_label, baseline_body, baseline_hash)) = &baseline {
            assert_eq!(
                report.body, *baseline_body,
                "PROPERTY: read-walk evidence body must not depend on index topology; baseline={baseline_label}, candidate={label}",
            );
            assert_eq!(
                report.body_hash, *baseline_hash,
                "PROPERTY: read-walk evidence hash must not depend on index topology; baseline={baseline_label}, candidate={label}",
            );
        } else {
            baseline = Some((label, report.body.clone(), report.body_hash));
        }
        store.close()?;
    }
    Ok(())
}

#[test]
fn projection_run_evidence_output_is_topology_independent() -> TestResult {
    let mut baseline = None;
    for (label, topology) in topology_cases() {
        let dir = TempDir::new()?;
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_index_topology(topology)
                .with_clock(Some(Arc::new(|| 2_000_000))),
        )?;
        let coord = Coordinate::new("entity:family-topology-projection", "scope:topology-proj")?;
        let relevant = EventKind::custom(0xE, 0x71);
        let irrelevant = EventKind::custom(0xE, 0x72);
        for i in 0..4 {
            store.append(&coord, relevant, &serde_json::json!({ "i": i }))?;
        }
        store.append(&coord, irrelevant, &serde_json::json!({ "ignored": true }))?;

        let (state, report) = store
            .project_run_evidence::<FamilyProjection>(coord.entity(), &Freshness::Consistent)?;
        assert_eq!(state, Some(FamilyProjection { count: 4 }));
        let expected_hash = body_hash_via_canonical(&report.body)?;
        assert_eq!(report.body_hash, expected_hash);

        if let Some((baseline_label, baseline_body, baseline_hash)) = &baseline {
            assert_eq!(
                report.body, *baseline_body,
                "PROPERTY: projection-run evidence body must not depend on index topology; baseline={baseline_label}, candidate={label}",
            );
            assert_eq!(
                report.body_hash, *baseline_hash,
                "PROPERTY: projection-run evidence hash must not depend on index topology; baseline={baseline_label}, candidate={label}",
            );
        } else {
            baseline = Some((label, report.body.clone(), report.body_hash));
        }
        store.close()?;
    }
    Ok(())
}

fn stable_id_fixture() -> &'static str {
    "batpak.family.schema_snapshot_fixture.v1"
}

fn topology_cases() -> [(&'static str, IndexTopology); 6] {
    [
        ("aos", IndexTopology::aos()),
        ("scan", IndexTopology::scan()),
        ("entity-local", IndexTopology::entity_local()),
        ("tiled", IndexTopology::tiled()),
        ("tiled-simd", IndexTopology::tiled_simd()),
        ("all", IndexTopology::all()),
    ]
}
