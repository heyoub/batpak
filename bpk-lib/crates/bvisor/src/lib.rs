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

#[cfg(feature = "host")]
pub mod host;

pub use backend::inert::InertBackend;

// The per-platform HONEST support matrices are always available (pure data, so
// the honesty is cross-platform testable). The OS backend STRUCTS are re-exported
// only when their feature + target are both active.
pub use backend::{linux, macos, wasm, windows};

#[cfg(all(feature = "backend-linux", target_os = "linux"))]
pub use backend::linux::LinuxBackend;
#[cfg(all(feature = "backend-macos", target_os = "macos"))]
pub use backend::macos::MacosBackend;
#[cfg(feature = "backend-wasm")]
pub use backend::wasm::WasmBackend;
#[cfg(all(feature = "backend-windows", target_os = "windows"))]
pub use backend::windows::WindowsBackend;

pub use contract::admission::{
    budget_membrane_equivalence_smt, budget_planted_disagreement_smt, smt_digest, translate,
    verify_receipt, ProofGateError, ProofReceipt, ProofStatus, QfBvError, TranslatedCircuit,
    ADMISSION_CIRCUIT_PROOF,
};
pub use contract::admission::{
    compile_admission, compile_budget_detail, compile_budget_membrane, compile_conflict_membrane,
    compile_evidence_membrane, compile_profile_drift_membrane, compile_schedule_membrane,
    compile_support_membrane, compose_membranes, decode_validated, evaluate, planner_reference,
    planner_shadow_check, reference_admission, reference_schedule_admission, schedule_refusal,
    schedule_shadow_check, shadow_check, shape_of, validate, verify_certificate,
    AdmissionDivergence, AdmissionInputs, AdmissionOutcome, AdmissionProgram, AdmissionShape,
    BudgetInputs, CertNode, CircuitBuilder, CompareRel, Decision, EvalError, InputDecl, InputSlot,
    Lane, LimitViolation, LookupTable, Node, NodeId, NodeOp, Outputs, PlannerInputs,
    PrimitiveDeclInputs, ProgramCertificate, ProgramError, ProgramLimits, RequirementInputs,
    ScheduleDivergence, ScheduleInputs, ScheduleOutcome, ScheduleRefusal, ScheduleShape,
    ScheduleSlotInputs, ValidationError, Width, ADMISSION_PROGRAM_SCHEMA_VERSION, FROZEN_LIMITS,
    MAX_LOOKUP_ENTRIES, MAX_PRIMITIVES, MAX_WIDTH,
};
pub use contract::backend::Backend;
pub use contract::budget::{
    adjudicate_dimension, budget_admit, AdmittedBudget, AdmittedBudgets, BudgetAvailability,
    BudgetDimension, BudgetFailure, BudgetProfile, BudgetRefusal, BudgetRequest,
    BudgetRequirements, DerivedMinimums, MinGuarantee,
};
pub use contract::budget_witness::{
    BudgetFinding, BudgetWitness, BudgetWitnesses, GuaranteeProfile,
};
pub use contract::canonical_policy::CanonicalPolicy;
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
    AdmissionProgramHash, ArtifactId, AttemptId, BackendId, BackendProfileHash, BoundaryPlanHash,
    BoundaryReportHash, ContentHash, Digest32,
};
pub use contract::lifecycle::{
    Boundary, BoundaryState, LifecycleError, Planned, Reported, Spec, Started,
};
pub use contract::lowering::{compile_schedule, LoweringError, LoweringSchedule, ScheduleEntry};
pub use contract::plan::{
    AdmittedRequirement, BoundaryPlan, BoundaryRequirement, BoundarySpec, EvidenceRequirements,
    PlanError, Workload, BOUNDARY_PLAN_SCHEMA_VERSION,
};
pub use contract::primitive::{
    classify_via_primitives, LoweringPhase, PrimitiveDecl, PrimitiveId, PrimitiveVersion, Privilege,
};
pub use contract::qualification::{
    enforced_claim_is_qualified, linux_ledger_row, linux_mechanism, MechanismDigest, ProfileFacts,
    ProfileFloor, QualificationRow, QualificationStatus, LINUX_QUALIFICATION_LEDGER,
};
pub use contract::recovery::{
    reconcile, ArtifactFix, ArtifactReality, DispositionState, QuarantineRecord, RecoveryAction,
    RecoveryClassification, RecoveryProbe, RunView,
};
pub use contract::registry::{
    derive_minimums, BackendRegistry, BoundaryPlanner, BoundaryRun, BoundaryRunner, RunStep,
};
pub use contract::report::{
    BoundaryFinding, BoundaryReport, BoundaryReportBody, CaptureRefs, DeniedAttempt, ExitStatus,
    ObservedFact, Outcome, StagedArtifact, BOUNDARY_REPORT_SCHEMA_VERSION,
};
pub use contract::support::{
    BackendProfile, BackendProfileSnapshot, RequirementKind, SupportMatrix,
};

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
    pub use crate::sim::supervisor::{SimProbe, SimRun, SimSupervisor};
}
