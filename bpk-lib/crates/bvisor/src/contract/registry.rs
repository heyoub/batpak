//! The "BAL" in code: [`BackendRegistry`] + [`BoundaryPlanner`] +
//! [`BoundaryRunner`]. No `struct Bal`.

use crate::contract::backend::Backend;
use crate::contract::capability::Enforcement;
use crate::contract::host_control::HostControl;
use crate::contract::ids::{BackendId, BoundaryPlanHash};
use crate::contract::plan::{
    AdmittedRequirement, BoundaryPlan, BoundaryRequirement, BoundarySpec, PlanError,
    BOUNDARY_PLAN_SCHEMA_VERSION,
};
use crate::contract::report::{BoundaryReport, BoundaryReportBody};
use crate::contract::support::{BackendProfile, BackendProfileSnapshot};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Holds the registered backends, keyed by [`BackendId`].
#[derive(Clone, Default)]
pub struct BackendRegistry {
    backends: BTreeMap<BackendId, Arc<dyn Backend>>,
}

impl BackendRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a backend under its own id.
    pub fn register(&mut self, backend: Arc<dyn Backend>) {
        self.backends.insert(backend.id(), backend);
    }

    /// Look up a registered backend by id.
    #[must_use]
    pub fn backend(&self, id: &BackendId) -> Option<&Arc<dyn Backend>> {
        self.backends.get(id)
    }
}

/// Admits a [`BoundarySpec`] against a chosen backend, FAIL-CLOSED.
pub struct BoundaryPlanner<'r> {
    registry: &'r BackendRegistry,
}

impl<'r> BoundaryPlanner<'r> {
    /// Bind a planner to a registry.
    #[must_use]
    pub fn new(registry: &'r BackendRegistry) -> Self {
        Self { registry }
    }

    /// Probe the chosen backend, derive its typed profile, classify every
    /// requirement, and admit — failing closed if any required requirement is
    /// [`Enforcement::Unsupported`].
    ///
    /// # Errors
    /// Returns [`PlanError`] for an unknown backend, an unsupported required
    /// requirement, or (on a hash failure) a profile-insufficient verdict.
    pub fn plan(
        &self,
        spec: &BoundarySpec,
        backend: &BackendId,
    ) -> Result<BoundaryPlan, PlanError> {
        let backend = self
            .registry
            .backend(backend)
            .ok_or_else(|| PlanError::UnknownBackend {
                backend: backend.clone(),
            })?;

        let snapshot = backend.probe();
        let profile = backend.profile(&snapshot);

        let mut admitted = Vec::new();
        for capability in &spec.capabilities {
            let req = BoundaryRequirement::Capability(capability.clone());
            admitted.push(admit_one(backend.as_ref(), req, &profile)?);
        }
        for control in &spec.controls {
            let req = BoundaryRequirement::HostControl(control.clone());
            admitted.push(admit_one(backend.as_ref(), req, &profile)?);
        }

        let plan_id =
            compute_plan_id(backend.id(), &snapshot, &admitted, spec).map_err(|error| {
                PlanError::ProfileInsufficient {
                    backend: backend.id(),
                    detail: format!("plan canonicalization failed: {error}"),
                }
            })?;

        Ok(BoundaryPlan {
            schema_version: BOUNDARY_PLAN_SCHEMA_VERSION,
            plan_id,
            backend: backend.id(),
            profile: snapshot,
            admitted,
            workload: spec.workload.clone(),
            budgets: spec.budgets,
            evidence: spec.evidence,
        })
    }
}

/// Classify one requirement; admit it (Enforced/Mediated) or fail closed.
fn admit_one(
    backend: &dyn Backend,
    requirement: BoundaryRequirement,
    profile: &BackendProfile,
) -> Result<AdmittedRequirement, PlanError> {
    let enforcement = backend.classify(&requirement, profile);
    match enforcement {
        Enforcement::Enforced | Enforcement::Mediated => Ok(AdmittedRequirement {
            mechanism: mechanism_for(backend.id(), &requirement, enforcement),
            requirement,
            enforcement,
        }),
        Enforcement::Unsupported => Err(PlanError::Unsupported {
            requirement,
            backend: backend.id(),
        }),
    }
}

/// The mechanism evidence string a backend records for an admitted requirement.
///
/// In C0 only the honest no-confinement reference backend admits anything, so
/// the mechanism reflects exactly what it does (host launch + stdio wiring with
/// no confinement). Real backends record their concrete primitive here.
fn mechanism_for(
    backend: BackendId,
    requirement: &BoundaryRequirement,
    enforcement: Enforcement,
) -> String {
    let primitive = match requirement {
        BoundaryRequirement::HostControl(HostControl::LaunchWorkload) => "host_spawn",
        BoundaryRequirement::HostControl(HostControl::CaptureStreams { .. }) => "host_pipe",
        // Everything else Inert can admit is a no-confinement restriction (e.g.
        // Network::DenyAll), so it records no real mechanism. Real backends name
        // their concrete primitive (landlock / job_object / preopen / …) here.
        BoundaryRequirement::Capability(_) | BoundaryRequirement::HostControl(_) => {
            "none/no-confinement"
        }
    };
    format!("{backend}:{primitive}:{enforcement:?}")
}

/// Canonical plan identity: hash of the plan core (backend + snapshot + admitted
/// + workload + budgets + evidence). Sorts the admitted set by requirement so
/// the digest is stable regardless of admission order.
fn compute_plan_id(
    backend: BackendId,
    snapshot: &BackendProfileSnapshot,
    admitted: &[AdmittedRequirement],
    spec: &BoundarySpec,
) -> Result<BoundaryPlanHash, rmp_serde::encode::Error> {
    let mut sorted_admitted = admitted.to_vec();
    sorted_admitted
        .sort_by(|a, b| format!("{:?}", a.requirement).cmp(&format!("{:?}", b.requirement)));

    let core = PlanFingerprint {
        schema_version: BOUNDARY_PLAN_SCHEMA_VERSION,
        backend,
        snapshot,
        admitted: &sorted_admitted,
        workload: format!("{:?}", spec.workload),
        budgets: format!("{:?}", spec.budgets),
        evidence: format!("{:?}", spec.evidence),
    };
    let bytes = batpak::canonical::to_bytes(&core)?;
    Ok(BoundaryPlanHash(batpak::event::hash::compute_hash(&bytes)))
}

#[derive(serde::Serialize)]
struct PlanFingerprint<'a> {
    schema_version: u16,
    backend: BackendId,
    snapshot: &'a BackendProfileSnapshot,
    admitted: &'a [AdmittedRequirement],
    workload: String,
    budgets: String,
    evidence: String,
}

/// Executes a plan via its bound backend, then SEALS the observed body.
pub struct BoundaryRunner<'r> {
    registry: &'r BackendRegistry,
}

impl<'r> BoundaryRunner<'r> {
    /// Bind a runner to a registry.
    #[must_use]
    pub fn new(registry: &'r BackendRegistry) -> Self {
        Self { registry }
    }

    /// Execute via the bound backend (which OBSERVES), then SEAL the observed
    /// body = canonicalize + compute `body_hash` → [`BoundaryReport`]. SEAL
    /// means hashed + canonical; it does NOT persist. The host appends it.
    ///
    /// # Errors
    /// Returns [`PlanError::UnknownBackend`] if the plan's bound backend is no
    /// longer registered, or [`PlanError::ProfileInsufficient`] if sealing the
    /// observed body fails to canonical-encode.
    pub fn run(&self, plan: &BoundaryPlan) -> Result<BoundaryReport, PlanError> {
        let backend =
            self.registry
                .backend(&plan.backend)
                .ok_or_else(|| PlanError::UnknownBackend {
                    backend: plan.backend.clone(),
                })?;

        let body: BoundaryReportBody = backend.execute(plan);
        let body_hash = body
            .body_hash()
            .map_err(|error| PlanError::ProfileInsufficient {
                backend: plan.backend.clone(),
                detail: format!("report sealing failed: {error}"),
            })?;
        Ok(BoundaryReport { body, body_hash })
    }
}
