//! bvisor-core — platform-agnostic boundary contract.
//!
//! **ZERO OS code, ZERO BatPak writes in the [`Backend`] trait.**
//!
//! bvisor is a sync-first boundary supervisor. The Boundary Adapter Layer (BAL,
//! prose only) lowers admitted [`Capability`]s + [`HostControl`]s from a
//! [`BoundaryPlan`] into honest platform [`Backend`]s; the [`BoundaryRunner`]
//! SEALS a [`BoundaryReport`] and the host PERSISTS it into BatPak.
//!
//! The loop (sealing ownership made precise):
//! ```text
//! BoundarySpec     asks      (Capabilities + HostControls + workload)
//! BoundaryPlanner  admits    → BoundaryPlan   (inert IR, bound to backend + machine profile; fail-closed)
//! Backend          OBSERVES  → BoundaryReportBody (unsealed facts; no hashing, no BatPak)
//! BoundaryRunner   SEALS     → BoundaryReport (canonicalize + body_hash)  [SEALED ≠ persisted]
//! Host             PERSISTS  → appends 0xE event into BatPak              [now durable; replay reconstructs]
//! ```
//!
//! C0 scope: the pure contract + a fail-closed planner + the honest
//! [`InertBackend`]. No real OS backends, no `SimBackend`, no host wiring yet.

mod backend;
mod contract;

#[cfg(feature = "dangerous-test-hooks")]
mod sim;

pub use backend::inert::InertBackend;

pub use contract::backend::Backend;
pub use contract::capability::{
    Capability, Enforcement, EnvPolicy, EvidenceClaim, EvidenceSet, FdPolicy, FsAccess,
    FsConfinement, NetDest, NetPolicy, PathSet, SpawnPolicy, SupportVerdict,
};
pub use contract::events::{
    BoundaryDispositionEvent, BoundaryRecoveryEvent, BoundaryReportEvent, BoundaryStartedEvent,
    DispositionAction, DispositionPhase,
};
pub use contract::host_control::{
    CommitDurability, HostControl, KillGuarantee, KillTarget, PathView, StdStreams,
};
pub use contract::ids::{
    ArtifactId, AttemptId, BackendId, BackendProfileHash, BoundaryPlanHash, BoundaryReportHash,
    ContentHash, Digest32,
};
pub use contract::plan::{
    AdmittedRequirement, BoundaryPlan, BoundaryRequirement, BoundarySpec, Budgets,
    EvidenceRequirements, PlanError, Workload, BOUNDARY_PLAN_SCHEMA_VERSION,
};
pub use contract::recovery::{QuarantineRecord, RecoveryClassification};
pub use contract::registry::{BackendRegistry, BoundaryPlanner, BoundaryRunner};
pub use contract::report::{
    BoundaryFinding, BoundaryReport, BoundaryReportBody, CaptureRefs, DeniedAttempt, ExitStatus,
    ObservedFact, Outcome, StagedArtifact, BOUNDARY_REPORT_SCHEMA_VERSION,
};
pub use contract::support::{BackendProfile, BackendProfileSnapshot, SupportMatrix};

/// Doc-hidden, test-only surface for the SimBackend monster, the harness-owned
/// [`sim::ground_truth::GroundTruth`] shadow oracle, the G1–G13 proof grid, and
/// the startup-reconciliation matrix. Compiled out unless the
/// `dangerous-test-hooks` feature is on, exactly like batpak's `__sim`.
///
/// THE MONSTER NEVER GRADES ITSELF: the grid + reconciliation oracles diff a
/// harness-owned [`sim::ground_truth::GroundTruth`] (what ACTUALLY happened)
/// against the backend's self-reported [`BoundaryReport`].
#[cfg(feature = "dangerous-test-hooks")]
#[doc(hidden)]
pub mod __sim {
    pub use crate::sim::backend::{LieInjector, LieMode, OneShotLiar, SeededLiar, SimBackend};
    pub use crate::sim::grid::{
        run_gate, GateKind, GateOutcome, GateScenario, GateViolation, GATE_SCENARIOS,
    };
    pub use crate::sim::ground_truth::{GroundTruth, GroundTruthDiff, Lie};
    pub use crate::sim::reconciliation_matrix::{
        all_crash_boundaries, reconciliation_replay_seed, run_reconciliation_matrix, CrashBoundary,
        ReconCell, ReconClass, ReconViolation,
    };
}
