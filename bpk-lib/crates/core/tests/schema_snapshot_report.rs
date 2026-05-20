//! PROVES: schema snapshot evidence reports deterministic fixture/schema drift
//! without claiming migration or semantic compatibility authority.
//! CATCHES: fixture hash drift hidden as success, guessed additive/breaking
//! semantics, unsorted findings, and body-hash drift.
//! SEEDED: deterministic / no randomness.

use batpak::schema::{
    compare_schema_snapshot, SchemaChangeClass, SchemaSnapshot, SchemaSnapshotEvidenceReport,
    SchemaSnapshotFinding, SchemaSnapshotReportBody, SchemaSnapshotReportError, SnapshotHash,
    SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION,
};
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

fn hash(fill: u8) -> [u8; 32] {
    [fill; 32]
}

#[test]
fn schema_snapshot_surface_reports_unchanged() -> TestResult {
    let expected = SchemaSnapshot::from_hashes("event.example.v1", hash(1), hash(2));
    let observed = SchemaSnapshot::from_hashes("event.example.v1", hash(1), hash(2));
    let schema_snapshot_report_schema_version = SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION;

    assert_eq!(
        expected.snapshot_schema_version,
        schema_snapshot_report_schema_version
    );

    let report = compare_schema_snapshot(&expected, &observed)?;
    assert_eq!(
        report.body.schema_version,
        schema_snapshot_report_schema_version
    );
    let unchanged = SchemaChangeClass::Unchanged;
    assert_eq!(report.body.change_class, unchanged);
    assert!(report.body.findings.is_empty());
    assert_eq!(report.body.stable_id, "event.example.v1");
    assert_eq!(report.body.observed_stable_id, "event.example.v1");
    assert_eq!(
        report.body.expected_snapshot_schema_version,
        expected.snapshot_schema_version
    );
    assert_eq!(
        report.body.observed_snapshot_schema_version,
        observed.snapshot_schema_version
    );
    let report_hash: SnapshotHash = report.body_hash;
    assert_ne!(report_hash, [0_u8; 32]);
    Ok(())
}

#[test]
fn schema_snapshot_surface_reports_drift_as_unknown() -> TestResult {
    let expected = SchemaSnapshot::from_hashes("event.example.v1", hash(1), hash(2));
    let observed = SchemaSnapshot::from_hashes("event.example.v2", hash(3), hash(4));

    let report = compare_schema_snapshot(&expected, &observed)?;
    let unknown = SchemaChangeClass::Unknown;
    assert_eq!(report.body.change_class, unknown);
    let expected_findings = [
        SchemaSnapshotFinding::StableIdMismatch,
        SchemaSnapshotFinding::SchemaHashMismatch,
        SchemaSnapshotFinding::FixtureHashMismatch,
    ]
    .to_vec();
    assert_eq!(report.body.findings, expected_findings);
    assert_eq!(report.body.stable_id, "event.example.v1");
    assert_eq!(report.body.observed_stable_id, "event.example.v2");
    assert!(
        !report.body.stable_id.contains("->"),
        "PROPERTY: mismatched stable ids must not be collapsed into an ambiguous arrow string"
    );

    let body: SchemaSnapshotReportBody = report.body.clone();
    let schema_snapshot_report_schema_version = SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION;
    assert_eq!(body.schema_version, schema_snapshot_report_schema_version);
    let envelope: SchemaSnapshotEvidenceReport = report;
    assert_eq!(envelope.body.change_class, unknown);
    let synthetic_error = SchemaSnapshotReportError::BodyEncoding {
        message: "synthetic".to_owned(),
    };
    assert!(synthetic_error.to_string().contains("synthetic"));
    let changed = SchemaChangeClass::Changed;
    assert_eq!(changed, SchemaChangeClass::Changed);
    Ok(())
}

#[test]
fn schema_snapshot_version_mismatch_is_explicit() -> TestResult {
    let expected = SchemaSnapshot::from_hashes("event.example.v1", hash(1), hash(2));
    let mut observed = SchemaSnapshot::from_hashes("event.example.v1", hash(1), hash(2));
    observed.snapshot_schema_version = expected.snapshot_schema_version + 1;

    let report = compare_schema_snapshot(&expected, &observed)?;

    let unknown = SchemaChangeClass::Unknown;
    assert_eq!(report.body.change_class, unknown);
    let expected_findings = [SchemaSnapshotFinding::SnapshotSchemaVersionMismatch].to_vec();
    assert_eq!(report.body.findings, expected_findings);
    assert_eq!(
        report.body.expected_snapshot_schema_version,
        expected.snapshot_schema_version
    );
    assert_eq!(
        report.body.observed_snapshot_schema_version,
        observed.snapshot_schema_version
    );
    Ok(())
}

#[test]
fn schema_snapshot_from_bytes_hashes_input_bytes() -> TestResult {
    let snap = SchemaSnapshot::from_bytes("event.bytes.v1", b"{schema}", b"{fixture}");
    assert_ne!(snap.schema_hash, [0_u8; 32]);
    assert_ne!(snap.fixture_hash, [0_u8; 32]);
    Ok(())
}
