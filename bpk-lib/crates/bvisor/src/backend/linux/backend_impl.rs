//! [`LinuxBackend`] struct (scaffolding stub).
//!
//! STEP (a): wires the honest [`super::support_matrix`] into the [`Backend`]
//! trait. `probe`/`profile` are PURE (raw strings, NO OS calls yet — step (b)
//! populates them from real ABI/cgroup/pidfd detection in [`super::sys`]).
//! `execute` is an honest STUB: it claims NOTHING was enforced and returns
//! [`Outcome::Unsupported`], because the real syscall lowering does not exist yet.
//! Returning anything stronger would be the exact lie the gauntlet must catch.

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

/// The Linux boundary backend: landlock + cgroup-v2 + pidfd (scaffolding stub).
pub struct LinuxBackend {
    id: BackendId,
    support: SupportMatrix,
}

impl LinuxBackend {
    /// The stable id of the Linux backend.
    pub const ID: &'static str = "linux";

    /// Construct the Linux backend with its honest support matrix (SCOPE §4).
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
        }
    }
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for LinuxBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn support(&self) -> &SupportMatrix {
        &self.support
    }

    fn probe(&self) -> BackendProfileSnapshot {
        // SCAFFOLDING: raw probe strings are placeholders — NO OS calls yet. Step
        // (b) replaces these with real landlock-ABI / cgroup-v2 / pidfd detection
        // performed in `super::sys`. Deterministic so replay re-derives identically.
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
        // SCAFFOLDING: a CONSERVATIVE ceiling — until real syscalls exist the
        // machine can back NOTHING, so the ceiling is empty (every kind floors to
        // the fail-closed bottom). This makes `classify` honest before step (b):
        // even where the family CLAIMS Enforced, the machine ceiling is bottom, so
        // `plan()` fails closed rather than admitting an unbacked guarantee.
        BackendProfile::from_ceiling(BTreeMap::new())
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn mechanism(&self, requirement: &BoundaryRequirement, enforcement: Enforcement) -> String {
        // Each backend AUTHORS its own mechanism vocabulary. Keyed off the
        // requirement KIND so it stays exhaustive and payload-independent.
        let primitive = match RequirementKind::of(requirement) {
            RequirementKind::Filesystem => "landlock",
            RequirementKind::NetworkDenyAll => "net_namespace",
            RequirementKind::NetworkAllowList => "none/unsupported-v1",
            RequirementKind::ChildSpawn => "clone3",
            RequirementKind::Environment => "env_clear",
            RequirementKind::InheritedFds => "fd_cloexec",
            RequirementKind::LaunchWorkload => "clone3+exec",
            RequirementKind::CaptureStreams => "pipe2",
            RequirementKind::Kill => "cgroup.kill+pidfd",
            RequirementKind::TempRoot => "tmpfs+pivot_root",
            RequirementKind::ExposePath => "bind_mount",
            RequirementKind::CommitArtifact | RequirementKind::DiscardArtifact => "rename_same_fs",
            RequirementKind::ListOutputs => "readdir",
        };
        format!("{}:{primitive}:{enforcement:?}", self.id)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        // HONEST STUB (step a): the syscall lowering does not exist yet, so the
        // backend enforced NOTHING and ran NOTHING. Report `Unsupported` with the
        // honest observed fact. Step (b) replaces this with real lowering whose
        // observed/denied are reconciled against live GroundTruth.
        let observed = vec![ObservedFact {
            kind: "backend_scaffolding".to_string(),
            detail: "linux backend execute() is a step-(a) stub; no syscalls performed".to_string(),
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
