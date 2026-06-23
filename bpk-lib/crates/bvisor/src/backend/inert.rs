//! [`InertBackend`] — enforces NOTHING, honestly.
//!
//! The no-confinement reference backend. It can launch a process and capture
//! its stdio with no confinement, so it classifies
//! [`HostControl::LaunchWorkload`] and [`HostControl::CaptureStreams`] as
//! [`Enforcement::Enforced`] and EVERYTHING ELSE as [`Enforcement::Unsupported`].
//! Therefore `plan()` against Inert succeeds ONLY for a spec requesting no real
//! confinement, and fails closed otherwise.
//!
//! `execute()` returns a [`BoundaryReportBody`] with the workload's outcome and
//! honest `observed` / `admitted` (no enforcement claimed). It MAY use
//! `std::process` because Inert is the no-confinement reference — but the
//! [`Backend`] trait and all contract types stay OS-free.

use crate::contract::backend::Backend;
use crate::contract::capability::{Enforcement, EvidenceClaim, SupportVerdict};
use crate::contract::host_control::HostControl;
use crate::contract::ids::BackendId;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement, Workload};
use crate::contract::report::{
    ArtifactRecord, BoundaryFinding, BoundaryReportBody, CaptureRefs, DeniedAttempt, ExitStatus,
    ObservedFact, Outcome, BOUNDARY_REPORT_SCHEMA_VERSION,
};
use crate::contract::support::{
    BackendProfile, BackendProfileSnapshot, RequirementKind, SupportMatrix,
};
use std::collections::BTreeMap;
use std::process::Command;

/// The honest no-confinement reference backend.
pub struct InertBackend {
    id: BackendId,
    support: SupportMatrix,
}

impl InertBackend {
    /// The stable id of the inert backend.
    pub const ID: &'static str = "inert";

    /// Construct the inert backend with its honest support matrix:
    /// `LaunchWorkload` + `CaptureStreams` Enforced, everything else absent
    /// (defaulting to Unsupported).
    #[must_use]
    pub fn new() -> Self {
        let mut best_case = BTreeMap::new();
        best_case.insert(
            RequirementKind::LaunchWorkload,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::TerminalOutcome].into_iter().collect(),
            ),
        );
        best_case.insert(
            RequirementKind::CaptureStreams,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::CapturedStreams].into_iter().collect(),
            ),
        );
        Self {
            id: BackendId::new(Self::ID),
            support: SupportMatrix::from_best_case(best_case),
        }
    }
}

impl Default for InertBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for InertBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn support(&self) -> &SupportMatrix {
        &self.support
    }

    fn probe(&self) -> BackendProfileSnapshot {
        // Inert probes nothing real — it states, as raw evidence, that it
        // confines nothing. Deterministic so replay re-derives identically.
        let mut probed = BTreeMap::new();
        probed.insert("confinement".to_string(), "none".to_string());
        probed.insert("reference".to_string(), "inert".to_string());
        BackendProfileSnapshot {
            backend: self.id.clone(),
            probed,
        }
    }

    fn profile(&self, _snap: &BackendProfileSnapshot) -> BackendProfile {
        // The machine ceiling matches the family best-case: it can launch a
        // process and wire pipes, and nothing else. Derived deterministically.
        let mut ceiling = BTreeMap::new();
        ceiling.insert(
            RequirementKind::LaunchWorkload,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::TerminalOutcome].into_iter().collect(),
            ),
        );
        ceiling.insert(
            RequirementKind::CaptureStreams,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::CapturedStreams].into_iter().collect(),
            ),
        );
        BackendProfile::from_ceiling(ceiling)
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        let mut observed = Vec::new();
        let mut findings = Vec::new();

        // Honest findings: Inert confines nothing, so every admitted
        // requirement is recorded as no-confinement evidence alongside the
        // admission record.
        for admitted in &plan.admitted {
            findings.push(BoundaryFinding::RequirementAdmitted {
                requirement: admitted.requirement.clone(),
                enforcement: admitted.enforcement,
            });
            findings.push(BoundaryFinding::NoConfinement {
                requirement: admitted.requirement.clone(),
            });
        }

        let wants_capture = plan.admitted.iter().any(|a| {
            matches!(
                a.requirement,
                BoundaryRequirement::HostControl(HostControl::CaptureStreams { .. })
            )
        });

        let (outcome, exit, captured) = self.launch(plan, wants_capture, &mut observed);

        BoundaryReportBody {
            schema_version: BOUNDARY_REPORT_SCHEMA_VERSION,
            plan_id: plan.plan_id,
            backend: self.id.clone(),
            profile: self.probe(),
            outcome,
            admitted: plan.admitted.clone(),
            observed,
            denied: Vec::<DeniedAttempt>::new(),
            exit,
            captured,
            artifacts: Vec::<ArtifactRecord>::new(),
            findings,
        }
    }
}

impl InertBackend {
    /// Launch the workload with no confinement, returning the terminal outcome,
    /// the exit status, and any captured-stream refs. Never panics: every
    /// failure path resolves to an honest [`Outcome`].
    fn launch(
        &self,
        plan: &BoundaryPlan,
        wants_capture: bool,
        observed: &mut Vec<ObservedFact>,
    ) -> (Outcome, Option<ExitStatus>, CaptureRefs) {
        let (exe, args) = match &plan.workload {
            Workload::Process { exe, args } => (exe, args),
            Workload::Wasm { module_ref } => {
                // Inert is a process reference; it cannot run a wasm guest.
                observed.push(ObservedFact {
                    kind: "workload_unsupported".to_string(),
                    detail: format!("inert cannot run wasm module {module_ref}"),
                });
                return (Outcome::Unsupported, None, CaptureRefs::default());
            }
        };

        let mut command = Command::new(exe);
        command.args(args);

        let result = if wants_capture {
            command.output().map(CommandResult::Captured)
        } else {
            command.status().map(CommandResult::Status)
        };

        match result {
            Ok(CommandResult::Status(status)) => {
                observed.push(ObservedFact {
                    kind: "workload_launched".to_string(),
                    detail: format!("inert spawned {exe} (no confinement)"),
                });
                let exit = exit_from_status(&status);
                (terminal_outcome(&exit), exit, CaptureRefs::default())
            }
            Ok(CommandResult::Captured(output)) => {
                observed.push(ObservedFact {
                    kind: "workload_launched".to_string(),
                    detail: format!("inert spawned {exe} (no confinement)"),
                });
                observed.push(ObservedFact {
                    kind: "stream_captured".to_string(),
                    detail: format!(
                        "captured {} stdout byte(s), {} stderr byte(s)",
                        output.stdout.len(),
                        output.stderr.len()
                    ),
                });
                let exit = exit_from_status(&output.status);
                let captured = CaptureRefs {
                    stdout: Some(format!("inline:{}b", output.stdout.len())),
                    stderr: Some(format!("inline:{}b", output.stderr.len())),
                };
                (terminal_outcome(&exit), exit, captured)
            }
            Err(error) => {
                observed.push(ObservedFact {
                    kind: "workload_launch_failed".to_string(),
                    detail: format!("inert could not spawn {exe}: {error}"),
                });
                (Outcome::Failed, None, CaptureRefs::default())
            }
        }
    }
}

enum CommandResult {
    Status(std::process::ExitStatus),
    Captured(std::process::Output),
}

/// Map a portable terminal exit into the run-time [`Outcome`].
fn terminal_outcome(exit: &Option<ExitStatus>) -> Outcome {
    match exit {
        Some(ExitStatus::Code(0)) => Outcome::Completed,
        Some(ExitStatus::Code(_)) | Some(ExitStatus::Signal(_)) => Outcome::Failed,
        None => Outcome::Failed,
    }
}

/// Convert a `std::process::ExitStatus` into the portable [`ExitStatus`].
fn exit_from_status(status: &std::process::ExitStatus) -> Option<ExitStatus> {
    if let Some(code) = status.code() {
        return Some(ExitStatus::Code(code));
    }
    signal_exit(status)
}

#[cfg(unix)]
fn signal_exit(status: &std::process::ExitStatus) -> Option<ExitStatus> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(ExitStatus::Signal)
}

#[cfg(not(unix))]
fn signal_exit(_status: &std::process::ExitStatus) -> Option<ExitStatus> {
    None
}
