//! `Boundary<State>` — the lifecycle typestate (borrow-checker-as-a-feature).
//!
//! Makes illegal orderings *uncompilable* rather than merely documented. The
//! states mirror BatPak's `Store<Open|ReadOnly|Closed>` sealed-marker pattern
//! (`crates/core/src/store/mod.rs:218-309`): a private `sealed::Sealed`
//! supertrait gates which types may be a [`BoundaryState`], and every transition
//! **consumes `self`** (affine) so a consumed state cannot be reused.
//!
//! ```text
//! Boundary<Spec> ─plan(reg)──→ Boundary<Planned>   [PURE: fail-closed admission]
//!                                   │ start(attempt)
//!                                   ▼
//! Boundary<Started> ─into_reported(report)→ Boundary<Reported>   [seal = PURE hashing]
//!                                   │ into_report_event()
//!                                   ▼
//!                          host persists 0x002
//! ```
//!
//! What the typestate guarantees is **ordering**, in the types: you cannot seal
//! an unstarted run, cannot persist before a run is reported, and cannot re-run a
//! consumed [`Started`] (a retry is a fresh [`Boundary::start`] minting a new
//! [`AttemptId`]). What it does NOT do is perform effects: the host wraps
//! [`Boundary::start`] with the effectful re-probe / `ProfileDrift` revalidation,
//! the durable 0x001 append, and the launch; the runner produces the sealed
//! [`BoundaryReport`] that [`Boundary::into_reported`] records. The borrow
//! checker *narrows* mistakes; recovery (§13) *closes* reality.
//!
//! # Illegal orderings (compile-fail proofs)
//!
//! Seal an unstarted run — there is no `into_reported` on `Boundary<Planned>`:
//! ```compile_fail
//! use bvisor::{Boundary, Planned, BoundaryReport};
//! fn illegal(planned: Boundary<Planned>, report: BoundaryReport) {
//!     let _ = planned.into_reported(report); // ERROR: must start() first
//! }
//! ```
//!
//! Persist before reported — there is no `into_report_event` on
//! `Boundary<Started>`:
//! ```compile_fail
//! use bvisor::{Boundary, Started};
//! fn illegal(started: Boundary<Started>) {
//!     let _ = started.into_report_event(); // ERROR: must into_reported() first
//! }
//! ```
//!
//! Re-run a consumed `Started` — transitions move `self`, so the second use is a
//! use-after-move:
//! ```compile_fail
//! use bvisor::{Boundary, Started, BoundaryReport};
//! fn illegal(started: Boundary<Started>, a: BoundaryReport, b: BoundaryReport) {
//!     let _ = started.into_reported(a);
//!     let _ = started.into_reported(b); // ERROR: `started` moved above
//! }
//! ```
//!
//! Start without planning — there is no `start` on `Boundary<Spec>`:
//! ```compile_fail
//! use bvisor::{Boundary, Spec, AttemptId};
//! fn illegal(proposed: Boundary<Spec>, attempt: AttemptId) {
//!     let _ = proposed.start(attempt); // ERROR: must plan() first
//! }
//! ```
//!
//! The legal sequence does compile. `no_run`: the all-`Unsupported` Inert floor
//! refuses every budgeted spec at runtime (it guarantees nothing) — a capable backend
//! (the honest `SimBackend`, or a native one) admits the SAME shape. This example's
//! purpose is to prove the typestate `propose → plan → start` COMPILES:
//! ```no_run
//! use bvisor::{
//!     Backend, BackendRegistry, Boundary, BoundarySpec, BudgetRequirements,
//!     EvidenceRequirements, HostControl, InertBackend, MinGuarantee, Workload,
//! };
//! use std::sync::Arc;
//!
//! let mut registry = BackendRegistry::new();
//! registry.register(Arc::new(InertBackend::new()));
//! let backend = InertBackend::new().id();
//!
//! let spec = BoundarySpec {
//!     workload: Workload::Process { exe: "true".into(), args: Vec::new() },
//!     capabilities: Vec::new(),
//!     controls: vec![HostControl::LaunchWorkload],
//!     budgets: BudgetRequirements::uniform(64, MinGuarantee::Mediated),
//!     evidence: EvidenceRequirements::default(),
//! };
//!
//! let planned = Boundary::propose(spec, backend).plan(&registry).unwrap();
//! let attempt = bvisor::AttemptId([7u8; 32]);
//! let _started = planned.start(attempt);
//! ```

use crate::contract::events::{BoundaryReportEvent, BoundaryStartedEvent};
use crate::contract::ids::{AttemptId, BackendId, BoundaryPlanHash};
use crate::contract::plan::{BoundaryPlan, BoundarySpec, PlanError};
use crate::contract::registry::{BackendRegistry, BoundaryPlanner};
use crate::contract::report::BoundaryReport;

/// Sealing module for [`BoundaryState`] (mirrors `store::sealed`).
///
/// `Sealed` is implemented only by this module's lifecycle markers, so
/// downstream code can neither add new [`BoundaryState`] implementors nor forge
/// a state out of order.
mod sealed {
    /// Marker implemented by every [`super::BoundaryState`] type.
    pub trait Sealed {}
}

/// Sealed marker bound for the [`Boundary`] typestate parameter.
///
/// Public so it can bound the `S` parameter on the public [`Boundary`] type, but
/// sealed via the private `sealed::Sealed` supertrait: only the four lifecycle
/// markers in this module implement it, and their payload fields are
/// module-private, so a state can only be reached through the legal transition
/// that constructs it.
pub trait BoundaryState: sealed::Sealed {}

/// A proposed boundary, not yet admitted. Carries the request and the chosen
/// backend; the next legal move is [`Boundary::plan`].
#[derive(Debug)]
pub struct Spec {
    spec: BoundarySpec,
    backend: BackendId,
}

/// An admitted, machine-bound plan. The next legal move is [`Boundary::start`].
#[derive(Debug)]
pub struct Planned {
    plan: BoundaryPlan,
}

/// A started attempt (one [`AttemptId`]). The next legal move is
/// [`Boundary::into_reported`] once the runner seals a [`BoundaryReport`].
#[derive(Debug)]
pub struct Started {
    plan: BoundaryPlan,
    attempt: AttemptId,
}

/// A reported attempt: the sealed terminal evidence is in hand. The next legal
/// move is [`Boundary::into_report_event`] (the host persists 0x002).
#[derive(Debug)]
pub struct Reported {
    plan: BoundaryPlan,
    attempt: AttemptId,
    report: BoundaryReport,
}

impl sealed::Sealed for Spec {}
impl sealed::Sealed for Planned {}
impl sealed::Sealed for Started {}
impl sealed::Sealed for Reported {}

impl BoundaryState for Spec {}
impl BoundaryState for Planned {}
impl BoundaryState for Started {}
impl BoundaryState for Reported {}

/// A boundary in lifecycle state `S`. NOT `Clone`/`Copy`: transitions move
/// `self`, so a consumed state cannot be reused (a retry mints a new attempt).
#[derive(Debug)]
pub struct Boundary<S: BoundaryState> {
    state: S,
}

impl Boundary<Spec> {
    /// Propose a boundary: a [`BoundarySpec`] against a chosen backend. PURE.
    #[must_use]
    pub fn propose(spec: BoundarySpec, backend: BackendId) -> Self {
        Self {
            state: Spec { spec, backend },
        }
    }

    /// Borrow the proposed spec.
    #[must_use]
    pub fn spec(&self) -> &BoundarySpec {
        &self.state.spec
    }

    /// The backend this proposal targets.
    #[must_use]
    pub fn backend(&self) -> &BackendId {
        &self.state.backend
    }

    /// Admit the spec against the chosen backend, FAIL-CLOSED. PURE
    /// (deterministic admission; the planner probes the registered backend's
    /// declared profile, never the live OS).
    ///
    /// Consumes the proposal: a [`PlanError`] leaves no half-planned boundary —
    /// the caller re-proposes.
    ///
    /// # Errors
    /// Any [`PlanError`] from [`BoundaryPlanner::plan`] (unknown backend,
    /// unsupported required requirement, unsatisfiable evidence, …).
    pub fn plan(self, registry: &BackendRegistry) -> Result<Boundary<Planned>, PlanError> {
        let Spec { spec, backend } = self.state;
        let plan = BoundaryPlanner::new(registry).plan(&spec, &backend)?;
        Ok(Boundary {
            state: Planned { plan },
        })
    }
}

impl Boundary<Planned> {
    /// Borrow the admitted plan.
    #[must_use]
    pub fn as_plan(&self) -> &BoundaryPlan {
        &self.state.plan
    }

    /// The plan's content-addressed identity.
    #[must_use]
    pub fn plan_id(&self) -> BoundaryPlanHash {
        self.state.plan.plan_id
    }

    /// Begin one attempt under the supplied [`AttemptId`]. PURE type move:
    /// it records the attempt and unlocks [`Boundary::started_event`] (the 0x001
    /// payload the host appends, gated `Durable`, BEFORE executing).
    ///
    /// The host owns the surrounding effects — re-probe + `ProfileDrift`
    /// revalidation, the durable append, and the launch (§5, step 5). The
    /// typestate guarantees only that those effects happen in order, after a
    /// plan and before a report. A retry is a *new* `start` with a *new*
    /// [`AttemptId`], never a reuse of a consumed [`Started`].
    #[must_use]
    pub fn start(self, attempt: AttemptId) -> Boundary<Started> {
        let Planned { plan } = self.state;
        Boundary {
            state: Started { plan, attempt },
        }
    }
}

impl Boundary<Started> {
    /// The attempt this run is recorded under.
    #[must_use]
    pub fn attempt(&self) -> AttemptId {
        self.state.attempt
    }

    /// The plan being attempted.
    #[must_use]
    pub fn as_plan(&self) -> &BoundaryPlan {
        &self.state.plan
    }

    /// The plan's content-addressed identity.
    #[must_use]
    pub fn plan_id(&self) -> BoundaryPlanHash {
        self.state.plan.plan_id
    }

    /// The 0x001 [`BoundaryStartedEvent`] the host appends (gated `Durable`)
    /// before the backend executes. Its presence in the stream == "this attempt
    /// started" (§3); a plan never started leaves no event.
    #[must_use]
    pub fn started_event(&self) -> BoundaryStartedEvent {
        BoundaryStartedEvent {
            plan: self.state.plan.clone(),
        }
    }

    /// Record the sealed [`BoundaryReport`] the runner produced, advancing to
    /// [`Reported`]. Consumes the [`Started`] — the run cannot be re-driven.
    ///
    /// Validates the semantic relation the substrate cannot (Law #0): the report
    /// must answer *this* attempt's plan. A mismatch fails closed with
    /// [`LifecycleError::ReportPlanMismatch`] rather than stapling foreign
    /// evidence to the attempt.
    ///
    /// # Errors
    /// [`LifecycleError::ReportPlanMismatch`] if the report's `plan_id` differs
    /// from the started plan's.
    pub fn into_reported(
        self,
        report: BoundaryReport,
    ) -> Result<Boundary<Reported>, LifecycleError> {
        let Started { plan, attempt } = self.state;
        if report.body.plan_id != plan.plan_id {
            return Err(LifecycleError::ReportPlanMismatch {
                expected: plan.plan_id,
                found: report.body.plan_id,
            });
        }
        Ok(Boundary {
            state: Reported {
                plan,
                attempt,
                report,
            },
        })
    }
}

impl Boundary<Reported> {
    /// The attempt this report belongs to.
    #[must_use]
    pub fn attempt(&self) -> AttemptId {
        self.state.attempt
    }

    /// The plan this report answers.
    #[must_use]
    pub fn as_plan(&self) -> &BoundaryPlan {
        &self.state.plan
    }

    /// The plan's content-addressed identity.
    #[must_use]
    pub fn plan_id(&self) -> BoundaryPlanHash {
        self.state.plan.plan_id
    }

    /// Borrow the sealed report.
    #[must_use]
    pub fn report(&self) -> &BoundaryReport {
        &self.state.report
    }

    /// Build the 0x002 [`BoundaryReportEvent`] the host persists, WITHOUT
    /// consuming the boundary (the host may still need the plan/attempt).
    #[must_use]
    pub fn report_event(&self) -> BoundaryReportEvent {
        BoundaryReportEvent {
            report: self.state.report.clone(),
        }
    }

    /// Consume into the 0x002 [`BoundaryReportEvent`] for persistence. Only a
    /// [`Reported`] boundary can produce this — "persist before reported" is
    /// uncompilable.
    #[must_use]
    pub fn into_report_event(self) -> BoundaryReportEvent {
        BoundaryReportEvent {
            report: self.state.report,
        }
    }

    /// Decompose into the owned plan, attempt, and sealed report (for the host's
    /// disposition ceremony, §7).
    #[must_use]
    pub fn into_parts(self) -> (BoundaryPlan, AttemptId, BoundaryReport) {
        let Reported {
            plan,
            attempt,
            report,
        } = self.state;
        (plan, attempt, report)
    }
}

/// Why a lifecycle transition refused. Distinct from [`PlanError`] (admission)
/// and [`crate::RecoveryClassification`] (reconciliation): this is the typestate
/// validating a semantic relation the substrate cannot prove on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LifecycleError {
    /// A sealed report was offered to an attempt whose plan it does not answer.
    ReportPlanMismatch {
        /// The started attempt's plan id.
        expected: BoundaryPlanHash,
        /// The plan id the offered report answers.
        found: BoundaryPlanHash,
    },
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReportPlanMismatch { expected, found } => write!(
                f,
                "report plan id {found:?} does not match the started attempt's plan id {expected:?}"
            ),
        }
    }
}

impl std::error::Error for LifecycleError {}
