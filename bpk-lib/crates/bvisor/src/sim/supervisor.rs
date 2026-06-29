//! [`SimSupervisor`] — pumps a [`BoundaryRun`] under injected crashes, and
//! [`SimProbe`] — derives the [`RecoveryProbe`] reality a crash leaves behind.
//!
//! The production runner pumps a [`BoundaryRun`] to its terminal step on the
//! calling thread; the supervisor pumps the SAME core but can stop after any step
//! (a crash injected in the gap between two steps). A crash before the terminal
//! `Sealed` step leaves NO report — exactly the in-flight state startup
//! reconciliation must classify. [`SimProbe`] then renders that as the
//! [`RecoveryProbe`] the host would independently observe, closing the loop
//! step → crash → probe → [`crate::reconcile`].

use crate::contract::recovery::{QuarantineRecord, RecoveryProbe};
use crate::contract::registry::{BoundaryRun, RunStep};
use crate::contract::report::{BoundaryReport, ObservedFact};
use std::collections::BTreeMap;

/// The result of driving a [`BoundaryRun`] under the [`SimSupervisor`].
#[derive(Clone, Debug)]
pub struct SimRun {
    /// Facts observed before the run ended (terminated or crashed).
    pub observed: Vec<ObservedFact>,
    /// The sealed report, if the run reached its terminal seal.
    pub sealed: Option<BoundaryReport>,
    /// The fault detail, if the run faulted at its terminal step.
    pub faulted: Option<String>,
    /// The step index a crash was injected after, if any (`None` = ran to end).
    pub crashed_after: Option<usize>,
}

impl SimRun {
    /// Whether the run sealed a report (reached durable terminal evidence).
    #[must_use]
    pub fn did_seal(&self) -> bool {
        self.sealed.is_some()
    }
}

/// Drives a [`BoundaryRun`] step by step, optionally crashing mid-flight.
pub struct SimSupervisor;

impl SimSupervisor {
    /// Pump `run` to completion (`crash_after = None`) or stop right AFTER the
    /// given step index (a crash injected in the following gap). `crash_after =
    /// Some(0)` crashes after the first step, before the second, and so on; a
    /// `crash_after` past the end simply runs to completion.
    #[must_use]
    pub fn drive(mut run: BoundaryRun, crash_after: Option<usize>) -> SimRun {
        let mut observed = Vec::new();
        let mut sealed = None;
        let mut faulted = None;
        let mut steps = 0usize;

        while let Some(step) = run.drive_step() {
            match step {
                RunStep::Observed(fact) => observed.push(fact),
                RunStep::Sealed(report) => {
                    sealed = Some(*report);
                    return SimRun {
                        observed,
                        sealed,
                        faulted,
                        crashed_after: None,
                    };
                }
                RunStep::Faulted(detail) => {
                    faulted = Some(detail);
                    return SimRun {
                        observed,
                        sealed,
                        faulted,
                        crashed_after: None,
                    };
                }
            }
            steps += 1;
            if crash_after == Some(steps - 1) {
                // Crash injected in the gap after this step: the run is dropped
                // mid-flight, so it never reaches its terminal seal.
                return SimRun {
                    observed,
                    sealed,
                    faulted,
                    crashed_after: Some(steps - 1),
                };
            }
        }

        SimRun {
            observed,
            sealed,
            faulted,
            crashed_after: None,
        }
    }
}

/// Renders the [`RecoveryProbe`] reality a crashed [`SimRun`] leaves behind — the
/// INDEPENDENT host observation, not the (absent) backend report.
pub struct SimProbe;

impl SimProbe {
    /// The host reality after `sim` crashed before sealing: the supplied orphans
    /// are still live, no report frame exists (clean crash, not torn), and no
    /// bytes were promoted (promotion is a post-report authorized act that never
    /// ran). A sealed run leaves no orphans to sweep.
    #[must_use]
    pub fn probe(sim: &SimRun, orphans: Vec<QuarantineRecord>) -> RecoveryProbe {
        if sim.did_seal() {
            return RecoveryProbe::default();
        }
        RecoveryProbe {
            orphans,
            torn_report: false,
            artifacts: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod supervisor_tests {
    use super::{SimProbe, SimSupervisor};
    use crate::contract::budget::{BudgetRequirements, MinGuarantee};
    use crate::contract::host_control::HostControl;
    use crate::contract::ids::{AttemptId, BackendId};
    use crate::contract::plan::{BoundaryPlan, BoundarySpec, EvidenceRequirements, Workload};
    use crate::contract::recovery::{reconcile, QuarantineRecord, RecoveryAction, RunView};
    use crate::contract::registry::{BackendRegistry, BoundaryPlanner, BoundaryRunner};
    use crate::sim::backend::{LieMode, OneShotLiar, SimBackend};
    use std::sync::Arc;

    fn sim_backend() -> SimBackend {
        // The honest monster: it admits a runnable budget (Inert, the floor, refuses
        // every budgeted spec). Honest mode produces a faithful report.
        SimBackend::new(Box::new(OneShotLiar::new(LieMode::Honest)))
    }

    fn plan() -> BoundaryPlan {
        let mut registry = BackendRegistry::new();
        registry.register(Arc::new(sim_backend()));
        let spec = BoundarySpec {
            workload: Workload::Process {
                exe: "true".to_string(),
                args: Vec::new(),
            },
            capabilities: Vec::new(),
            controls: vec![HostControl::LaunchWorkload],
            budgets: BudgetRequirements::uniform(64, MinGuarantee::Mediated),
            evidence: EvidenceRequirements::default(),
        };
        BoundaryPlanner::new(&registry)
            .plan(&spec, &BackendId::new(SimBackend::ID))
            .expect("honest sim plans a runnable budgeted launch spec")
    }

    fn run_for(plan: &BoundaryPlan) -> crate::contract::registry::BoundaryRun {
        let mut registry = BackendRegistry::new();
        registry.register(Arc::new(sim_backend()));
        // Leak a registry for the run's lifetime is unnecessary: begin borrows it
        // only for the call. Build, begin, return the owned run.
        BoundaryRunner::new(&registry)
            .begin(plan)
            .expect("begin a run")
    }

    // Driving to completion seals the same report run() produces — one core.
    #[test]
    fn drive_to_completion_seals() {
        let plan = plan();
        let sim = SimSupervisor::drive(run_for(&plan), None);
        assert!(sim.did_seal(), "an uninjured run seals");
        assert!(sim.faulted.is_none());
        assert!(sim.crashed_after.is_none());

        let mut registry = BackendRegistry::new();
        registry.register(Arc::new(sim_backend()));
        let direct = BoundaryRunner::new(&registry)
            .run(&plan)
            .expect("run seals");
        assert_eq!(
            sim.sealed.expect("sealed").body_hash,
            direct.body_hash,
            "the stepped seal equals the one-shot run() seal — shared core",
        );
    }

    // A crash before the seal leaves NO report; SimProbe renders the in-flight
    // reality, and production reconcile rolls it back, sweeping the orphans.
    #[test]
    fn crash_before_seal_reconciles_to_rollback() {
        let plan = plan();
        // Crash after the first observed fact (before the terminal seal).
        let sim = SimSupervisor::drive(run_for(&plan), Some(0));
        assert!(!sim.did_seal(), "a crash before seal leaves no report");
        assert_eq!(sim.crashed_after, Some(0));

        // The host independently observes one orphan; build the view (started, no
        // report) and reconcile.
        let orphan = QuarantineRecord {
            kind: "process".to_string(),
            reference: "pid:99".to_string(),
        };
        let probe = SimProbe::probe(&sim, vec![orphan.clone()]);
        let mut view = RunView::new(AttemptId([1u8; 32]), plan.plan_id);
        view.started = true; // a 0x001 was durable before execute

        assert_eq!(
            reconcile(&view, &probe),
            RecoveryAction::RollBack {
                orphans: vec![orphan]
            },
            "an in-flight run with live orphans rolls back",
        );
    }
}
