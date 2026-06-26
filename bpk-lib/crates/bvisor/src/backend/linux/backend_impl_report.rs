//! The launcher-observation → report-body mapping for the Linux backend, split out of
//! `backend_impl.rs` to hold it under the non-overridable file-size cap. SAFE std: it only
//! shapes the honest [`LaunchObservation`] into the durable [`BoundaryReportBody`] the
//! seal/persist path consumes (no OS work — that is the launcher's). `super` is
//! `backend_impl`; these helpers read the backend's private id/probe like its siblings.

use super::LinuxBackend;
use crate::backend::linux::launch::LaunchObservation;
use crate::contract::backend::Backend;
use crate::contract::budget_witness::BudgetWitnesses;
use crate::contract::capability::{Capability, Enforcement, FsAccess, PathSet};
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::{
    BoundaryReportBody, CaptureRefs, ExitStatus, ObservedFact, Outcome,
    BOUNDARY_REPORT_SCHEMA_VERSION,
};

/// Map the launcher's honest [`LaunchObservation`] onto the report body, preserving
/// the report/evidence contract downstream (seal/persist 0xE) consumes.
///
/// HONESTY: the launcher reports its setup transcript (the terminal, the phase
/// resolutions, and `confinement_installed`) AND the host captures the WORKLOAD's
/// stdout/stderr through the launcher's inherited piped stdio (the launcher's clone3
/// child inherits the launcher's fd 0/1/2, and the launcher is stdio-silent on every
/// workload-running path, so the launcher process's piped stdout/stderr carry exactly
/// the workload's output). Those captured bytes back `CaptureStreams=Enforced`'s
/// `CapturedStreams` evidence claim — the body records the captured stream references
/// alongside a `stream_captured` byte-count fact. A landlock denial is STILL proven by
/// the INDEPENDENT on-disk oracle (the G-grid), and the honest confinement evidence is the
/// launcher's `confinement_installed` mechanism attestation. The terminal maps via the
/// protocol's `outcome_class` (ExecSucceeded becomes Completed, SetupRefused becomes
/// Unsupported as a fail-closed deny, and SetupFaulted becomes SupervisorFault), and a
/// missing terminal becomes SupervisorFault (the launcher died before resolving, so the
/// workload never ran).
pub(super) fn map_observation(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    exe: &str,
    confined: bool,
    obs: &LaunchObservation,
    mut observed: Vec<ObservedFact>,
    process_peak: Option<u64>,
) -> BoundaryReportBody {
    observed.push(ObservedFact {
        kind: "workload_launched".to_string(),
        detail: format!("launcher exec {exe} (confined={confined})"),
    });
    observed.push(ObservedFact {
        kind: "launcher_terminal".to_string(),
        detail: format!(
            "terminal={:?} confinement_installed={} launcher_exit={:?}",
            obs.terminal, obs.confinement_installed, obs.launcher_exit
        ),
    });
    // Surface the launcher's own mechanism notes (its honest attestation: the clone3
    // child pid, the confinement result/install). These are the mechanism evidence
    // the report carries now that the launcher (not the backend) confines.
    for note in &obs.notes {
        observed.push(ObservedFact {
            kind: "launcher_note".to_string(),
            detail: note.clone(),
        });
    }
    // A confined plan whose launcher reports NO install is an honesty fault, not a
    // silent pass: record it (the Outcome below still reflects the terminal).
    if confined && !obs.confinement_installed {
        observed.push(ObservedFact {
            kind: "confinement_not_installed".to_string(),
            detail: "a Filesystem-scoped plan ran but the launcher reported no \
                     landlock install"
                .to_string(),
        });
    }

    // The host captured the workload's stdout/stderr through the launcher's inherited
    // piped stdio (the launcher is stdio-silent on every workload-running path). Record
    // the honest byte-count fact + the stream references — this backs the
    // `CaptureStreams=Enforced` ceiling's `CapturedStreams` evidence claim. The bytes
    // are referenced (not inlined) to keep the report body bounded; the byte counts are
    // the audit evidence that capture actually flowed.
    observed.push(ObservedFact {
        kind: "stream_captured".to_string(),
        detail: format!(
            "captured {} stdout byte(s), {} stderr byte(s) via the launcher's \
             inherited piped stdio",
            obs.captured_stdout.len(),
            obs.captured_stderr.len()
        ),
    });
    let captured = CaptureRefs {
        stdout: Some(format!("inline:{}b", obs.captured_stdout.len())),
        stderr: Some(format!("inline:{}b", obs.captured_stderr.len())),
    };

    // The process_count budget witness: a REAL `pids.peak` measurement when a cgroup cap
    // was installed (admitted Enforced) AND the kernel exposed `pids.peak`; otherwise the
    // unwitnessed echo (Hard guarantee from the admitted Enforced, ObservationUnavailable
    // — never a fabricated peak). Surface the measured peak as honest evidence too.
    let process_enforced = plan.budgets.process_count.selected_guarantee == Enforcement::Enforced;
    let budget = match process_peak {
        Some(peak) if process_enforced => {
            observed.push(ObservedFact {
                kind: "process_count_witnessed".to_string(),
                detail: format!(
                    "cgroup pids.peak={peak} against pids.max={} (cgroup_v2_pids: Hard cap)",
                    plan.budgets.process_count.effective_limit
                ),
            });
            BudgetWitnesses::with_process_count(&plan.budgets, peak)
        }
        _ => BudgetWitnesses::unwitnessed(&plan.budgets),
    };

    let outcome = obs.outcome().unwrap_or(Outcome::SupervisorFault);
    // The launcher does not surface the workload's own exit code (it reports its
    // setup terminal); ExecSucceeded means the workload image began executing under
    // confinement. No portable workload ExitStatus is available through this path.
    let exit = exec_exit(outcome);
    body(backend, plan, outcome, exit, captured, observed, budget)
}

/// The portable workload exit the launcher path can honestly report. The launcher
/// surfaces ONLY its own setup terminal, not the workload's exit code: a `Completed`
/// outcome means the workload exec'd under confinement (a clean image start), which
/// we report as `ExitStatus::Code(0)`; every non-Completed terminal carries no
/// workload exit (the workload never ran, or the launcher itself faulted).
fn exec_exit(outcome: Outcome) -> Option<ExitStatus> {
    match outcome {
        Outcome::Completed => Some(ExitStatus::Code(0)),
        Outcome::Denied
        | Outcome::Failed
        | Outcome::Timeout
        | Outcome::Killed
        | Outcome::Unsupported
        | Outcome::SupervisorFault => None,
    }
}

/// Extract the admitted Filesystem capability's access + scope, if one was
/// admitted into the plan.
pub(super) fn filesystem_capability(plan: &BoundaryPlan) -> Option<(FsAccess, PathSet)> {
    plan.admitted.iter().find_map(|a| match &a.requirement {
        BoundaryRequirement::Capability(Capability::Filesystem { access, scope, .. }) => {
            Some((*access, scope.clone()))
        }
        BoundaryRequirement::Capability(_) | BoundaryRequirement::HostControl(_) => None,
    })
}

/// Assemble a fail-closed report body (the workload never ran / ran-but-faulted
/// honestly): the given non-Completed [`Outcome`], no exit, no captured streams, no
/// denials. The accumulated `observed` facts carry WHY it failed closed.
pub(super) fn fail_closed(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    outcome: Outcome,
    observed: Vec<ObservedFact>,
) -> BoundaryReportBody {
    // A fail-closed path: the workload never ran (or faulted), so no dimension is
    // witnessed — the unwitnessed echo preserves the admitted contract honestly.
    body(
        backend,
        plan,
        outcome,
        None,
        CaptureRefs::default(),
        observed,
        BudgetWitnesses::unwitnessed(&plan.budgets),
    )
}

/// Assemble the honest report body. `budget` is the per-dimension witness set the
/// caller computed (the process_count dimension is genuinely witnessed from `pids.peak`
/// when a cgroup cap was installed; every other path passes the unwitnessed echo).
///
/// `denied` is always empty through the launcher path: a confinement DENIAL is proven by
/// the INDEPENDENT on-disk oracle (the G-grid), NOT self-reported here (the workload
/// inherits the launcher's stdio, so there is no stderr-derived denial to surface).
fn body(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    outcome: Outcome,
    exit: Option<ExitStatus>,
    captured: CaptureRefs,
    observed: Vec<ObservedFact>,
    budget: BudgetWitnesses,
) -> BoundaryReportBody {
    BoundaryReportBody {
        schema_version: BOUNDARY_REPORT_SCHEMA_VERSION,
        plan_id: plan.plan_id,
        backend: backend.id.clone(),
        profile: backend.probe(),
        outcome,
        admitted: plan.admitted.clone(),
        observed,
        denied: Vec::new(),
        exit,
        captured,
        budget,
        artifacts: Vec::new(),
        findings: Vec::new(),
    }
}
