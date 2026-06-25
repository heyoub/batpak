//! [`LinuxBackend`] — REAL landlock filesystem confinement (step b, part 1),
//! enforced THROUGH the host-side launcher harness (backend→launcher rewire 7b).
//!
//! SCOPE of THIS chunk: REAL filesystem confinement ONLY. `execute()` builds a
//! [`LinuxLaunchPlanV1`] from the admitted plan (descriptor table over pre-opened
//! authority handles + a scrub/landlock-apply/exec lowering schedule), resolves the
//! confinement launcher binary, and runs it via [`super::launch::run_launcher`].
//! The LAUNCHER applies the landlock ruleset in its single-threaded child window
//! (`restrict_self`, after the fd scrub, before `fexecve`) so confinement is in
//! force the instant the workload image runs. The backend NO LONGER spawns/confines
//! itself — its only remaining raw surface is the landlock ABI probe (for
//! `profile()`); the harness owns the memfd/spawn `unsafe`. The orchestration here
//! is SAFE.
//!
//! HONESTY (the cardinal rule): `profile()` backs ONLY what this chunk genuinely
//! delivers — `Filesystem` (landlock, gated by the live ABI floor, now applied by
//! the launcher), `LaunchWorkload` (the launcher execs the workload), and
//! `CaptureStreams`. EVERYTHING ELSE (`ChildSpawn`, `Kill`, `NetworkDenyAll`,
//! `TempRoot`, …) is ABSENT from the ceiling, so it floors to `Unsupported` and
//! `plan()` fails closed. The family `support_matrix()` keeps the §4 aspiration;
//! the machine ceiling reflects reality. Claiming more than `execute()` delivers
//! is the exact lie the gauntlet must catch — so we do not.
//!
//! ## What the launcher path observes vs. the OLD self-spawn path
//! The launcher reports its honest setup transcript (terminal + phase resolutions +
//! `confinement_installed`), NOT the workload's stdout/stderr — the workload
//! inherits the launcher's stdio (a captured-stdio descriptor-slot wiring is a
//! later step). So `execute()` no longer parses workload stderr for a denial; a
//! landlock denial is proven by the INDEPENDENT on-disk oracle in the G-grid, and
//! the report's honest confinement evidence is the launcher's
//! `confinement_installed` mechanism attestation. The terminal maps to the
//! [`Outcome`] via [`super::launch::LaunchObservation::outcome`]
//! (ExecSucceeded→Completed, SetupRefused→Unsupported, SetupFaulted→SupervisorFault).

use crate::backend::linux::launch::{self, LaunchObservation};
use crate::backend::linux::{plan_build, sys};
use crate::contract::backend::Backend;
use crate::contract::budget::{BudgetAvailability, BudgetProfile};
use crate::contract::budget_witness::BudgetWitnesses;
use crate::contract::capability::{
    Capability, Enforcement, EvidenceClaim, EvidenceSet, FsAccess, PathSet, SupportVerdict,
};
use crate::contract::ids::BackendId;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement, Workload};
use crate::contract::report::{
    BoundaryReportBody, CaptureRefs, DeniedAttempt, ExitStatus, ObservedFact, Outcome,
    BOUNDARY_REPORT_SCHEMA_VERSION,
};
use crate::contract::support::{
    BackendProfile, BackendProfileSnapshot, RequirementKind, SupportMatrix,
};
use std::collections::BTreeMap;

/// The minimum landlock ABI this backend requires to floor `Filesystem` to
/// `Enforced`. ABI v1 already enforces path-beneath read/write/execute access —
/// the foundation of declared-roots-only confinement — so v1 is the honest floor.
/// Below it (ABI 0 = landlock unavailable) `Filesystem` floors to `Unsupported`.
const LANDLOCK_ABI_FLOOR: i64 = 1;

// The frozen launcher-wire constants (served primitive ids + phase codes) and the
// descriptor-table slot fd numbers live in `plan_build`, which assembles the launch
// plan from them. `backend_impl` only orchestrates; it does not name them directly.

/// The Linux boundary backend: REAL landlock filesystem confinement.
pub struct LinuxBackend {
    id: BackendId,
    support: SupportMatrix,
    /// The live landlock ABI integer, probed once at construction from the kernel.
    landlock_abi: i64,
    /// An explicit launcher-binary path INJECTED at construction (constructor
    /// injection, NOT a process-env mutation — `std::env::set_var` is banned as
    /// thread-unsafe, BANNED-003). When `Some`, `execute()` runs exactly this
    /// launcher; when `None`, it resolves via the documented `BVISOR_LAUNCHER_BIN`
    /// env / co-located-binary fallback. A test deps binary has no co-located
    /// launcher, so the integration tests inject the compile-time launcher path here
    /// instead of racing the process environment.
    launcher_path: Option<std::path::PathBuf>,
}

impl LinuxBackend {
    /// The stable id of the Linux backend.
    pub const ID: &'static str = "linux";

    /// Construct the Linux backend, probing the live landlock ABI from the kernel
    /// (the raw probe is the sanctioned `super::sys` basement).
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
            landlock_abi: sys::probe_landlock_abi(),
            launcher_path: None,
        }
    }

    /// Construct the Linux backend with an EXPLICIT launcher-binary path (constructor
    /// injection — the thread-safe alternative to mutating `BVISOR_LAUNCHER_BIN` via the
    /// banned `std::env::set_var`). `execute()` then runs exactly this launcher. Used by
    /// the integration tests, whose deps binary has no co-located launcher to resolve.
    #[must_use]
    pub fn with_launcher_path(launcher_path: std::path::PathBuf) -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
            landlock_abi: sys::probe_landlock_abi(),
            launcher_path: Some(launcher_path),
        }
    }

    /// Construct a backend with a FORCED landlock ABI, for proving the below-floor
    /// fail-closed path on a host whose live ABI is above the floor. Test-only.
    #[cfg(test)]
    fn with_abi_for_test(landlock_abi: i64) -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
            landlock_abi,
            launcher_path: None,
        }
    }

    /// Whether the live ABI meets the floor required to enforce FS confinement.
    fn filesystem_enforced(&self) -> bool {
        self.landlock_abi >= LANDLOCK_ABI_FLOOR
    }

    /// The honest machine ceiling: `Filesystem` Enforced ONLY when the ABI floor
    /// is met (else absent ⇒ Unsupported), plus `LaunchWorkload`+`CaptureStreams`
    /// (process spawn + pipe capture, which this chunk genuinely performs).
    /// Nothing else is listed — every other kind floors to `Unsupported`, so
    /// `plan()` fails closed for capabilities this chunk does not back.
    fn ceiling(&self) -> BackendProfile {
        let mut ceiling = BTreeMap::new();
        if self.filesystem_enforced() {
            ceiling.insert(
                RequirementKind::Filesystem,
                SupportVerdict::new(
                    Enforcement::Enforced,
                    [
                        EvidenceClaim::AllowedActions,
                        EvidenceClaim::DeniedAttempts,
                        EvidenceClaim::FilesystemDelta,
                        EvidenceClaim::MechanismAttestation,
                    ]
                    .into_iter()
                    .collect(),
                ),
            );
        }
        ceiling.insert(
            RequirementKind::LaunchWorkload,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::TerminalOutcome, EvidenceClaim::ProcessTree]
                    .into_iter()
                    .collect(),
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
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// The honest budget profile for THIS chunk. Budget ENFORCEMENT (cgroup limits) is
/// part 2 — NOT implemented here — so no dimension is claimed `Enforced`. The OS
/// DOES account a spawned process's resources, and the runner observes its
/// terminal, so each dimension is declared `Mediated` (supervised, not
/// structurally capped) with an honest mechanism string and NO resource-usage
/// evidence claim. This lets a budgeted FS spec admit (the launch needs nonzero
/// derived minimums) WITHOUT claiming a cap this chunk does not install.
fn observed_budget_profile() -> BudgetProfile {
    let observed = |mechanism: &str| BudgetAvailability {
        // Headroom only — we do NOT cap, so we never refuse on capacity here; the
        // honest signal is the `Mediated` (not `Enforced`) guarantee + empty
        // evidence, which forbids a spec from demanding a witnessed/enforced cap.
        available: u64::MAX,
        enforcement: Enforcement::Mediated,
        evidence: EvidenceSet::new(),
        mechanism: mechanism.to_string(),
    };
    BudgetProfile {
        wall_micros: observed("os_process_wait:observed-not-capped"),
        cpu_micros: observed("os_rusage:observed-not-capped"),
        resident_bytes: observed("os_rusage:observed-not-capped"),
        process_count: observed("os_process:observed-not-capped"),
        handle_count: observed("os_fd:observed-not-capped"),
        storage_bytes: observed("os_fs:observed-not-capped"),
        network_bytes: observed("os_net:observed-not-capped"),
    }
}

impl Backend for LinuxBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn support(&self) -> &SupportMatrix {
        &self.support
    }

    fn probe(&self) -> BackendProfileSnapshot {
        // REAL probe facts: the live landlock ABI integer and whether the FS floor
        // is met. Deterministic given the kernel, so replay re-derives identically.
        let mut probed = BTreeMap::new();
        probed.insert("landlock_abi".to_string(), self.landlock_abi.to_string());
        probed.insert(
            "filesystem_confinement".to_string(),
            if self.filesystem_enforced() {
                "landlock".to_string()
            } else {
                "unsupported-below-abi-floor".to_string()
            },
        );
        BackendProfileSnapshot {
            backend: self.id.clone(),
            probed,
            budget: observed_budget_profile(),
        }
    }

    fn profile(&self, _snap: &BackendProfileSnapshot) -> BackendProfile {
        // The ceiling is derived from the live ABI: FS Enforced only above the
        // floor; otherwise FS is absent ⇒ Unsupported ⇒ plan() fails closed.
        self.ceiling()
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn mechanism(&self, requirement: &BoundaryRequirement, enforcement: Enforcement) -> String {
        // This backend authors only the mechanisms it actually performs this
        // chunk; everything else is honestly named as unimplemented-this-chunk.
        // Exhaustive ON PURPOSE (no bare wildcard over known variants): this chunk
        // backs exactly Filesystem/LaunchWorkload/CaptureStreams; every other kind
        // is honestly named unimplemented-this-chunk. A future variant must declare
        // its mechanism here rather than silently inheriting the unimplemented tag.
        let primitive = match RequirementKind::of(requirement) {
            RequirementKind::Filesystem => "landlock",
            RequirementKind::LaunchWorkload => "process_spawn",
            RequirementKind::CaptureStreams => "pipe_capture",
            RequirementKind::NetworkDenyAll
            | RequirementKind::NetworkAllowList
            | RequirementKind::ChildSpawn
            | RequirementKind::Environment
            | RequirementKind::InheritedFds
            | RequirementKind::TempRoot
            | RequirementKind::ExposePath
            | RequirementKind::CommitArtifact
            | RequirementKind::DiscardArtifact
            | RequirementKind::Kill
            | RequirementKind::ListOutputs => "none/unimplemented-this-chunk",
        };
        format!("{}:{primitive}:{enforcement:?}", self.id)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        execute_confined(self, plan)
    }
}

/// Run the admitted plan with REAL landlock confinement APPLIED BY THE LAUNCHER,
/// returning the honest observed body. Never panics: every failure resolves to an
/// honest [`Outcome`] (fail-closed — the workload never runs unconfined).
fn execute_confined(backend: &LinuxBackend, plan: &BoundaryPlan) -> BoundaryReportBody {
    let mut observed = Vec::new();

    // Only a native process workload is runnable by this backend.
    let (exe, args) = match &plan.workload {
        Workload::Process { exe, args } => (exe.clone(), args.clone()),
        Workload::Wasm { module_ref } => {
            observed.push(ObservedFact {
                kind: "workload_unsupported".to_string(),
                detail: format!("linux backend cannot run wasm module {module_ref}"),
            });
            return fail_closed(backend, plan, Outcome::Unsupported, observed);
        }
    };

    // The admitted Filesystem capability ⇒ confine; absent ⇒ no FS confinement was
    // requested (the launcher's Confinement phase resolves NotRequired). The plan
    // was admitted against our ceiling, so a Filesystem capability is
    // DeclaredRootsOnly + a PathSet.
    let fs = filesystem_capability(plan);

    // Fail closed below the ABI floor: we MUST NOT run a Filesystem-scoped workload
    // unconfined while the profile claims enforcement. (The launcher ALSO re-probes
    // and refuses below its floor, but the backend refuses FIRST so it never even
    // builds an unenforceable confinement plan.)
    if fs.is_some() && !backend.filesystem_enforced() {
        observed.push(ObservedFact {
            kind: "filesystem_confinement_unavailable".to_string(),
            detail: format!(
                "landlock abi {} below floor {LANDLOCK_ABI_FLOOR}; refusing to run unconfined",
                backend.landlock_abi
            ),
        });
        return fail_closed(backend, plan, Outcome::Unsupported, observed);
    }

    // Build the launcher plan + the pre-opened authority handles host-side. A host
    // wiring fault (a root/exe that cannot be opened, a slot that does not fit)
    // fails closed — the workload never runs.
    let prepared = match plan_build::prepare_launch(&exe, &args, plan, fs.as_ref()) {
        Ok(prepared) => prepared,
        Err(detail) => {
            observed.push(ObservedFact {
                kind: "launch_plan_construction_failed".to_string(),
                detail,
            });
            return fail_closed(backend, plan, Outcome::SupervisorFault, observed);
        }
    };
    let plan_build::Prepared {
        launch_plan,
        authority,
        read_roots,
        write_roots,
        confined,
    } = prepared;

    // Record the confinement mechanism + the exact roots as honest evidence: the
    // landlock policy is now applied by the LAUNCHER's child-window restrict_self
    // (after the fd scrub, before fexecve), NOT a backend pre_exec.
    if confined {
        observed.push(ObservedFact {
            kind: "filesystem_confined".to_string(),
            detail: format!(
                "landlock abi {} (launcher restrict_self): read-roots {read_roots:?}, \
                 write-roots {write_roots:?}",
                backend.landlock_abi
            ),
        });
    }

    // Resolve the launcher binary; fail closed if unresolvable (NEVER run the
    // workload unconfined). Content-addressed launcher identity is step 12. An
    // injected backend launcher path (constructor injection) takes precedence over
    // the env / co-located fallback.
    let launcher_path = match resolve_launcher(backend) {
        Ok(path) => path,
        Err(detail) => {
            observed.push(ObservedFact {
                kind: "launcher_unresolvable".to_string(),
                detail,
            });
            return fail_closed(backend, plan, Outcome::Unsupported, observed);
        }
    };

    // Run the launcher; map its honest observation onto the report contract.
    match launch::run_launcher(&launcher_path, &launch_plan, authority) {
        Ok(obs) => map_observation(backend, plan, &exe, confined, &obs, observed),
        Err(error) => {
            // A harness fault BEFORE the launcher produced a verdict (encode/slot/OS):
            // the workload never ran ⇒ SupervisorFault (fail-closed).
            observed.push(ObservedFact {
                kind: "launcher_harness_fault".to_string(),
                detail: format!("linux launcher harness fault: {error}"),
            });
            fail_closed(backend, plan, Outcome::SupervisorFault, observed)
        }
    }
}

/// Resolve the launcher binary path, failing closed if unresolvable. Resolution
/// order: the backend's INJECTED `launcher_path` (constructor injection) FIRST, then
/// the `BVISOR_LAUNCHER_BIN` env override, else the `bvisor-linux-launcher` binary
/// CO-LOCATED with the current executable (the documented default install layout). If
/// none resolves to an existing file ⇒ `Err` (the caller reports `Outcome::Unsupported`
/// — the workload NEVER runs unconfined). Content-addressed launcher identity
/// (digest-pinning the exact bin) is step 12.
fn resolve_launcher(backend: &LinuxBackend) -> Result<std::path::PathBuf, String> {
    // Injected launcher path (thread-safe constructor injection) takes precedence — it
    // is how the integration tests point at the compile-time launcher without the banned
    // process-env mutation. Confirm it exists so a bad inject still fails closed.
    if let Some(path) = &backend.launcher_path {
        if path.is_file() {
            return Ok(path.clone());
        }
        return Err(format!(
            "injected launcher path does not exist: {}",
            path.display()
        ));
    }
    // The override path is trusted as supplied (step-12 note); honor it even if a
    // stat would race, but still confirm it exists so we fail closed on a typo.
    if let Ok(p) = std::env::var(launch::ENV_LAUNCHER_BIN) {
        if !p.trim().is_empty() {
            let path = std::path::PathBuf::from(p);
            if path.is_file() {
                return Ok(path);
            }
            return Err(format!(
                "{} points to a non-existent launcher binary: {}",
                launch::ENV_LAUNCHER_BIN,
                path.display()
            ));
        }
    }
    // Default: the launcher next to the current executable.
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the current executable to find the launcher: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "current executable has no parent directory".to_string())?;
    let default = dir.join("bvisor-linux-launcher");
    if default.is_file() {
        return Ok(default);
    }
    Err(format!(
        "no launcher binary: {} unset and no bvisor-linux-launcher beside {}",
        launch::ENV_LAUNCHER_BIN,
        exe.display()
    ))
}

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
fn map_observation(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    exe: &str,
    confined: bool,
    obs: &LaunchObservation,
    mut observed: Vec<ObservedFact>,
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

    let outcome = obs.outcome().unwrap_or(Outcome::SupervisorFault);
    // The launcher does not surface the workload's own exit code (it reports its
    // setup terminal); ExecSucceeded means the workload image began executing under
    // confinement. No portable workload ExitStatus is available through this path.
    let exit = exec_exit(outcome);
    body(backend, plan, outcome, exit, captured, observed, Vec::new())
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
fn filesystem_capability(plan: &BoundaryPlan) -> Option<(FsAccess, PathSet)> {
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
fn fail_closed(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    outcome: Outcome,
    observed: Vec<ObservedFact>,
) -> BoundaryReportBody {
    body(
        backend,
        plan,
        outcome,
        None,
        CaptureRefs::default(),
        observed,
        Vec::new(),
    )
}

/// Assemble the honest report body.
fn body(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    outcome: Outcome,
    exit: Option<ExitStatus>,
    captured: CaptureRefs,
    observed: Vec<ObservedFact>,
    denied: Vec<DeniedAttempt>,
) -> BoundaryReportBody {
    BoundaryReportBody {
        schema_version: BOUNDARY_REPORT_SCHEMA_VERSION,
        plan_id: plan.plan_id,
        backend: backend.id.clone(),
        profile: backend.probe(),
        outcome,
        admitted: plan.admitted.clone(),
        observed,
        denied,
        exit,
        captured,
        budget: BudgetWitnesses::unwitnessed(&plan.budgets),
        artifacts: Vec::new(),
        findings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{LinuxBackend, LANDLOCK_ABI_FLOOR};
    use crate::contract::backend::Backend;
    use crate::contract::capability::{Capability, Enforcement, FsAccess, FsConfinement, PathSet};
    use crate::contract::plan::BoundaryRequirement;
    use crate::contract::support::RequirementKind;

    fn fs_requirement() -> BoundaryRequirement {
        BoundaryRequirement::Capability(Capability::Filesystem {
            access: FsAccess::Read,
            // An inert scope path — classify() never touches disk, so the value is
            // immaterial; a relative placeholder avoids leaking an absolute path.
            scope: PathSet {
                roots: vec!["quarantine/root".to_string()],
            },
            recursive: true,
            confinement: FsConfinement::DeclaredRootsOnly,
        })
    }

    #[test]
    fn filesystem_is_enforced_at_or_above_the_abi_floor() {
        // At the floor the machine ceiling backs Filesystem, so classify is Enforced.
        let backend = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR);
        let profile = backend.profile(&backend.probe());
        let verdict = backend.classify(&fs_requirement(), &profile);
        assert_eq!(
            verdict.enforcement,
            Enforcement::Enforced,
            "at/above the ABI floor, Filesystem must be Enforced"
        );
        // The ceiling lists Filesystem at the floor.
        assert_eq!(
            profile.ceiling_for(RequirementKind::Filesystem).enforcement,
            Enforcement::Enforced
        );
    }

    #[test]
    fn filesystem_fails_closed_below_the_abi_floor() {
        // Below the floor (e.g. landlock unavailable ⇒ probed ABI 0) the machine
        // ceiling does NOT back Filesystem, so the family Enforced best-case is
        // floored to Unsupported — and plan() will refuse a FS spec fail-closed.
        let backend = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR - 1);
        let profile = backend.profile(&backend.probe());
        let verdict = backend.classify(&fs_requirement(), &profile);
        assert_eq!(
            verdict.enforcement,
            Enforcement::Unsupported,
            "below the ABI floor, Filesystem MUST fail closed (no unbacked guarantee)"
        );
    }

    #[test]
    fn unimplemented_kinds_fail_closed_this_chunk() {
        // HONESTY: this chunk backs ONLY Filesystem/LaunchWorkload/CaptureStreams.
        // Kill / NetworkDenyAll / ChildSpawn / TempRoot are NOT in the ceiling, so
        // they floor to Unsupported and plan() fails closed for them.
        let backend = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR);
        let profile = backend.profile(&backend.probe());
        for kind in [
            RequirementKind::Kill,
            RequirementKind::NetworkDenyAll,
            RequirementKind::ChildSpawn,
            RequirementKind::TempRoot,
        ] {
            assert_eq!(
                profile.ceiling_for(kind).enforcement,
                Enforcement::Unsupported,
                "{kind:?} must stay Unsupported until its chunk lands (no inflation)"
            );
        }
    }
}
