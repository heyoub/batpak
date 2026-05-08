//! Canonical envelope: **body digest** (payload only) vs **envelope digest**
//! (signatures, attestations, diagnostics).
//!
//! Compositional API: free functions hash the body and the envelope framing;
//! [`verify_envelope`](crate::envelope::verify_envelope) folds signature checks over canonical body bytes.
//!
//! This module does not depend on [`crate::store`]. Public names avoid the
//! banned product noun checked in `build.rs` (see `REFERENCE.md` vocabulary).
// justifies: INV-ALLOW-IS-DESIGN; canonical envelope `Result` paths are MessagePack encode-only; `tests/lane_a_fullsend_substrate.rs`
#![allow(clippy::missing_errors_doc)]
use crate::evidence::content_hash;
use serde::{Deserialize, Serialize};

/// Fixed-width digest for bodies and envelope framing.
pub type ContentDigest = [u8; 32];

/// Framing version for [`EnvelopeIdentity`] serialization.
pub const CANONICAL_ENVELOPE_FRAMING_VERSION: u32 = 1;

/// Opaque signature material with caller-defined algorithm/key ids.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SignatureRef {
    /// Caller-defined algorithm discriminant.
    pub algorithm_id: u32,
    /// Caller-defined key fingerprint.
    pub key_id: ContentDigest,
    /// Raw signature bytes.
    pub signature_bytes: Vec<u8>,
}

/// Signed blob (structure only; semantics are caller-defined).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SignatureEnvelope {
    /// Wrapped signature.
    pub signature: SignatureRef,
}

/// Opaque attestation bytes with kind id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AttestationRef {
    /// Caller-defined kind discriminant.
    pub kind_id: u32,
    /// Opaque attestation payload.
    pub bytes: Vec<u8>,
}

/// Payload plus envelope-owned attachments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanonicalEnvelope<T> {
    /// Domain payload: only this value participates in body hashing.
    pub body: T,
    /// Version for this envelope field layout.
    pub envelope_schema_version: u32,
    /// Envelope-only wall clock (diagnostics).
    pub generated_at_wall_ms: Option<u64>,
    /// Envelope-only note (diagnostics).
    pub diagnostic_note: Option<String>,
    /// Signatures (canonical order applied before hashing).
    pub signatures: Vec<SignatureEnvelope>,
    /// Attestations (canonical order applied before hashing).
    pub attestations: Vec<AttestationRef>,
}

/// Serialized envelope identity: `body_hash` anchor + envelope-owned fields (no raw `body`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvelopeIdentity {
    /// [`CANONICAL_ENVELOPE_FRAMING_VERSION`].
    pub framing_schema_version: u32,
    /// Body identity digest (hash of canonical body bytes only).
    pub body_hash: ContentDigest,
    /// Echo of [`CanonicalEnvelope::envelope_schema_version`].
    pub envelope_schema_version: u32,
    /// Wall clock for envelope; included in envelope hash only.
    pub generated_at_wall_ms: Option<u64>,
    /// Diagnostics for envelope; included in envelope hash only.
    pub diagnostic_note: Option<String>,
    /// Lexicographically sorted signatures.
    pub signatures_sorted: Vec<SignatureEnvelope>,
    /// Lexicographically sorted attestations.
    pub attestations_sorted: Vec<AttestationRef>,
}

/// Verification output (deterministic given envelope + verifier closures).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeVerificationReport {
    /// Hash of canonical body bytes only.
    pub body_hash: ContentDigest,
    /// Hash of canonical [`EnvelopeIdentity`].
    pub envelope_hash: ContentDigest,
    /// Sorted findings.
    pub findings: Vec<EnvelopeVerificationFinding>,
}

/// Structural finding from verification.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EnvelopeVerificationFinding {
    /// Signature bytes failed `verify_signature`.
    InvalidSignature {
        /// Key fingerprint from the signature.
        key_id: ContentDigest,
        /// Deterministic failure text from verifier.
        reason: String,
    },
}

/// Canonical body bytes for `body` using [`crate::encoding::to_bytes`].
pub fn body_bytes<T: Serialize>(body: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    crate::encoding::to_bytes(body)
}

/// [`ContentDigest`] over body bytes only (identity of `T` alone).
pub fn body_hash_from_body<T: Serialize>(
    body: &T,
) -> Result<ContentDigest, rmp_serde::encode::Error> {
    let bytes = body_bytes(body)?;
    Ok(content_hash(&bytes))
}

/// Build [`EnvelopeIdentity`] with sorted attachments.
pub fn envelope_identity<T: Serialize>(
    envelope: &CanonicalEnvelope<T>,
    body_hash: ContentDigest,
) -> Result<EnvelopeIdentity, rmp_serde::encode::Error> {
    let mut signatures_sorted = envelope.signatures.clone();
    signatures_sorted.sort();
    let mut attestations_sorted = envelope.attestations.clone();
    attestations_sorted.sort();
    Ok(EnvelopeIdentity {
        framing_schema_version: CANONICAL_ENVELOPE_FRAMING_VERSION,
        body_hash,
        envelope_schema_version: envelope.envelope_schema_version,
        generated_at_wall_ms: envelope.generated_at_wall_ms,
        diagnostic_note: envelope.diagnostic_note.clone(),
        signatures_sorted,
        attestations_sorted,
    })
}

/// Hash of canonical [`EnvelopeIdentity`] (MessagePack named fields).
pub fn envelope_hash_from_identity(
    identity: &EnvelopeIdentity,
) -> Result<ContentDigest, rmp_serde::encode::Error> {
    let bytes = crate::encoding::to_bytes(identity)?;
    Ok(content_hash(&bytes))
}

/// [`body_hash_from_body`] + [`envelope_identity`] + [`envelope_hash_from_identity`].
pub fn envelope_hash_for<T: Serialize>(
    envelope: &CanonicalEnvelope<T>,
) -> Result<ContentDigest, rmp_serde::encode::Error> {
    let bh = body_hash_from_body(&envelope.body)?;
    let id = envelope_identity(envelope, bh)?;
    envelope_hash_from_identity(&id)
}

/// Verify each signature against **body** bytes using `verify_signature`.
pub fn verify_envelope<T: Serialize, F>(
    envelope: &CanonicalEnvelope<T>,
    mut verify_signature: F,
) -> Result<EnvelopeVerificationReport, rmp_serde::encode::Error>
where
    F: FnMut(&SignatureRef, &[u8]) -> Result<(), String>,
{
    let body_raw = body_bytes(&envelope.body)?;
    let body_digest = content_hash(&body_raw);
    let env_id = envelope_identity(envelope, body_digest)?;
    let env_digest = envelope_hash_from_identity(&env_id)?;

    let mut findings = Vec::new();
    for sig in &env_id.signatures_sorted {
        if let Err(reason) = verify_signature(&sig.signature, &body_raw) {
            findings.push(EnvelopeVerificationFinding::InvalidSignature {
                key_id: sig.signature.key_id,
                reason,
            });
        }
    }
    findings.sort();

    Ok(EnvelopeVerificationReport {
        body_hash: body_digest,
        envelope_hash: env_digest,
        findings,
    })
}

/// Deterministic digest of a verification report (`findings` sorted for canonicalization).
pub fn verification_report_body_hash(
    report: &EnvelopeVerificationReport,
) -> Result<ContentDigest, rmp_serde::encode::Error> {
    let mut findings = report.findings.clone();
    findings.sort();
    let normalized = EnvelopeVerificationReport {
        findings,
        ..report.clone()
    };
    let bytes = crate::encoding::to_bytes(&normalized)?;
    Ok(content_hash(&bytes))
}

impl<T: Serialize> CanonicalEnvelope<T> {
    /// [`ContentDigest`] over the canonical body only.
    pub fn body_hash(&self) -> Result<ContentDigest, rmp_serde::encode::Error> {
        body_hash_from_body(&self.body)
    }

    /// [`ContentDigest`] over [`EnvelopeIdentity`] for this envelope.
    pub fn envelope_hash(&self) -> Result<ContentDigest, rmp_serde::encode::Error> {
        envelope_hash_for(self)
    }
}
