//! Deterministic Batpak Substrate Closure schema/fixture snapshot drift evidence.
//!
//! This module provides a small, generic report surface for comparing expected
//! and observed schema snapshots. It reports structural drift facts; it does
//! not classify application-level semantics.

use serde::{Deserialize, Serialize};

/// Current report-body schema version for schema snapshot evidence.
pub const SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION: u16 = 1;

/// Fixed-width hash type used by schema snapshot evidence.
pub type SnapshotHash = [u8; 32];

/// Stable expected/observed schema fixture identity snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    /// Stable logical identifier for the compared schema subject.
    pub stable_id: String,
    /// Snapshot schema version used by this record.
    pub snapshot_schema_version: u16,
    /// Canonical hash of the schema bytes.
    pub schema_hash: SnapshotHash,
    /// Canonical hash of the fixture/golden bytes.
    pub fixture_hash: SnapshotHash,
}

impl SchemaSnapshot {
    /// Build a snapshot from precomputed schema/fixture hashes.
    #[must_use]
    pub fn from_hashes(
        stable_id: impl Into<String>,
        schema_hash: SnapshotHash,
        fixture_hash: SnapshotHash,
    ) -> Self {
        Self {
            stable_id: stable_id.into(),
            snapshot_schema_version: SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION,
            schema_hash,
            fixture_hash,
        }
    }

    /// Build a snapshot from schema and fixture bytes.
    #[must_use]
    pub fn from_bytes(
        stable_id: impl Into<String>,
        schema_bytes: &[u8],
        fixture_bytes: &[u8],
    ) -> Self {
        Self::from_hashes(
            stable_id,
            crate::event::hash::compute_hash(schema_bytes),
            crate::event::hash::compute_hash(fixture_bytes),
        )
    }
}

/// Coarse drift class for schema snapshot comparison.
///
/// The comparison surface defaults to `Unknown` when drift exists; callers may
/// layer richer additive/breaking reasoning above this module when they can
/// prove it structurally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaChangeClass {
    /// Expected and observed snapshots are identical.
    Unchanged,
    /// Reserved for future structurally proven classifications that are more
    /// precise than [`SchemaChangeClass::Unknown`].
    Changed,
    /// Drift exists but this module does not classify semantic compatibility.
    Unknown,
}

/// Deterministic structural finding emitted by schema snapshot comparison.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SchemaSnapshotFinding {
    /// `stable_id` differs between expected and observed snapshots.
    StableIdMismatch,
    /// `snapshot_schema_version` differs between expected and observed snapshots.
    SnapshotSchemaVersionMismatch,
    /// `schema_hash` differs between expected and observed snapshots.
    SchemaHashMismatch,
    /// `fixture_hash` differs between expected and observed snapshots.
    FixtureHashMismatch,
}

/// Deterministic report body for schema snapshot drift evidence.
///
/// This body is the canonical, hash-bearing report payload. Operational
/// metadata such as generation time may live outside this body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSnapshotReportBody {
    /// Stable identity of the expected compared subject.
    pub stable_id: String,
    /// Stable identity of the observed compared subject.
    pub observed_stable_id: String,
    /// Report-body schema version.
    pub schema_version: u16,
    /// Snapshot schema version from the expected record.
    pub expected_snapshot_schema_version: u16,
    /// Snapshot schema version from the observed record.
    pub observed_snapshot_schema_version: u16,
    /// Expected schema hash.
    pub expected_schema_hash: SnapshotHash,
    /// Observed schema hash.
    pub observed_schema_hash: SnapshotHash,
    /// Expected fixture hash.
    pub expected_fixture_hash: SnapshotHash,
    /// Observed fixture hash.
    pub observed_fixture_hash: SnapshotHash,
    /// Coarse change class.
    pub change_class: SchemaChangeClass,
    /// Deterministic drift findings.
    pub findings: Vec<SchemaSnapshotFinding>,
}

/// Schema snapshot comparison evidence report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSnapshotEvidenceReport {
    /// Deterministic report body.
    pub body: SchemaSnapshotReportBody,
    /// Canonical hash of `body` bytes.
    pub body_hash: SnapshotHash,
    /// Optional generation timestamp metadata outside deterministic body identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside deterministic body identity.
    pub batpak_version: Option<String>,
    /// Optional non-authority diagnostics outside deterministic body identity.
    pub diagnostics: Vec<String>,
}

/// Structured errors for schema snapshot evidence generation.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaSnapshotReportError {
    /// Canonical report-body encoding failed.
    BodyEncoding {
        /// Human-readable encoding error.
        message: String,
    },
}

impl std::fmt::Display for SchemaSnapshotReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyEncoding { message } => {
                write!(f, "schema snapshot report body encoding failed: {message}")
            }
        }
    }
}

impl std::error::Error for SchemaSnapshotReportError {}

/// Compare expected and observed schema snapshots and build a deterministic
/// evidence report.
///
/// Drift defaults to `SchemaChangeClass::Unknown` unless snapshots are exactly
/// equal.
///
/// # Errors
/// Returns `SchemaSnapshotReportError::BodyEncoding` when canonical encoding of
/// the deterministic report body fails.
pub fn compare_schema_snapshot(
    expected: &SchemaSnapshot,
    observed: &SchemaSnapshot,
) -> Result<SchemaSnapshotEvidenceReport, SchemaSnapshotReportError> {
    let mut findings = Vec::new();
    if expected.stable_id != observed.stable_id {
        findings.push(SchemaSnapshotFinding::StableIdMismatch);
    }
    if expected.snapshot_schema_version != observed.snapshot_schema_version {
        findings.push(SchemaSnapshotFinding::SnapshotSchemaVersionMismatch);
    }
    if expected.schema_hash != observed.schema_hash {
        findings.push(SchemaSnapshotFinding::SchemaHashMismatch);
    }
    if expected.fixture_hash != observed.fixture_hash {
        findings.push(SchemaSnapshotFinding::FixtureHashMismatch);
    }
    crate::evidence::sort_findings(&mut findings);

    let change_class = if findings.is_empty() {
        SchemaChangeClass::Unchanged
    } else {
        SchemaChangeClass::Unknown
    };

    let body = SchemaSnapshotReportBody {
        stable_id: expected.stable_id.clone(),
        observed_stable_id: observed.stable_id.clone(),
        schema_version: SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION,
        expected_snapshot_schema_version: expected.snapshot_schema_version,
        observed_snapshot_schema_version: observed.snapshot_schema_version,
        expected_schema_hash: expected.schema_hash,
        observed_schema_hash: observed.schema_hash,
        expected_fixture_hash: expected.fixture_hash,
        observed_fixture_hash: observed.fixture_hash,
        change_class,
        findings,
    };

    let body_hash = report_body_hash(&body)?;
    Ok(SchemaSnapshotEvidenceReport {
        body,
        body_hash,
        generated_at_unix_ms: None,
        batpak_version: None,
        diagnostics: Vec::new(),
    })
}

fn report_body_hash(
    body: &SchemaSnapshotReportBody,
) -> Result<SnapshotHash, SchemaSnapshotReportError> {
    crate::evidence::report_body_hash(body, |message| SchemaSnapshotReportError::BodyEncoding {
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        compare_schema_snapshot, SchemaChangeClass, SchemaSnapshot, SchemaSnapshotFinding,
    };
    use std::error::Error;

    type TestResult = Result<(), Box<dyn Error>>;

    fn hash(fill: u8) -> [u8; 32] {
        [fill; 32]
    }

    #[test]
    fn unchanged_snapshot_reports_unchanged_and_stable_hash() -> TestResult {
        let expected = SchemaSnapshot::from_hashes("event.user.v1", hash(1), hash(2));
        let observed = SchemaSnapshot::from_hashes("event.user.v1", hash(1), hash(2));

        let first = compare_schema_snapshot(&expected, &observed)?;
        let second = compare_schema_snapshot(&expected, &observed)?;

        assert_eq!(first.body.change_class, SchemaChangeClass::Unchanged);
        assert!(first.body.findings.is_empty());
        assert_eq!(
            first.body_hash, second.body_hash,
            "PROPERTY: deterministic report body hash must remain stable for unchanged snapshots",
        );
        assert_eq!(first.body, second.body);
        Ok(())
    }

    #[test]
    fn changed_fixture_reports_deterministic_hash_mismatch() -> TestResult {
        let expected = SchemaSnapshot::from_hashes("event.user.v1", hash(1), hash(2));
        let observed = SchemaSnapshot::from_hashes("event.user.v1", hash(1), hash(9));

        let report = compare_schema_snapshot(&expected, &observed)?;

        assert_eq!(report.body.change_class, SchemaChangeClass::Unknown);
        assert_eq!(
            report.body.findings,
            vec![SchemaSnapshotFinding::FixtureHashMismatch]
        );
        assert_eq!(report.body.expected_schema_hash, hash(1));
        assert_eq!(report.body.observed_schema_hash, hash(1));
        assert_eq!(report.body.expected_fixture_hash, hash(2));
        assert_eq!(report.body.observed_fixture_hash, hash(9));
        Ok(())
    }

    #[test]
    fn drift_defaults_to_unknown_and_findings_order_is_deterministic() -> TestResult {
        let expected = SchemaSnapshot::from_hashes("event.user.v1", hash(1), hash(2));
        let observed = SchemaSnapshot::from_hashes("event.user.v2", hash(7), hash(9));

        let report = compare_schema_snapshot(&expected, &observed)?;

        assert_eq!(report.body.change_class, SchemaChangeClass::Unknown);
        assert_eq!(
            report.body.findings,
            vec![
                SchemaSnapshotFinding::StableIdMismatch,
                SchemaSnapshotFinding::SchemaHashMismatch,
                SchemaSnapshotFinding::FixtureHashMismatch,
            ],
            "PROPERTY: findings must be emitted in deterministic order",
        );
        assert_eq!(report.body.stable_id, "event.user.v1");
        assert_eq!(report.body.observed_stable_id, "event.user.v2");
        assert!(!report.body.stable_id.contains("->"));
        Ok(())
    }
}
