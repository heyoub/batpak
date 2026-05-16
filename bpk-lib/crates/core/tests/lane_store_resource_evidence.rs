//! Store resource evidence report doctrine.
//!
//! PROVES: `StoreResourceEvidenceReport` bodies hash deterministically, exclude
//! envelope metadata from `body_hash`, and align helper constructors with
//! `Store::store_resource_evidence_report`.
//! CATCHES: accidental coupling of raw paths or diagnostics into canonical body
//! identity, and silent drift between free-function and `Store` entrypoints.
//! SEEDED: deterministic / no randomness.

use batpak::prelude::*;
use batpak::store::{
    store_data_dir_identity_hash, store_resource_evidence_report_from_diagnostics,
    store_resource_report_body_from_diagnostics, store_resource_report_body_hash, OpenIndexReport,
    StoreResourceEnvelope, StoreResourceEvidenceReport, StoreResourceFrontierBody,
    StoreResourceHash, StoreResourceReportBody, StoreResourceReportError,
    StoreResourceRestartPolicyShape, STORE_RESOURCE_REPORT_SCHEMA_VERSION,
};
use std::error::Error;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult = Result<(), Box<dyn Error>>;

fn assert_open_index_report_phase_micros_sane(report: &OpenIndexReport) {
    assert!(
        report.elapsed_us > 0,
        "PROPERTY: open_index elapsed_us should be non-zero for exercised paths, got {}",
        report.elapsed_us
    );
    let sum = report
        .phase_plan_build_us
        .saturating_add(report.phase_interner_us)
        .saturating_add(report.phase_restore_index_us)
        .saturating_add(report.phase_hidden_ranges_us);
    assert!(
        sum <= report.elapsed_us,
        "PROPERTY: cold-start phase micros must not exceed total elapsed; sum={sum} elapsed_us={}",
        report.elapsed_us
    );
}

fn assert_stable_resource_shape(left: &StoreResourceReportBody, right: &StoreResourceReportBody) {
    assert_eq!(left.schema_version, right.schema_version);
    assert_eq!(left.data_dir_identity_hash, right.data_dir_identity_hash);
    assert_eq!(left.event_count, right.event_count);
    assert_eq!(left.global_sequence, right.global_sequence);
    assert_eq!(left.visible_sequence, right.visible_sequence);
    assert_eq!(left.segment_max_bytes, right.segment_max_bytes);
    assert_eq!(left.fd_budget, right.fd_budget);
    assert_eq!(left.restart_policy, right.restart_policy);
    assert_eq!(left.index_topology, right.index_topology);
    assert_eq!(left.tile_count, right.tile_count);
}

#[test]
fn store_resource_evidence_family_invariants_and_reopen_stable() -> TestResult {
    let (store, dir) = small_store_support::small_segment_store()?;
    let path = dir.path().to_path_buf();
    let coord = Coordinate::new("entity:lane-store-resource", "scope:lane_sr")?;
    let kind = EventKind::custom(0xE, 0x91);
    store.append(&coord, kind, &serde_json::json!({"n": 0}))?;
    store.append(&coord, kind, &serde_json::json!({"n": 1}))?;

    let before_ct = store.stats().event_count;
    let diag = store.diagnostics();
    let body_direct: StoreResourceReportBody = store_resource_report_body_from_diagnostics(&diag);
    let from_fn: StoreResourceEvidenceReport =
        store_resource_evidence_report_from_diagnostics(&diag)?;
    let body_hash_direct: StoreResourceHash = store_resource_report_body_hash(&body_direct)?;
    assert_eq!(body_hash_direct, from_fn.body_hash);

    let rep1: StoreResourceEvidenceReport = store.store_resource_evidence_report()?;
    let envelope: StoreResourceEnvelope = rep1.clone();
    assert_eq!(envelope.body_hash, rep1.body_hash);

    let diag = store.diagnostics();
    let diag_open = diag
        .open_report
        .as_ref()
        .expect("PROPERTY: diagnostics must carry open_report after open");
    assert_open_index_report_phase_micros_sane(diag_open);
    let body_open = rep1
        .body
        .open_report
        .as_ref()
        .expect("PROPERTY: store resource body must echo open_report when present");
    assert_eq!(body_open, diag_open);
    assert_open_index_report_phase_micros_sane(body_open);

    let _: StoreResourceFrontierBody = rep1.body.frontier;
    let _: StoreResourceRestartPolicyShape = rep1.body.restart_policy;
    let ok_typed: Result<(), StoreResourceReportError> = Ok(());
    assert!(ok_typed.is_ok());
    assert_eq!(
        store.stats().event_count,
        before_ct,
        "store resource evidence must not append"
    );
    assert_eq!(
        rep1.body.schema_version,
        STORE_RESOURCE_REPORT_SCHEMA_VERSION
    );
    assert_ne!(rep1.body.schema_version, 0);
    assert_eq!(
        rep1.body.data_dir_identity_hash,
        store_data_dir_identity_hash(&path)
    );
    let expected_hash = store_resource_report_body_hash(&rep1.body)?;
    assert_eq!(rep1.body_hash, expected_hash);

    let snap_b = store.store_resource_evidence_report()?;
    assert_stable_resource_shape(&snap_b.body, &rep1.body);
    assert_eq!(
        snap_b.body_hash,
        store_resource_report_body_hash(&snap_b.body)?
    );

    let mut noisy = rep1.clone();
    noisy.generated_at_unix_ms = Some(9_001);
    noisy.batpak_version = Some("audit-noise".into());
    noisy.diagnostics.push("not in body".into());
    assert_eq!(noisy.body_hash, rep1.body_hash);
    assert_eq!(noisy.body, rep1.body);

    store.close()?;
    let store2 = Store::open(small_store_support::small_segment_store_config(&path))?;
    let rep2: StoreResourceEvidenceReport = store2.store_resource_evidence_report()?;
    let rep2_again: StoreResourceEvidenceReport = store2.store_resource_evidence_report()?;
    assert_stable_resource_shape(&rep2.body, &rep2_again.body);
    assert_eq!(
        rep2_again.body_hash,
        store_resource_report_body_hash(&rep2_again.body)?
    );
    assert_eq!(
        rep2.body.data_dir_identity_hash,
        store_data_dir_identity_hash(&path)
    );
    assert_eq!(rep2.body.segment_max_bytes, rep1.body.segment_max_bytes);
    assert_eq!(rep2.body.fd_budget, rep1.body.fd_budget);
    assert_eq!(rep2.body.index_topology, rep1.body.index_topology);
    assert_eq!(
        rep2.body.schema_version,
        STORE_RESOURCE_REPORT_SCHEMA_VERSION
    );
    let reopen_hash = store_resource_report_body_hash(&rep2.body)?;
    assert_eq!(rep2.body_hash, reopen_hash);
    store2.close()?;
    Ok(())
}
