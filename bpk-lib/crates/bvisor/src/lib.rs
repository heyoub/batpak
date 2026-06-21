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

pub use backend::inert::InertBackend;

pub use contract::backend::Backend;
pub use contract::capability::{
    Capability, Enforcement, EnvPolicy, FdPolicy, FsAccess, FsConfinement, NetDest, NetPolicy,
    PathSet, SpawnPolicy,
};
pub use contract::events::{BoundaryPlanEvent, BoundaryRecoveryEvent, BoundaryReportEvent};
pub use contract::host_control::{
    CommitDurability, HostControl, KillGuarantee, KillTarget, PathView, StdStreams,
};
pub use contract::ids::{BackendId, BoundaryPlanHash, BoundaryReportHash};
pub use contract::plan::{
    AdmittedRequirement, BoundaryPlan, BoundaryRequirement, BoundarySpec, Budgets,
    EvidenceRequirements, PlanError, Workload, BOUNDARY_PLAN_SCHEMA_VERSION,
};
pub use contract::recovery::{QuarantineRecord, RecoveryClassification};
pub use contract::registry::{BackendRegistry, BoundaryPlanner, BoundaryRunner};
pub use contract::report::{
    ArtifactRecord, BoundaryFinding, BoundaryReport, BoundaryReportBody, CaptureRefs,
    DeniedAttempt, ExitStatus, ObservedFact, Outcome, BOUNDARY_REPORT_SCHEMA_VERSION,
};
pub use contract::support::{BackendProfile, BackendProfileSnapshot, SupportMatrix};
