//! The "BAL" in code: [`BackendRegistry`] + [`BoundaryPlanner`] +
//! [`BoundaryRunner`]. No `struct Bal`.

use crate::contract::admission::{planner_shadow_check, AdmissionOutcome, PlannerInputs};
use crate::contract::backend::Backend;
use crate::contract::budget::{budget_admit, AdmittedBudgets, DerivedMinimums};
use crate::contract::capability::{
    Capability, Enforcement, EvidenceClaim, EvidenceSet, FdPolicy, FsAccess, NetPolicy,
    SpawnPolicy, SupportVerdict,
};
use crate::contract::host_control::HostControl;
use crate::contract::ids::{BackendId, BoundaryPlanHash, Digest32};
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

        // CONTRACT GATE (proof-spine §5 D2): every capability policy must be
        // structurally valid BEFORE classification/execution — a malformed policy fails
        // closed HERE, so the workload never runs.
        validate_capability_policies(&spec.capabilities)?;

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
                build_admitted_plan(backend.as_ref(), snapshot, &classified, spec)
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

/// Build the admitted [`BoundaryPlan`] once the support + evidence membranes passed:
/// adjudicate the seven-dimensional budget membrane (LOAD-BEARING, fail-closed) against
/// this backend's declared budget profile + the spec's derived structural minimums,
/// then assemble the admitted requirements + plan identity. Split out of
/// [`BoundaryPlanner::plan`] to hold that function under the complexity budget.
fn build_admitted_plan(
    backend: &dyn Backend,
    snapshot: BackendProfileSnapshot,
    classified: &[(BoundaryRequirement, SupportVerdict)],
    spec: &BoundarySpec,
) -> Result<BoundaryPlan, PlanError> {
    let derived = derive_minimums(spec);
    let profile_digest =
        budget_profile_digest(&snapshot).map_err(|error| PlanError::ProfileInsufficient {
            backend: backend.id(),
            detail: format!("budget profile canonicalization failed: {error}"),
        })?;
    let admitted_budgets = budget_admit(&spec.budgets, &snapshot.budget, &derived, profile_digest)
        .map_err(|refusal| PlanError::BudgetRefused {
            backend: backend.id(),
            dimension: refusal.dimension,
            failure: refusal.failure,
        })?;
    let admitted: Vec<AdmittedRequirement> = classified
        .iter()
        .map(|(requirement, verdict)| AdmittedRequirement {
            mechanism: backend.mechanism(requirement, verdict.enforcement),
            requirement: requirement.clone(),
            enforcement: verdict.enforcement,
        })
        .collect();
    let plan_id = compute_plan_id(backend.id(), &snapshot, &admitted, spec, &admitted_budgets)
        .map_err(|error| PlanError::ProfileInsufficient {
            backend: backend.id(),
            detail: format!("plan canonicalization failed: {error}"),
        })?;
    Ok(BoundaryPlan {
        schema_version: BOUNDARY_PLAN_SCHEMA_VERSION,
        plan_id,
        backend: backend.id(),
        profile: snapshot,
        admitted,
        workload: spec.workload.clone(),
        budgets: admitted_budgets,
        evidence: spec.evidence,
    })
}

/// The capability-policy CONTRACT GATE (proof-spine §5 D2): fail closed if any
/// capability carries a structurally-invalid policy, BEFORE any classification or
/// execution. Today only `Environment::Exact` has a contract validator (its `validate`
/// rejects duplicate/reserved-byte names, NUL values, over-cap tables); other policies
/// add a single match arm here when they gain one.
fn validate_capability_policies(capabilities: &[Capability]) -> Result<(), PlanError> {
    for capability in capabilities {
        if let Capability::Environment { policy } = capability {
            policy
                .validate()
                .map_err(|error| PlanError::InvalidPolicy {
                    detail: error.to_string(),
                })?;
        }
    }
    Ok(())
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

/// Compute the cross-dimensional derived structural minimums `DerivedMinimum_d(S)`
/// — the floor each budget dimension's requested limit must meet, from what the
/// spec structurally implies (kernel plan §7). The values are SYMBOLIC structural
/// floors (a nonzero `1`, the 3 standard descriptors), not precise resource sizing:
/// they encode coherence, e.g. a launched workload needs ≥1 process and nonzero
/// time/cpu/memory; requested network access cannot be 0 bytes.
#[must_use]
pub fn derive_minimums(spec: &BoundarySpec) -> DerivedMinimums {
    let mut minimums = DerivedMinimums::default();

    // A launched workload needs at least one process and nonzero time/cpu/memory,
    // plus the three standard descriptors.
    let launches = spec
        .controls
        .iter()
        .any(|control| matches!(control, HostControl::LaunchWorkload));
    if launches {
        minimums.process_count = 1;
        minimums.wall_micros = 1;
        minimums.cpu_micros = 1;
        minimums.resident_bytes = 1;
        minimums.handle_count = 3;
    }

    for control in &spec.controls {
        if let HostControl::CaptureStreams { streams } = control {
            minimums.handle_count +=
                u64::from(streams.stdout) + u64::from(streams.stderr) + u64::from(streams.stdin);
        }
        if matches!(
            control,
            HostControl::TempRoot { .. } | HostControl::CommitArtifact { .. }
        ) {
            minimums.storage_bytes = minimums.storage_bytes.max(1);
        }
        if let HostControl::ExposePath { access, .. } = control {
            if matches!(access, FsAccess::Write | FsAccess::ReadWrite) {
                minimums.storage_bytes = minimums.storage_bytes.max(1);
            }
        }
    }

    for capability in &spec.capabilities {
        if matches!(
            capability,
            Capability::ChildSpawn {
                policy: SpawnPolicy::Allow
            }
        ) {
            // Child-spawn authority must fit inside the process-tree bound.
            minimums.process_count = minimums.process_count.max(2);
        }
        if let Capability::InheritedFds {
            policy: FdPolicy::Only(fds),
        } = capability
        {
            minimums.handle_count += u64::try_from(fds.len()).unwrap_or(u64::MAX);
        }
        if matches!(
            capability,
            Capability::Network {
                policy: NetPolicy::AllowList(_)
            }
        ) {
            // Requested network access cannot be coherent with a 0-byte budget.
            minimums.network_bytes = minimums.network_bytes.max(1);
        }
    }

    minimums
}

/// Canonical plan identity: hash of the plan core (backend, snapshot, admitted,
/// workload, budgets, evidence). Sorts the admitted set by requirement so the digest
/// is stable regardless of admission order. `H_B` binds BOTH the budget REQUEST and
/// the ADJUDICATED contract — changing either changes plan identity.
fn compute_plan_id(
    backend: BackendId,
    snapshot: &BackendProfileSnapshot,
    admitted: &[AdmittedRequirement],
    spec: &BoundarySpec,
    admitted_budgets: &AdmittedBudgets,
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
        budget_request: format!("{:?}", spec.budgets),
        admitted_budgets: format!("{admitted_budgets:?}"),
        evidence: format!("{:?}", spec.evidence),
    };
    let bytes = batpak::canonical::to_bytes(&core)?;
    Ok(BoundaryPlanHash(batpak::event::hash::compute_hash(&bytes)))
}

/// The digest of the backend's declared budget profile, bound into each adjudicated
/// dimension as the source-profile provenance. The profile is already hashed into
/// `plan_id` via the snapshot; this is its standalone identity for budget admission.
fn budget_profile_digest(
    snapshot: &BackendProfileSnapshot,
) -> Result<Digest32, rmp_serde::encode::Error> {
    let bytes = batpak::canonical::to_bytes(&snapshot.budget)?;
    Ok(batpak::event::hash::compute_hash(&bytes))
}

#[derive(serde::Serialize)]
struct PlanFingerprint<'a> {
    schema_version: u16,
    backend: BackendId,
    snapshot: &'a BackendProfileSnapshot,
    admitted: &'a [AdmittedRequirement],
    workload: String,
    budget_request: String,
    admitted_budgets: String,
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
    use super::{derive_minimums, BackendRegistry, BoundaryPlanner};
    use crate::contract::backend::Backend;
    use crate::contract::budget::{
        BudgetDimension, BudgetFailure, BudgetRequirements, DerivedMinimums,
    };
    use crate::contract::capability::{
        Capability, FdPolicy, NetPolicy, SpawnPolicy, SupportVerdict,
    };
    use crate::contract::host_control::{HostControl, PathView, StdStreams};
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
    fn inert_deterministically_refuses_a_budgeted_spec_at_the_floor() {
        // The all-Unsupported Inert floor refuses every budgeted spec — here
        // deny_all, whose zero wall limit is below the launched workload's derived
        // minimum — and does so DETERMINISTICALLY: same spec, same typed refusal.
        // (Positive admit + determinism is proven on the honest Sim; Inert is the
        // permanent fail-closed reference.)
        let registry = registry_with(Arc::new(InertBackend::new()));
        let planner = BoundaryPlanner::new(&registry);
        let spec = admissible_spec();

        let first = planner.plan(&spec, &inert_id());
        let second = planner.plan(&spec, &inert_id());
        assert_eq!(first, second, "Inert refuses deterministically");
        assert!(
            matches!(
                first,
                Err(PlanError::BudgetRefused {
                    dimension: BudgetDimension::Wall,
                    failure: BudgetFailure::BelowDerivedMinimum,
                    ..
                })
            ),
            "the floor refuses at the budget membrane: {first:?}"
        );
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

        // Inert refuses at the budget membrane, but the probe still happens EXACTLY
        // once — probing precedes adjudication, and both admission paths share it.
        let _ = planner.plan(&admissible_spec(), &inert_id());
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "both admission paths must share one probe, not re-probe"
        );
    }

    #[test]
    fn derived_minimums_are_zero_without_a_launch() {
        let mut spec = admissible_spec();
        spec.controls = vec![];
        assert_eq!(derive_minimums(&spec), DerivedMinimums::default());
    }

    #[test]
    fn a_launch_implies_process_resource_and_std_handle_floors() {
        let minimums = derive_minimums(&admissible_spec());
        assert_eq!(minimums.process_count, 1);
        assert_eq!(minimums.wall_micros, 1);
        assert_eq!(minimums.cpu_micros, 1);
        assert_eq!(minimums.resident_bytes, 1);
        assert_eq!(minimums.handle_count, 3, "the three standard descriptors");
        assert_eq!(minimums.storage_bytes, 0);
        assert_eq!(minimums.network_bytes, 0);
    }

    #[test]
    fn structural_floors_from_streams_fds_spawn_storage_and_network() {
        let mut spec = admissible_spec();
        spec.controls = vec![
            HostControl::LaunchWorkload,
            HostControl::CaptureStreams {
                streams: StdStreams {
                    stdout: true,
                    stderr: true,
                    stdin: false,
                },
            },
            HostControl::TempRoot {
                visibility: PathView::PrivateToBoundary,
            },
        ];
        spec.capabilities = vec![
            Capability::ChildSpawn {
                policy: SpawnPolicy::Allow,
            },
            Capability::InheritedFds {
                policy: FdPolicy::Only(vec![5, 6]),
            },
            Capability::Network {
                policy: NetPolicy::AllowList(vec![]),
            },
        ];
        let minimums = derive_minimums(&spec);
        assert_eq!(
            minimums.process_count, 2,
            "child-spawn needs >= 2 processes"
        );
        assert_eq!(
            minimums.handle_count,
            3 + 2 + 2,
            "std(3) + captured stdout/stderr(2) + inherited fds(2)"
        );
        assert_eq!(minimums.storage_bytes, 1, "a temp root needs > 0 storage");
        assert_eq!(
            minimums.network_bytes, 1,
            "requested network access cannot be a 0-byte budget"
        );
    }
}
