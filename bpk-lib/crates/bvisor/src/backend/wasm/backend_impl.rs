//! [`WasmBackend`] struct (scaffolding stub).
//!
//! STEP (a): wires the honest [`super::support_matrix`] into the [`Backend`]
//! trait. `probe`/`profile` are PURE placeholders; `execute` is an honest STUB
//! returning [`Outcome::Unsupported`] (the `wasmi`/`wasmtime` lowering lands in
//! step (c)). Wasm has NO unsafe basement.

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

/// The Wasm boundary backend: WASI preopen confinement (scaffolding stub).
pub struct WasmBackend {
    id: BackendId,
    support: SupportMatrix,
}

impl WasmBackend {
    /// The stable id of the wasm backend.
    pub const ID: &'static str = "wasm";

    /// Construct the wasm backend with its honest support matrix (SCOPE §4).
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
        }
    }
}

impl Default for WasmBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for WasmBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn support(&self) -> &SupportMatrix {
        &self.support
    }

    fn probe(&self) -> BackendProfileSnapshot {
        let mut probed = BTreeMap::new();
        probed.insert("scaffolding".to_string(), "true".to_string());
        probed.insert("runtime".to_string(), "unimplemented".to_string());
        BackendProfileSnapshot {
            backend: self.id.clone(),
            probed,
            budget: BudgetProfile::all_unenforced(),
        }
    }

    fn profile(&self, _snap: &BackendProfileSnapshot) -> BackendProfile {
        // Conservative empty ceiling until the runtime exists (step c).
        BackendProfile::from_ceiling(BTreeMap::new())
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn mechanism(&self, requirement: &BoundaryRequirement, enforcement: Enforcement) -> String {
        let primitive = match RequirementKind::of(requirement) {
            RequirementKind::Filesystem => "wasi_preopen",
            RequirementKind::NetworkDenyAll => "no_socket_cap",
            RequirementKind::Environment => "wasi_env",
            RequirementKind::LaunchWorkload => "wasi_instantiate",
            RequirementKind::CaptureStreams => "wasi_stdio",
            RequirementKind::TempRoot => "wasi_preopen_tmp",
            RequirementKind::CommitArtifact | RequirementKind::DiscardArtifact => "preopen_commit",
            RequirementKind::ListOutputs => "preopen_readdir",
            // Structurally unsupported on wasm — named honestly.
            RequirementKind::ChildSpawn
            | RequirementKind::Kill
            | RequirementKind::ExposePath
            | RequirementKind::NetworkAllowList
            | RequirementKind::InheritedFds => "none/structurally-unsupported",
        };
        format!("{}:{primitive}:{enforcement:?}", self.id)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        let observed = vec![ObservedFact {
            kind: "backend_scaffolding".to_string(),
            detail: "wasm backend execute() is a step-(a) stub; no guest instantiated".to_string(),
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
