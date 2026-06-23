//! Lifecycle typestate runtime suite: the legal Spec→Planned→Started→Reported
//! sequence works end-to-end against the honest [`InertBackend`], the emitted
//! 0x001/0x002 events carry the right plan, and the [`Boundary::into_reported`]
//! semantic guard refuses a report that answers a different plan (Law #0:
//! bvisor validates "this report belongs to this attempt", which the substrate
//! cannot). The illegal *orderings* are proven uncompilable by the
//! `compile_fail` doctests on `contract::lifecycle`.

use bvisor::{
    Backend, BackendRegistry, Boundary, BoundaryRunner, BoundarySpec, Budgets,
    EvidenceRequirements, HostControl, InertBackend, LifecycleError, Workload,
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

/// A zero-confinement spec running `exe` — plannable on Inert.
fn spec_running(exe: &str) -> BoundarySpec {
    BoundarySpec {
        workload: Workload::Process {
            exe: exe.to_string(),
            args: Vec::new(),
        },
        capabilities: Vec::new(),
        controls: vec![HostControl::LaunchWorkload],
        budgets: Budgets::default(),
        evidence: EvidenceRequirements::default(),
    }
}

// The legal lifecycle path: propose → plan → start → into_reported, with the
// emitted events bound to the right plan and attempt.
#[test]
fn legal_lifecycle_sequence_runs_end_to_end() {
    let registry = registry();
    let runner = BoundaryRunner::new(&registry);

    let planned = Boundary::propose(spec_running("true"), inert_id())
        .plan(&registry)
        .expect("zero-confinement spec must plan on inert");
    let plan_id = planned.plan_id();

    let attempt = bvisor::AttemptId([7u8; 32]);
    let started = planned.start(attempt);
    assert_eq!(started.attempt(), attempt, "the attempt id is carried");
    assert_eq!(
        started.started_event().plan.plan_id,
        plan_id,
        "the 0x001 started event embeds the started plan"
    );

    // The runner seals the report the typestate then records.
    let report = runner.run(started.as_plan()).expect("inert run seals");
    let reported = started
        .into_reported(report)
        .expect("a report answering this plan is accepted");

    assert_eq!(reported.attempt(), attempt, "the attempt survives the seal");
    assert_eq!(reported.plan_id(), plan_id, "the plan id survives the seal");
    assert_eq!(
        reported.report_event().report.body.plan_id,
        plan_id,
        "the 0x002 report event answers the started plan"
    );

    // into_parts hands the host the owned plan/attempt/report for disposition.
    let (plan, parts_attempt, parts_report) = reported.into_parts();
    assert_eq!(plan.plan_id, plan_id);
    assert_eq!(parts_attempt, attempt);
    assert_eq!(parts_report.body.plan_id, plan_id);
}

// The semantic guard: a report sealed for a DIFFERENT plan is refused at
// into_reported — foreign evidence never staples to an attempt.
#[test]
fn into_reported_rejects_a_report_for_a_different_plan() {
    let registry = registry();
    let runner = BoundaryRunner::new(&registry);

    // Two distinct plans (different workloads → different plan ids).
    let started = Boundary::propose(spec_running("true"), inert_id())
        .plan(&registry)
        .expect("plan A")
        .start(bvisor::AttemptId([1u8; 32]));
    let other_planned = Boundary::propose(spec_running("false"), inert_id())
        .plan(&registry)
        .expect("plan B");
    assert_ne!(
        started.plan_id(),
        other_planned.plan_id(),
        "the two specs must produce distinct plan ids for this test to bite"
    );

    // Seal a report for plan B, then offer it to the attempt started on plan A.
    let foreign_report = runner.run(other_planned.as_plan()).expect("run B seals");
    let foreign_id = foreign_report.body.plan_id;
    let started_id = started.plan_id();

    let err = started
        .into_reported(foreign_report)
        .expect_err("a report for plan B must not bind to an attempt on plan A");
    assert_eq!(
        err,
        LifecycleError::ReportPlanMismatch {
            expected: started_id,
            found: foreign_id,
        },
        "the mismatch names both plan ids"
    );
}
