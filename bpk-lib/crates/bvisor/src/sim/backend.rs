//! [`SimBackend`] — a [`Backend`] that LIES deterministically.
//!
//! Sibling of batpak's fault framework (`crates/core/src/store/fault.rs`):
//! [`LieMode`] is the sibling of `FaultMode`, [`LieInjector`] the sibling of
//! `FaultInjector`. One seeded PRNG is advanced ONCE per consultation, so the
//! same seed yields the same lie sequence (`crates/core/src/store/sim/`).
//!
//! The monster performs a (simulated) dangerous effect, records the REAL effect
//! into the harness-owned [`GroundTruth`], and SEPARATELY constructs a possibly
//! LYING [`BoundaryReportBody`]. It never grades itself: the [`GroundTruth`]
//! handle the harness holds is the independent record the oracle diffs against.
//!
//! INVERSION RULE (from the recovery matrix): a backend may DENY MORE than asked
//! (fail-closed always legal); it may never REPORT LESS DANGER THAN OCCURRED.
//! The hide-danger lies ([`Lie::DropOrphanFromReport`], [`Lie::DropDeniedAttempt`],
//! [`Lie::AutoCommitButReportFalse`]) are illegal in every mode.

use crate::contract::backend::Backend;
use crate::contract::budget::{BudgetAvailability, BudgetProfile};
use crate::contract::capability::{Enforcement, EvidenceClaim, EvidenceSet, SupportVerdict};
use crate::contract::host_control::HostControl;
use crate::contract::ids::BackendId;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement, Workload};
use crate::contract::report::{
    BoundaryFinding, BoundaryReportBody, CaptureRefs, DeniedAttempt, ExitStatus, ObservedFact,
    Outcome, BOUNDARY_REPORT_SCHEMA_VERSION,
};
use crate::contract::support::{
    BackendProfile, BackendProfileSnapshot, RequirementKind, SupportMatrix,
};
use crate::sim::ground_truth::{GroundTruth, Lie};
use crate::sim::Prng;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// One mode of the monster: an honest backend, or a single deterministic lie.
///
/// Sibling of `FaultMode`. `Honest` is the GREEN control: the report matches the
/// GroundTruth exactly. Every other mode tells exactly the one lie named, mapped
/// 1:1 to the [`Lie`] (and thence to the Gn) that catches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LieMode {
    /// Tell the truth: the report matches the GroundTruth (the control).
    Honest,
    /// Tell exactly one lie.
    Lie(Lie),
}

impl LieMode {
    /// A stable digest/label token discriminating each mode.
    #[must_use]
    pub fn token(self) -> u64 {
        match self {
            LieMode::Honest => 0x0000_0001,
            LieMode::Lie(lie) => 0x1000_0000 | (lie as u64),
        }
    }

    /// A short stable label for this mode.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            LieMode::Honest => "honest".to_string(),
            LieMode::Lie(lie) => format!("lie:{}:{lie:?}", lie.gate()),
        }
    }
}

/// Trait for choosing which lie the monster tells on a given consultation.
///
/// Sibling of `FaultInjector`. Implementors inspect the run context and return
/// the [`LieMode`] to apply. A seeded implementor ([`SeededLiar`]) advances a
/// PRNG once per consultation, so the same seed yields the same lie sequence.
pub trait LieInjector: Send + Sync {
    /// Decide the lie mode for this consultation. Called once per `execute`.
    fn consult(&self) -> LieMode;
}

/// A seeded liar: advances a splitmix64 PRNG once per consultation and picks a
/// lie from the catalogue (or `Honest`). Same seed ⇒ same lie sequence.
pub struct SeededLiar {
    prng: Mutex<Prng>,
    catalogue: Vec<LieMode>,
}

impl SeededLiar {
    /// Seed the liar over the FULL catalogue (Honest + every lie).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            prng: Mutex::new(Prng::new(seed)),
            catalogue: full_catalogue(),
        }
    }

    /// Seed the liar over a restricted catalogue (e.g. one mode).
    #[must_use]
    pub fn over(seed: u64, catalogue: Vec<LieMode>) -> Self {
        let catalogue = if catalogue.is_empty() {
            vec![LieMode::Honest]
        } else {
            catalogue
        };
        Self {
            prng: Mutex::new(Prng::new(seed)),
            catalogue,
        }
    }
}

impl LieInjector for SeededLiar {
    fn consult(&self) -> LieMode {
        let draw = match self.prng.lock() {
            Ok(mut prng) => prng.next_u64(),
            // A poisoned lock is the supervisor faulting; fall back to Honest so
            // the monster never panics (panic is denied workspace-wide).
            Err(poisoned) => poisoned.into_inner().next_u64(),
        };
        let idx = usize::try_from(draw % self.catalogue.len() as u64).unwrap_or(0);
        self.catalogue.get(idx).copied().unwrap_or(LieMode::Honest)
    }
}

/// A one-shot liar that always returns a FIXED mode — the sibling of
/// `OneShotInjector` for the deterministic per-gate grid scenarios.
pub struct OneShotLiar {
    mode: LieMode,
}

impl OneShotLiar {
    /// Always tell exactly this mode.
    #[must_use]
    pub fn new(mode: LieMode) -> Self {
        Self { mode }
    }
}

impl LieInjector for OneShotLiar {
    fn consult(&self) -> LieMode {
        self.mode
    }
}

/// The full lie catalogue plus the Honest control.
fn full_catalogue() -> Vec<LieMode> {
    let mut modes = vec![LieMode::Honest];
    for lie in [
        Lie::ClaimEnforcedButAllowRead,
        Lie::ClaimEnforcedButAllowNet,
        Lie::WriteEscapesQuarantine,
        Lie::SpawnDespiteDeny,
        Lie::DropOrphanFromReport,
        Lie::ProxyInheritedFd,
        Lie::AutoCommitButReportFalse,
        Lie::SkipSealing,
        Lie::DropDeniedAttempt,
        Lie::MisreportEnforcementDepth,
        Lie::CrashMidBoundary,
    ] {
        modes.push(LieMode::Lie(lie));
    }
    modes
}

/// The monster. Claims a deep support matrix (so `plan()` admits anything), then
/// LIES at execution time according to its [`LieInjector`]. Performs simulated
/// effects only — ZERO real OS code — and records the REAL effect into a
/// harness-owned [`GroundTruth`] the oracle later diffs against the report.
pub struct SimBackend {
    id: BackendId,
    support: SupportMatrix,
    injector: Box<dyn LieInjector>,
    /// The honest seven-dimensional budget profile the monster declares (it enforces
    /// every dimension via a simulated mechanism).
    budget: BudgetProfile,
    /// The harness-owned independent record. `Mutex` only because `execute`
    /// takes `&self`; the harness reads it back via [`SimBackend::ground_truth`].
    truth: Mutex<GroundTruth>,
}

/// The honest budget profile the monster declares: it ENFORCES every dimension via a
/// simulated mechanism, witnesses resource usage, and offers ample headroom — so a
/// budgeted spec ADMITS on Sim (the positive reference) where it is refused on the
/// all-`Unsupported` Inert floor. The lie modes misreport at EXECUTION, not here.
fn sim_budget_profile() -> BudgetProfile {
    let enforced = |available: u64, mechanism: &str| {
        let mut evidence = EvidenceSet::new();
        evidence.insert(EvidenceClaim::ResourceUsage);
        BudgetAvailability {
            available,
            enforcement: Enforcement::Enforced,
            evidence,
            mechanism: mechanism.to_string(),
        }
    };
    BudgetProfile {
        wall_micros: enforced(60_000_000, "sim_timer"),
        cpu_micros: enforced(60_000_000, "sim_cpu_accountant"),
        resident_bytes: enforced(1u64 << 32, "sim_mem_accountant"),
        process_count: enforced(64, "sim_process_table"),
        handle_count: enforced(1024, "sim_descriptor_table"),
        storage_bytes: enforced(1u64 << 32, "sim_storage_quota"),
        network_bytes: enforced(1u64 << 30, "sim_network_meter"),
    }
}

impl SimBackend {
    /// The stable id of the sim backend.
    pub const ID: &'static str = "sim";

    /// Construct the monster with a lie injector. Its support matrix claims
    /// `Enforced` for every requirement kind, so `plan()` admits anything and the
    /// lie surfaces at execution time, not at admission.
    #[must_use]
    pub fn new(injector: Box<dyn LieInjector>) -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: deep_support(),
            injector,
            budget: sim_budget_profile(),
            truth: Mutex::new(GroundTruth::new()),
        }
    }

    /// Take a SNAPSHOT of the harness-owned GroundTruth recorded during the most
    /// recent `execute`. This is the INDEPENDENT record the oracle diffs against
    /// the backend's self-reported body — the monster never grades itself.
    #[must_use]
    pub fn ground_truth(&self) -> GroundTruth {
        match self.truth.lock() {
            Ok(truth) => truth.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Record an effect into the harness-owned GroundTruth.
    fn record(&self, f: impl FnOnce(&mut GroundTruth)) {
        match self.truth.lock() {
            Ok(mut truth) => f(&mut truth),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    /// Reset the GroundTruth before a fresh run (so consecutive `execute` calls
    /// on the same backend do not accumulate).
    fn reset_truth(&self) {
        match self.truth.lock() {
            Ok(mut truth) => *truth = GroundTruth::new(),
            Err(poisoned) => *poisoned.into_inner() = GroundTruth::new(),
        }
    }
}

/// The monster's deep verdict: `Enforced` with FULL evidence, so `plan()` admits
/// any spec (any enforcement + any evidence requirement) and the lie surfaces at
/// execution time, never at admission.
fn deep_verdict() -> SupportVerdict {
    SupportVerdict::new(
        Enforcement::Enforced,
        [
            EvidenceClaim::TerminalOutcome,
            EvidenceClaim::CapturedStreams,
            EvidenceClaim::ResourceUsage,
            EvidenceClaim::AllowedActions,
            EvidenceClaim::DeniedAttempts,
            EvidenceClaim::FilesystemDelta,
            EvidenceClaim::ProcessTree,
            EvidenceClaim::NetworkActivity,
            EvidenceClaim::ArtifactLineage,
            EvidenceClaim::MechanismAttestation,
        ]
        .into_iter()
        .collect(),
    )
}

/// A deep support matrix: every requirement kind claims the deep verdict so
/// `plan()` admits any spec against the monster.
fn deep_support() -> SupportMatrix {
    let mut best = BTreeMap::new();
    for kind in [
        RequirementKind::Filesystem,
        RequirementKind::NetworkDenyAll,
        RequirementKind::NetworkAllowList,
        RequirementKind::ChildSpawn,
        RequirementKind::Environment,
        RequirementKind::InheritedFds,
        RequirementKind::LaunchWorkload,
        RequirementKind::CaptureStreams,
        RequirementKind::TempRoot,
        RequirementKind::ExposePath,
        RequirementKind::CommitArtifact,
        RequirementKind::DiscardArtifact,
        RequirementKind::Kill,
        RequirementKind::ListOutputs,
    ] {
        best.insert(kind, deep_verdict());
    }
    SupportMatrix::from_best_case(best)
}

/// The deep machine ceiling (matches the family best-case: all Enforced).
fn deep_ceiling() -> BackendProfile {
    let mut ceiling = BTreeMap::new();
    for kind in [
        RequirementKind::Filesystem,
        RequirementKind::NetworkDenyAll,
        RequirementKind::NetworkAllowList,
        RequirementKind::ChildSpawn,
        RequirementKind::Environment,
        RequirementKind::InheritedFds,
        RequirementKind::LaunchWorkload,
        RequirementKind::CaptureStreams,
        RequirementKind::TempRoot,
        RequirementKind::ExposePath,
        RequirementKind::CommitArtifact,
        RequirementKind::DiscardArtifact,
        RequirementKind::Kill,
        RequirementKind::ListOutputs,
    ] {
        ceiling.insert(kind, deep_verdict());
    }
    BackendProfile::from_ceiling(ceiling)
}

impl Backend for SimBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn support(&self) -> &SupportMatrix {
        &self.support
    }

    fn probe(&self) -> BackendProfileSnapshot {
        let mut probed = BTreeMap::new();
        probed.insert("backend".to_string(), "sim".to_string());
        probed.insert("confinement".to_string(), "simulated".to_string());
        BackendProfileSnapshot {
            backend: self.id.clone(),
            probed,
            budget: self.budget.clone(),
        }
    }

    fn profile(&self, _snap: &BackendProfileSnapshot) -> BackendProfile {
        deep_ceiling()
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        self.reset_truth();
        let mode = self.injector.consult();
        let mut run = RunState::new();

        // The monster performs its (simulated) dangerous effects, recording the
        // REAL effect to GroundTruth and applying the lie to the report.
        self.simulate(plan, mode, &mut run);

        run.into_body(plan, &self.id, &self.probe())
    }
}

impl SimBackend {
    /// Drive the simulated run for `mode`, recording REAL effects to GroundTruth
    /// and applying the lie to `run` (the report-in-progress).
    fn simulate(&self, plan: &BoundaryPlan, mode: LieMode, run: &mut RunState) {
        match mode {
            LieMode::Honest => self.simulate_honest(plan, run),
            LieMode::Lie(lie) => self.simulate_lie(plan, lie, run),
        }
    }

    /// The honest control: every real effect is also observed in the report, and
    /// the run reaches a terminal that is sealed.
    fn simulate_honest(&self, _plan: &BoundaryPlan, run: &mut RunState) {
        self.record(GroundTruth::reached_terminal);
        run.outcome = Outcome::Completed;
        run.exit = Some(ExitStatus::Code(0));
        // Mechanism honesty: claim exactly what was done (no confinement here).
        run.mechanism_override = Some("sim:honest:no-confinement".to_string());
    }

    /// Apply exactly one lie: do the dangerous thing (record it to GroundTruth)
    /// but hide it in the report.
    fn simulate_lie(&self, plan: &BoundaryPlan, lie: Lie, run: &mut RunState) {
        self.record(GroundTruth::reached_terminal);
        run.outcome = Outcome::Completed;
        run.exit = Some(ExitStatus::Code(0));

        match lie {
            Lie::ClaimEnforcedButAllowRead => {
                self.record(|t| t.served_secret("secret.key"));
                // Report stays silent → diff catches G1.
            }
            Lie::ClaimEnforcedButAllowNet => {
                self.record(|t| t.opened_socket("10.0.0.1:443"));
                // Report stays silent → diff catches G2.
            }
            Lie::WriteEscapesQuarantine => {
                self.record(|t| t.wrote_outside_quarantine("/etc/passwd"));
                // Report stays silent → diff catches G3.
            }
            Lie::SpawnDespiteDeny => {
                self.record(|t| t.spawned_child(4242));
                // Report stays silent → diff catches G4.
            }
            Lie::DropOrphanFromReport => {
                self.record(|t| t.live_pid(9001));
                // Report omits the orphan → diff catches G5 (hide-danger).
            }
            Lie::ProxyInheritedFd => {
                self.record(|t| t.inherited_fd_reachable(7));
                // Report omits the leak → diff catches G6.
            }
            Lie::AutoCommitButReportFalse => {
                // The backend auto-promoted bytes OUT of quarantine — an act it
                // has no authority to do (commit is a host disposition). It then
                // stays SILENT in the report (no "artifact_committed" observation),
                // so no disposition ever governs the artifact → diff catches G7.
                self.record(|t| t.committed_artifact("out.bin"));
            }
            Lie::SkipSealing => {
                // The run REACHES a terminal (GroundTruth recorded it above) but
                // no report is sealed: the grid drops the report (`sealed=false`)
                // so the oracle catches "terminal without a seal" (G8).
            }
            Lie::DropDeniedAttempt => {
                self.record(|t| t.denied_attempt("network:DenyAll"));
                // Report omits the denial → diff catches G9 (hide-danger).
            }
            Lie::MisreportEnforcementDepth => {
                // Actually did nothing real, but claims a deep mechanism.
                self.record(|t| t.enforcement_depth("none/no-confinement"));
                run.mechanism_override = Some("sim:landlock_abi4+pivot_root".to_string());
            }
            Lie::CrashMidBoundary => {
                // Crashed BEFORE a terminal (GroundTruth records no terminal),
                // but the backend still seals a report claiming Completed. The
                // oracle catches the sealed-terminal-for-a-crashed-run lie (G11).
                self.reset_truth();
                run.outcome = Outcome::Completed;
            }
        }

        // Mirror the plan's admitted requirements into the report findings so the
        // report is otherwise well-formed (the lie is the only divergence).
        let _ = plan;
    }
}

/// The report-in-progress plus the seal-existence axis.
struct RunState {
    outcome: Outcome,
    exit: Option<ExitStatus>,
    observed: Vec<ObservedFact>,
    denied: Vec<DeniedAttempt>,
    /// G10: override the admitted mechanism strings in the report.
    mechanism_override: Option<String>,
}

impl RunState {
    fn new() -> Self {
        Self {
            outcome: Outcome::Completed,
            exit: None,
            observed: Vec::new(),
            denied: Vec::new(),
            mechanism_override: None,
        }
    }

    /// Finalize the report body from the plan + run state.
    fn into_body(
        mut self,
        plan: &BoundaryPlan,
        backend: &BackendId,
        profile: &BackendProfileSnapshot,
    ) -> BoundaryReportBody {
        let mut admitted = plan.admitted.clone();
        if let Some(mechanism) = &self.mechanism_override {
            for a in &mut admitted {
                a.mechanism = mechanism.clone();
            }
        }
        let mut findings = Vec::new();
        for a in &admitted {
            findings.push(BoundaryFinding::RequirementAdmitted {
                requirement: a.requirement.clone(),
                enforcement: a.enforcement,
            });
        }
        // Honest backends observe LaunchWorkload; keep that so the body is
        // structurally plausible regardless of the lie.
        if plan.admitted.iter().any(|a| {
            matches!(
                a.requirement,
                BoundaryRequirement::HostControl(HostControl::LaunchWorkload)
            )
        }) {
            self.observed.push(ObservedFact {
                kind: "workload_launched".to_string(),
                detail: workload_detail(&plan.workload),
            });
        }

        BoundaryReportBody {
            schema_version: BOUNDARY_REPORT_SCHEMA_VERSION,
            plan_id: plan.plan_id,
            backend: backend.clone(),
            profile: profile.clone(),
            outcome: self.outcome,
            admitted,
            observed: self.observed,
            denied: self.denied,
            exit: self.exit,
            captured: CaptureRefs::default(),
            artifacts: Vec::new(),
            findings,
        }
    }
}

/// A stable workload detail string for the launch observation.
fn workload_detail(workload: &Workload) -> String {
    match workload {
        Workload::Process { exe, .. } => format!("sim launched process {exe}"),
        Workload::Wasm { module_ref } => format!("sim launched wasm {module_ref}"),
    }
}

/// Whether the run SEALS a report body — the seal-existence axis the grid uses
/// to set the `sealed` flag without re-running.
///
/// - [`Lie::SkipSealing`] (G8): the run REACHED a terminal but NO report is
///   sealed → `false`. The oracle catches "terminal without a seal".
/// - [`Lie::CrashMidBoundary`] (G11): the run did NOT reach a terminal but the
///   backend SEALS a report claiming `Completed` anyway → `true`. The oracle
///   catches "sealed terminal report for a run that never terminated".
#[must_use]
pub(crate) fn run_seals(mode: LieMode) -> bool {
    !matches!(mode, LieMode::Lie(Lie::SkipSealing))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_liar_same_seed_same_sequence() {
        let a = SeededLiar::new(0x1234);
        let b = SeededLiar::new(0x1234);
        for _ in 0..16 {
            assert_eq!(a.consult(), b.consult(), "same seed ⇒ same lie sequence");
        }
    }

    #[test]
    fn one_shot_liar_is_fixed() {
        let liar = OneShotLiar::new(LieMode::Lie(Lie::SpawnDespiteDeny));
        assert_eq!(liar.consult(), LieMode::Lie(Lie::SpawnDespiteDeny));
        assert_eq!(liar.consult(), LieMode::Lie(Lie::SpawnDespiteDeny));
    }

    #[test]
    fn run_seals_only_when_not_seal_suppressing() {
        assert!(run_seals(LieMode::Honest));
        assert!(run_seals(LieMode::Lie(Lie::SpawnDespiteDeny)));
        assert!(
            !run_seals(LieMode::Lie(Lie::SkipSealing)),
            "G8 seals nothing"
        );
        assert!(
            run_seals(LieMode::Lie(Lie::CrashMidBoundary)),
            "G11 seals a (lying) terminal report"
        );
    }

    // The positive reference: the honest monster ENFORCES every budget dimension, so a
    // budgeted spec ADMITS on Sim where the all-Unsupported Inert floor must refuse.
    #[test]
    fn honest_sim_admits_a_budget_that_the_inert_floor_refuses() {
        use crate::backend::inert::InertBackend;
        use crate::contract::budget::{
            budget_admit, BudgetFailure, BudgetRefusal, BudgetRequest, BudgetRequirements,
            DerivedMinimums, MinGuarantee,
        };

        // A modest limit within every dimension's Sim capacity (process_count = 64
        // is the smallest), so only the GUARANTEE distinguishes Sim from the floor.
        let dimension = || {
            let mut evidence = EvidenceSet::new();
            evidence.insert(EvidenceClaim::ResourceUsage);
            BudgetRequest {
                limit: 8,
                guarantee: MinGuarantee::Mediated,
                evidence,
            }
        };
        let request = BudgetRequirements {
            wall_micros: dimension(),
            cpu_micros: dimension(),
            resident_bytes: dimension(),
            process_count: dimension(),
            handle_count: dimension(),
            storage_bytes: dimension(),
            network_bytes: dimension(),
        };
        let derived = DerivedMinimums::default();

        // Honest Sim enforces every dimension -> admits (the positive reference).
        let sim = SimBackend::new(Box::new(OneShotLiar::new(LieMode::Honest)));
        let admitted = budget_admit(&request, &sim.probe().budget, &derived, [0u8; 32]);
        assert!(
            admitted.is_ok(),
            "honest sim admits a Mediated-guarantee budget: {admitted:?}"
        );

        // The Inert floor is all-Unsupported -> refuses on the first dimension's guarantee.
        let inert = InertBackend::new();
        let refused = budget_admit(&request, &inert.probe().budget, &derived, [0u8; 32]);
        assert!(
            matches!(
                refused,
                Err(BudgetRefusal {
                    failure: BudgetFailure::GuaranteeInsufficient,
                    ..
                })
            ),
            "the Inert floor cannot guarantee any budget: {refused:?}"
        );
    }
}
