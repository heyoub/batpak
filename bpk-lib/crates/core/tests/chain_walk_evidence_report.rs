//! PROVES: chain-walk evidence reports deterministic continuity, corruption,
//! missing-link, truncation, and body-hash facts over store chain material.
//! CATCHES: silent truncation, missing parent links reported as success,
//! unchecked hash mismatches, unsorted findings, and body-hash drift.
//! SEEDED: deterministic / no randomness.

use batpak::prelude::*;
use batpak::store::{
    ChainWalkEvidenceReport, ChainWalkFinding, ChainWalkHash, ChainWalkMode, ChainWalkReportBody,
    ChainWalkReportError, ChainWalkRequest, ChainWalkStartRef, CHAIN_WALK_REPORT_SCHEMA_VERSION,
};
use std::error::Error;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult = Result<(), Box<dyn Error>>;

fn hash(fill: u8) -> [u8; 32] {
    [fill; 32]
}

#[test]
fn linear_chain_reports_no_findings_and_deterministic_body_hash() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:chain-evidence-ok", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x10);
    let first_receipt = store.append(&coord, kind, &serde_json::json!({"step": 0}))?;
    let second_receipt = store.append(&coord, kind, &serde_json::json!({"step": 1}))?;
    assert_ne!(first_receipt.event_id, second_receipt.event_id);

    let request = ChainWalkRequest::linear(ChainWalkStartRef::EventId(u128::from(second_receipt.event_id)), 16);
    let first = store.chain_walk_evidence(&request)?;
    let second = store.chain_walk_evidence(&request)?;

    assert_eq!(first.body.schema_version, CHAIN_WALK_REPORT_SCHEMA_VERSION);
    assert_eq!(first.body.mode, ChainWalkMode::Linear);
    assert!(first.body.findings.is_empty());
    let expected_checked_count = if cfg!(feature = "blake3") { 2 } else { 1 };
    assert_eq!(
        first.body.checked_count, expected_checked_count,
        "PROPERTY: no-blake builds can only prove the configured zero-hash chain surface; blake3 builds prove full parent depth"
    );
    assert_eq!(first.body_hash, second.body_hash);
    assert_eq!(first.body.walk_digest, second.body.walk_digest);
    let report_hash: ChainWalkHash = first.body_hash;
    assert_ne!(report_hash, [0_u8; 32]);
    let synthetic_error = ChainWalkReportError::BodyEncoding {
        message: "test".to_owned(),
    };
    assert!(synthetic_error.to_string().contains("test"));
    Ok(())
}

#[test]
fn missing_start_event_reports_deterministic_finding() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let report = store.chain_walk_evidence(&ChainWalkRequest::linear(
        ChainWalkStartRef::EventId(0xDEAD_BEEF),
        8,
    ))?;

    assert_eq!(report.body.checked_count, 0);
    assert_eq!(report.body.first_ref, None);
    assert_eq!(report.body.last_ref, None);
    assert_eq!(
        report.body.findings,
        vec![ChainWalkFinding::MissingStart {
            event_id: 0xDEAD_BEEF
        }]
    );
    Ok(())
}

#[test]
fn limit_truncation_is_reported_not_silent() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:chain-evidence-limit", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x11);

    let mut last = 0_u128;
    for step in 0..5 {
        last = u128::from(
            store
                .append(&coord, kind, &serde_json::json!({ "step": step }))?
                .event_id,
        );
    }

    let report = store.chain_walk_evidence(&ChainWalkRequest::linear(
        ChainWalkStartRef::EventId(last),
        2,
    ))?;

    if cfg!(feature = "blake3") {
        assert_eq!(report.body.checked_count, 2);
        assert!(
            matches!(
                report.body.findings.first(),
                Some(ChainWalkFinding::TruncatedByLimit { limit, .. }) if *limit == 2
            ),
            "PROPERTY: limit-bound linear walk must emit TruncatedByLimit when ancestry continues beyond the checked prefix",
        );
    } else {
        assert_eq!(report.body.checked_count, 1);
        assert!(
            report.body.findings.is_empty(),
            "PROPERTY: no-blake builds must not fake a truncation edge after the configured zero-hash chain surface ends, got {:?}",
            report.body.findings
        );
    }
    Ok(())
}

#[test]
fn zero_limit_is_reported_as_invalid() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:chain-evidence-zero-limit", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x14);
    let receipt = store.append(&coord, kind, &serde_json::json!({"step": 0}))?;

    let report = store.chain_walk_evidence(&ChainWalkRequest::linear(
        ChainWalkStartRef::EventId(u128::from(receipt.event_id)),
        0,
    ))?;

    assert_eq!(report.body.checked_count, 0);
    assert_eq!(
        report.body.findings,
        vec![ChainWalkFinding::InvalidLimit { limit: 0 }],
        "PROPERTY: limit=0 must not produce a clean chain-walk report"
    );
    Ok(())
}

#[test]
fn duplicate_payload_parent_hash_chooses_nearest_prior_and_reports_ambiguity() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:chain-evidence-duplicate-parent", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x15);
    let _first_same = store.append(&coord, kind, &serde_json::json!({"same": true}))?;
    let _middle = store.append(&coord, kind, &serde_json::json!({"middle": true}))?;
    let second_same = store.append(&coord, kind, &serde_json::json!({"same": true}))?;
    let child = store.append(&coord, kind, &serde_json::json!({"child": true}))?;

    let report = store.chain_walk_evidence(&ChainWalkRequest {
        start: ChainWalkStartRef::EventId(u128::from(child.event_id)),
        end_event_id: Some(u128::from(second_same.event_id)),
        limit: 8,
        mode: ChainWalkMode::Linear,
    })?;

    if cfg!(feature = "blake3") {
        assert_eq!(report.body.checked_count, 2);
        assert_eq!(report.body.last_ref, Some(u128::from(second_same.event_id)));
        assert!(
            report.body.findings.iter().any(|finding| matches!(
                finding,
                ChainWalkFinding::ParentHashAmbiguous {
                    child_event_id,
                    selected_parent_event_id,
                    matching_parent_count: 2,
                    ..
                } if *child_event_id == u128::from(child.event_id)
                    && *selected_parent_event_id == u128::from(second_same.event_id)
            )),
            "PROPERTY: duplicate payload parent hashes must select the nearest prior match and report ambiguity, got {:?}",
            report.body.findings
        );
    } else {
        assert_eq!(report.body.checked_count, 1);
        assert_eq!(report.body.last_ref, Some(u128::from(child.event_id)));
        assert!(
            matches!(
                report.body.findings.as_slice(),
                [ChainWalkFinding::EndNotReached {
                    expected_end_event_id
                }] if *expected_end_event_id == u128::from(second_same.event_id)
            ),
            "PROPERTY: no-blake builds must report the explicit end as unreached instead of inventing duplicate-parent ambiguity without a configured content hash surface, got {:?}",
            report.body.findings
        );
    }
    Ok(())
}

#[test]
fn receipt_start_hash_mismatch_is_reported() -> TestResult {
    let (store, data_dir_guard) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:chain-evidence-receipt", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x12);
    let receipt = store.append(&coord, kind, &serde_json::json!({"step": 0}))?;

    let report = store.chain_walk_evidence(&ChainWalkRequest {
        start: ChainWalkStartRef::Receipt {
            event_id: u128::from(receipt.event_id),
            content_hash: hash(9),
        },
        end_event_id: None,
        limit: 4,
        mode: ChainWalkMode::Linear,
    })?;

    assert!(
        matches!(
            report.body.findings.first(),
            Some(ChainWalkFinding::StartHashMismatch {
                event_id,
                expected,
                ..
            }) if *event_id == u128::from(receipt.event_id) && *expected == hash(9)
        ),
        "PROPERTY: receipt-based start checks must report deterministic start hash mismatch when receipt hash does not match stored chain hash",
    );
    let body: ChainWalkReportBody = report.body;
    assert_eq!(body.schema_version, CHAIN_WALK_REPORT_SCHEMA_VERSION);
    let envelope: ChainWalkEvidenceReport = store.chain_walk_evidence(
        &ChainWalkRequest::linear(ChainWalkStartRef::EventId(u128::from(receipt.event_id)), 4),
    )?;
    assert_eq!(envelope.body.mode, ChainWalkMode::Linear);
    Ok(())
}
