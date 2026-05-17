//! Batpak Substrate Closure backup manifest body identity and deterministic **restore proof**
//! reports for store segment byte digests. Composes [`crate::artifact::CanonicalArtifactEnvelope`]
//! for attested manifests without embedding transport or application-specific semantics in field
//! names.
//!
//! Manifest [`BackupManifestBody::segments`] are sorted by [`BackupSegmentRef`] total order before
//! [`backup_manifest_body_hash`] so caller-supplied slice order is immaterial. Envelope-only timestamps
//! and notes do not participate in manifest body identity.

use crate::artifact::{
    artifact_envelope_hash_from_identity, artifact_envelope_identity,
    verify_canonical_artifact_envelope, ArtifactVerificationReport, AttestationRef,
    CanonicalArtifactEnvelope, SignatureEnvelope, SignatureRef,
};
use crate::evidence::{content_hash, sort_findings, sorted_findings};
use serde::{Deserialize, Serialize};

/// Schema version for canonical [`BackupManifestBody`] encoding.
pub const BACKUP_MANIFEST_BODY_SCHEMA_VERSION: u32 = 1;

/// Schema version for canonical [`RestoreProofReportBody`].
pub const RESTORE_PROOF_REPORT_SCHEMA_VERSION: u32 = 1;

/// Content digest for a segment file byte span (store-native width).
pub type SegmentBytesDigest = [u8; 32];

/// One sealed segment identity in a backup manifest (`segment_id` matches store segment numbering).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BackupSegmentRef {
    /// Numeric segment id (store segment file stem).
    pub segment_id: u64,
    /// Digest over the segment bytes included in the backup scope.
    pub bytes_digest: SegmentBytesDigest,
}

/// Canonical backup manifest **body** (hashed by [`backup_manifest_body_hash`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifestBody {
    /// Must equal [`BACKUP_MANIFEST_BODY_SCHEMA_VERSION`] for v1 helpers.
    pub schema_version: u32,
    /// Opaque backup run identity chosen by the caller.
    pub backup_id: SegmentBytesDigest,
    /// Caller-defined manifest layout revision.
    pub layout_revision: u32,
    /// Caller-defined tooling revision slot (opaque integer).
    pub tooling_revision: u32,
    /// Segment refs; normalized by sorting before hashing.
    pub segments: Vec<BackupSegmentRef>,
}

/// Attested manifest envelope (signatures and attestations are outside manifest body identity).
///
/// This is intentionally not a type alias to [`CanonicalArtifactEnvelope`]: backup manifest segment
/// refs must be normalized before body and envelope identity hashing. The methods on this type
/// route through the backup-specific normalized hash helpers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifestEnvelope {
    /// Backup manifest body; segment refs are sorted by normalized hash helpers.
    pub body: BackupManifestBody,
    /// Envelope field-layout version.
    pub envelope_schema_version: u32,
    /// Envelope-only wall clock (outside body identity).
    pub generated_at_wall_ms: Option<u64>,
    /// Envelope-only diagnostics (outside body identity).
    pub diagnostic_note: Option<String>,
    /// Signatures (canonical sort before envelope hashing).
    pub signatures: Vec<SignatureEnvelope>,
    /// Attestations (canonical sort before envelope hashing).
    pub attestations: Vec<AttestationRef>,
}

impl BackupManifestEnvelope {
    /// Convert to the generic artifact envelope used by signature verification after backup
    /// normalization has already selected the body bytes.
    #[must_use]
    fn to_canonical_envelope(&self) -> CanonicalArtifactEnvelope<BackupManifestBody> {
        CanonicalArtifactEnvelope {
            body: self.body.clone(),
            envelope_schema_version: self.envelope_schema_version,
            generated_at_wall_ms: self.generated_at_wall_ms,
            diagnostic_note: self.diagnostic_note.clone(),
            signatures: self.signatures.clone(),
            attestations: self.attestations.clone(),
        }
    }

    /// Normalized backup manifest body digest.
    ///
    /// # Errors
    /// MessagePack encode failure from `rmp-serde`.
    pub fn body_hash(&self) -> Result<SegmentBytesDigest, rmp_serde::encode::Error> {
        backup_manifest_envelope_body_hash(self)
    }

    /// Normalized backup manifest envelope digest.
    ///
    /// # Errors
    /// MessagePack encode failure from `rmp-serde`.
    pub fn envelope_hash(&self) -> Result<SegmentBytesDigest, rmp_serde::encode::Error> {
        backup_manifest_envelope_hash(self)
    }
}

impl From<CanonicalArtifactEnvelope<BackupManifestBody>> for BackupManifestEnvelope {
    fn from(envelope: CanonicalArtifactEnvelope<BackupManifestBody>) -> Self {
        Self {
            body: envelope.body,
            envelope_schema_version: envelope.envelope_schema_version,
            generated_at_wall_ms: envelope.generated_at_wall_ms,
            diagnostic_note: envelope.diagnostic_note,
            signatures: envelope.signatures,
            attestations: envelope.attestations,
        }
    }
}

/// Structural findings for manifest audit and restore proof (sorted before report `body_hash`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BackupEnvelopeFinding {
    /// [`BackupManifestBody::schema_version`] is not supported by these v1 helpers.
    UnsupportedManifestBodySchemaVersion {
        /// Observed manifest body schema version.
        observed: u32,
        /// Supported manifest body schema version.
        expected: u32,
    },
    /// Adjacent duplicate segment rows after canonical sort (identical [`BackupSegmentRef`]).
    DuplicateSegmentRef {
        /// Repeated segment id.
        segment_id: u64,
        /// Digest on the duplicate row.
        bytes_digest: SegmentBytesDigest,
    },
    /// Same `segment_id` with different digests after canonical sort.
    InconsistentSegmentId {
        /// Conflicting segment id.
        segment_id: u64,
        /// First digest encountered for the id.
        first_digest: SegmentBytesDigest,
        /// Conflicting digest on the following row.
        second_digest: SegmentBytesDigest,
    },
    /// Caller-claimed manifest hash disagrees with recomputed canonical body hash.
    ManifestBodyHashMismatch {
        /// Claimed digest.
        claimed: SegmentBytesDigest,
        /// Recomputed digest from [`backup_manifest_body_hash`].
        computed: SegmentBytesDigest,
    },
    /// Expected segment id absent from the observed set at restore time.
    MissingExpectedSegment {
        /// Missing segment id.
        segment_id: u64,
    },
    /// Observed segment id not listed in the manifest.
    UnexpectedObservedSegment {
        /// Extra segment id.
        segment_id: u64,
    },
    /// Segment id present on both sides but digest differs.
    SegmentBytesDigestMismatch {
        /// Segment id with digest disagreement.
        segment_id: u64,
        /// Digest from the manifest body.
        expected: SegmentBytesDigest,
        /// Digest observed at restore time.
        observed: SegmentBytesDigest,
    },
}

/// Verification output for a signed backup manifest envelope plus structural manifest checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifestVerification {
    /// Signature and envelope digest plane from [`verify_canonical_artifact_envelope`].
    pub envelope_plane: ArtifactVerificationReport,
    /// Manifest structural and hash-claim findings (sorted).
    pub findings: Vec<BackupEnvelopeFinding>,
}

/// Deterministic restore proof **body** over a manifest digest and observed segment digests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreProofReportBody {
    /// Must equal [`RESTORE_PROOF_REPORT_SCHEMA_VERSION`] for v1.
    pub schema_version: u32,
    /// Canonical digest of the normalized manifest body being checked against.
    pub manifest_body_hash: SegmentBytesDigest,
    /// Observed segment refs sorted by [`BackupSegmentRef`] order.
    pub observed_segments_sorted: Vec<BackupSegmentRef>,
    /// Findings (sorted before [`restore_proof_report_body_hash`]).
    pub findings: Vec<BackupEnvelopeFinding>,
}

/// Sort segment refs in place (ascending `segment_id`, then `bytes_digest` lexicographic).
pub fn sort_backup_segment_refs(segments: &mut [BackupSegmentRef]) {
    segments.sort();
}

/// Normalize manifest body for canonical digest (sorts `segments`).
#[must_use]
pub fn normalize_backup_manifest_body(body: &BackupManifestBody) -> BackupManifestBody {
    let mut segments = body.segments.clone();
    segments.sort();
    BackupManifestBody {
        segments,
        ..body.clone()
    }
}

/// Normalize a backup manifest envelope by sorting manifest segment refs before envelope hashing
/// or signature verification.
#[must_use]
pub fn normalize_backup_manifest_envelope(
    envelope: &BackupManifestEnvelope,
) -> BackupManifestEnvelope {
    BackupManifestEnvelope {
        body: normalize_backup_manifest_body(&envelope.body),
        envelope_schema_version: envelope.envelope_schema_version,
        generated_at_wall_ms: envelope.generated_at_wall_ms,
        diagnostic_note: envelope.diagnostic_note.clone(),
        signatures: envelope.signatures.clone(),
        attestations: envelope.attestations.clone(),
    }
}

/// Canonical MessagePack bytes for the normalized manifest body.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn backup_manifest_body_bytes(
    body: &BackupManifestBody,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let normalized = normalize_backup_manifest_body(body);
    crate::encoding::to_bytes(&normalized)
}

/// Digest of canonical normalized manifest body bytes.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn backup_manifest_body_hash(
    body: &BackupManifestBody,
) -> Result<SegmentBytesDigest, rmp_serde::encode::Error> {
    let bytes = backup_manifest_body_bytes(body)?;
    Ok(content_hash(&bytes))
}

/// Normalized body digest for a backup manifest envelope.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn backup_manifest_envelope_body_hash(
    envelope: &BackupManifestEnvelope,
) -> Result<SegmentBytesDigest, rmp_serde::encode::Error> {
    backup_manifest_body_hash(&envelope.body)
}

/// Normalized envelope digest for a backup manifest envelope.
///
/// This uses the normalized manifest body hash as the envelope identity anchor while preserving
/// envelope-owned timestamps, diagnostics, signatures, and attestations.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn backup_manifest_envelope_hash(
    envelope: &BackupManifestEnvelope,
) -> Result<SegmentBytesDigest, rmp_serde::encode::Error> {
    let normalized = normalize_backup_manifest_envelope(envelope);
    let body_hash = backup_manifest_body_hash(&normalized.body)?;
    let artifact_envelope = normalized.to_canonical_envelope();
    let identity = artifact_envelope_identity(&artifact_envelope, body_hash);
    artifact_envelope_hash_from_identity(&identity)
}

fn collect_segment_digest_index(
    sorted_segments: &[BackupSegmentRef],
    findings: &mut Vec<BackupEnvelopeFinding>,
) -> std::collections::BTreeMap<u64, SegmentBytesDigest> {
    let mut map = std::collections::BTreeMap::new();
    for seg in sorted_segments {
        if let Some(prev) = map.get(&seg.segment_id).copied() {
            if prev == seg.bytes_digest {
                findings.push(BackupEnvelopeFinding::DuplicateSegmentRef {
                    segment_id: seg.segment_id,
                    bytes_digest: seg.bytes_digest,
                });
            } else {
                findings.push(BackupEnvelopeFinding::InconsistentSegmentId {
                    segment_id: seg.segment_id,
                    first_digest: prev,
                    second_digest: seg.bytes_digest,
                });
            }
            continue;
        }
        map.insert(seg.segment_id, seg.bytes_digest);
    }
    map
}

/// Structural scan over normalized segments: duplicate rows and inconsistent ids for the same `segment_id`.
#[must_use]
pub fn audit_backup_manifest_segments(body: &BackupManifestBody) -> Vec<BackupEnvelopeFinding> {
    let normalized = normalize_backup_manifest_body(body);
    let mut findings = Vec::new();
    if body.schema_version != BACKUP_MANIFEST_BODY_SCHEMA_VERSION {
        findings.push(
            BackupEnvelopeFinding::UnsupportedManifestBodySchemaVersion {
                observed: body.schema_version,
                expected: BACKUP_MANIFEST_BODY_SCHEMA_VERSION,
            },
        );
    }
    let _map = collect_segment_digest_index(&normalized.segments, &mut findings);
    sort_findings(&mut findings);
    findings
}

/// Verify signatures on a normalized manifest envelope and check `claimed_manifest_hash` plus structural segments.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde` while hashing or verifying the body.
pub fn verify_backup_manifest_envelope<F>(
    envelope: &BackupManifestEnvelope,
    claimed_manifest_hash: SegmentBytesDigest,
    verify_signature: F,
) -> Result<BackupManifestVerification, rmp_serde::encode::Error>
where
    F: FnMut(&SignatureRef, &[u8]) -> Result<(), String>,
{
    let envelope_norm = normalize_backup_manifest_envelope(envelope);
    let artifact_envelope = envelope_norm.to_canonical_envelope();
    let envelope_plane = verify_canonical_artifact_envelope(&artifact_envelope, verify_signature)?;
    let mut findings = audit_backup_manifest_segments(&envelope.body);
    let computed = backup_manifest_body_hash(&envelope.body)?;
    if computed != claimed_manifest_hash {
        findings.push(BackupEnvelopeFinding::ManifestBodyHashMismatch {
            claimed: claimed_manifest_hash,
            computed,
        });
    }
    sort_findings(&mut findings);
    Ok(BackupManifestVerification {
        envelope_plane,
        findings,
    })
}

/// Signature-only plane for a backup manifest envelope (normalized body bytes).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn verify_backup_manifest_signatures_only<F>(
    envelope: &BackupManifestEnvelope,
    verify_signature: F,
) -> Result<ArtifactVerificationReport, rmp_serde::encode::Error>
where
    F: FnMut(&SignatureRef, &[u8]) -> Result<(), String>,
{
    let envelope_norm = normalize_backup_manifest_envelope(envelope);
    let artifact_envelope = envelope_norm.to_canonical_envelope();
    verify_canonical_artifact_envelope(&artifact_envelope, verify_signature)
}

/// Build a restore proof body: compares normalized manifest segments to sorted `observed` multiset.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde` while computing [`backup_manifest_body_hash`].
pub fn restore_proof_report_body(
    expected_manifest: &BackupManifestBody,
    observed_segments: &[BackupSegmentRef],
) -> Result<RestoreProofReportBody, rmp_serde::encode::Error> {
    let manifest_body_hash = backup_manifest_body_hash(expected_manifest)?;
    let mut observed_segments_sorted: Vec<BackupSegmentRef> = observed_segments.to_vec();
    observed_segments_sorted.sort();

    let mut findings = Vec::new();
    if expected_manifest.schema_version != BACKUP_MANIFEST_BODY_SCHEMA_VERSION {
        findings.push(
            BackupEnvelopeFinding::UnsupportedManifestBodySchemaVersion {
                observed: expected_manifest.schema_version,
                expected: BACKUP_MANIFEST_BODY_SCHEMA_VERSION,
            },
        );
    }
    let normalized = normalize_backup_manifest_body(expected_manifest);
    let expected_map = collect_segment_digest_index(&normalized.segments, &mut findings);
    let observed_map = collect_segment_digest_index(&observed_segments_sorted, &mut findings);

    for (&id, &exp_digest) in &expected_map {
        match observed_map.get(&id) {
            None => findings.push(BackupEnvelopeFinding::MissingExpectedSegment { segment_id: id }),
            Some(&obs_digest) if obs_digest != exp_digest => {
                findings.push(BackupEnvelopeFinding::SegmentBytesDigestMismatch {
                    segment_id: id,
                    expected: exp_digest,
                    observed: obs_digest,
                });
            }
            Some(_) => {}
        }
    }
    for id in observed_map.keys() {
        if !expected_map.contains_key(id) {
            findings.push(BackupEnvelopeFinding::UnexpectedObservedSegment { segment_id: *id });
        }
    }
    sort_findings(&mut findings);

    Ok(RestoreProofReportBody {
        schema_version: RESTORE_PROOF_REPORT_SCHEMA_VERSION,
        manifest_body_hash,
        observed_segments_sorted,
        findings,
    })
}

/// Deterministic digest over [`RestoreProofReportBody`] (sorts `findings` clone).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn restore_proof_report_body_hash(
    report: &RestoreProofReportBody,
) -> Result<SegmentBytesDigest, rmp_serde::encode::Error> {
    let findings = sorted_findings(&report.findings);
    let mut observed_segments_sorted = report.observed_segments_sorted.clone();
    observed_segments_sorted.sort();
    let normalized = RestoreProofReportBody {
        findings,
        observed_segments_sorted,
        ..report.clone()
    };
    let bytes = crate::encoding::to_bytes(&normalized)?;
    Ok(content_hash(&bytes))
}
