//! Stable identifier + hash newtypes shared across the contract.
//!
//! Hashes are the same 32-byte blake3 digest BatPak uses for report identity,
//! so a sealed [`crate::BoundaryReport`] hashes byte-identically whether
//! re-derived here or by the substrate.

use serde::{Deserialize, Serialize};

/// 32-byte blake3 digest, matching BatPak's evidence-report identity width.
pub type Digest32 = [u8; 32];

/// Stable identity of a backend FAMILY (`"inert"`, `"linux"`, `"wasm"`, …).
///
/// Newtype over a small owned string so it can key the [`crate::BackendRegistry`]
/// and travel inside persisted plans/reports as audit evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BackendId(String);

impl BackendId {
    /// Construct a backend id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BackendId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Canonical content hash of a [`crate::BoundaryPlan`] (its `plan_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BoundaryPlanHash(pub Digest32);

/// Canonical `body_hash` over a [`crate::BoundaryReportBody`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BoundaryReportHash(pub Digest32);

/// Opaque identity of ONE attempt to run a plan.
///
/// Distinct from `plan_id` (which is content-addressed and immutable): the same
/// plan may be attempted more than once (a retry, a post-crash re-run), and each
/// attempt gets a fresh `AttemptId`. The pure contract treats it as opaque
/// bytes; the host maps it to a batpak `CorrelationId` at the persistence seam,
/// so one run is one correlation group. A branded type — never interchangeable
/// with a content hash or a plan id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AttemptId(pub Digest32);

/// blake3 content hash of an artifact's bytes — its BYTE identity.
///
/// Two artifacts with identical bytes share a `ContentHash` even when they are
/// different occurrences (see [`ArtifactId`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub Digest32);

/// Lifecycle identity of one artifact OCCURRENCE.
///
/// Deliberately distinct from [`ContentHash`] (byte identity) and [`AttemptId`]
/// (producer identity): two artifacts may have identical bytes yet come from
/// different attempts, different logical slots, and receive different
/// dispositions. Derived deterministically from `(attempt, slot, content)` so
/// replay and recovery re-derive the same id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub Digest32);

impl ArtifactId {
    /// Derive the occurrence id from its producing attempt, logical slot name,
    /// and content hash. The slot is pre-hashed to a fixed width so the three
    /// fixed-size fields concatenate unambiguously (no length framing needed).
    #[must_use]
    pub fn derive(attempt: AttemptId, slot: &str, content: ContentHash) -> Self {
        let slot_hash = batpak::event::hash::compute_hash(slot.as_bytes());
        let mut framed = [0u8; 96];
        framed[..32].copy_from_slice(&attempt.0);
        framed[32..64].copy_from_slice(&slot_hash);
        framed[64..].copy_from_slice(&content.0);
        Self(batpak::event::hash::compute_hash(&framed))
    }
}

/// Canonical hash of a [`crate::BackendProfileSnapshot`].
///
/// Bound into a [`crate::BoundaryPlan`] and revalidated by re-probing the
/// machine immediately before execution: if the live profile's hash differs
/// from the plan-bound hash, the run fails closed rather than execute against a
/// stale admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BackendProfileHash(pub Digest32);

impl BackendProfileHash {
    /// Hash already-canonical profile-snapshot bytes. The caller canonicalizes
    /// the snapshot (the same encoding the plan id uses) and passes the bytes,
    /// keeping this newtype free of any snapshot-type dependency.
    #[must_use]
    pub fn of(canonical_bytes: &[u8]) -> Self {
        Self(batpak::event::hash::compute_hash(canonical_bytes))
    }
}

/// Canonical content hash of an [`crate::contract::admission::AdmissionProgram`]
/// (`H_A`).
///
/// The admission decision is a bounded validated circuit; its digest is bound
/// into a [`BoundaryPlan`] so the *exact decision program* — not just its inputs —
/// is part of plan identity. Two plans that admit the same boundary by a different
/// circuit are different plans. A branded type, never interchangeable with a plan,
/// report, or profile hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AdmissionProgramHash(pub Digest32);

#[cfg(test)]
mod id_tests {
    use super::{ArtifactId, AttemptId, BackendProfileHash, ContentHash};

    #[test]
    fn artifact_id_derivation_is_deterministic() {
        let attempt = AttemptId([1u8; 32]);
        let content = ContentHash([2u8; 32]);
        assert_eq!(
            ArtifactId::derive(attempt, "out/log", content),
            ArtifactId::derive(attempt, "out/log", content),
        );
    }

    #[test]
    fn artifact_id_separates_attempt_slot_and_content() {
        let attempt = AttemptId([1u8; 32]);
        let content = ContentHash([2u8; 32]);
        let base = ArtifactId::derive(attempt, "slot", content);
        assert_ne!(
            base,
            ArtifactId::derive(AttemptId([9u8; 32]), "slot", content),
            "a different attempt is a different occurrence",
        );
        assert_ne!(
            base,
            ArtifactId::derive(attempt, "other", content),
            "a different slot is a different occurrence",
        );
        assert_ne!(
            base,
            ArtifactId::derive(attempt, "slot", ContentHash([8u8; 32])),
            "different content is a different occurrence",
        );
    }

    #[test]
    fn identical_bytes_from_different_attempts_are_distinct_artifacts() {
        // The whole reason `ArtifactId` is not `ContentHash`: identical bytes,
        // two attempts, two occurrences with independent dispositions.
        let content = ContentHash([7u8; 32]);
        assert_ne!(
            ArtifactId::derive(AttemptId([1u8; 32]), "out", content),
            ArtifactId::derive(AttemptId([2u8; 32]), "out", content),
        );
    }

    #[test]
    fn profile_hash_is_stable_and_distinguishing() {
        assert_eq!(
            BackendProfileHash::of(b"abc"),
            BackendProfileHash::of(b"abc")
        );
        assert_ne!(
            BackendProfileHash::of(b"abc"),
            BackendProfileHash::of(b"abd")
        );
    }
}
