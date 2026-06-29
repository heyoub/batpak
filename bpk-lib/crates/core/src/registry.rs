//! Batpak Substrate Closure attested registry row: stable row identity, canonical row body digest,
//! lifecycle and supersession pointers, drift evidence, and verification reports that compose
//! [`crate::artifact::CanonicalArtifactEnvelope`] without importing [`crate::store`].
//!
//! Public evidence bodies use [`crate::encoding::to_bytes`] (the [`crate::canonical`] alias) for byte identity.
//! Callers supply opaque `row_kind`, `opaque_payload`, and `named_digests`; batpak does not interpret
//! protocol or application meaning in those fields.

use crate::artifact::{
    verify_canonical_artifact_envelope, ArtifactHash, ArtifactVerificationReport,
    CanonicalArtifactEnvelope, SignatureRef,
};
use crate::evidence::{content_hash, sort_findings, sorted_findings};
use serde::{Deserialize, Serialize};

/// Schema version baked into canonical [`RegistryRowBody`] encoding.
pub const REGISTRY_ROW_BODY_SCHEMA_VERSION: u32 = 1;

/// Schema version for canonical [`RegistryDriftReportBody`].
pub const REGISTRY_DRIFT_REPORT_SCHEMA_VERSION: u32 = 1;

/// Schema version for canonical [`RegistryVerificationReport`].
pub const REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION: u32 = 1;

/// Row published but not yet active ([`RegistryRowBody::lifecycle`]).
pub const REGISTRY_LIFECYCLE_ANNOUNCED: u32 = 0;
/// Active row ([`RegistryRowBody::lifecycle`]).
pub const REGISTRY_LIFECYCLE_LIVE: u32 = 1;
/// Superseded or slated for removal; still discoverable ([`RegistryRowBody::lifecycle`]).
pub const REGISTRY_LIFECYCLE_DEPRECATED: u32 = 2;
/// Retired; structural checks flag `supersedes` on removed rows ([`RegistryRowBody::lifecycle`]).
pub const REGISTRY_LIFECYCLE_REMOVED: u32 = 3;

/// Stable opaque row identifier (digest-sized).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegistryRowId(pub ArtifactHash);

/// Named digest anchor (sorted before row body hashing).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NamedDigest {
    /// Caller-defined stable name (sorted lexicographically with ties broken by digest).
    pub name: String,
    /// Content digest for the named attachment or sidecar.
    pub digest: ArtifactHash,
}

/// Canonical immutable row **body** (hashed for `row_hash`; envelope fields stay outside).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryRowBody {
    /// Must equal [`REGISTRY_ROW_BODY_SCHEMA_VERSION`] for v1 hashing helpers.
    pub schema_version: u32,
    /// Stable row identity (included in the body so the digest commits to the id).
    pub row_id: RegistryRowId,
    /// Opaque kind discriminant for caller registries.
    pub row_kind: u64,
    /// Layout version for `opaque_payload` interpretation (caller-owned).
    pub row_layout_version: u32,
    /// Opaque payload bytes (caller-owned).
    pub opaque_payload: Vec<u8>,
    /// Named digests; normalized to sorted order before hashing.
    pub named_digests: Vec<NamedDigest>,
    /// Lifecycle lane; must be one of [`REGISTRY_LIFECYCLE_ANNOUNCED`], [`REGISTRY_LIFECYCLE_LIVE`],
    /// [`REGISTRY_LIFECYCLE_DEPRECATED`], or [`REGISTRY_LIFECYCLE_REMOVED`] for clean verification.
    pub lifecycle: u32,
    /// Optional prior row this entry supersedes.
    pub supersedes: Option<RegistryRowId>,
}

/// Normalize row body for canonical digest (sorts `named_digests`).
#[must_use]
pub fn normalize_registry_row_body(body: &RegistryRowBody) -> RegistryRowBody {
    let mut named_digests = body.named_digests.clone();
    named_digests.sort();
    RegistryRowBody {
        named_digests,
        ..body.clone()
    }
}

/// Canonical MessagePack bytes for the normalized row body (same encoding plane as
/// [`crate::artifact::artifact_body_bytes`] on the normalized [`RegistryRowBody`]).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn registry_row_body_bytes(
    body: &RegistryRowBody,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let normalized = normalize_registry_row_body(body);
    crate::encoding::to_bytes(&normalized)
}

/// Digest of canonical normalized row body bytes.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn registry_row_body_hash(
    body: &RegistryRowBody,
) -> Result<ArtifactHash, rmp_serde::encode::Error> {
    let bytes = registry_row_body_bytes(body)?;
    Ok(content_hash(&bytes))
}

/// Structural drift finding between expected and observed registries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RegistryDriftFinding {
    /// Expected row missing from observed set.
    MissingRow {
        /// Row id absent on the observed side.
        row_id: RegistryRowId,
    },
    /// Observed row not present in expected set.
    ExtraRow {
        /// Row id only on the observed side.
        row_id: RegistryRowId,
    },
    /// Same `row_id` but digest mismatch.
    HashMismatch {
        /// Conflicting row id.
        row_id: RegistryRowId,
        /// Expected canonical row hash.
        expected: ArtifactHash,
        /// Observed canonical row hash.
        observed: ArtifactHash,
    },
}

/// Deterministic drift report **body** (hash with [`registry_drift_report_body_hash`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryDriftReportBody {
    /// Must equal [`REGISTRY_DRIFT_REPORT_SCHEMA_VERSION`] for v1.
    pub schema_version: u32,
    /// Lexicographically sorted `(row_id, row_hash)` expected side.
    pub expected: Vec<(RegistryRowId, ArtifactHash)>,
    /// Lexicographically sorted `(row_id, row_hash)` observed side.
    pub observed: Vec<(RegistryRowId, ArtifactHash)>,
    /// Drift findings (sorted before body hash).
    pub findings: Vec<RegistryDriftFinding>,
}

/// Sort `(row_id, hash)` pairs by row id then hash.
pub fn sort_registry_row_hash_pairs(pairs: &mut [(RegistryRowId, ArtifactHash)]) {
    pairs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
}

/// Build drift findings from sorted expected/observed maps (must be sorted, see [`sort_registry_row_hash_pairs`]).
#[must_use]
pub fn registry_drift_findings_sorted(
    expected: &[(RegistryRowId, ArtifactHash)],
    observed: &[(RegistryRowId, ArtifactHash)],
) -> Vec<RegistryDriftFinding> {
    let mut i = 0usize;
    let mut j = 0usize;
    let mut out = Vec::new();
    while i < expected.len() && j < observed.len() {
        match expected[i].0.cmp(&observed[j].0) {
            std::cmp::Ordering::Less => {
                out.push(RegistryDriftFinding::MissingRow {
                    row_id: expected[i].0,
                });
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(RegistryDriftFinding::ExtraRow {
                    row_id: observed[j].0,
                });
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                if expected[i].1 != observed[j].1 {
                    out.push(RegistryDriftFinding::HashMismatch {
                        row_id: expected[i].0,
                        expected: expected[i].1,
                        observed: observed[j].1,
                    });
                }
                i += 1;
                j += 1;
            }
        }
    }
    while i < expected.len() {
        out.push(RegistryDriftFinding::MissingRow {
            row_id: expected[i].0,
        });
        i += 1;
    }
    while j < observed.len() {
        out.push(RegistryDriftFinding::ExtraRow {
            row_id: observed[j].0,
        });
        j += 1;
    }
    sort_findings(&mut out);
    out
}

/// Deterministic digest over drift report body (sorts `findings` clone).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn registry_drift_report_body_hash(
    report: &RegistryDriftReportBody,
) -> Result<ArtifactHash, rmp_serde::encode::Error> {
    let findings = sorted_findings(&report.findings);
    let mut expected = report.expected.clone();
    let mut observed = report.observed.clone();
    sort_registry_row_hash_pairs(&mut expected);
    sort_registry_row_hash_pairs(&mut observed);
    let normalized = RegistryDriftReportBody {
        expected,
        observed,
        findings,
        ..report.clone()
    };
    let bytes = crate::encoding::to_bytes(&normalized)?;
    Ok(content_hash(&bytes))
}

/// Registry-specific verification finding layered on [`ArtifactVerificationReport`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RegistryVerificationFinding {
    /// [`RegistryRowBody::schema_version`] is not supported by these v1 helpers.
    UnsupportedRowSchemaVersion {
        /// Row id from the decoded body.
        row_id: RegistryRowId,
        /// Observed row body schema version.
        observed: u32,
        /// Supported row body schema version.
        expected: u32,
    },
    /// [`RegistryRowBody::lifecycle`] is not one of the `REGISTRY_LIFECYCLE_*` constants.
    InvalidLifecycle {
        /// Row id from the decoded body.
        row_id: RegistryRowId,
        /// Raw discriminant observed.
        lifecycle: u32,
    },
    /// Declared row hash does not match recomputed canonical body hash.
    RowHashMismatch {
        /// Row id from the body.
        row_id: RegistryRowId,
        /// Caller-claimed digest.
        claimed: ArtifactHash,
        /// Recomputed digest from [`registry_row_body_hash`].
        computed: ArtifactHash,
    },
    /// [`RegistryRowBody::row_id`] disagrees with the claim.
    RowIdMismatch {
        /// Identity in the body.
        body_row_id: RegistryRowId,
        /// Identity supplied by caller.
        claimed_row_id: RegistryRowId,
    },
}

/// Full-stack verification report for an attested row envelope plus structural checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryVerificationReport {
    /// Must equal [`REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION`] for v1 hashing.
    pub schema_version: u32,
    /// Signature and envelope digest plane from [`verify_canonical_artifact_envelope`].
    pub envelope_plane: ArtifactVerificationReport,
    /// Registry-only findings (sorted before [`registry_verification_report_body_hash`]).
    pub findings: Vec<RegistryVerificationFinding>,
}

/// Deterministic digest over [`RegistryVerificationReport`] (sorts `findings` clone).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn registry_verification_report_body_hash(
    report: &RegistryVerificationReport,
) -> Result<ArtifactHash, rmp_serde::encode::Error> {
    let findings = sorted_findings(&report.findings);
    let mut envelope_plane = report.envelope_plane.clone();
    sort_findings(&mut envelope_plane.findings);
    let normalized = RegistryVerificationReport {
        envelope_plane,
        findings,
        ..report.clone()
    };
    let bytes = crate::encoding::to_bytes(&normalized)?;
    Ok(content_hash(&bytes))
}

/// Supersession graph finding across a closed catalog of row bodies (sorted by `row_id` before calling).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RegistrySupersessionFinding {
    /// `supersedes` points to a row id not present in `catalog`.
    DanglingSupersedes {
        /// Row declaring the pointer.
        from: RegistryRowId,
        /// Missing target id.
        target: RegistryRowId,
    },
    /// A `Removed` row still declares a `supersedes` edge (structural hygiene).
    RemovedDeclaresSupersedes {
        /// Offending row id.
        from: RegistryRowId,
    },
    /// Same `row_id` appears more than once in the sorted catalog input.
    DuplicateRowId {
        /// Repeated id (second and later occurrences are ignored by merge-walk callers).
        row_id: RegistryRowId,
    },
    /// Following `supersedes` edges within the catalog revisits a row on the path.
    SupersedesCycle {
        /// Edge that closes or participates in a cycle (first edge found in stable walk order).
        edge_from: RegistryRowId,
        /// Head of the back edge (already on the active walk stack).
        edge_to: RegistryRowId,
    },
}

/// Deterministic supersession audit over a catalog keyed by [`RegistryRowId`].
///
/// `catalog` must be sorted by ascending `row_id` and must contain unique ids.
#[must_use]
pub fn registry_supersession_findings_sorted(
    catalog: &[(RegistryRowId, RegistryRowBody)],
) -> Vec<RegistrySupersessionFinding> {
    let mut out = Vec::new();
    for w in catalog.windows(2) {
        if w[0].0 == w[1].0 {
            out.push(RegistrySupersessionFinding::DuplicateRowId { row_id: w[1].0 });
        }
    }
    let id_set: std::collections::BTreeSet<RegistryRowId> =
        catalog.iter().map(|(id, _)| *id).collect();
    let by_id: std::collections::BTreeMap<RegistryRowId, &RegistryRowBody> =
        catalog.iter().map(|(id, body)| (*id, body)).collect();

    for (id, body) in catalog {
        if let Some(target) = body.supersedes {
            if !id_set.contains(&target) {
                out.push(RegistrySupersessionFinding::DanglingSupersedes { from: *id, target });
            }
        }
        if body.lifecycle == REGISTRY_LIFECYCLE_REMOVED && body.supersedes.is_some() {
            out.push(RegistrySupersessionFinding::RemovedDeclaresSupersedes { from: *id });
        }
    }

    let mut cycle_edges: std::collections::BTreeSet<(RegistryRowId, RegistryRowId)> =
        std::collections::BTreeSet::new();
    for &(start, _) in catalog {
        let mut path: Vec<RegistryRowId> = Vec::new();
        supersession_walk_for_cycles(&by_id, start, &mut path, &mut cycle_edges);
    }
    for edge in cycle_edges {
        out.push(RegistrySupersessionFinding::SupersedesCycle {
            edge_from: edge.0,
            edge_to: edge.1,
        });
    }
    out.sort();
    out
}

fn supersession_walk_for_cycles(
    by_id: &std::collections::BTreeMap<RegistryRowId, &RegistryRowBody>,
    cur: RegistryRowId,
    path: &mut Vec<RegistryRowId>,
    cycle_edges: &mut std::collections::BTreeSet<(RegistryRowId, RegistryRowId)>,
) {
    if path.contains(&cur) {
        if let Some(&prev) = path.last() {
            cycle_edges.insert((prev, cur));
        }
        return;
    }
    path.push(cur);
    if let Some(body) = by_id.get(&cur) {
        if let Some(next) = body.supersedes {
            if by_id.contains_key(&next) {
                supersession_walk_for_cycles(by_id, next, path, cycle_edges);
            }
        }
    }
    path.pop();
}

/// Verify signatures on a canonical envelope whose body is [`RegistryRowBody`], then apply structural checks.
///
/// `claimed_row_id` and `claimed_row_hash` let callers pin identity to the envelope body and digest.
///
/// # Errors
/// MessagePack encode failure while hashing or verifying the body.
pub fn verify_registry_attested_row<F>(
    envelope: &CanonicalArtifactEnvelope<RegistryRowBody>,
    claimed_row_id: RegistryRowId,
    claimed_row_hash: ArtifactHash,
    verify_signature: F,
) -> Result<RegistryVerificationReport, rmp_serde::encode::Error>
where
    F: FnMut(&SignatureRef, &[u8]) -> Result<(), String>,
{
    let normalized_body = normalize_registry_row_body(&envelope.body);
    let envelope_norm = CanonicalArtifactEnvelope {
        body: normalized_body,
        envelope_schema_version: envelope.envelope_schema_version,
        generated_at_wall_ms: envelope.generated_at_wall_ms,
        diagnostic_note: envelope.diagnostic_note.clone(),
        signatures: envelope.signatures.clone(),
        attestations: envelope.attestations.clone(),
    };
    let envelope_plane = verify_canonical_artifact_envelope(&envelope_norm, verify_signature)?;
    let mut findings = Vec::new();

    let body = &envelope_norm.body;
    if body.schema_version != REGISTRY_ROW_BODY_SCHEMA_VERSION {
        findings.push(RegistryVerificationFinding::UnsupportedRowSchemaVersion {
            row_id: body.row_id,
            observed: body.schema_version,
            expected: REGISTRY_ROW_BODY_SCHEMA_VERSION,
        });
    }

    if body.row_id != claimed_row_id {
        findings.push(RegistryVerificationFinding::RowIdMismatch {
            body_row_id: body.row_id,
            claimed_row_id,
        });
    }

    let computed = registry_row_body_hash(body)?;
    if computed != claimed_row_hash {
        findings.push(RegistryVerificationFinding::RowHashMismatch {
            row_id: body.row_id,
            claimed: claimed_row_hash,
            computed,
        });
    }

    if body.lifecycle != REGISTRY_LIFECYCLE_ANNOUNCED
        && body.lifecycle != REGISTRY_LIFECYCLE_LIVE
        && body.lifecycle != REGISTRY_LIFECYCLE_DEPRECATED
        && body.lifecycle != REGISTRY_LIFECYCLE_REMOVED
    {
        findings.push(RegistryVerificationFinding::InvalidLifecycle {
            row_id: body.row_id,
            lifecycle: body.lifecycle,
        });
    }

    sort_findings(&mut findings);

    Ok(RegistryVerificationReport {
        schema_version: REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION,
        envelope_plane,
        findings,
    })
}

/// Expose canonical body bytes used for signature verification (normalized row body).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn registry_row_signing_bytes(
    body: &RegistryRowBody,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    registry_row_body_bytes(body)
}

/// Verify signatures treating the registry row body as the signed payload (same bytes as [`registry_row_body_hash`] input plane).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn verify_registry_row_signatures_only<F>(
    envelope: &CanonicalArtifactEnvelope<RegistryRowBody>,
    verify_signature: F,
) -> Result<ArtifactVerificationReport, rmp_serde::encode::Error>
where
    F: FnMut(&SignatureRef, &[u8]) -> Result<(), String>,
{
    let normalized_body = normalize_registry_row_body(&envelope.body);
    let envelope_norm = CanonicalArtifactEnvelope {
        body: normalized_body,
        envelope_schema_version: envelope.envelope_schema_version,
        generated_at_wall_ms: envelope.generated_at_wall_ms,
        diagnostic_note: envelope.diagnostic_note.clone(),
        signatures: envelope.signatures.clone(),
        attestations: envelope.attestations.clone(),
    };
    verify_canonical_artifact_envelope(&envelope_norm, verify_signature)
}

/// `true` when canonical row bytes match [`crate::artifact::artifact_body_bytes`] on the normalized body.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn registry_row_body_hash_matches_signing_bytes(
    body: &RegistryRowBody,
) -> Result<bool, rmp_serde::encode::Error> {
    let n = normalize_registry_row_body(body);
    let a = crate::artifact::artifact_body_bytes(&n)?;
    let b = registry_row_body_bytes(body)?;
    Ok(a == b)
}

#[cfg(test)]
mod tests {
    use super::{registry_drift_findings_sorted, RegistryDriftFinding, RegistryRowId};

    /// Build a `(row_id, hash)` pair whose id and hash are both stamped from a
    /// single discriminant byte, so ordering is the leading byte of the id.
    fn pair(id_byte: u8, hash_byte: u8) -> (RegistryRowId, [u8; 32]) {
        let mut id = [0u8; 32];
        id[0] = id_byte;
        let mut hash = [0u8; 32];
        hash[0] = hash_byte;
        (RegistryRowId(id), hash)
    }

    fn row_id(id_byte: u8) -> RegistryRowId {
        let mut id = [0u8; 32];
        id[0] = id_byte;
        RegistryRowId(id)
    }

    #[test]
    fn drift_merge_advances_both_cursors_across_every_branch_kind() {
        // expected ids: 1 (missing from observed), 2 (hash mismatch), 4 (present, equal).
        // observed ids: 2 (mismatching hash), 3 (extra), 4 (matching hash).
        //
        // The Less branch fires FIRST at index 0 (expected id 1 < observed id 2),
        // so an `i += 1` -> `i *= 1` mutant never advances past it; an
        // `j += 1` -> `j *= 1` mutant stalls on the Greater branch (observed id 3).
        // Asserting the EXACT finding set fails fast on any stalled cursor before
        // a non-terminating loop can hang the test.
        let expected = [pair(1, 10), pair(2, 20), pair(4, 40)];
        let observed = [pair(2, 99), pair(3, 30), pair(4, 40)];

        let findings = registry_drift_findings_sorted(&expected, &observed);

        // `registry_drift_findings_sorted` sorts findings before returning.
        let mut want = vec![
            RegistryDriftFinding::MissingRow { row_id: row_id(1) },
            RegistryDriftFinding::HashMismatch {
                row_id: row_id(2),
                expected: pair(2, 20).1,
                observed: pair(2, 99).1,
            },
            RegistryDriftFinding::ExtraRow { row_id: row_id(3) },
        ];
        crate::evidence::sort_findings(&mut want);

        assert_eq!(
            findings, want,
            "PROPERTY: the sorted merge-diff visits Missing, HashMismatch, and Extra \
             exactly once each — every cursor advance must be `+= 1`"
        );
    }

    #[test]
    fn drift_merge_drains_observed_tail_as_extra_rows() {
        // expected is a single early id; observed carries a tail of higher ids.
        // The Greater branch (and the observed-tail drain) must `j += 1`; a
        // mutant that fails to advance observed yields the wrong Extra set.
        let expected = [pair(1, 10)];
        let observed = [pair(1, 10), pair(2, 20), pair(3, 30)];

        let findings = registry_drift_findings_sorted(&expected, &observed);

        let mut want = vec![
            RegistryDriftFinding::ExtraRow { row_id: row_id(2) },
            RegistryDriftFinding::ExtraRow { row_id: row_id(3) },
        ];
        crate::evidence::sort_findings(&mut want);

        assert_eq!(
            findings, want,
            "PROPERTY: every surplus observed row past the expected set is reported once"
        );
    }
}
