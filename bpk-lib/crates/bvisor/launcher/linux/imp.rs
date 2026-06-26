//! The SAFE coordinator implementation (Linux). All decision logic, sequencing, and
//! the control-fd transcript live here; every `unsafe` syscall is delegated to the
//! [`crate::sys`] basement. See `main.rs` for the topology + honesty contract.

use crate::sys::{self, ChildExecPlan, LandlockRoot, ObservedShape, UsernsSyncPipe};
use bvisor::linux::protocol::{
    confinement_installed, phase_resolution_consistent, ready_to_exec, validate_table,
    DescriptorKind, DescriptorRole, DescriptorSlotV1, LauncherState, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, PhaseResult, RefusalReason, SetupPhase,
};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::RawFd;

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
    // 1. LauncherStarted + single-thread check.
    control.emit(LauncherState::LauncherStarted);
    let tasks = count_self_tasks()?;
    if tasks != 1 {
        return Err(BootError::NotSingleThreaded { observed: tasks });
    }

    // 2. Read + decode the plan from the plan fd.
    let plan_fd = fd_from_env(ENV_PLAN_FD)?;
    let error_fd = fd_from_env(ENV_ERROR_FD)?;
    let error_read_fd = fd_from_env(ENV_ERROR_READ_FD)?;
    let plan_bytes = sys::read_fd_to_vec(plan_fd)?;
    let plan = match LinuxLaunchPlanV1::decode(&plan_bytes) {
        Ok(p) => p,
        Err(_) => {
            // A bad/tampered/bad-magic plan is a fail-closed REFUSAL (PlanInvalid):
            // structurally unusable, but the launcher itself did not fault.
            return Ok(refuse(control, RefusalReason::PlanInvalid));
        }
    };
    let body = &plan.body;

    // 3. IdentityVerified — schedule-digest binding ONLY (see module note).
    let observed_digest = schedule_digest(body);
    if observed_digest != body.h_l {
        return Ok(refuse(control, RefusalReason::IdentityMismatch));
    }
    control.emit(LauncherState::IdentityVerified);

    // 4. PlanVerified — table structure + the schedule bucketing (the launcher serves
    //    scrub@AmbientAuthority + landlock-apply@Confinement + exec; anything else ⇒
    //    MissingPrimitive).
    if validate_table(&body.descriptor_table).is_err() {
        return Ok(refuse(control, RefusalReason::PlanInvalid));
    }
    let schedule = match classify_schedule(body) {
        Ok(schedule) => schedule,
        Err(reason) => return Ok(refuse(control, reason)),
    };
    control.emit(LauncherState::PlanVerified);

    // 5. HandlesVerified — fstat each declared slot against its declared SHAPE (kind +
    //    writability). A shape mismatch (e.g. a dir fd where a `Regular` is declared) ⇒
    //    `HandleMismatch`. The coordinator does NOT enumerate the open-fd table and
    //    refuse on an undeclared inherited fd: the child-side scrub (step 8) closes
    //    every non-allowlisted fd before `fexecve`, so G6 (no-fd-escape) is enforced by
    //    the scrub, not a coordinator refusal. (The landlock roots are validated HERE,
    //    before the ruleset is built from their inherited fds.)
    let known = KnownFds { error: error_fd };
    if verify_handles(body)?.is_err() {
        return Ok(refuse(control, RefusalReason::HandleMismatch));
    }
    control.emit(LauncherState::HandlesVerified);

    // 6. CONFINEMENT: if a landlock-apply action is scheduled, BUILD the ruleset in the
    //    PARENT now (all allocation + add_rule syscalls, async-signal-safety) from the
    //    just-validated root fds. `None` ⇒ no landlock action (Confinement stays
    //    NotRequired). A build failure fails CLOSED to a refusal — the launcher never
    //    advertises a confinement it cannot install. The ruleset fd(s) are diffed
    //    against the PRE-build fd snapshot so they can be scrub-exempted + CLOEXEC'd.
    let open_before = open_fd_set()?;
    let confinement = if schedule.confine.is_empty() {
        None
    } else {
        match build_confinement(body, &open_before) {
            Ok(built) => Some(built),
            Err(ConfineRefusal::AbiBelowFloor) => {
                return Ok(refuse(control, RefusalReason::MissingPrimitive));
            }
            Err(ConfineRefusal::NoUsableRoot) => {
                return Ok(refuse(control, RefusalReason::HandleMismatch));
            }
        }
    };
    let confine_built = confinement.is_some();

    // 7. Compute the four phase results and hold the ReadyToExec gate BEFORE any child
    //    is created. Confinement is `Applied` IFF a landlock action was scheduled AND
    //    its ruleset was built (the child WILL restrict_self).
    let phases = compute_phases(&schedule, confine_built);
    // Phase-honesty self-check (anti over/under-claim) before we trust the results.
    if !phases_are_honest(&schedule, &phases) {
        return Ok(Verdict::Faulted);
    }
    let phase_results = [
        (SetupPhase::Identity, phases.identity),
        (SetupPhase::Visibility, phases.visibility),
        (SetupPhase::AmbientAuthority, phases.ambient),
        (SetupPhase::Confinement, phases.confinement),
    ];
    // confinement_installed is REAL evidence: true IFF a landlock action was scheduled
    // AND applied. With no landlock action it MUST be false (no over-claim); with one
    // built+applied it MUST be true.
    debug_assert_eq!(
        confinement_installed(schedule.confine.len(), phases.confinement),
        confine_built
    );
    if !ready_to_exec(true, phase_results, observed_digest, body.h_l) {
        // The decision is fail-closed: refuse NOW, no child.
        return Ok(refuse(control, RefusalReason::PlanInvalid));
    }

    // 8. Build EVERYTHING ELSE the child needs BEFORE clone3 (async-signal-safety).
    //    The ruleset fd(s) join the allowlist so the scrub leaves them open for
    //    restrict_self (they CLOEXEC-close on the workload's fexecve — no leak).
    let exe_fd = exe_slot_fd(body)?;
    let ruleset_fds: &[RawFd] = match &confinement {
        Some(built) => &built.ruleset_fds,
        None => &[],
    };
    // OPT-IN user-namespace rendezvous (S8): when the plan requests a userns, create the
    // parent→child sync pipe NOW (single-threaded, pre-clone3). The READ end is packed
    // into the child plan + allowlisted (so the child blocks on it post-clone3, inside
    // its new userns, BEFORE the scrub); the WRITE end stays with the parent to release
    // the child after the uid/gid maps are written. With NO userns request this is `None`
    // and the no-userns path is byte-for-byte unchanged. A pipe-create failure fails
    // CLOSED to a fault (no child is created).
    let userns_requested = body.target.user_namespace.is_some();
    // OPT-IN empty network namespace = NetworkDenyAll (S9 / D3): when the plan requests a
    // netns, the launcher births the child in a NEW, EMPTY netns (CLONE_NEWNET). Unprivileged
    // CLONE_NEWNET REQUIRES the child to be root-in-userns, so a netns request WITHOUT a
    // userns request is a malformed plan — fail CLOSED (no child is created) rather than
    // attempt an unprivileged CLONE_NEWNET that would EPERM.
    let netns_requested = body.target.network_namespace.is_some();
    if netns_requested && !userns_requested {
        let _ = control.note("network_namespace=requested_without_userns fail_closed");
        return Ok(Verdict::Faulted);
    }
    let sync_pipe = if userns_requested {
        match sys::make_sync_pipe() {
            Ok(pipe) => Some(pipe),
            Err(_) => return Ok(Verdict::Faulted),
        }
    } else {
        None
    };
    let child_sync = sync_pipe.map(|(read, write)| UsernsSyncPipe { read, write });
    // The sync READ end is allowlisted (the child reads it before the scrub); the sync
    // WRITE end is NOT (the parent owns it). The child closes its inherited write-end copy
    // explicitly in its window BEFORE the read (so the fail-closed EOF is honest); the
    // write end therefore also lands in the scrub close-list as a redundant safety.
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
        Ok(p) => p,
        Err(_) => return Ok(Verdict::Faulted),
    };

    // 9. Re-check single-thread, then clone3 (carrying the parent-built ruleset, which
    //    the child applies via restrict_self after scrub, before fexecve).
    let tasks = count_self_tasks()?;
    if tasks != 1 {
        return Err(BootError::NotSingleThreaded { observed: tasks });
    }
    // Resolve the optional cgroup leaf fd: when present, clone3 births the child
    // INSIDE the prepared leaf (CLONE_INTO_CGROUP), so the workload is resource-confined
    // the instant it exists — no post-fork migration race. The fd was fstat-validated as
    // a directory in step 5; the kernel consumes it during the syscall.
    let cgroup_fd = cgroup_slot_fd(body);
    let child_pid = sys::clone3_child(
        &child_plan,
        confinement.map(|c| c.ruleset),
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

    // USER-NAMESPACE RENDEZVOUS (S8): when a userns was requested, the child is now born
    // in a NEW userns and BLOCKED on the sync pipe (unmapped/overflow uid). The PARENT
    // (heap fine — NOT the child window) writes the uid/gid maps in the LOAD-BEARING
    // order (uid_map, setgroups=deny, gid_map), then RELEASES the child. If ANY map-write
    // fails we FAIL CLOSED: the child is NOT released (its sync read gets EOF when the
    // write end drops → it `_exit`s), it is reaped, and the target never runs.
    if let Some((_read, write)) = sync_pipe {
        match user_namespace_rendezvous(child_pid, write) {
            Ok(()) => {
                let _ = control.note("user_namespace=mapped child_uid0_egid0");
                if netns_requested {
                    // The child was born into a NEW, EMPTY netns at clone3 time (CLONE_NEWNET):
                    // only `lo` (no address, no routes => unreachable), no external interface —
                    // NetworkDenyAll is structural.
                    // The HOST independently confirms this by reading the child's
                    // /proc/<pid>/net/dev (the §4 oracle); this note is the honest attestation.
                    let _ = control.note("network_namespace=empty_netns child_isolated");
                }
            }
            Err(()) => {
                // The write end was already closed by the rendezvous on failure, so the
                // child's blocking read saw EOF and `_exit`s; reap it and fault. The
                // target NEVER ran.
                sys::reap_child(child_pid);
                let _ = control.note("user_namespace=map_write_failed fail_closed");
                control.emit(LauncherState::SetupFaulted);
                return Ok(Verdict::Faulted);
            }
        }
    }

    // Close the COORDINATOR's own copy of the error-pipe WRITE end so only the child
    // holds a write end — then the read end gets EOF the instant the child's
    // successful execve CLOEXEC-closes its copy. A raw best-effort close (the child
    // shares the fd post-clone3; closing the parent's copy must not abort).
    sys::close_fd(error_fd);

    // 9. Wait: read the error pipe (read end), then reap the child.
    let child_outcome = wait_for_child(error_read_fd, child_pid)?;
    match child_outcome {
        ChildOutcome::ExecedToEof => {
            // The deterministic no-branch child sequence ran to exec (scrub → maybe
            // restrict_self → fexecve): resolve the four phases honestly, then
            // ReadyToExec → ExecSucceeded. The error pipe gave EOF, so restrict_self
            // (if scheduled) did NOT fail-close — it applied before the workload ran.
            control.emit(LauncherState::IdentityPhaseResolved);
            control.emit(LauncherState::VisibilityPhaseResolved);
            control.emit(LauncherState::AmbientAuthorityPhaseResolved);
            // Note the HONEST confinement result: Applied (real landlock install) only
            // when a landlock action was scheduled + its ruleset built+applied.
            let installed = confinement_installed(schedule.confine.len(), phases.confinement);
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
            // Scrub or fexecve failed: the child never ran the target.
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
}

/// Classify the wire lowering: confirm an `linux.exec.v1` entry exists, collect the
/// scrub (AmbientAuthority) and landlock-apply (Confinement) entries, and refuse
/// `MissingPrimitive` on ANY entry the launcher does not serve (unknown id, or a
/// known id in the wrong phase, or any scheduled action in a phase it can't serve).
fn classify_schedule(body: &LinuxLaunchBodyV1) -> Result<ClassifiedSchedule, RefusalReason> {
    let mut scrub: Vec<LoweringWireEntryV1> = Vec::new();
    let mut confine: Vec<LoweringWireEntryV1> = Vec::new();
    let mut saw_exec = false;
    for entry in &body.lowering.entries {
        match (entry.id.as_str(), entry.phase_code) {
            (ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB) => scrub.push(entry.clone()),
            (ID_LANDLOCK_APPLY, PHASE_CODE_CONFINE) => confine.push(entry.clone()),
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
    Ok(ClassifiedSchedule { scrub, confine })
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

/// Compute the four phase results: Identity/Visibility have no scheduled actions ⇒
/// `NotRequired`; AmbientAuthority has the scrub action the child WILL run ⇒ `Applied`;
/// Confinement is `Applied` IFF a landlock-apply action is scheduled AND its ruleset
/// was built (the child WILL `restrict_self`), else `NotRequired` (no landlock action).
///
/// `confine_built` is the coordinator's evidence that the ruleset was actually
/// constructed in the parent — Confinement is reported `Applied` ONLY when the action
/// was scheduled and the launcher really built (and will apply) the ruleset, so the
/// phase result can never over-claim an install that did not happen.
fn compute_phases(schedule: &ClassifiedSchedule, confine_built: bool) -> Phases {
    let ambient = if schedule.scrub.is_empty() {
        // No scrub scheduled — but the launcher REQUIRES the scrub (the mandatory
        // ambient-authority action), so an empty ambient phase is a refusal upstream.
        // `ready_to_exec` enforces ambient==Applied, so NotRequired here fails closed.
        PhaseResult::NotRequired
    } else {
        PhaseResult::Applied
    };
    let confinement = if schedule.confine.is_empty() {
        // No landlock action scheduled ⇒ Confinement stays NotRequired (unchanged).
        PhaseResult::NotRequired
    } else if confine_built {
        // A landlock action IS scheduled and the ruleset was built in the parent — the
        // child will restrict_self before exec ⇒ Applied (REAL confinement evidence).
        PhaseResult::Applied
    } else {
        // Scheduled but un-buildable (ABI below floor / fd not a root): fail closed.
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
    phase_resolution_consistent(&empty, &empty, phases.identity)
        && phase_resolution_consistent(&empty, &empty, phases.visibility)
        && phase_resolution_consistent(&schedule.scrub, &schedule.scrub, phases.ambient)
        && phase_resolution_consistent(&schedule.confine, &schedule.confine, phases.confinement)
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

// ── Handle verification ────────────────────────────────────────────────────────

/// The launcher's own well-known fds the scrub allowlist is built from.
struct KnownFds {
    /// The child-facing error-pipe WRITE end (kept across the scrub so the child can
    /// report a scrub/fexecve failure; CLOEXEC-closed by a successful `fexecve`).
    error: RawFd,
}

/// `fstat` each declared slot and check its kind + writability against its declaration
/// — the deterministic declared-slot SHAPE verification (a dir fd where a `Regular` is
/// declared ⇒ `HandleMismatch`). This is the ONLY handle check the coordinator runs.
///
/// It does NOT enumerate the open-fd table and REFUSE on an undeclared inherited fd.
/// The no-fd-escape guarantee (G6) is enforced child-side by the scrub — the child
/// closes EVERY non-allowlisted fd (raw `SYS_close`) before `fexecve`, so an unexpected
/// fd the launcher inherited from its forking host is defensively CLOSED in the child,
/// never seen by the workload, and never a launch-abort. Refusing here would be both
/// redundant with that scrub and timing-flaky (a sibling thread's transient
/// non-CLOEXEC fd, captured at fork, is not an escape — it is scrubbed).
fn verify_handles(body: &LinuxLaunchBodyV1) -> Result<Result<(), ()>, BootError> {
    for slot in &body.descriptor_table {
        let observed = sys::fstat_shape(raw(slot.slot_index))?;
        if !shape_matches(slot, &observed) {
            return Ok(Err(()));
        }
    }
    Ok(Ok(()))
}

/// Whether an observed `fstat` shape matches a declared slot (kind + writability).
/// `DescriptorKind` is `#[non_exhaustive]`, so an unknown FUTURE kind the skeleton
/// does not model fails the match (fail closed — never silently accept).
fn shape_matches(slot: &DescriptorSlotV1, observed: &ObservedShape) -> bool {
    // (expected file-type bits in the platform `mode_t`, whether writability is
    // meaningful for this kind). Compared directly against the observed `mode_t`.
    let expected = match slot.expected.kind {
        DescriptorKind::Directory => Some((libc::S_IFDIR, true)),
        DescriptorKind::Regular => Some((libc::S_IFREG, true)),
        DescriptorKind::Socket => Some((libc::S_IFSOCK, false)),
        DescriptorKind::Pipe => Some((libc::S_IFIFO, false)),
        // An unknown future kind the skeleton does not model ⇒ fail closed.
        _ => None,
    };
    let Some((file_type, writability_meaningful)) = expected else {
        return false;
    };
    if observed.file_type != file_type {
        return false;
    }
    // Writability is meaningful only for directories/regular files (the protocol
    // says the launcher ignores it for sockets/pipes).
    if writability_meaningful {
        observed.writable == slot.expected.writable
    } else {
        true
    }
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

/// Run the unprivileged user-namespace RENDEZVOUS in the PARENT (heap is fine here — this
/// is NOT the async-signal-safe child window). The child (`child_pid`) was born in a new
/// userns and is BLOCKED on the sync pipe; `sync_write_fd` is the parent's write end.
///
/// Writes, in the LOAD-BEARING order the unprivileged-userns recipe requires:
///   1. `/proc/<pid>/uid_map`   = `0 <euid> 1`  (child uid 0 → the launcher's euid),
///   2. `/proc/<pid>/setgroups` = `deny`        (MANDATORY and MUST precede gid_map for
///      an unprivileged writer — the kernel rejects gid_map otherwise),
///   3. `/proc/<pid>/gid_map`   = `0 <egid> 1`  (child gid 0 → the launcher's egid),
///
/// then RELEASES the child by writing 1 byte to the sync pipe. The `/proc` writes are
/// ordinary `std::fs::write` (SAFE — NOT unsafe).
///
/// FAIL-CLOSED: on ANY map-write failure the child is NOT released — the write end is
/// closed (dropping it gives the child's blocking read EOF, so it `_exit`s) and `Err(())`
/// is returned so the caller reaps the child and faults. The target never runs unmapped.
fn user_namespace_rendezvous(child_pid: libc::pid_t, sync_write_fd: RawFd) -> Result<(), ()> {
    let (euid, egid) = sys::effective_ids();
    let uid_map = format!("0 {euid} 1\n");
    let gid_map = format!("0 {egid} 1\n");
    let base = format!("/proc/{child_pid}");
    // The leaf each map write targets. A `dangerous-test-hooks` build may redirect the
    // FIRST write (uid_map) to a non-existent `/proc/<pid>/` attribute so the write FAILS
    // deterministically on ANY host — exercising the fail-closed reap-and-fault branch
    // even where unprivileged userns IS supported (so the teeth do not merely SKIP). The
    // hook is compiled out of every non-test build, so production NEVER reads the env.
    let uid_map_leaf = forced_map_fail_leaf().unwrap_or("uid_map");
    let write_step = |leaf: &str, contents: &str| -> Result<(), ()> {
        std::fs::write(format!("{base}/{leaf}"), contents).map_err(|_| ())
    };
    // Order is load-bearing: uid_map, THEN setgroups=deny, THEN gid_map.
    let mapped = write_step(uid_map_leaf, &uid_map)
        .and_then(|()| write_step("setgroups", "deny"))
        .and_then(|()| write_step("gid_map", &gid_map));
    if mapped.is_err() {
        // FAIL CLOSED: do NOT release. Close the write end so the child's blocking read
        // sees EOF and exits; the caller reaps it.
        sys::close_fd(sync_write_fd);
        return Err(());
    }
    // RELEASE: one byte unblocks the child's `read()`. A failed write is treated as a
    // fail-closed rendezvous failure (the child would block forever otherwise).
    let mut writer = sys::adopt_fd(sync_write_fd);
    if writer.write_all(&[1u8]).is_err() {
        return Err(());
    }
    // `writer` drops here, closing the parent's write end (the child has already been
    // released by the byte above; the EOF that the drop produces is harmless).
    Ok(())
}

/// `dangerous-test-hooks` fault injection: when `BVISOR_TEST_FORCE_USERNS_MAP_FAIL=1`,
/// return a `/proc/<pid>/` leaf that does NOT exist, so the FIRST userns map write fails
/// deterministically and the coordinator's fail-closed reap-and-fault branch runs. In
/// every non-test build this fn is compiled to a constant `None` — production never reads
/// the environment, so the injection cannot affect a real launch.
#[cfg(feature = "dangerous-test-hooks")]
fn forced_map_fail_leaf() -> Option<&'static str> {
    match std::env::var("BVISOR_TEST_FORCE_USERNS_MAP_FAIL") {
        Ok(v) if v.trim() == "1" => Some("bvisor_nonexistent_map_attr"),
        _ => None,
    }
}

/// Production stub: the userns map-fail injection hook is ABSENT without the
/// `dangerous-test-hooks` feature, so the launcher always writes the real `uid_map` leaf.
#[cfg(not(feature = "dangerous-test-hooks"))]
fn forced_map_fail_leaf() -> Option<&'static str> {
    None
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

// ── Child wait ─────────────────────────────────────────────────────────────────

/// What the child reported.
enum ChildOutcome {
    /// The error pipe gave EOF (write end CLOEXEC-closed by a successful execve) ⇒
    /// the child's deterministic scrub→fexecve ran to exec.
    ExecedToEof,
    /// The error pipe yielded errno bytes ⇒ scrub or fexecve failed; the target
    /// never ran.
    Errno(i32),
}

/// Read the error pipe to EOF or an errno, then reap the child. EOF ⇒ exec success;
/// any bytes ⇒ the reported errno.
fn wait_for_child(error_read_fd: RawFd, child_pid: libc::pid_t) -> Result<ChildOutcome, BootError> {
    let mut pipe = sys::adopt_fd(error_read_fd);
    let mut buf = Vec::new();
    pipe.read_to_end(&mut buf)?;
    // Reap the child (best-effort: the outcome is decided by the pipe, not the code).
    reap(child_pid);
    if buf.len() >= 4 {
        let errno = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
        Ok(ChildOutcome::Errno(errno))
    } else if buf.is_empty() {
        Ok(ChildOutcome::ExecedToEof)
    } else {
        // A short, non-empty write is still a failure signal (the child only writes
        // on the failure path); treat any bytes as a fault.
        Ok(ChildOutcome::Errno(-1))
    }
}

/// Reap a child pid through the SAFE std waiting surface is unavailable (we did not
/// create the child via `Command`), so we delegate the single `waitid` to the
/// basement. Best-effort: a failed reap does not change the pipe-derived outcome.
fn reap(child_pid: libc::pid_t) {
    sys::reap_child(child_pid);
}

// ── Transcript (control-fd writer) ─────────────────────────────────────────────

/// The control-fd transcript: newline-delimited launcher-state names + free-form
/// notes. Writes bytes through an owned `File` (never the denied print macros). A
/// failed write is swallowed — the transcript is best-effort observability, and the
/// EXIT CODE is the authoritative result.
struct Transcript {
    sink: File,
}

impl Transcript {
    fn new(sink: File) -> Self {
        Self { sink }
    }

    /// Emit one launcher state as a line (e.g. `LauncherStarted`).
    fn emit(&mut self, state: LauncherState) {
        let _ = writeln!(self.sink, "{state:?}");
        let _ = self.sink.flush();
    }

    /// Emit a free-form note line, prefixed `# ` so a reader can separate notes from
    /// state lines.
    fn note(&mut self, text: &str) -> std::io::Result<()> {
        writeln!(self.sink, "# {text}")?;
        self.sink.flush()
    }
}

/// Report a boot fault that happened BEFORE a control channel existed: write a typed
/// line to stderr (via `Write`, not the denied `eprintln!`) and exit non-zero. This is
/// the SOLE path on which the launcher writes to its own stderr — and only because no
/// control fd exists yet to carry the diagnostic AND no workload runs in this case, so
/// the write cannot contaminate the host's workload-stream capture (see the run()
/// stdio-silence contract).
fn boot_fault(err: &BootError) -> std::process::ExitCode {
    let mut sink = std::io::stderr();
    let _ = writeln!(sink, "bvisor-linux-launcher: {err}");
    std::process::ExitCode::from(4)
}
