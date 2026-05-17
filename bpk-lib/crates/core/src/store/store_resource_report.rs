//! Deterministic structural evidence over stable store-owned diagnostics facts.
//!
//! Canonical identity intentionally excludes raw filesystem paths, free-form
//! diagnostics strings, and per-process timestamps outside the structured
//! [`crate::store::cold_start::rebuild::OpenIndexReport`] snapshot (which is
//! already part of cold-start truth, not host noise).
//!
//! Bodies are **point-in-time** snapshots: comparing full bodies across
//! `close`/`open` is not a substrate contract because cold-start path,
//! frontier timing, and system event replay can legitimately differ while the
//! store remains consistent.

use crate::store::cold_start::rebuild::OpenIndexReport;
use crate::store::stats::{
    FrontierView, PlatformEvidenceSummary, StoreDiagnostics, WriterPressure,
};
use crate::store::RestartPolicy;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Schema version for store resource evidence bodies.
pub const STORE_RESOURCE_REPORT_SCHEMA_VERSION: u32 = 1;

/// Hash alias for store resource report bodies (`body_hash`).
pub type StoreResourceHash = [u8; 32];

/// Error returned when store resource evidence generation fails.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreResourceReportError {
    /// Canonical body encoding failed.
    BodyEncoding {
        /// Human-readable encoding error.
        message: String,
    },
}

impl std::fmt::Display for StoreResourceReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyEncoding { message } => {
                write!(f, "store resource report body encoding failed: {message}")
            }
        }
    }
}

impl std::error::Error for StoreResourceReportError {}

/// Writer restart policy shape captured in resource evidence (no extra behavior).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StoreResourceRestartPolicyShape {
    /// At most one automatic restart after a writer panic.
    Once,
    /// Bounded restarts within a rolling millisecond window.
    Bounded {
        /// Maximum restarts permitted within the time window.
        max_restarts: u32,
        /// Rolling window length in milliseconds.
        within_ms: u64,
    },
}

fn restart_policy_shape(policy: &RestartPolicy) -> StoreResourceRestartPolicyShape {
    match policy {
        RestartPolicy::Once => StoreResourceRestartPolicyShape::Once,
        RestartPolicy::Bounded {
            max_restarts,
            within_ms,
        } => StoreResourceRestartPolicyShape::Bounded {
            max_restarts: *max_restarts,
            within_ms: *within_ms,
        },
        // justifies: forward compatible default for future RestartPolicy variants on this non exhaustive enum without changing stable resource evidence shape; anchor src/store/write/writer.rs
        #[allow(unreachable_patterns)]
        _ => StoreResourceRestartPolicyShape::Once,
    }
}

/// Stable digest of the store data directory identity.
///
/// Existing paths are canonicalized before hashing so equivalent spellings of
/// the same directory share one identity. If canonicalization fails, the raw
/// path bytes remain the fallback identity material.
#[must_use]
pub fn store_data_dir_identity_hash(path: &Path) -> StoreResourceHash {
    let canonical;
    let identity_path = match std::fs::canonicalize(path) {
        Ok(path) => {
            canonical = path;
            canonical.as_path()
        }
        Err(_) => path,
    };
    let bytes =
        crate::store::platform::path_identity::path_bytes_for_identity_digest(identity_path);
    crate::evidence::content_hash(&bytes)
}

/// Frontier coordinates serialized without `HlcPoint` to keep the body fully serde-stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreResourceFrontierBody {
    /// Accepted watermark wall clock (ms).
    pub accepted_wall_ms: u64,
    /// Accepted watermark global sequence.
    pub accepted_global_sequence: u64,
    /// Written watermark wall clock (ms).
    pub written_wall_ms: u64,
    /// Written watermark global sequence.
    pub written_global_sequence: u64,
    /// Durable watermark wall clock (ms).
    pub durable_wall_ms: u64,
    /// Durable watermark global sequence.
    pub durable_global_sequence: u64,
    /// Visible watermark wall clock (ms).
    pub visible_wall_ms: u64,
    /// Visible watermark global sequence.
    pub visible_global_sequence: u64,
    /// Applied watermark wall clock (ms).
    pub applied_wall_ms: u64,
    /// Applied watermark global sequence.
    pub applied_global_sequence: u64,
    /// Emitted watermark wall clock (ms).
    pub emitted_wall_ms: u64,
    /// Emitted watermark global sequence.
    pub emitted_global_sequence: u64,
    /// Signed visible-minus-durable sequence gap at snapshot time.
    pub visible_minus_durable_seq: i64,
    /// Oldest undurable write age in milliseconds when known.
    pub oldest_pending_write_age_ms: Option<u64>,
}

impl From<FrontierView> for StoreResourceFrontierBody {
    fn from(f: FrontierView) -> Self {
        Self {
            accepted_wall_ms: f.accepted_hlc.wall_ms,
            accepted_global_sequence: f.accepted_hlc.global_sequence,
            written_wall_ms: f.written_hlc.wall_ms,
            written_global_sequence: f.written_hlc.global_sequence,
            durable_wall_ms: f.durable_hlc.wall_ms,
            durable_global_sequence: f.durable_hlc.global_sequence,
            visible_wall_ms: f.visible_hlc.wall_ms,
            visible_global_sequence: f.visible_hlc.global_sequence,
            applied_wall_ms: f.applied_hlc.wall_ms,
            applied_global_sequence: f.applied_hlc.global_sequence,
            emitted_wall_ms: f.emitted_hlc.wall_ms,
            emitted_global_sequence: f.emitted_hlc.global_sequence,
            visible_minus_durable_seq: f.visible_minus_durable_seq,
            oldest_pending_write_age_ms: f.oldest_pending_write_age_ms,
        }
    }
}

/// Deterministic store resource / diagnostics snapshot body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreResourceReportBody {
    /// Schema version for this evidence shape.
    pub schema_version: u32,
    /// Identity hash over canonical data-directory path bytes (never the raw path).
    pub data_dir_identity_hash: StoreResourceHash,
    /// Events currently indexed.
    pub event_count: u64,
    /// Global sequence allocator.
    pub global_sequence: u64,
    /// Visibility sequence bound.
    pub visible_sequence: u64,
    /// Segment rotation bound from config.
    pub segment_max_bytes: u64,
    /// FD budget from config.
    pub fd_budget: u64,
    /// Writer restart policy shape from config.
    pub restart_policy: StoreResourceRestartPolicyShape,
    /// Writer mailbox pressure snapshot.
    pub writer_pressure: WriterPressure,
    /// Frontier coordinates at snapshot time.
    pub frontier: StoreResourceFrontierBody,
    /// Index topology label at snapshot time.
    pub index_topology: String,
    /// Tile count from index overlay accounting.
    pub tile_count: u64,
    /// Cold-start open report when present (structural cold-start truth only).
    pub open_report: Option<OpenIndexReport>,
    /// Platform evidence summary for the configured store path.
    pub platform_evidence: PlatformEvidenceSummary,
}

/// Build a canonical body from live diagnostics.
#[must_use]
pub fn store_resource_report_body_from_diagnostics(
    d: &StoreDiagnostics,
) -> StoreResourceReportBody {
    StoreResourceReportBody {
        schema_version: STORE_RESOURCE_REPORT_SCHEMA_VERSION,
        data_dir_identity_hash: store_data_dir_identity_hash(&d.data_dir),
        event_count: u64::try_from(d.event_count).unwrap_or(u64::MAX),
        global_sequence: d.global_sequence,
        visible_sequence: d.visible_sequence,
        segment_max_bytes: d.segment_max_bytes,
        fd_budget: u64::try_from(d.fd_budget).unwrap_or(u64::MAX),
        restart_policy: restart_policy_shape(&d.restart_policy),
        writer_pressure: d.writer_pressure,
        frontier: StoreResourceFrontierBody::from(d.frontier),
        index_topology: d.index_topology.to_string(),
        tile_count: u64::try_from(d.tile_count).unwrap_or(u64::MAX),
        open_report: d.open_report.clone(),
        platform_evidence: d.platform_evidence.clone(),
    }
}

/// Canonical `body_hash` over the report body.
///
/// # Errors
/// Canonical MessagePack encode failure.
pub fn store_resource_report_body_hash(
    body: &StoreResourceReportBody,
) -> Result<StoreResourceHash, StoreResourceReportError> {
    crate::evidence::report_body_hash(body, |message| StoreResourceReportError::BodyEncoding {
        message,
    })
}

/// Store resource evidence report envelope (metadata outside deterministic identity).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreResourceEvidenceReport {
    /// Deterministic report body.
    pub body: StoreResourceReportBody,
    /// Canonical hash of `body`.
    pub body_hash: StoreResourceHash,
    /// Optional generation timestamp metadata outside deterministic identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside deterministic identity.
    pub batpak_version: Option<String>,
    /// Optional diagnostics outside deterministic identity.
    pub diagnostics: Vec<String>,
}

/// Preferred alias for callers who speak in “envelope” vocabulary.
pub type StoreResourceEnvelope = StoreResourceEvidenceReport;

/// Build evidence from diagnostics, including `body_hash`.
///
/// # Errors
/// Canonical body encoding failure while computing `body_hash`.
pub fn store_resource_evidence_report_from_diagnostics(
    d: &StoreDiagnostics,
) -> Result<StoreResourceEvidenceReport, StoreResourceReportError> {
    let body = store_resource_report_body_from_diagnostics(d);
    let body_hash = store_resource_report_body_hash(&body)?;
    Ok(StoreResourceEvidenceReport {
        body,
        body_hash,
        generated_at_unix_ms: None,
        batpak_version: None,
        diagnostics: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::store_data_dir_identity_hash;

    #[test]
    fn data_dir_identity_hash_canonicalizes_existing_path_spellings() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let raw_spelling = dir.path().join(".");
        let canonical = std::fs::canonicalize(dir.path()).expect("canonicalize temp dir");

        assert_eq!(
            store_data_dir_identity_hash(&raw_spelling),
            store_data_dir_identity_hash(&canonical)
        );
    }
}
