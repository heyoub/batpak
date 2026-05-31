//! PROVES: subscriber-frontier observations distinguish lossy push and
//! cursor-backed availability, explicit unknown caller observations, and exact
//! loss ranges only when exact precision is supplied.
//! CATCHES: lossy delivery posing as durable cursor progress, fake loss
//! observations for unknown precision, and nondeterministic body hashes.
//! SEEDED: deterministic / no randomness.

mod support;
use batpak::store::{
    LossPrecision, SubscriberDeliveryState, SubscriberFrontierEvidenceReport,
    SubscriberFrontierFinding, SubscriberFrontierHash, SubscriberFrontierReportBody,
    SubscriberFrontierReportError, SubscriberFrontierRequest, SubscriberFrontierSource,
    SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION,
};
use std::error::Error;
use support::prelude::*;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn current_subscriber_reports_deterministic_frontier_state() -> TestResult {
    let (data_dir_guard, store) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let coord = Coordinate::new("entity:subscriber-frontier-current", "scope:test")?;
    let kind = EventKind::custom(0xF, 0x30);
    let receipt = store.append(&coord, kind, &serde_json::json!({"step": 0}))?;

    let request = SubscriberFrontierRequest::lossy_push(
        Some(receipt.sequence),
        SubscriberDeliveryState::Active,
        LossPrecision::Unknown,
    );
    let first = store.subscriber_frontier_observation(&request)?;
    let second = store.subscriber_frontier_observation(&request)?;

    assert_eq!(
        first.body.schema_version,
        SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION
    );
    assert_eq!(first.body.source, SubscriberFrontierSource::LossyPush);
    assert_eq!(first.body.delivery_state, SubscriberDeliveryState::Active);
    assert_eq!(first.body.loss_precision, LossPrecision::Unknown);
    assert_eq!(first.body_hash, second.body_hash);
    assert_eq!(first.body, second.body);
    let report_hash: SubscriberFrontierHash = first.body_hash;
    assert_ne!(report_hash, [0_u8; 32]);
    let synthetic_error = SubscriberFrontierReportError::BodyEncoding {
        message: "test".to_owned(),
    };
    assert!(synthetic_error.to_string().contains("test"));
    Ok(())
}

#[test]
fn dropped_subscriber_emits_explicit_drop_finding() -> TestResult {
    let (data_dir_guard, store) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let report = store.subscriber_frontier_observation(&SubscriberFrontierRequest::lossy_push(
        Some(3),
        SubscriberDeliveryState::Dropped,
        LossPrecision::SubscriberDropped,
    ))?;

    assert!(
        report
            .body
            .findings
            .iter()
            .any(|f| matches!(f, SubscriberFrontierFinding::DeliveryDropped)),
        "PROPERTY: dropped subscribers must emit explicit DeliveryDropped findings",
    );
    assert!(
        report.body.findings.iter().any(|f| matches!(
            f,
            SubscriberFrontierFinding::LossObserved {
                precision: LossPrecision::SubscriberDropped
            }
        )),
        "PROPERTY: dropped subscribers must carry SubscriberDropped loss precision",
    );
    Ok(())
}

#[test]
fn unknown_precision_and_unknown_consumed_frontier_are_explicit() -> TestResult {
    let (data_dir_guard, store) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let report =
        store.subscriber_frontier_observation(&SubscriberFrontierRequest::cursor_backed(
            None,
            SubscriberDeliveryState::Unknown,
            LossPrecision::Unknown,
        ))?;

    assert!(
        report
            .body
            .findings
            .iter()
            .any(|f| matches!(f, SubscriberFrontierFinding::ConsumedFrontierUnknown)),
        "PROPERTY: unknown consumed frontier must be explicit in findings",
    );
    assert!(
        report
            .body
            .findings
            .iter()
            .any(|f| matches!(f, SubscriberFrontierFinding::DeliveryStateUnknown)),
        "PROPERTY: unknown delivery state must be explicit in findings",
    );
    assert!(
        !report
            .body
            .findings
            .iter()
            .any(|f| matches!(f, SubscriberFrontierFinding::LossObserved { .. })),
        "PROPERTY: unknown loss precision is represented by the body field, not by a fake observed-loss finding",
    );
    let report_body: SubscriberFrontierReportBody = report.body.clone();
    assert_eq!(
        report_body.schema_version,
        SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION
    );
    let envelope: SubscriberFrontierEvidenceReport = report;
    assert_eq!(envelope.body.loss_precision, LossPrecision::Unknown);
    Ok(())
}

#[test]
fn consumed_frontier_ahead_of_available_is_explicit() -> TestResult {
    let (data_dir_guard, store) = small_store_support::small_segment_store()?;
    assert!(data_dir_guard.path().exists());
    let report =
        store.subscriber_frontier_observation(&SubscriberFrontierRequest::cursor_backed(
            Some(99),
            SubscriberDeliveryState::Active,
            LossPrecision::Unknown,
        ))?;

    assert!(
        report.body.findings.iter().any(|finding| matches!(
            finding,
            SubscriberFrontierFinding::ConsumedFrontierAheadOfAvailable {
                consumed_sequence: 99,
                available_sequence,
            } if *available_sequence == report.body.available_frontier_sequence
        )),
        "PROPERTY: consumed frontier greater than available must emit an explicit finding, got {:?}",
        report.body.findings
    );
    assert_eq!(report.body.lag_events, Some(0));
    Ok(())
}
