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
    store_resource_report_body_from_diagnostics, store_resource_report_body_hash,
    StoreResourceEnvelope, StoreResourceEvidenceReport, StoreResourceFrontierBody,
    StoreResourceHash, StoreResourceReportBody, StoreResourceReportError,
    StoreResourceRestartPolicyShape, STORE_RESOURCE_REPORT_SCHEMA_VERSION,
};
use serde::Serialize;
use std::error::Error;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult = Result<(), Box<dyn Error>>;

fn body_hash_via_canonical(body: &(impl Serialize + Sized)) -> Result<[u8; 32], Box<dyn Error>> {
    let bytes = batpak::canonical::to_bytes(body)?;
    Ok(batpak::event::hash::compute_hash(&bytes))
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
    let expected_hash = body_hash_via_canonical(&rep1.body)?;
    assert_eq!(rep1.body_hash, expected_hash);

    let snap_b = store.store_resource_evidence_report()?;
    assert_eq!(snap_b.body, rep1.body);
    assert_eq!(snap_b.body_hash, rep1.body_hash);

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
    assert_eq!(rep2.body, rep2_again.body);
    assert_eq!(rep2.body_hash, rep2_again.body_hash);
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
    let reopen_hash = body_hash_via_canonical(&rep2.body)?;
    assert_eq!(rep2.body_hash, reopen_hash);
    store2.close()?;
    Ok(())
}
