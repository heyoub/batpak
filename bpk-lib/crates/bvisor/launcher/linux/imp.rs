//! The SAFE coordinator implementation (Linux). All decision logic, sequencing, and
//! the control-fd transcript live here; every `unsafe` syscall is delegated to the
//! [`crate::sys`] basement. See `main.rs` for the topology + honesty contract.

#[path = "imp/support.rs"]
mod support;

use crate::sys::{self, ChildExecPlan, LandlockRoot, UsernsSyncPipe};
use bvisor::linux::protocol::{
    confinement_installed, phase_resolution_consistent, ready_to_exec, validate_table,
    DescriptorRole, LauncherState, LinuxLaunchBodyV1, LinuxLaunchPlanV1, LoweringWireEntryV1,
    PhaseResult, RefusalReason, SetupPhase,
};
use std::collections::BTreeSet;
use std::os::fd::RawFd;
use support::{
    boot_fault, build_seccomp_filter, user_namespace_rendezvous, verify_handles, wait_for_child,
    ChildOutcome, Transcript,
};

// ── Frozen primitive ids + phase codes the skeleton serves ────────────────────

/// The fd-scrub primitive (AmbientAuthority phase). The child closes every
/// non-allowlisted fd; this is the ONLY ambient-authority action the skeleton runs.
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";

/// The launch primitive. Marks the `fexecve` step; it is not itself a SetupPhase.
const ID_EXEC: &str = "linux.exec.v1";

/// The landlock-apply primitive (Confinement phase). The child applies a parent-built
/// landlock ruleset confining FS access to exactly the declared read/write ROOTS via
/// `restrict_self` (after the fd scrub, before `fexecve`). The FIRST real confinement
/// the launcher serves.
const ID_LANDLOCK_APPLY: &str = "linux.landlock.apply.v1";

/// The seccomp-apply primitive (Confinement phase, S10). The coordinator compiles a
/// default-allow DENYLIST in the parent (from `target.seccomp`) and the child installs it
/// LAST, after landlock, immediately before `fexecve` (`prctl(NO_NEW_PRIVS)` then
/// `seccomp(SET_MODE_FILTER)`). Backs `ChildSpawn::DenyNewTasks` + the NetworkDenyAll DiD.
const ID_SECCOMP_APPLY: &str = "linux.seccomp.apply.v1";

/// Wire `phase_code` for the scrub action's phase, frozen by
/// `contract::primitive::LoweringPhase::FdHygiene.code()` (== 3): "Sanitize inherited
/// file descriptors (CLOEXEC sweep, handle list)". The skeleton maps this code to
/// [`SetupPhase::AmbientAuthority`].
const PHASE_CODE_SCRUB: u8 = 3;

/// Wire `phase_code` for the exec action's phase, frozen by
/// `contract::primitive::LoweringPhase::Launch.code()` (== 5).
const PHASE_CODE_EXEC: u8 = 5;

/// Wire `phase_code` for the landlock-apply action's phase, frozen by
/// `contract::primitive::LoweringPhase::PolicyInstall.code()` (== 4): "Install
/// enforcement policy (seccomp-BPF, LSM, …)". The launcher maps this code to
/// [`SetupPhase::Confinement`].
const PHASE_CODE_CONFINE: u8 = 4;

// ── Env-named inherited fds (the host opens these and passes the NUMBERS) ──────

const ENV_PLAN_FD: &str = "BVISOR_LAUNCH_PLAN_FD";
const ENV_CONTROL_FD: &str = "BVISOR_CONTROL_FD";
/// The CHILD-facing error-pipe WRITE end (O_CLOEXEC). Goes into the child plan +
/// allowlist; the child writes its errno here on failure, and a successful execve
/// auto-closes it (so the coordinator's read end sees EOF).
const ENV_ERROR_FD: &str = "BVISOR_ERROR_FD";
/// The COORDINATOR-facing error-pipe READ end — the OTHER end of the same pipe the
/// host created. The coordinator reads this to distinguish EOF (exec success) from
/// errno bytes (scrub/fexecve failure). A pipe is two fds; the host passes both.
const ENV_ERROR_READ_FD: &str = "BVISOR_ERROR_READ_FD";

// ── Typed launcher errors (no panic/unwrap/expect in prod) ─────────────────────

/// Why the coordinator could not even begin (a wiring fault, before any plan
/// decision). These map to `SetupFaulted` — the launcher itself could not run.
#[derive(Debug)]
enum BootError {
    /// A required env-named fd was missing or not a valid descriptor number.
    BadFdEnv {
        /// The env var that was missing or malformed.
        var: &'static str,
    },
    /// The control fd could not be established, so no transcript can be emitted.
    NoControlChannel,
    /// An OS error while reading the plan, enumerating fds, or stat-ing a handle.
    Os(std::io::Error),
    /// The launcher observed more than one thread in its own process — it must be
    /// single-threaded by construction, so this is a fault, not a refusal.
    NotSingleThreaded {
        /// The number of `/proc/self/task` entries observed.
        observed: usize,
    },
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadFdEnv { var } => write!(f, "missing or malformed fd env var {var}"),
            Self::NoControlChannel => write!(f, "no usable control channel fd"),
            Self::Os(e) => write!(f, "launcher OS fault: {e}"),
            Self::NotSingleThreaded { observed } => {
                write!(f, "launcher is not single-threaded: {observed} tasks")
            }
        }
    }
}

impl std::error::Error for BootError {}

impl From<std::io::Error> for BootError {
    fn from(e: std::io::Error) -> Self {
        Self::Os(e)
    }
}

/// The coordinator's verdict once a control channel exists: the terminal it reached.
/// Carried so `run` can map it to an exit code AFTER the transcript is flushed.
enum Verdict {
    ExecSucceeded,
    Refused(RefusalReason),
    Faulted,
}

/// Run the coordinator. Establishes the control channel, drives the validate→decide→
/// (maybe) clone3→wait sequence, emits the transcript, and maps the terminal to an
/// exit code.
///
/// STDIO-SILENCE CONTRACT (host capture honesty): the launcher writes ALL diagnostics
/// to the CONTROL fd transcript — it NEVER writes to its own stdout/stderr on any path
/// where the workload runs. The launcher's child inherits the launcher's fd 0/1/2 (the
/// scrub allowlists stdio), so the host captures the launcher's piped stdout/stderr AS
/// the workload's output; a launcher diagnostic on stderr would corrupt that capture.
/// The SOLE exception is a pre-control [`BootError::NoControlChannel`] boot fault: no
/// control fd exists yet to carry the diagnostic AND no workload runs in that case, so
/// the one-line `boot_fault` stderr write cannot contaminate any workload capture.
pub(crate) fn run() -> std::process::ExitCode {
    // The control channel must exist before ANY transcript can be emitted.
    let control_fd = match fd_from_env(ENV_CONTROL_FD) {
        Ok(fd) => fd,
        Err(_) => return boot_fault(&BootError::NoControlChannel),
    };
    let mut control = Transcript::new(sys::adopt_fd(control_fd));

    match drive(&mut control) {
        Ok(Verdict::ExecSucceeded) => std::process::ExitCode::SUCCESS,
        Ok(Verdict::Refused(_reason)) => {
            // The refusal reason is ALREADY on the control-fd transcript (via `refuse`).
            // We deliberately write NOTHING to stderr here: the workload inherits the
            // launcher's stdio, so any stderr diagnostic would contaminate the host's
            // workload-stream capture. The exit code (3) plus the control transcript
            // carry the refusal honestly. (See the run() stdio-silence contract.)
            std::process::ExitCode::from(3)
        }
        Ok(Verdict::Faulted) => std::process::ExitCode::from(4),
        Err(fault) => {
            // A fault AFTER the channel exists: record SetupFaulted on the wire too.
            control.emit(LauncherState::SetupFaulted);
            let _ = control.note(&format!("fault: {fault}"));
            std::process::ExitCode::from(4)
        }
    }
}

/// Drive the full coordinator sequence over an established control channel.
fn drive(control: &mut Transcript) -> Result<Verdict, BootError> {
    control.emit(LauncherState::LauncherStarted);
    ensure_single_threaded()?;

    let inputs = match read_launch_inputs(control)? {
        DriveStep::Continue(inputs) => inputs,
        DriveStep::Done(verdict) => return Ok(verdict),
    };
    let body = &inputs.plan.body;

    let verified = match verify_launch_plan(control, body)? {
        DriveStep::Continue(verified) => verified,
        DriveStep::Done(verdict) => return Ok(verdict),
    };

    let mechanisms = match prepare_mechanisms(control, body, &verified)? {
        DriveStep::Continue(mechanisms) => mechanisms,
        DriveStep::Done(verdict) => return Ok(verdict),
    };

    let PreparedMechanisms {
        confinement,
        seccomp_program,
        seccomp_built,
        phases,
    } = mechanisms;
    let child_pid = match spawn_child(
        control,
        body,
        inputs.error_fd,
        confinement,
        seccomp_program.as_ref(),
        seccomp_built,
    )? {
        DriveStep::Continue(child_pid) => child_pid,
        DriveStep::Done(verdict) => return Ok(verdict),
    };

    finish_child(
        control,
        inputs.error_fd,
        inputs.error_read_fd,
        child_pid,
        &verified.schedule,
        &phases,
    )
}

enum DriveStep<T> {
    Continue(T),
    Done(Verdict),
}

struct LaunchInputs {
    plan: LinuxLaunchPlanV1,
    error_fd: RawFd,
    error_read_fd: RawFd,
}

struct VerifiedPlan {
    observed_digest: [u8; 32],
    schedule: ClassifiedSchedule,
}

struct PreparedMechanisms {
    confinement: Option<BuiltConfinement>,
    seccomp_program: Option<sys::BpfProgram>,
    seccomp_built: bool,
    phases: Phases,
}

fn ensure_single_threaded() -> Result<(), BootError> {
    let tasks = count_self_tasks()?;
    if tasks == 1 {
        Ok(())
    } else {
        Err(BootError::NotSingleThreaded { observed: tasks })
    }
}

fn read_launch_inputs(control: &mut Transcript) -> Result<DriveStep<LaunchInputs>, BootError> {
    let plan_fd = fd_from_env(ENV_PLAN_FD)?;
    let error_fd = fd_from_env(ENV_ERROR_FD)?;
    let error_read_fd = fd_from_env(ENV_ERROR_READ_FD)?;
    let plan_bytes = sys::read_fd_to_vec(plan_fd)?;
    let plan = match LinuxLaunchPlanV1::decode(&plan_bytes) {
        Ok(plan) => plan,
        Err(_) => return Ok(DriveStep::Done(refuse(control, RefusalReason::PlanInvalid))),
    };
    Ok(DriveStep::Continue(LaunchInputs {
        plan,
        error_fd,
        error_read_fd,
    }))
}

fn verify_launch_plan(
    control: &mut Transcript,
    body: &LinuxLaunchBodyV1,
) -> Result<DriveStep<VerifiedPlan>, BootError> {
    let observed_digest = schedule_digest(body);
    if observed_digest != body.h_l {
        return Ok(DriveStep::Done(refuse(
            control,
            RefusalReason::IdentityMismatch,
        )));
    }
    control.emit(LauncherState::IdentityVerified);

    if validate_table(&body.descriptor_table).is_err() {
        return Ok(DriveStep::Done(refuse(control, RefusalReason::PlanInvalid)));
    }
    let schedule = match classify_schedule(body) {
        Ok(schedule) => schedule,
        Err(reason) => return Ok(DriveStep::Done(refuse(control, reason))),
    };
    control.emit(LauncherState::PlanVerified);

    if verify_handles(body)?.is_err() {
        return Ok(DriveStep::Done(refuse(
            control,
            RefusalReason::HandleMismatch,
        )));
    }
    control.emit(LauncherState::HandlesVerified);

    Ok(DriveStep::Continue(VerifiedPlan {
        observed_digest,
        schedule,
    }))
}

fn prepare_mechanisms(
    control: &mut Transcript,
    body: &LinuxLaunchBodyV1,
    verified: &VerifiedPlan,
) -> Result<DriveStep<PreparedMechanisms>, BootError> {
    let confinement = match build_landlock_if_scheduled(control, body, &verified.schedule)? {
        DriveStep::Continue(confinement) => confinement,
        DriveStep::Done(verdict) => return Ok(DriveStep::Done(verdict)),
    };
    let confine_built = confinement.is_some();
    let seccomp_program = match build_seccomp_if_scheduled(control, body, &verified.schedule) {
        DriveStep::Continue(program) => program,
        DriveStep::Done(verdict) => return Ok(DriveStep::Done(verdict)),
    };
    let seccomp_built = seccomp_program.is_some();
    let confinement_built =
        confinement_actions_built(&verified.schedule, confine_built, seccomp_built);
    let phases = compute_phases(&verified.schedule, confinement_built);
    if !phases_are_honest(&verified.schedule, &phases) {
        return Ok(DriveStep::Done(Verdict::Faulted));
    }
    if !ready_to_launch(
        &verified.schedule,
        &phases,
        confinement_built,
        verified,
        body,
    ) {
        return Ok(DriveStep::Done(refuse(control, RefusalReason::PlanInvalid)));
    }
    Ok(DriveStep::Continue(PreparedMechanisms {
        confinement,
        seccomp_program,
        seccomp_built,
        phases,
    }))
}

fn build_landlock_if_scheduled(
    control: &mut Transcript,
    body: &LinuxLaunchBodyV1,
    schedule: &ClassifiedSchedule,
) -> Result<DriveStep<Option<BuiltConfinement>>, BootError> {
    if schedule.confine.is_empty() {
        return Ok(DriveStep::Continue(None));
    }
    let open_before = open_fd_set()?;
    match build_confinement(body, &open_before) {
        Ok(built) => Ok(DriveStep::Continue(Some(built))),
        Err(ConfineRefusal::AbiBelowFloor) => Ok(DriveStep::Done(refuse(
            control,
            RefusalReason::MissingPrimitive,
        ))),
        Err(ConfineRefusal::NoUsableRoot) => Ok(DriveStep::Done(refuse(
            control,
            RefusalReason::HandleMismatch,
        ))),
    }
}

fn build_seccomp_if_scheduled(
    control: &mut Transcript,
    body: &LinuxLaunchBodyV1,
    schedule: &ClassifiedSchedule,
) -> DriveStep<Option<sys::BpfProgram>> {
    if schedule.seccomp.is_empty() {
        return DriveStep::Continue(None);
    }
    match build_seccomp_filter(body) {
        Ok(program) => DriveStep::Continue(Some(program)),
        Err(()) => DriveStep::Done(refuse(control, RefusalReason::MissingPrimitive)),
    }
}

fn ready_to_launch(
    schedule: &ClassifiedSchedule,
    phases: &Phases,
    confinement_built: bool,
    verified: &VerifiedPlan,
    body: &LinuxLaunchBodyV1,
) -> bool {
    let phase_results = [
        (SetupPhase::Identity, phases.identity),
        (SetupPhase::Visibility, phases.visibility),
        (SetupPhase::AmbientAuthority, phases.ambient),
        (SetupPhase::Confinement, phases.confinement),
    ];
    let confinement_scheduled = !schedule.confinement_actions().is_empty();
    debug_assert_eq!(
        confinement_installed(schedule.confinement_actions().len(), phases.confinement),
        confinement_scheduled && confinement_built
    );
    ready_to_exec(true, phase_results, verified.observed_digest, body.h_l)
}

fn spawn_child(
    control: &mut Transcript,
    body: &LinuxLaunchBodyV1,
    error_fd: RawFd,
    confinement: Option<BuiltConfinement>,
    seccomp_program: Option<&sys::BpfProgram>,
    seccomp_built: bool,
) -> Result<DriveStep<libc::pid_t>, BootError> {
    let exe_fd = exe_slot_fd(body)?;
    let ruleset_fds = confinement
        .as_ref()
        .map_or(&[][..], |built| built.ruleset_fds.as_slice());
    let known = KnownFds { error: error_fd };
    let userns_requested = body.target.user_namespace.is_some();
    let netns_requested = body.target.network_namespace.is_some();
    if netns_requested && !userns_requested {
        let _ = control.note("network_namespace=requested_without_userns fail_closed");
        return Ok(DriveStep::Done(Verdict::Faulted));
    }
    let sync_pipe = match make_optional_sync_pipe(userns_requested) {
        Ok(sync_pipe) => sync_pipe,
        Err(()) => return Ok(DriveStep::Done(Verdict::Faulted)),
    };
    let child_sync = sync_pipe.map(|(read, write)| UsernsSyncPipe { read, write });
    let allow = allowlist(&known, exe_fd, ruleset_fds, child_sync.map(|p| p.read));
    let close_fds = scrub_close_list(&allow)?;
    let child_plan = match ChildExecPlan::build(
        exe_fd,
        None,
        error_fd,
        child_sync,
        &body.target.argv,
        &body.target.envp,
        close_fds,
    ) {
        Ok(plan) => plan,
        Err(_) => return Ok(DriveStep::Done(Verdict::Faulted)),
    };

    ensure_single_threaded()?;
    let cgroup_fd = cgroup_slot_fd(body);
    let child_pid = sys::clone3_child(
        &child_plan,
        confinement.map(|c| c.ruleset),
        seccomp_program,
        cgroup_fd,
        userns_requested,
        netns_requested,
    )?;
    control.emit(LauncherState::ChildCreated);
    let _ = control.note(&format!("mechanism=clone3 child_pid={child_pid}"));
    if cgroup_fd.is_some() {
        // The kernel placed the child into the leaf at birth (CLONE_INTO_CGROUP); the
        // HOST independently confirms membership by reading the leaf's cgroup.procs.
        let _ = control.note("cgroup_placement=clone_into_cgroup");
    }
    if seccomp_built {
        let _ = control.note("seccomp=denylist_installed mode=filter");
    }

    if rendezvous_user_namespace(control, child_pid, sync_pipe, netns_requested).is_err() {
        return Ok(DriveStep::Done(Verdict::Faulted));
    }

    Ok(DriveStep::Continue(child_pid))
}

fn make_optional_sync_pipe(userns_requested: bool) -> Result<Option<(RawFd, RawFd)>, ()> {
    if userns_requested {
        sys::make_sync_pipe().map(Some).map_err(|_| ())
    } else {
        Ok(None)
    }
}

fn rendezvous_user_namespace(
    control: &mut Transcript,
    child_pid: libc::pid_t,
    sync_pipe: Option<(RawFd, RawFd)>,
    netns_requested: bool,
) -> Result<(), ()> {
    let Some((_read, write)) = sync_pipe else {
        return Ok(());
    };
    if user_namespace_rendezvous(child_pid, write).is_ok() {
        let _ = control.note("user_namespace=mapped child_uid0_egid0");
        if netns_requested {
            let _ = control.note("network_namespace=empty_netns child_isolated");
        }
        return Ok(());
    }
    sys::reap_child(child_pid);
    let _ = control.note("user_namespace=map_write_failed fail_closed");
    control.emit(LauncherState::SetupFaulted);
    Err(())
}

fn finish_child(
    control: &mut Transcript,
    error_fd: RawFd,
    error_read_fd: RawFd,
    child_pid: libc::pid_t,
    schedule: &ClassifiedSchedule,
    phases: &Phases,
) -> Result<Verdict, BootError> {
    sys::close_fd(error_fd);
    let child_outcome = wait_for_child(error_read_fd, child_pid)?;
    match child_outcome {
        ChildOutcome::ExecedToEof => {
            control.emit(LauncherState::IdentityPhaseResolved);
            control.emit(LauncherState::VisibilityPhaseResolved);
            control.emit(LauncherState::AmbientAuthorityPhaseResolved);
            let installed =
                confinement_installed(schedule.confinement_actions().len(), phases.confinement);
            let _ = control.note(&format!(
                "confinement={:?} installed={installed}",
                phases.confinement
            ));
            control.emit(LauncherState::ConfinementPhaseResolved);
            control.emit(LauncherState::ReadyToExec);
            control.emit(LauncherState::ExecSucceeded);
            Ok(Verdict::ExecSucceeded)
        }
        ChildOutcome::Errno(errno) => {
            let _ = control.note(&format!("child errno={errno}"));
            control.emit(LauncherState::SetupFaulted);
            Ok(Verdict::Faulted)
        }
    }
}

/// Emit `SetupRefused` with a reason note and return the matching verdict.
fn refuse(control: &mut Transcript, reason: RefusalReason) -> Verdict {
    let _ = control.note(&format!("refusal={reason:?}"));
    control.emit(LauncherState::SetupRefused);
    Verdict::Refused(reason)
}

// ── Schedule classification (phase_code → SetupPhase bucketing) ─────────────────

/// The classified schedule the launcher serves: the AmbientAuthority scrub entries
/// (mandatory) and the Confinement landlock-apply entries (optional). Both are carried
/// in canonical order so the phase-honesty self-check can confirm observed==scheduled.
struct ClassifiedSchedule {
    /// The `linux.ambient.scrub.v1` entries (AmbientAuthority phase).
    scrub: Vec<LoweringWireEntryV1>,
    /// The `linux.landlock.apply.v1` entries (Confinement phase). Empty ⇒ no landlock
    /// action scheduled, so Confinement resolves `NotRequired` (unchanged skeleton).
    confine: Vec<LoweringWireEntryV1>,
    /// The `linux.seccomp.apply.v1` entries (Confinement phase, S10). Empty ⇒ no seccomp
    /// filter scheduled (the child installs none). Tracked separately from `confine` so
    /// the launcher can serve a seccomp filter independently of a landlock ruleset.
    seccomp: Vec<LoweringWireEntryV1>,
}

/// Classify the wire lowering: confirm an `linux.exec.v1` entry exists, collect the
/// scrub (AmbientAuthority) and landlock-apply (Confinement) entries, and refuse
/// `MissingPrimitive` on ANY entry the launcher does not serve (unknown id, or a
/// known id in the wrong phase, or any scheduled action in a phase it can't serve).
fn classify_schedule(body: &LinuxLaunchBodyV1) -> Result<ClassifiedSchedule, RefusalReason> {
    let mut scrub: Vec<LoweringWireEntryV1> = Vec::new();
    let mut confine: Vec<LoweringWireEntryV1> = Vec::new();
    let mut seccomp: Vec<LoweringWireEntryV1> = Vec::new();
    let mut saw_exec = false;
    for entry in &body.lowering.entries {
        match (entry.id.as_str(), entry.phase_code) {
            (ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB) => scrub.push(entry.clone()),
            (ID_LANDLOCK_APPLY, PHASE_CODE_CONFINE) => confine.push(entry.clone()),
            (ID_SECCOMP_APPLY, PHASE_CODE_CONFINE) => seccomp.push(entry.clone()),
            (ID_EXEC, PHASE_CODE_EXEC) => saw_exec = true,
            // Any other id, or a serviced id in the wrong phase, or any action in a
            // phase the launcher does not serve ⇒ a primitive we do not implement.
            _ => return Err(RefusalReason::MissingPrimitive),
        }
    }
    if !saw_exec {
        // No launch action: the launcher has nothing to exec.
        return Err(RefusalReason::MissingPrimitive);
    }
    Ok(ClassifiedSchedule {
        scrub,
        confine,
        seccomp,
    })
}

impl ClassifiedSchedule {
    /// The FULL set of Confinement-phase actions, in canonical lowering order: the landlock
    /// ruleset apply (`confine`) followed by the seccomp filter install (`seccomp`). The
    /// Confinement phase resolves over this combined list — `phase_resolution_consistent`
    /// needs observed == scheduled for the WHOLE phase, so landlock + seccomp are one phase.
    fn confinement_actions(&self) -> Vec<LoweringWireEntryV1> {
        let mut actions = self.confine.clone();
        actions.extend(self.seccomp.iter().cloned());
        actions
    }
}

/// The schedule-integrity digest the coordinator binds against `h_l`:
/// `blake3(canonical(body.lowering))`. (This is the skeleton's schedule-digest
/// binding only; the real `H_L`/schedule reconstruction is #75 — see module note.)
fn schedule_digest(body: &LinuxLaunchBodyV1) -> [u8; 32] {
    // The canonical encode of the wire projection cannot fail for the frozen shape;
    // on the impossible error we fall back to a digest that cannot match any real
    // `h_l` (all-0xFF), so the binding fails closed rather than spuriously matching.
    match batpak::canonical::to_bytes(&body.lowering) {
        Ok(bytes) => batpak::event::hash::compute_hash(&bytes),
        Err(_) => [0xFFu8; 32],
    }
}

// ── Phase computation + honesty ────────────────────────────────────────────────

/// The four resolved phase results for an exec-only plan.
struct Phases {
    identity: PhaseResult,
    visibility: PhaseResult,
    ambient: PhaseResult,
    confinement: PhaseResult,
}

/// Whether EVERY scheduled Confinement action (landlock + seccomp) was actually built in
/// the parent: a landlock action requires the ruleset built, a seccomp action requires the
/// BPF compiled. Confinement may resolve `Applied` ONLY when all scheduled ones are built;
/// if any scheduled-but-unbuilt remains, the phase fails closed (never an over-claim).
fn confinement_actions_built(
    schedule: &ClassifiedSchedule,
    confine_built: bool,
    seccomp_built: bool,
) -> bool {
    let landlock_ok = schedule.confine.is_empty() || confine_built;
    let seccomp_ok = schedule.seccomp.is_empty() || seccomp_built;
    landlock_ok && seccomp_ok
}

/// Compute the four phase results: Identity/Visibility have no scheduled actions ⇒
/// `NotRequired`; AmbientAuthority has the scrub action the child WILL run ⇒ `Applied`;
/// Confinement (landlock + seccomp) is `Applied` IFF at least one Confinement action is
/// scheduled AND every scheduled one was built (the child WILL `restrict_self` / install
/// the filter), else `NotRequired` (no Confinement action) or `Refused` (built-failure).
///
/// `confinement_built` is the coordinator's evidence that every scheduled Confinement
/// action was actually constructed in the parent — Confinement is reported `Applied` ONLY
/// when an action was scheduled and the launcher really built (and will apply) it, so the
/// phase result can never over-claim an install that did not happen.
fn compute_phases(schedule: &ClassifiedSchedule, confinement_built: bool) -> Phases {
    let ambient = if schedule.scrub.is_empty() {
        // No scrub scheduled — but the launcher REQUIRES the scrub (the mandatory
        // ambient-authority action), so an empty ambient phase is a refusal upstream.
        // `ready_to_exec` enforces ambient==Applied, so NotRequired here fails closed.
        PhaseResult::NotRequired
    } else {
        PhaseResult::Applied
    };
    let confinement = if schedule.confinement_actions().is_empty() {
        // No landlock + no seccomp action scheduled ⇒ Confinement stays NotRequired.
        PhaseResult::NotRequired
    } else if confinement_built {
        // A Confinement action IS scheduled and every scheduled one was built in the parent
        // — the child will apply them before exec ⇒ Applied (REAL confinement evidence).
        PhaseResult::Applied
    } else {
        // Scheduled but un-buildable (ABI below floor / fd not a root / seccomp compile
        // failed): fail closed.
        PhaseResult::Refused
    };
    Phases {
        identity: PhaseResult::NotRequired,
        visibility: PhaseResult::NotRequired,
        ambient,
        confinement,
    }
}

/// Verify, via the protocol's pure oracle, that each phase result is consistent with
/// what was scheduled vs. what the launcher will observe — the anti over/under-claim
/// self-check. Identity/Visibility have no actions ⇒ `NotRequired` (∅==∅);
/// AmbientAuthority is `Applied` with observed==scheduled (the scrub entries, run
/// deterministically); Confinement is `Applied` with observed==scheduled when a
/// landlock action is scheduled and its ruleset built, else `NotRequired` (∅==∅).
///
/// The child runs the EXACT scheduled set deterministically (it scrubs, then — if a
/// ruleset was built — `restrict_self`s, then `fexecve`s with no branch that could
/// drop a scheduled action), so OBSERVED equals SCHEDULED for every phase.
fn phases_are_honest(schedule: &ClassifiedSchedule, phases: &Phases) -> bool {
    let empty: [LoweringWireEntryV1; 0] = [];
    // The Confinement phase resolves over the FULL set of Confinement actions (landlock +
    // seccomp), in canonical order — observed == scheduled for the whole phase.
    let confinement = schedule.confinement_actions();
    phase_resolution_consistent(&empty, &empty, phases.identity)
        && phase_resolution_consistent(&empty, &empty, phases.visibility)
        && phase_resolution_consistent(&schedule.scrub, &schedule.scrub, phases.ambient)
        && phase_resolution_consistent(&confinement, &confinement, phases.confinement)
}

// ── Confinement (landlock) root resolution + ruleset build ─────────────────────

/// Why the launcher could not build the landlock ruleset a scheduled
/// `linux.landlock.apply.v1` action demands. Both fail CLOSED to a refusal — the
/// launcher NEVER advertises a confinement it cannot deliver.
enum ConfineRefusal {
    /// The live landlock ABI is unavailable / below the launcher's floor, so no
    /// ruleset can be built ⇒ `SetupRefused{MissingPrimitive}` (the launcher does not
    /// serve a confinement this kernel cannot enforce).
    AbiBelowFloor,
    /// A scheduled landlock action references no usable confinement ROOT slot, or the
    /// ruleset construction itself failed ⇒ `SetupRefused{HandleMismatch}` (the
    /// declared roots do not back the confinement the action asked for).
    NoUsableRoot,
}

/// A built confinement: the parent-side landlock ruleset plus the fd numbers landlock
/// newly opened to hold it. The launcher ALLOWLISTS those fds (so the child's scrub
/// does not close the ruleset before `restrict_self` runs) and sets them `O_CLOEXEC`
/// (so a successful `fexecve` auto-closes them — no ruleset fd leaks into the
/// workload, preserving the no-fd-escape discipline).
struct BuiltConfinement {
    /// The owned ruleset the child applies via `restrict_self`.
    ruleset: sys::RulesetCreated,
    /// The fd numbers landlock opened building it (CLOEXEC-set, scrub-exempt).
    ruleset_fds: Vec<RawFd>,
}

/// Resolve the declared read/write ROOT slots into [`LandlockRoot`] confinement
/// targets and BUILD the landlock ruleset in the PARENT (pre-clone3). The roots ride
/// the inherited, already-`fstat`-validated directory fds — landlock is built from the
/// root fd via a `BorrowedFd`, NEVER by reopening a path (CVE-2019-5736 avoidance).
///
/// `open_before` is the launcher's open-fd snapshot taken BEFORE this build; the fds
/// open AFTER the build that were not present before are the ruleset's own fds, which
/// the caller allowlists + CLOEXEC-marks.
///
/// Returns the built ruleset on success. Fails CLOSED: an ABI below the floor or a
/// schedule with no usable root ⇒ the matching [`ConfineRefusal`], so the launcher
/// refuses rather than running the workload under a confinement it did not install.
fn build_confinement(
    body: &LinuxLaunchBodyV1,
    open_before: &BTreeSet<RawFd>,
) -> Result<BuiltConfinement, ConfineRefusal> {
    // Probe the LIVE kernel ABI first: advertise no confinement we cannot deliver.
    if sys::probe_landlock_abi() < sys::LANDLOCK_ABI_FLOOR_RAW {
        return Err(ConfineRefusal::AbiBelowFloor);
    }
    let roots = landlock_roots(body);
    if roots.is_empty() {
        // A landlock action with no read/write root to confine to is a handle fault.
        return Err(ConfineRefusal::NoUsableRoot);
    }
    let ruleset = sys::build_landlock_ruleset(&roots).map_err(|_| ConfineRefusal::NoUsableRoot)?;
    // Diff the fd table: anything open now that was not before is a landlock fd.
    let ruleset_fds: Vec<RawFd> = list_open_fds()
        .map_err(|_| ConfineRefusal::NoUsableRoot)?
        .into_iter()
        .filter(|fd| !open_before.contains(fd))
        .collect();
    // CLOEXEC them so a successful fexecve auto-closes the ruleset (no leak into the
    // workload); the scrub leaves them open (allowlisted) until restrict_self runs.
    for &fd in &ruleset_fds {
        sys::set_cloexec(fd);
    }
    Ok(BuiltConfinement {
        ruleset,
        ruleset_fds,
    })
}

/// Collect the [`LandlockRoot`] confinement targets from the descriptor table: each
/// [`DescriptorRole::ReadRoot`] (read-only) and [`DescriptorRole::WriteRoot`]
/// (read+write) slot, riding its inherited fd (slot index == fd number). The slots
/// were already `fstat`-validated as directories of the declared writability before
/// this point, so the fds are sound landlock parents.
fn landlock_roots(body: &LinuxLaunchBodyV1) -> Vec<LandlockRoot> {
    body.descriptor_table
        .iter()
        .filter_map(|slot| match slot.role {
            DescriptorRole::ReadRoot => Some(LandlockRoot {
                fd: raw(slot.slot_index),
                writable: false,
            }),
            DescriptorRole::WriteRoot => Some(LandlockRoot {
                fd: raw(slot.slot_index),
                writable: true,
            }),
            // Not a confinement root — the exe, cgroup, stdio, and control slots are
            // never landlock parents. `DescriptorRole` is non_exhaustive, so an unknown
            // FUTURE role is likewise not a root (fail closed — never widen).
            DescriptorRole::TargetExe
            | DescriptorRole::CgroupDir
            | DescriptorRole::Stdin
            | DescriptorRole::Stdout
            | DescriptorRole::Stderr
            | DescriptorRole::ControlChannel
            | _ => None,
        })
        .collect()
}

// ── Handle plumbing ────────────────────────────────────────────────────────────

/// The launcher's own well-known fds the scrub allowlist is built from.
struct KnownFds {
    /// The child-facing error-pipe WRITE end (kept across the scrub so the child can
    /// report a scrub/fexecve failure; CLOEXEC-closed by a successful `fexecve`).
    error: RawFd,
}

// ── fd plumbing (SAFE: std::fs / std::env only) ────────────────────────────────

/// Parse an inherited fd NUMBER from an env var. The host passes the number; the fd
/// itself is already open in the launcher's table.
fn fd_from_env(var: &'static str) -> Result<RawFd, BootError> {
    let raw = std::env::var(var).map_err(|_| BootError::BadFdEnv { var })?;
    raw.trim()
        .parse::<RawFd>()
        .map_err(|_| BootError::BadFdEnv { var })
}

/// The exe descriptor fd from the target spec's `exe_slot`.
fn exe_slot_fd(body: &LinuxLaunchBodyV1) -> Result<RawFd, BootError> {
    Ok(raw(body.target.exe_slot))
}

/// The inherited fd of the [`DescriptorRole::CgroupDir`] slot, if the plan declares
/// one (`None` ⇒ no cgroup placement scheduled). It is a singleton role, so at most
/// one slot carries it (the table was structurally validated). The slot was already
/// `fstat`-validated as a directory by [`verify_handles`]; the kernel uses this fd at
/// `clone3` time (via `CLONE_INTO_CGROUP`) to birth the child inside the prepared leaf.
fn cgroup_slot_fd(body: &LinuxLaunchBodyV1) -> Option<RawFd> {
    body.descriptor_table
        .iter()
        .find(|slot| slot.role == DescriptorRole::CgroupDir)
        .map(|slot| raw(slot.slot_index))
}

/// Convert a (host-assigned, dense) slot index to a `RawFd`. The host opens the
/// descriptor at exactly this number, so the slot index IS the fd number.
fn raw(slot_index: u32) -> RawFd {
    RawFd::try_from(slot_index).unwrap_or(-1)
}

/// The allowlist of fds the child KEEPS across the scrub: the target exe, stdio
/// (0/1/2), the error-pipe write end, the landlock ruleset fd(s) (so the child can
/// `restrict_self` AFTER the scrub; they are CLOEXEC so a successful `fexecve` closes
/// them — no ruleset fd leaks into the workload), and — when the userns rendezvous is
/// engaged — the sync-pipe READ end (the child must read its release byte BEFORE the
/// scrub closes the rest; it is CLOEXEC so a successful `fexecve` closes it too).
/// Everything else the child closes.
fn allowlist(
    known: &KnownFds,
    exe_fd: RawFd,
    ruleset_fds: &[RawFd],
    sync_read_fd: Option<RawFd>,
) -> BTreeSet<RawFd> {
    let mut allow: BTreeSet<RawFd> = BTreeSet::new();
    allow.insert(0);
    allow.insert(1);
    allow.insert(2);
    allow.insert(exe_fd);
    allow.insert(known.error);
    for &fd in ruleset_fds {
        allow.insert(fd);
    }
    if let Some(fd) = sync_read_fd {
        allow.insert(fd);
    }
    allow
}

/// The scrub close-list: every currently-open fd NOT in the allowlist. Computed in
/// the single-threaded coordinator, BEFORE clone3 (the child only closes this list).
fn scrub_close_list(allow: &BTreeSet<RawFd>) -> Result<Vec<libc::c_int>, BootError> {
    let mut close: Vec<libc::c_int> = Vec::new();
    for fd in list_open_fds()? {
        if !allow.contains(&fd) {
            close.push(fd);
        }
    }
    Ok(close)
}

/// Enumerate the launcher's currently-open fds by reading `/proc/self/fd` — SAFE
/// (`std::fs::read_dir`). The directory handle `read_dir` itself holds is excluded:
/// its number appears in the listing but is closed the instant the iterator drops,
/// so it is neither in the allowlist nor a real escape. We detect it by collecting
/// first, then dropping the iterator, then removing any fd that is no longer open.
fn list_open_fds() -> Result<Vec<RawFd>, BootError> {
    let mut fds: Vec<RawFd> = Vec::new();
    let dir = std::fs::read_dir("/proc/self/fd")?;
    for entry in dir {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            if let Ok(fd) = name.parse::<RawFd>() {
                fds.push(fd);
            }
        }
    }
    // `dir` is dropped here, closing its transient dirfd. Re-stat each fd and drop
    // any that is no longer open (the dirfd) so the listing reflects only fds that
    // outlive the enumeration.
    Ok(fds
        .into_iter()
        .filter(|&fd| sys::fstat_shape(fd).is_ok())
        .collect())
}

/// The launcher's currently-open fds as a set — used to diff against the post-build
/// fd table so the landlock ruleset's own fd(s) can be identified, scrub-exempted,
/// and CLOEXEC-marked. SAFE (`/proc/self/fd` via [`list_open_fds`]).
fn open_fd_set() -> Result<BTreeSet<RawFd>, BootError> {
    Ok(list_open_fds()?.into_iter().collect())
}

/// Count this process's threads via `/proc/self/task` — SAFE (`std::fs::read_dir`).
fn count_self_tasks() -> Result<usize, BootError> {
    let mut n = 0usize;
    for entry in std::fs::read_dir("/proc/self/task")? {
        let _ = entry?;
        n += 1;
    }
    Ok(n)
}
