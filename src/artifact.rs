//! Canonical **artifact** envelope: digest of the serializable **body** vs digest of the **envelope**
//! (signatures, attestations, diagnostics).
//!
//! Composition: free functions build [`crate::artifact::ArtifactEnvelopeIdentity`], hash it, and fold
//! [`crate::artifact::verify_canonical_artifact_envelope`] over caller-supplied signature predicates.
//!
//! This module does **not** depend on [`crate::store`].

use crate::evidence::{content_hash, sort_findings, sorted_findings};
use serde::{Deserialize, Serialize};

/// Fixed-width digest for artifact bodies and envelope framing.
pub type ArtifactHash = [u8; 32];

/// Serialization version for [`ArtifactEnvelopeIdentity`] (framing contract).
pub const ARTIFACT_ENVELOPE_FRAMING_VERSION: u32 = 1;

/// Opaque signature bytes with caller-defined algorithm and key identifiers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SignatureRef {
    /// Caller-defined algorithm discriminant.
    pub algorithm_id: u32,
    /// Caller-defined key fingerprint.
    pub key_id: ArtifactHash,
    /// Raw signature bytes.
    pub signature_bytes: Vec<u8>,
}

/// Signature payload wrapper (structure only).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SignatureEnvelope {
    /// Inner signature.
    pub signature: SignatureRef,
}

/// Opaque attestation bytes with kind discriminant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AttestationRef {
    /// Caller-defined attestation kind discriminant.
    pub kind_id: u32,
    /// Opaque payload.
    pub bytes: Vec<u8>,
}

/// Serializable body plus envelope-owned fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanonicalArtifactEnvelope<T> {
    /// Domain body: **only** this value is hashed for [`ArtifactHash`] body identity.
    pub body: T,
    /// Envelope field-layout version.
    pub envelope_schema_version: u32,
    /// Envelope-only wall clock (outside body identity).
    pub generated_at_wall_ms: Option<u64>,
    /// Envelope-only diagnostics (outside body identity).
    pub diagnostic_note: Option<String>,
    /// Signatures (canonical sort before hashing).
    pub signatures: Vec<SignatureEnvelope>,
    /// Attestations (canonical sort before hashing).
    pub attestations: Vec<AttestationRef>,
}

/// Canonical serialized envelope **identity** (anchors `body_hash`; never embeds raw `body` bytes).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactEnvelopeIdentity {
    /// [`ARTIFACT_ENVELOPE_FRAMING_VERSION`].
    pub framing_schema_version: u32,
    /// Digest of canonical body bytes only.
    pub body_hash: ArtifactHash,
    /// Copies [`CanonicalArtifactEnvelope::envelope_schema_version`].
    pub envelope_schema_version: u32,
    /// Envelope-only wall clock copied from [`CanonicalArtifactEnvelope::generated_at_wall_ms`].
    pub generated_at_wall_ms: Option<u64>,
    /// Envelope-only note copied from [`CanonicalArtifactEnvelope::diagnostic_note`].
    pub diagnostic_note: Option<String>,
    /// Sorted signatures.
    pub signatures_sorted: Vec<SignatureEnvelope>,
    /// Sorted attestations.
    pub attestations_sorted: Vec<AttestationRef>,
}

/// Deterministic verification view over an envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVerificationReport {
    /// Canonical body digest only.
    pub body_hash: ArtifactHash,
    /// Digest of canonical [`ArtifactEnvelopeIdentity`].
    pub envelope_hash: ArtifactHash,
    /// Findings (sorted before [`artifact_verification_report_body_hash`]).
    pub findings: Vec<ArtifactEnvelopeFinding>,
}

/// Structural verification finding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ArtifactEnvelopeFinding {
    /// `verify_signature` returned `Err`.
    InvalidSignature {
        /// From [`SignatureRef::key_id`].
        key_id: ArtifactHash,
        /// Verifier-provided deterministic text.
        reason: String,
    },
}

/// Canonical MessagePack bytes for `body`.
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde`.
pub fn artifact_body_bytes<T: Serialize>(body: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    crate::encoding::to_bytes(body)
}

/// Digest of canonical body bytes for `T`.
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde`.
pub fn artifact_body_hash_from_body<T: Serialize>(
    body: &T,
) -> Result<ArtifactHash, rmp_serde::encode::Error> {
    let bytes = artifact_body_bytes(body)?;
    Ok(content_hash(&bytes))
}

/// Build [`ArtifactEnvelopeIdentity`] with sorted attachment vectors.
pub fn artifact_envelope_identity<T: Serialize>(
    envelope: &CanonicalArtifactEnvelope<T>,
    body_hash: ArtifactHash,
) -> ArtifactEnvelopeIdentity {
    let mut signatures_sorted = envelope.signatures.clone();
    signatures_sorted.sort();
    let mut attestations_sorted = envelope.attestations.clone();
    attestations_sorted.sort();
    ArtifactEnvelopeIdentity {
        framing_schema_version: ARTIFACT_ENVELOPE_FRAMING_VERSION,
        body_hash,
        envelope_schema_version: envelope.envelope_schema_version,
        generated_at_wall_ms: envelope.generated_at_wall_ms,
        diagnostic_note: envelope.diagnostic_note.clone(),
        signatures_sorted,
        attestations_sorted,
    }
}

/// Digest of canonical [`ArtifactEnvelopeIdentity`] (MessagePack named fields).
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde`.
pub fn artifact_envelope_hash_from_identity(
    identity: &ArtifactEnvelopeIdentity,
) -> Result<ArtifactHash, rmp_serde::encode::Error> {
    let bytes = crate::encoding::to_bytes(identity)?;
    Ok(content_hash(&bytes))
}

/// [`artifact_body_hash_from_body`] then identity + envelope hash chain.
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde`.
pub fn artifact_envelope_hash_for<T: Serialize>(
    envelope: &CanonicalArtifactEnvelope<T>,
) -> Result<ArtifactHash, rmp_serde::encode::Error> {
    let bh = artifact_body_hash_from_body(&envelope.body)?;
    let id = artifact_envelope_identity(envelope, bh);
    artifact_envelope_hash_from_identity(&id)
}

/// Verify each [`SignatureEnvelope`] using `verify_signature` over **body** bytes only.
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde` while serializing the body.
pub fn verify_canonical_artifact_envelope<T: Serialize, F>(
    envelope: &CanonicalArtifactEnvelope<T>,
    mut verify_signature: F,
) -> Result<ArtifactVerificationReport, rmp_serde::encode::Error>
where
    F: FnMut(&SignatureRef, &[u8]) -> Result<(), String>,
{
    let body_raw = artifact_body_bytes(&envelope.body)?;
    let body_digest = content_hash(&body_raw);
    let env_id = artifact_envelope_identity(envelope, body_digest);
    let env_digest = artifact_envelope_hash_from_identity(&env_id)?;

    let mut findings = Vec::new();
    for sig in &env_id.signatures_sorted {
        if let Err(reason) = verify_signature(&sig.signature, &body_raw) {
            findings.push(ArtifactEnvelopeFinding::InvalidSignature {
                key_id: sig.signature.key_id,
                reason,
            });
        }
    }
    sort_findings(&mut findings);

    Ok(ArtifactVerificationReport {
        body_hash: body_digest,
        envelope_hash: env_digest,
        findings,
    })
}

/// Deterministic digest over a verification report (findings normalized-sorted).
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde`.
pub fn artifact_verification_report_body_hash(
    report: &ArtifactVerificationReport,
) -> Result<ArtifactHash, rmp_serde::encode::Error> {
    let findings = sorted_findings(&report.findings);
    let normalized = ArtifactVerificationReport {
        findings,
        ..report.clone()
    };
    let bytes = crate::encoding::to_bytes(&normalized)?;
    Ok(content_hash(&bytes))
}

impl<T: Serialize> CanonicalArtifactEnvelope<T> {
    /// Digest of the canonical body bytes only.
    ///
    /// # Errors
    /// MessagePack encoding failure from `rmp-serde`.
    pub fn body_hash(&self) -> Result<ArtifactHash, rmp_serde::encode::Error> {
        artifact_body_hash_from_body(&self.body)
    }

    /// Digest of the canonical [`ArtifactEnvelopeIdentity`] for this envelope.
    ///
    /// # Errors
    /// MessagePack encoding failure from `rmp-serde`.
    pub fn envelope_hash(&self) -> Result<ArtifactHash, rmp_serde::encode::Error> {
        artifact_envelope_hash_for(self)
    }
}
