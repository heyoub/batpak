//! Spec → Plan: the inert IR and its fail-closed admission types.

use crate::contract::capability::{Capability, Enforcement, EvidenceClaim, EvidenceSet};
use crate::contract::host_control::HostControl;
use crate::contract::ids::{BackendId, BoundaryPlanHash};
use crate::contract::support::BackendProfileSnapshot;
use serde::{Deserialize, Serialize};

/// Schema version for [`BoundaryPlan`] bodies.
pub const BOUNDARY_PLAN_SCHEMA_VERSION: u16 = 1;

/// The unit the matrix/profile classifies: a guest [`Capability`] OR a
/// host-provisioned [`HostControl`]. One `classify`, one path through `plan()`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BoundaryRequirement {
    /// A guest-invokable capability.
    Capability(Capability),
    /// A host-provisioned control.
    HostControl(HostControl),
}

/// What the boundary runs. `Wasm` is always present in the type; a real wasm
/// backend is gated later, but the contract stays uniform.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Workload {
    /// A native process workload.
    Process {
        /// Executable path, as a portable string.
        exe: String,
        /// Arguments passed to the executable.
        args: Vec<String>,
    },
    /// A WASM guest workload (bvisor runs the guest; it does not run in wasm).
    Wasm {
        /// Reference to the wasm module, as a portable string.
        module_ref: String,
    },
}

/// Resource budgets for a boundary. Minimal in C0; widened as backends land.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Budgets {
    /// Wall-clock timeout in milliseconds; `None` means no timeout.
    pub wall_clock_ms: Option<u64>,
    /// Memory ceiling in bytes; `None` means no explicit ceiling.
    pub memory_bytes: Option<u64>,
}

/// What evidence the caller requires the report to carry. Minimal in C0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceRequirements {
    /// Require captured stdout/stderr refs in the report.
    pub require_captured_streams: bool,
    /// Require the exit status in the report.
    pub require_exit_status: bool,
}

impl EvidenceRequirements {
    /// The set of [`EvidenceClaim`]s the caller requires the plan to be able to
    /// produce. Planning admits only when this is a subset of the union of the
    /// admitted requirements' verdict evidence (required ⊆ available); otherwise
    /// it fails closed with [`PlanError::EvidenceUnsatisfiable`].
    #[must_use]
    pub fn required_claims(&self) -> EvidenceSet {
        let mut set = EvidenceSet::new();
        if self.require_captured_streams {
            set.insert(EvidenceClaim::CapturedStreams);
        }
        if self.require_exit_status {
            set.insert(EvidenceClaim::TerminalOutcome);
        }
        set
    }
}

/// The caller's request: authority + controls + workload + budgets + evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundarySpec {
    /// The workload to run.
    pub workload: Workload,
    /// Guest authority requested.
    pub capabilities: Vec<Capability>,
    /// Host lifecycle requested.
    pub controls: Vec<HostControl>,
    /// Resource budgets.
    pub budgets: Budgets,
    /// Evidence the report must carry.
    pub evidence: EvidenceRequirements,
}

/// One admitted requirement with its verdict and the ACTUAL mechanism chosen.
///
/// INVARIANT: `enforcement` is [`Enforcement::Enforced`] or
/// [`Enforcement::Mediated`], never [`Enforcement::Unsupported`] — the plan
/// fails closed before an unsupported requirement is admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedRequirement {
    /// The admitted requirement.
    pub requirement: BoundaryRequirement,
    /// The verdict (Enforced | Mediated; never Unsupported here).
    pub enforcement: Enforcement,
    /// The ACTUAL backend mechanism chosen, as evidence — e.g.
    /// `"pivot_root+landlock_abi4"`, `"job_object"`, `"preopen"`,
    /// `"rename_same_fs"`, `"cgroup.kill+pidfd"`, `"none/no-confinement"`.
    pub mechanism: String,
}

/// Admitted authority + controls. INERT typed data — not bytecode/executable.
/// Bound to ONE backend AND the machine profile it was admitted against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryPlan {
    /// Plan body schema version.
    pub schema_version: u16,
    /// Canonical content hash of this plan (its identity).
    pub plan_id: BoundaryPlanHash,
    /// The backend the plan is bound to.
    pub backend: BackendId,
    /// What THIS machine could do at plan time (raw evidence, audit/replay).
    pub profile: BackendProfileSnapshot,
    /// Every admitted requirement (each Enforced or Mediated, never Unsupported).
    pub admitted: Vec<AdmittedRequirement>,
    /// The workload to run.
    pub workload: Workload,
    /// Resource budgets.
    pub budgets: Budgets,
    /// Evidence the report must carry.
    pub evidence: EvidenceRequirements,
}

/// Why a boundary could not be planned. The planner fails closed: any
/// unsupported REQUIRED requirement aborts admission rather than degrading.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlanError {
    /// A required requirement classified as [`Enforcement::Unsupported`].
    Unsupported {
        /// The requirement the backend cannot honor.
        requirement: BoundaryRequirement,
        /// The backend that returned the verdict.
        backend: BackendId,
    },
    /// The backend cannot run this workload shape at all.
    WorkloadIncompatible {
        /// The backend.
        backend: BackendId,
        /// The incompatible workload.
        workload: Workload,
    },
    /// The machine lacks a primitive a required requirement depends on.
    ProfileInsufficient {
        /// The backend.
        backend: BackendId,
        /// Human-readable detail.
        detail: String,
    },
    /// The requested budgets are invalid.
    BudgetInvalid {
        /// Human-readable detail.
        detail: String,
    },
    /// The required evidence cannot be produced by this backend.
    EvidenceUnsatisfiable {
        /// The backend.
        backend: BackendId,
        /// Human-readable detail.
        detail: String,
    },
    /// The named backend is not registered.
    UnknownBackend {
        /// The unknown backend id.
        backend: BackendId,
    },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported {
                requirement,
                backend,
            } => write!(
                f,
                "backend {backend} cannot enforce required requirement {requirement:?}"
            ),
            Self::WorkloadIncompatible { backend, workload } => {
                write!(f, "backend {backend} cannot run workload {workload:?}")
            }
            Self::ProfileInsufficient { backend, detail } => {
                write!(
                    f,
                    "backend {backend} machine profile insufficient: {detail}"
                )
            }
            Self::BudgetInvalid { detail } => write!(f, "invalid budget: {detail}"),
            Self::EvidenceUnsatisfiable { backend, detail } => write!(
                f,
                "backend {backend} cannot satisfy required evidence: {detail}"
            ),
            Self::UnknownBackend { backend } => write!(f, "unknown backend {backend}"),
        }
    }
}

impl std::error::Error for PlanError {}
