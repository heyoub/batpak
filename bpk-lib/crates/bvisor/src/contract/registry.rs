//! The "BAL" in code: [`BackendRegistry`] + [`BoundaryPlanner`] +
//! [`BoundaryRunner`]. No `struct Bal`.

use crate::contract::admission::{planner_shadow_check, AdmissionOutcome, PlannerInputs};
use crate::contract::backend::Backend;
use crate::contract::capability::{Enforcement, EvidenceClaim, EvidenceSet, SupportVerdict};
use crate::contract::host_control::HostControl;
use crate::contract::ids::{BackendId, BoundaryPlanHash};
use crate::contract::plan::{
    AdmittedRequirement, BoundaryPlan, BoundaryRequirement, BoundarySpec, PlanError,
    BOUNDARY_PLAN_SCHEMA_VERSION,
};
use crate::contract::report::{BoundaryReport, BoundaryReportBody, ObservedFact};
use crate::contract::support::BackendProfileSnapshot;
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

        // ONE probe per planning attempt; one typed profile derived from it.
        let snapshot = backend.probe();
        let profile = backend.profile(&snapshot);

        // Classify EVERY requirement ONCE, in admission order (capabilities then
        // controls). This single classification feeds BOTH admission paths.
        let mut classified: Vec<(BoundaryRequirement, SupportVerdict)> = Vec::new();
        for capability in &spec.capabilities {
            let req = BoundaryRequirement::Capability(capability.clone());
            let verdict = backend.classify(&req, &profile);
            classified.push((req, verdict));
        }
        for control in &spec.controls {
            let req = BoundaryRequirement::HostControl(control.clone());
            let verdict = backend.classify(&req, &profile);
            classified.push((req, verdict));
        }

        // Normalize ONE immutable input object. The evidence union is computed
        // here (not in-circuit) and fed identically to both paths.
        let required = spec.evidence.required_claims();
        let mut available = EvidenceSet::new();
        for (_, verdict) in &classified {
            available.extend_from(&verdict.evidence);
        }
        let inputs = PlannerInputs {
            enforcement: classified
                .iter()
                .map(|(_, verdict)| enforcement_code(verdict.enforcement))
                .collect(),
            evidence_required: evidence_bits(&required),
            evidence_available: evidence_bits(&available),
        };

        // Authoritative imperative reference + non-persistent shadow circuit over
        // the IDENTICAL inputs. A disagreement fails closed BEFORE any plan is
        // built — no backend effect, nothing persisted, plan identity untouched.
        let outcome =
            planner_shadow_check(&inputs).map_err(|divergence| PlanError::ShadowDivergence {
                detail: divergence.to_string(),
            })?;

        // Map the AUTHORITATIVE reference outcome back to the existing surface.
        match outcome {
            AdmissionOutcome::Admitted { .. } => {
                let admitted: Vec<AdmittedRequirement> = classified
                    .iter()
                    .map(|(requirement, verdict)| AdmittedRequirement {
                        mechanism: mechanism_for(&backend.id(), requirement, verdict.enforcement),
                        requirement: requirement.clone(),
                        enforcement: verdict.enforcement,
                    })
                    .collect();
                let plan_id = compute_plan_id(backend.id(), &snapshot, &admitted, spec).map_err(
                    |error| PlanError::ProfileInsufficient {
                        backend: backend.id(),
                        detail: format!("plan canonicalization failed: {error}"),
                    },
                )?;
                Ok(BoundaryPlan {
                    schema_version: BOUNDARY_PLAN_SCHEMA_VERSION,
                    plan_id,
                    backend: backend.id(),
                    profile: snapshot,
                    admitted,
                    workload: spec.workload.clone(),
                    budgets: spec.budgets.clone(),
                    evidence: spec.evidence,
                })
            }
            // Membrane 1 = support, membrane 2 = evidence (the current contract).
            AdmissionOutcome::Refused { membrane: 1, .. } => {
                match classified
                    .iter()
                    .find(|(_, verdict)| verdict.enforcement == Enforcement::Unsupported)
                {
                    Some((requirement, _)) => Err(PlanError::Unsupported {
                        requirement: requirement.clone(),
                        backend: backend.id(),
                    }),
                    None => Err(PlanError::ShadowDivergence {
                        detail: "support refusal without an unsupported requirement".to_string(),
                    }),
                }
            }
            AdmissionOutcome::Refused { membrane: 2, .. } => Err(PlanError::EvidenceUnsatisfiable {
                backend: backend.id(),
                detail: format!(
                    "required evidence {required:?} is not a subset of admitted evidence {available:?}"
                ),
            }),
            AdmissionOutcome::Refused { membrane, .. } => Err(PlanError::ShadowDivergence {
                detail: format!("unexpected refusal membrane {membrane}"),
            }),
        }
    }
}

/// The 2-bit enforcement code the admission inputs use
/// (`0` Unsupported, `1` Mediated, `2` Enforced).
fn enforcement_code(enforcement: Enforcement) -> u8 {
    match enforcement {
        Enforcement::Unsupported => 0,
        Enforcement::Mediated => 1,
        Enforcement::Enforced => 2,
    }
}

/// The fixed bit position of an evidence claim in the admission evidence bitset.
/// Exhaustive ON PURPOSE: adding an `EvidenceClaim` must assign it a bit here.
fn evidence_bit(claim: EvidenceClaim) -> u32 {
    match claim {
        EvidenceClaim::TerminalOutcome => 0,
        EvidenceClaim::CapturedStreams => 1,
        EvidenceClaim::ResourceUsage => 2,
        EvidenceClaim::AllowedActions => 3,
        EvidenceClaim::DeniedAttempts => 4,
        EvidenceClaim::FilesystemDelta => 5,
        EvidenceClaim::ProcessTree => 6,
        EvidenceClaim::NetworkActivity => 7,
        EvidenceClaim::ArtifactLineage => 8,
        EvidenceClaim::MechanismAttestation => 9,
    }
}

/// Pack an evidence set into a bitset lane (one bit per claim).
fn evidence_bits(set: &EvidenceSet) -> u16 {
    let mut bits = 0u16;
    for claim in set.iter() {
        bits |= 1u16 << evidence_bit(claim);
    }
    bits
}

/// The mechanism evidence string a backend records for an admitted requirement.
///
/// In C0 only the honest no-confinement reference backend admits anything, so
/// the mechanism reflects exactly what it does (host launch + stdio wiring with
/// no confinement). Real backends record their concrete primitive here.
fn mechanism_for(
    backend: &BackendId,
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

/// Canonical plan identity: hash of the plan core (backend, snapshot, admitted,
/// workload, budgets, evidence). Sorts the admitted set by requirement so the
/// digest is stable regardless of admission order.
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

/// One discrete step of a driven boundary run.
///
/// The steppable core (mirrors `WriterCore::drive_command -> DriveStep`): a run
/// surfaces each observed fact, then exactly one terminal step (`Sealed` or
/// `Faulted`). The crash-injection points the [`crate::__sim`] supervisor uses
/// are the gaps BETWEEN steps — a sim crash before `Sealed` leaves no report,
/// which reconciliation then classifies (§13).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunStep {
    /// The backend observed a fact; the run continues.
    Observed(ObservedFact),
    /// Terminal: the observed body sealed into a [`BoundaryReport`]. SEAL is
    /// hashed + canonical; it is NOT persisted (the host appends it). Boxed so a
    /// per-fact [`RunStep::Observed`] does not carry the sealed report's size.
    Sealed(Box<BoundaryReport>),
    /// Terminal: the run could not seal (canonical-encode failure). Stable detail.
    Faulted(String),
}

/// A driven boundary run: pumped one [`RunStep`] at a time.
///
/// Created by [`BoundaryRunner::begin`]. Drive it with [`BoundaryRun::drive_step`]
/// or, equivalently, `for step in run` (it is an [`Iterator`]). Prod pumps to the
/// terminal step on the calling thread; the sim supervisor pumps the IDENTICAL
/// core, injecting a crash between any two steps. Both share one core — there is
/// no second execution path to drift.
pub struct BoundaryRun {
    facts: std::vec::IntoIter<ObservedFact>,
    /// The observed body, taken at the seal step. `None` once terminal.
    body: Option<BoundaryReportBody>,
    backend: BackendId,
}

impl BoundaryRun {
    /// Advance the run by one step. Yields each [`RunStep::Observed`] fact, then
    /// the terminal [`RunStep::Sealed`]/[`RunStep::Faulted`], then `None`.
    pub fn drive_step(&mut self) -> Option<RunStep> {
        if let Some(fact) = self.facts.next() {
            return Some(RunStep::Observed(fact));
        }
        // Facts exhausted: seal the body exactly once, then go terminal.
        let body = self.body.take()?;
        match body.body_hash() {
            Ok(body_hash) => Some(RunStep::Sealed(Box::new(BoundaryReport {
                body,
                body_hash,
            }))),
            Err(error) => Some(RunStep::Faulted(format!(
                "report sealing failed on {}: {error}",
                self.backend
            ))),
        }
    }
}

impl Iterator for BoundaryRun {
    type Item = RunStep;
    fn next(&mut self) -> Option<RunStep> {
        self.drive_step()
    }
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

    /// Begin a steppable run: the backend OBSERVES (executes) now; sealing is
    /// deferred to the terminal [`RunStep`] so a sim crash can land before it.
    ///
    /// # Errors
    /// [`PlanError::UnknownBackend`] if the plan's bound backend is no longer
    /// registered.
    pub fn begin(&self, plan: &BoundaryPlan) -> Result<BoundaryRun, PlanError> {
        let backend =
            self.registry
                .backend(&plan.backend)
                .ok_or_else(|| PlanError::UnknownBackend {
                    backend: plan.backend.clone(),
                })?;
        let body: BoundaryReportBody = backend.execute(plan);
        Ok(BoundaryRun {
            facts: body.observed.clone().into_iter(),
            body: Some(body),
            backend: plan.backend.clone(),
        })
    }

    /// Execute via the bound backend (which OBSERVES), then SEAL the observed
    /// body = canonicalize + compute `body_hash` → [`BoundaryReport`]. SEAL
    /// means hashed + canonical; it does NOT persist. The host appends it.
    ///
    /// Pumps the same [`BoundaryRun`] core as the sim supervisor (one core, no
    /// drift): drive to the terminal step and surface it.
    ///
    /// # Errors
    /// Returns [`PlanError::UnknownBackend`] if the plan's bound backend is no
    /// longer registered, or [`PlanError::ProfileInsufficient`] if sealing the
    /// observed body fails to canonical-encode.
    pub fn run(&self, plan: &BoundaryPlan) -> Result<BoundaryReport, PlanError> {
        let mut run = self.begin(plan)?;
        loop {
            match run.drive_step() {
                Some(RunStep::Observed(_)) => {}
                Some(RunStep::Sealed(report)) => return Ok(*report),
                Some(RunStep::Faulted(detail)) => {
                    return Err(PlanError::ProfileInsufficient {
                        backend: plan.backend.clone(),
                        detail,
                    })
                }
                None => {
                    return Err(PlanError::ProfileInsufficient {
                        backend: plan.backend.clone(),
                        detail: "run ended with no terminal step".to_string(),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod planner_shadow_integration_tests {
    use super::{BackendRegistry, BoundaryPlanner};
    use crate::contract::backend::Backend;
    use crate::contract::budget::BudgetRequirements;
    use crate::contract::capability::{Capability, Enforcement, NetPolicy, SupportVerdict};
    use crate::contract::host_control::HostControl;
    use crate::contract::ids::BackendId;
    use crate::contract::plan::{
        BoundaryPlan, BoundaryRequirement, BoundarySpec, EvidenceRequirements, PlanError, Workload,
    };
    use crate::contract::report::BoundaryReportBody;
    use crate::contract::support::{BackendProfile, BackendProfileSnapshot, SupportMatrix};
    use crate::InertBackend;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn launch_control() -> HostControl {
        HostControl::LaunchWorkload
    }

    /// A spec Inert admits: launch only, requiring just the terminal outcome.
    fn admissible_spec() -> BoundarySpec {
        BoundarySpec {
            workload: Workload::Process {
                exe: "/bin/true".to_string(),
                args: vec![],
            },
            capabilities: vec![],
            controls: vec![launch_control()],
            budgets: BudgetRequirements::deny_all(),
            evidence: EvidenceRequirements {
                require_captured_streams: false,
                require_exit_status: true,
            },
        }
    }

    fn registry_with(backend: Arc<dyn Backend>) -> BackendRegistry {
        let mut registry = BackendRegistry::new();
        registry.register(backend);
        registry
    }

    fn inert_id() -> BackendId {
        BackendId::new(InertBackend::ID)
    }

    #[test]
    fn admits_a_no_confinement_spec_and_is_deterministic() {
        let registry = registry_with(Arc::new(InertBackend::new()));
        let planner = BoundaryPlanner::new(&registry);
        let spec = admissible_spec();

        let first = planner.plan(&spec, &inert_id()).expect("admit");
        let second = planner.plan(&spec, &inert_id()).expect("admit");
        // Accepted plan bytes are unchanged across attempts (deterministic identity).
        assert_eq!(first, second);
        assert_eq!(first.admitted.len(), 1);
        assert_eq!(
            first.admitted[0].requirement,
            BoundaryRequirement::HostControl(HostControl::LaunchWorkload)
        );
        assert_eq!(first.admitted[0].enforcement, Enforcement::Enforced);
        assert_eq!(first.admitted[0].mechanism, "inert:host_spawn:Enforced");
    }

    #[test]
    fn refuses_unsupported_requirement_at_support_membrane() {
        // A network capability Inert cannot enforce -> support refusal, naming the
        // first unsupported requirement (parity with the pre-shadow planner).
        let registry = registry_with(Arc::new(InertBackend::new()));
        let planner = BoundaryPlanner::new(&registry);
        let mut spec = admissible_spec();
        spec.capabilities = vec![Capability::Network {
            policy: NetPolicy::DenyAll,
        }];

        let error = planner.plan(&spec, &inert_id()).expect_err("refuse");
        assert_eq!(
            error,
            PlanError::Unsupported {
                requirement: BoundaryRequirement::Capability(Capability::Network {
                    policy: NetPolicy::DenyAll,
                }),
                backend: inert_id(),
            }
        );
    }

    #[test]
    fn refuses_unsatisfiable_evidence_at_evidence_membrane() {
        // Launch is admitted (support passes) but the caller demands captured
        // streams Inert cannot witness from launch alone -> evidence refusal.
        let registry = registry_with(Arc::new(InertBackend::new()));
        let planner = BoundaryPlanner::new(&registry);
        let mut spec = admissible_spec();
        spec.evidence.require_captured_streams = true;

        let error = planner.plan(&spec, &inert_id()).expect_err("refuse");
        assert!(
            matches!(error, PlanError::EvidenceUnsatisfiable { .. }),
            "expected evidence refusal, got {error:?}"
        );
    }

    #[test]
    fn unknown_backend_is_rejected() {
        let registry = BackendRegistry::new();
        let planner = BoundaryPlanner::new(&registry);
        let error = planner
            .plan(&admissible_spec(), &BackendId::new("ghost"))
            .expect_err("unknown");
        assert!(matches!(error, PlanError::UnknownBackend { .. }));
    }

    /// Wraps Inert, counting probe() calls — proves planning probes exactly once.
    struct ProbeCounting {
        inner: InertBackend,
        probes: Arc<AtomicUsize>,
    }

    impl Backend for ProbeCounting {
        fn id(&self) -> BackendId {
            self.inner.id()
        }
        fn support(&self) -> &SupportMatrix {
            self.inner.support()
        }
        fn probe(&self) -> BackendProfileSnapshot {
            self.probes.fetch_add(1, Ordering::SeqCst);
            self.inner.probe()
        }
        fn profile(&self, snap: &BackendProfileSnapshot) -> BackendProfile {
            self.inner.profile(snap)
        }
        fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
            self.inner.classify(req, profile)
        }
        fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
            self.inner.execute(plan)
        }
    }

    #[test]
    fn planning_probes_the_backend_exactly_once() {
        let probes = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(ProbeCounting {
            inner: InertBackend::new(),
            probes: Arc::clone(&probes),
        });
        let registry = registry_with(backend);
        let planner = BoundaryPlanner::new(&registry);

        planner
            .plan(&admissible_spec(), &inert_id())
            .expect("admit");
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "both admission paths must share one probe, not re-probe"
        );
    }
}
