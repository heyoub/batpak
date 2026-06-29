//! Report: backend OBSERVES (body) → runner SEALS (report) → host PERSISTS (0xE).
//!
//! The seal mirrors BatPak's `*ReportBody` idiom (see
//! `crates/core/src/store/fork_report.rs`): findings are sorted, the body is
//! canonical-encoded with the crate's named-field MessagePack surface, and the
//! `body_hash` is the blake3 digest of those bytes. "Sealed" = hashed +
//! canonical; it is NOT persisted — the host appends it as a 0xE event.

use crate::contract::budget_witness::BudgetWitnesses;
use crate::contract::capability::Enforcement;
use crate::contract::ids::{
    ArtifactId, BackendId, BoundaryPlanHash, BoundaryReportHash, ContentHash,
};
use crate::contract::plan::{AdmittedRequirement, BoundaryRequirement};
use crate::contract::support::BackendProfileSnapshot;
use serde::{Deserialize, Serialize};

/// Schema version for [`BoundaryReportBody`] bodies.
pub const BOUNDARY_REPORT_SCHEMA_VERSION: u16 = 1;

/// Run-time terminal outcome. DISTINCT from
/// [`crate::RecoveryClassification`], which is a startup-reconciliation verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Outcome {
    /// The workload ran to a normal terminal.
    Completed,
    /// The boundary denied the workload's attempt(s).
    Denied,
    /// The workload failed (non-zero / error terminal).
    Failed,
    /// The workload exceeded its time budget.
    Timeout,
    /// The boundary killed the run tree.
    Killed,
    /// The backend could not honor the plan at execution time.
    Unsupported,
    /// The supervisor itself faulted while running the boundary.
    SupervisorFault,
}

/// A fact the backend OBSERVED about what actually happened.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedFact {
    /// Stable kind tag, e.g. `"workload_launched"`, `"stream_captured"`.
    pub kind: String,
    /// Stable detail string (audit evidence; no decoded domain payload).
    pub detail: String,
}

/// An attempt the boundary blocked.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeniedAttempt {
    /// The requirement whose policy blocked the attempt.
    pub requirement: BoundaryRequirement,
    /// Stable detail of what was attempted and denied.
    pub detail: String,
}

/// References to captured standard streams (not the bytes themselves).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRefs {
    /// Reference to captured stdout, if captured.
    pub stdout: Option<String>,
    /// Reference to captured stderr, if captured.
    pub stderr: Option<String>,
}

/// A record of an artifact the boundary STAGED (produced + quarantined).
///
/// The report STAGES artifacts; it NEVER commits them. Committal/discard is the
/// host's post-report [`crate::BoundaryDispositionEvent`], so a report is never
/// self-authorizing. Identity is the occurrence [`ArtifactId`] (distinct from
/// the byte-identity [`ContentHash`]: two attempts producing identical bytes
/// are distinct occurrences with independent dispositions).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StagedArtifact {
    /// Occurrence identity (derived from attempt + logical slot + content).
    pub artifact_id: ArtifactId,
    /// blake3 content hash of the artifact's bytes (byte identity).
    pub content_hash: ContentHash,
    /// Stable artifact name/path, as a portable string.
    pub name: String,
}

/// Process exit status, portably encoded (no OS `ExitStatus` type).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExitStatus {
    /// Exited with a numeric code.
    Code(i32),
    /// Terminated by a signal number (Unix-shaped; portable evidence).
    Signal(i32),
}

/// A deterministic structural finding, sorted before the body is hashed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BoundaryFinding {
    /// A requirement was admitted at the recorded enforcement level.
    RequirementAdmitted {
        /// The admitted requirement.
        requirement: BoundaryRequirement,
        /// The enforcement level it was admitted at.
        enforcement: Enforcement,
    },
    /// The backend enforced nothing for a requirement (honest no-confinement).
    NoConfinement {
        /// The requirement that received no real confinement.
        requirement: BoundaryRequirement,
    },
}

/// OBSERVED facts, unsealed. Returned by [`crate::Backend::execute`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryReportBody {
    /// Report body schema version.
    pub schema_version: u16,
    /// The plan this report answers.
    pub plan_id: BoundaryPlanHash,
    /// The backend that produced this report.
    pub backend: BackendId,
    /// Raw probe snapshot at execution time (audit/replay evidence).
    pub profile: BackendProfileSnapshot,
    /// The run-time terminal outcome.
    pub outcome: Outcome,
    /// What the backend CLAIMED enforceable (echoed from the plan).
    pub admitted: Vec<AdmittedRequirement>,
    /// What ACTUALLY happened.
    pub observed: Vec<ObservedFact>,
    /// Attempts the boundary blocked.
    pub denied: Vec<DeniedAttempt>,
    /// The workload exit status, if it reached a terminal.
    pub exit: Option<ExitStatus>,
    /// References to captured streams.
    pub captured: CaptureRefs,
    /// Artifacts the boundary STAGED (quarantined; disposition is post-report).
    pub artifacts: Vec<StagedArtifact>,
    /// The seven per-dimension execution-budget witnesses `W_d = (L,G,E,M,O,R,F)` —
    /// the admitted contract echoed plus observed usage + terminal finding. A backend
    /// that does not yet witness a dimension emits an UNWITNESSED echo (usage
    /// unobserved, finding `ObservationUnavailable`) — uncertainty is never fabricated.
    pub budget: BudgetWitnesses,
    /// Structural findings, sorted before hashing (canonical).
    pub findings: Vec<BoundaryFinding>,
}

impl BoundaryReportBody {
    /// Canonical `body_hash`, with findings sorted before encoding.
    ///
    /// Mirrors `fork_report_body_hash`: sort findings, encode with the crate's
    /// canonical named-field MessagePack surface, blake3 the bytes.
    ///
    /// # Errors
    /// MessagePack encoding failure from `rmp-serde`.
    pub fn body_hash(&self) -> Result<BoundaryReportHash, rmp_serde::encode::Error> {
        let mut body = self.clone();
        body.findings.sort();
        let bytes = batpak::canonical::to_bytes(&body)?;
        Ok(BoundaryReportHash(batpak::event::hash::compute_hash(
            &bytes,
        )))
    }
}

/// SEALED = canonicalized + body_hash. NOT persisted; the host appends it (0xE).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryReport {
    /// The deterministic report body.
    pub body: BoundaryReportBody,
    /// Canonical hash of `body`.
    pub body_hash: BoundaryReportHash,
}
