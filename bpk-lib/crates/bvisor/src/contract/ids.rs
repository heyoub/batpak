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
