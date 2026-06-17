//! Deterministic evidence report for a store snapshot operation.

use crate::evidence::{content_hash, sort_findings};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Report body schema version for snapshot evidence.
pub const SNAPSHOT_EVIDENCE_REPORT_SCHEMA_VERSION: u16 = 1;

/// Hash alias for snapshot evidence report bodies.
pub type SnapshotEvidenceHash = [u8; 32];

/// Private visibility-fence token that covered the snapshot copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SnapshotFenceTokenRef {
    /// Writer/index visibility-fence token.
    pub token: u64,
}

/// Source segment watermark observed for the snapshot source directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SnapshotWatermarkRef {
    /// Highest source segment id observed.
    pub segment_id: u64,
    /// Byte offset at the tail of `segment_id`.
    pub offset: u64,
}

/// Snapshot artifact kind referenced by structural findings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SnapshotFileKind {
    /// Segment file (`*.fbat`).
    Segment,
    /// `visibility_ranges.fbv`.
    VisibilityRanges,
    /// Durable idempotency store (`index.idemp`).
    IdempotencyStore,
    /// Pending compaction marker.
    PendingCompactionMarker,
}

/// Deterministic structural findings for a snapshot attempt.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SnapshotFinding {
    /// Existing snapshot artifacts were cleared before copying.
    DestinationCleared {
        /// Number of destination artifacts removed.
        artifact_count: usize,
    },
    /// A byte hash was intentionally unavailable for this artifact class.
    CopyByteHashUnavailable {
        /// Stable reason string.
        reason: String,
        /// Artifact kind whose bytes were not hashed into the report.
        file_kind: SnapshotFileKind,
    },
    /// The private visibility fence was explicitly cancelled after copying.
    FenceTokenCancelled,
}

#[derive(Serialize)]
struct SnapshotStructuralFingerprint {
    schema_version: u16,
    fence_token: SnapshotFenceTokenRef,
    source_watermark: SnapshotWatermarkRef,
    copied_segment_ids_sorted: Vec<u64>,
    copied_visibility_ranges_present: bool,
    copied_pending_compaction_marker_present: bool,
    destination_path_digest: SnapshotEvidenceHash,
}

fn snapshot_id_digest(
    fp: &SnapshotStructuralFingerprint,
) -> Result<SnapshotEvidenceHash, rmp_serde::encode::Error> {
    let bytes = crate::encoding::to_bytes(fp)?;
    Ok(content_hash(&bytes))
}

/// Deterministic report body for one snapshot copy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotReportBody {
    /// Report-body schema version.
    pub schema_version: u16,
    /// Stable digest over the structural snapshot core.
    pub snapshot_id: SnapshotEvidenceHash,
    /// Private visibility-fence token covering the copied segment set.
    pub fence_token: SnapshotFenceTokenRef,
    /// Source segment watermark after draining the writer.
    pub source_watermark: SnapshotWatermarkRef,
    /// Copied segment ids sorted ascending.
    pub copied_segment_ids_sorted: Vec<u64>,
    /// Whether `visibility_ranges.fbv` was copied.
    pub copied_visibility_ranges_present: bool,
    /// Whether the pending-compaction marker was copied.
    pub copied_pending_compaction_marker_present: bool,
    /// Digest of the destination path bytes.
    pub destination_path_digest: SnapshotEvidenceHash,
    /// Structural findings sorted before `body_hash`.
    pub findings: Vec<SnapshotFinding>,
}

impl SnapshotReportBody {
    /// Full report-body digest, with findings sorted before encoding.
    ///
    /// # Errors
    /// MessagePack encoding failure from `rmp-serde`.
    pub fn body_hash(&self) -> Result<SnapshotEvidenceHash, rmp_serde::encode::Error> {
        snapshot_report_body_hash(self)
    }
}

/// Snapshot evidence report envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEvidenceReport {
    /// Deterministic report body.
    pub body: SnapshotReportBody,
    /// Canonical hash of `body`.
    pub body_hash: SnapshotEvidenceHash,
    /// Optional generation timestamp metadata outside deterministic identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside deterministic identity.
    pub batpak_version: Option<String>,
    /// Optional diagnostics outside deterministic identity.
    pub diagnostics: Vec<String>,
}

/// Canonical `body_hash` over a snapshot report body.
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde`.
pub fn snapshot_report_body_hash(
    body: &SnapshotReportBody,
) -> Result<SnapshotEvidenceHash, rmp_serde::encode::Error> {
    let mut body = body.clone();
    sort_findings(&mut body.findings);
    let bytes = crate::encoding::to_bytes(&body)?;
    Ok(content_hash(&bytes))
}

pub(crate) fn destination_path_digest(dest: &Path) -> SnapshotEvidenceHash {
    content_hash(dest.as_os_str().as_encoded_bytes())
}

pub(crate) struct SnapshotReportInput {
    pub(crate) fence_token: u64,
    pub(crate) source_watermark_segment_id: u64,
    pub(crate) source_watermark_offset: u64,
    pub(crate) copied_segment_ids_sorted: Vec<u64>,
    pub(crate) copied_visibility_ranges_present: bool,
    pub(crate) copied_pending_compaction_marker_present: bool,
    pub(crate) destination_path_digest: SnapshotEvidenceHash,
    pub(crate) findings: Vec<SnapshotFinding>,
}

pub(crate) fn snapshot_evidence_report(
    input: SnapshotReportInput,
) -> Result<SnapshotEvidenceReport, rmp_serde::encode::Error> {
    let fence_token = SnapshotFenceTokenRef {
        token: input.fence_token,
    };
    let source_watermark = SnapshotWatermarkRef {
        segment_id: input.source_watermark_segment_id,
        offset: input.source_watermark_offset,
    };
    let mut copied_segment_ids_sorted = input.copied_segment_ids_sorted;
    copied_segment_ids_sorted.sort_unstable();
    let mut findings = input.findings;
    sort_findings(&mut findings);

    let fp = SnapshotStructuralFingerprint {
        schema_version: SNAPSHOT_EVIDENCE_REPORT_SCHEMA_VERSION,
        fence_token,
        source_watermark,
        copied_segment_ids_sorted: copied_segment_ids_sorted.clone(),
        copied_visibility_ranges_present: input.copied_visibility_ranges_present,
        copied_pending_compaction_marker_present: input.copied_pending_compaction_marker_present,
        destination_path_digest: input.destination_path_digest,
    };
    let snapshot_id = snapshot_id_digest(&fp)?;
    let body = SnapshotReportBody {
        schema_version: SNAPSHOT_EVIDENCE_REPORT_SCHEMA_VERSION,
        snapshot_id,
        fence_token,
        source_watermark,
        copied_segment_ids_sorted,
        copied_visibility_ranges_present: input.copied_visibility_ranges_present,
        copied_pending_compaction_marker_present: input.copied_pending_compaction_marker_present,
        destination_path_digest: input.destination_path_digest,
        findings,
    };
    let body_hash = snapshot_report_body_hash(&body)?;
    Ok(SnapshotEvidenceReport {
        body,
        body_hash,
        generated_at_unix_ms: None,
        batpak_version: None,
        diagnostics: Vec::new(),
    })
}
