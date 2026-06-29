//! Cross-platform scaffolding backends — honest family matrix, empty machine ceiling.
//!
//! Mirrors the windows/macos/wasm `backend_impl` stubs (step a): the family
//! `support_matrix()` may advertise `Enforced`, but `profile()` derives a
//! conservative empty ceiling until real syscalls land, so `BoundaryPlanner::plan`
//! fails closed. Used by refusal proofs on any host without enabling OS features.

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

/// A scaffolding backend: honest family support, empty machine ceiling.
pub struct ScaffoldingBackend {
    id: BackendId,
    support: SupportMatrix,
}

impl ScaffoldingBackend {
    /// Windows scaffolding (`windows` id + honest SCOPE §4 matrix).
    #[must_use]
    pub fn windows() -> Self {
        Self::new("windows", super::windows::support_matrix())
    }

    /// macOS scaffolding (`macos` id + honest SCOPE §4 matrix).
    #[must_use]
    pub fn macos() -> Self {
        Self::new("macos", super::macos::support_matrix())
    }

    /// Wasm scaffolding (`wasm` id + honest SCOPE §4 matrix).
    #[must_use]
    pub fn wasm() -> Self {
        Self::new("wasm", super::wasm::support_matrix())
    }

    fn new(id: &str, support: SupportMatrix) -> Self {
        Self {
            id: BackendId::new(id),
            support,
        }
    }
}

impl Backend for ScaffoldingBackend {
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
        BackendProfile::from_ceiling(BTreeMap::new())
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn mechanism(&self, requirement: &BoundaryRequirement, enforcement: Enforcement) -> String {
        let primitive = match RequirementKind::of(requirement) {
            RequirementKind::Filesystem => "scaffolding_fs",
            RequirementKind::NetworkDenyAll => "scaffolding_net_deny",
            RequirementKind::NetworkAllowList => "scaffolding_net_allow",
            RequirementKind::ChildSpawnDenyNewTasks
            | RequirementKind::ChildSpawnAllowThreads
            | RequirementKind::ChildSpawnAllowDescendants => "scaffolding_spawn",
            RequirementKind::Environment => "scaffolding_env",
            RequirementKind::InheritedFdsNone | RequirementKind::InheritedFdsOnly => {
                "scaffolding_fds"
            }
            RequirementKind::LaunchWorkload => "scaffolding_launch",
            RequirementKind::CaptureStreams => "scaffolding_capture",
            RequirementKind::Kill => "scaffolding_kill",
            RequirementKind::TempRoot => "scaffolding_temp",
            RequirementKind::ExposePath => "scaffolding_expose",
            RequirementKind::CommitArtifact | RequirementKind::DiscardArtifact => {
                "scaffolding_artifact"
            }
            RequirementKind::ListOutputs => "scaffolding_list",
        };
        format!("{}:{primitive}:{enforcement:?}", self.id)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        let observed = vec![ObservedFact {
            kind: "backend_scaffolding".to_string(),
            detail: format!(
                "{} scaffolding execute() stub; no platform syscalls performed",
                self.id
            ),
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
