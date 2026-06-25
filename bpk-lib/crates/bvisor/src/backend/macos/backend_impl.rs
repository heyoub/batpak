//! [`MacosBackend`] struct (scaffolding stub).
//!
//! STEP (a): wires the honest [`super::support_matrix`] into the [`Backend`]
//! trait. `probe`/`profile` are PURE placeholders; `execute` is an honest STUB
//! returning [`Outcome::Unsupported`] (Seatbelt `sandbox_init` + pgid lowering
//! lands in step (e), in the [`super::sys`] unsafe basement).

use crate::contract::backend::Backend;
use crate::contract::budget::BudgetProfile;
use crate::contract::budget_witness::BudgetWitnesses;
use crate::contract::capability::{Enforcement, SupportVerdict};
use crate::contract::ids::BackendId;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::{
    BoundaryReportBody, CaptureRefs, DeniedAttempt, ObservedFact, Outcome, StagedArtifact,
    BOUNDARY_REPORT_SCHEMA_VERSION,
};
use crate::contract::support::{
    BackendProfile, BackendProfileSnapshot, RequirementKind, SupportMatrix,
};
use std::collections::BTreeMap;

/// The macOS boundary backend: Seatbelt + pgid (scaffolding stub).
pub struct MacosBackend {
    id: BackendId,
    support: SupportMatrix,
}

impl MacosBackend {
    /// The stable id of the macOS backend.
    pub const ID: &'static str = "macos";

    /// Construct the macOS backend with its honest support matrix (SCOPE §4).
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
        }
    }
}

impl Default for MacosBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for MacosBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn support(&self) -> &SupportMatrix {
        &self.support
    }

    fn probe(&self) -> BackendProfileSnapshot {
        let mut probed = BTreeMap::new();
        probed.insert("scaffolding".to_string(), "true".to_string());
        probed.insert("confinement".to_string(), "unimplemented".to_string());
        BackendProfileSnapshot {
            backend: self.id.clone(),
            probed,
            budget: BudgetProfile::all_unenforced(),
        }
    }

    fn profile(&self, _snap: &BackendProfileSnapshot) -> BackendProfile {
        // Conservative empty ceiling until the real syscalls exist (step e).
        BackendProfile::from_ceiling(BTreeMap::new())
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn mechanism(&self, requirement: &BoundaryRequirement, enforcement: Enforcement) -> String {
        let primitive = match RequirementKind::of(requirement) {
            RequirementKind::Filesystem => "seatbelt_profile",
            RequirementKind::NetworkDenyAll => "seatbelt_deny_net",
            RequirementKind::NetworkAllowList => "none/unsupported",
            RequirementKind::Environment => "env_clear",
            RequirementKind::InheritedFdsNone | RequirementKind::InheritedFdsOnly => "fd_cloexec",
            RequirementKind::LaunchWorkload => "posix_spawn",
            RequirementKind::CaptureStreams => "pipe",
            RequirementKind::Kill => "killpg",
            RequirementKind::TempRoot => "tmpdir",
            RequirementKind::ExposePath => "none/unsupported",
            RequirementKind::CommitArtifact | RequirementKind::DiscardArtifact => "rename_same_fs",
            RequirementKind::ListOutputs => "readdir",
            RequirementKind::ChildSpawnDeny | RequirementKind::ChildSpawnAllow => "posix_spawn_child",
        };
        format!("{}:{primitive}:{enforcement:?}", self.id)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        let observed = vec![ObservedFact {
            kind: "backend_scaffolding".to_string(),
            detail: "macos backend execute() is a step-(a) stub; no syscalls performed".to_string(),
        }];
        BoundaryReportBody {
            schema_version: BOUNDARY_REPORT_SCHEMA_VERSION,
            plan_id: plan.plan_id,
            backend: self.id.clone(),
            profile: self.probe(),
            outcome: Outcome::Unsupported,
            admitted: plan.admitted.clone(),
            observed,
            denied: Vec::<DeniedAttempt>::new(),
            exit: None,
            captured: CaptureRefs::default(),
            budget: BudgetWitnesses::unwitnessed(&plan.budgets),
            artifacts: Vec::<StagedArtifact>::new(),
            findings: Vec::new(),
        }
    }
}
