//! C0 contract suite: fail-closed planning, sealed reports, deterministic
//! profile derivation, and 0xE event round-trips — all against the honest
//! [`InertBackend`].

use bvisor::{
    Backend, BackendRegistry, BoundaryPlanner, BoundarySpec, BudgetRequirements, Capability,
    EvidenceRequirements, FsAccess, FsConfinement, HostControl, InertBackend, PathSet, PlanError,
    Workload,
};
#[cfg(feature = "dangerous-test-hooks")]
use bvisor::{
    BoundaryRecoveryEvent, BoundaryReportEvent, BoundaryRunner, BoundaryStartedEvent, MinGuarantee,
    Outcome, StdStreams,
};
use std::sync::Arc;

fn registry() -> BackendRegistry {
    let mut registry = BackendRegistry::new();
    registry.register(Arc::new(InertBackend::new()));
    registry
}

fn inert_id() -> bvisor::BackendId {
    InertBackend::new().id()
}

/// The honest [`SimBackend`] monster: Enforced support for everything, full
/// evidence, and a runnable budget profile. Unlike Inert (the all-Unsupported
/// lattice floor, which refuses every budgeted spec at the budget membrane), the
/// honest Sim admits any spec whose budget fits its capacities — so it is what
/// the POSITIVE (admit/run/seal) tests must plan against. Only available under
/// `dangerous-test-hooks`.
#[cfg(feature = "dangerous-test-hooks")]
fn sim_registry() -> BackendRegistry {
    let mut registry = BackendRegistry::new();
    registry.register(Arc::new(bvisor::__sim::SimBackend::new(Box::new(
        bvisor::__sim::OneShotLiar::new(bvisor::__sim::LieMode::Honest),
    ))));
    registry
}

#[cfg(feature = "dangerous-test-hooks")]
fn sim_id() -> bvisor::BackendId {
    bvisor::BackendId::new(bvisor::__sim::SimBackend::ID)
}

/// A workload that exists on every CI host: `true` (exit 0). Avoids shell quirks.
fn trivial_workload() -> Workload {
    Workload::Process {
        exe: "true".to_string(),
        args: Vec::new(),
    }
}

// (a) plan() is fail-closed: a spec requiring real filesystem confinement on
//     InertBackend returns Err(Unsupported).
#[test]
fn plan_fails_closed_on_required_confinement() {
    let registry = registry();
    let planner = BoundaryPlanner::new(&registry);
    let spec = BoundarySpec {
        workload: trivial_workload(),
        capabilities: vec![Capability::Filesystem {
            access: FsAccess::ReadWrite,
            scope: PathSet::empty(),
            recursive: true,
            confinement: FsConfinement::DeclaredRootsOnly,
        }],
        controls: vec![HostControl::LaunchWorkload],
        budgets: BudgetRequirements::deny_all(),
        evidence: EvidenceRequirements::default(),
    };

    let err = planner
        .plan(&spec, &inert_id())
        .expect_err("inert must refuse real filesystem confinement");
    assert!(
        matches!(&err, PlanError::Unsupported { backend, .. } if *backend == inert_id()),
        "expected Unsupported naming the inert backend, got {err:?}"
    );
}

// (b) a zero-confinement spec (just LaunchWorkload + CaptureStreams) with a
//     runnable budget plans on the honest Sim, and run() yields a sealed report
//     with a stable body_hash. (Inert refuses every budgeted spec at the floor.)
#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn zero_confinement_plans_runs_and_seals_stably() {
    let registry = sim_registry();
    let planner = BoundaryPlanner::new(&registry);
    let runner = BoundaryRunner::new(&registry);

    let spec = BoundarySpec {
        workload: trivial_workload(),
        capabilities: Vec::new(),
        controls: vec![
            HostControl::LaunchWorkload,
            HostControl::CaptureStreams {
                streams: StdStreams::capture_out_err(),
            },
        ],
        budgets: BudgetRequirements::uniform(64, MinGuarantee::Mediated),
        evidence: EvidenceRequirements::default(),
    };

    let plan = planner
        .plan(&spec, &sim_id())
        .expect("zero-confinement spec must admit on the honest sim");
    assert_eq!(plan.admitted.len(), 2, "both host controls are admitted");

    let report = runner.run(&plan).expect("run must seal a report");
    assert_eq!(report.body.outcome, Outcome::Completed);

    // The seal is stable: re-hashing the body reproduces the sealed hash, and a
    // second identical run seals the same body_hash.
    let rehash = report.body.body_hash().expect("body re-hashes");
    assert_eq!(rehash, report.body_hash, "seal is reproducible");

    let report2 = runner.run(&plan).expect("second run seals");
    assert_eq!(
        report.body_hash, report2.body_hash,
        "identical plans seal identical body hashes"
    );
}

// (c) BackendProfile derivation from a snapshot is deterministic.
#[test]
fn profile_derivation_is_deterministic() {
    let backend = InertBackend::new();
    let snapshot = backend.probe();
    let snapshot_again = backend.probe();
    assert_eq!(snapshot, snapshot_again, "probe is deterministic");

    let profile = backend.profile(&snapshot);
    let profile_again = backend.profile(&snapshot);
    assert_eq!(
        profile, profile_again,
        "profile derivation is deterministic for the same snapshot"
    );
}

// (d) the 0xE EventPayload derives compile and round-trip serialize.
#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn event_payloads_round_trip() {
    let registry = sim_registry();
    let planner = BoundaryPlanner::new(&registry);
    let runner = BoundaryRunner::new(&registry);

    let spec = BoundarySpec {
        workload: trivial_workload(),
        capabilities: Vec::new(),
        controls: vec![HostControl::LaunchWorkload],
        budgets: BudgetRequirements::uniform(64, MinGuarantee::Mediated),
        evidence: EvidenceRequirements::default(),
    };
    let plan = planner.plan(&spec, &sim_id()).expect("plan admits");
    let report = runner.run(&plan).expect("run seals");

    let plan_event = BoundaryStartedEvent { plan: plan.clone() };
    let report_event = BoundaryReportEvent {
        report: report.clone(),
    };
    let recovery_event = BoundaryRecoveryEvent {
        plan_id: plan.plan_id,
        classification: bvisor::RecoveryClassification::Completed,
        quarantined: Vec::new(),
    };

    let plan_bytes = batpak::canonical::to_bytes(&plan_event).expect("encode plan event");
    let report_bytes = batpak::canonical::to_bytes(&report_event).expect("encode report event");
    let recovery_bytes =
        batpak::canonical::to_bytes(&recovery_event).expect("encode recovery event");

    let plan_back: BoundaryStartedEvent =
        batpak::canonical::from_bytes(&plan_bytes).expect("decode plan event");
    let report_back: BoundaryReportEvent =
        batpak::canonical::from_bytes(&report_bytes).expect("decode report event");
    let recovery_back: BoundaryRecoveryEvent =
        batpak::canonical::from_bytes(&recovery_bytes).expect("decode recovery event");

    assert_eq!(plan_back, plan_event);
    assert_eq!(report_back, report_event);
    assert_eq!(recovery_back, recovery_event);
}

// (e) plan() is fail-closed on UNCOVERABLE evidence: requiring captured streams
//     without admitting CaptureStreams yields EvidenceUnsatisfiable. Inert's
//     LaunchWorkload witnesses only the terminal outcome, so the required
//     `CapturedStreams` claim is not a subset of the admitted evidence.
#[test]
fn plan_fails_closed_when_required_evidence_uncoverable() {
    let registry = registry();
    let planner = BoundaryPlanner::new(&registry);
    let spec = BoundarySpec {
        workload: trivial_workload(),
        capabilities: Vec::new(),
        controls: vec![HostControl::LaunchWorkload],
        budgets: BudgetRequirements::deny_all(),
        evidence: EvidenceRequirements {
            require_captured_streams: true,
            require_exit_status: false,
        },
    };

    let err = planner
        .plan(&spec, &inert_id())
        .expect_err("inert cannot witness captured streams without CaptureStreams admitted");
    assert!(
        matches!(&err, PlanError::EvidenceUnsatisfiable { backend, .. } if *backend == inert_id()),
        "expected EvidenceUnsatisfiable naming the inert backend, got {err:?}"
    );
}

// (f) the inverse: requiring captured streams AND exit status, WITH both
//     LaunchWorkload + CaptureStreams admitted and a runnable budget, plans OK
//     on the honest Sim (required ⊆ available evidence; budget fits capacities).
#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn plan_admits_when_required_evidence_is_covered() {
    let registry = sim_registry();
    let planner = BoundaryPlanner::new(&registry);
    let spec = BoundarySpec {
        workload: trivial_workload(),
        capabilities: Vec::new(),
        controls: vec![
            HostControl::LaunchWorkload,
            HostControl::CaptureStreams {
                streams: StdStreams::capture_out_err(),
            },
        ],
        budgets: BudgetRequirements::uniform(64, MinGuarantee::Mediated),
        evidence: EvidenceRequirements {
            require_captured_streams: true,
            require_exit_status: true,
        },
    };

    planner
        .plan(&spec, &sim_id())
        .expect("covered evidence (CapturedStreams + TerminalOutcome) must admit");
}
