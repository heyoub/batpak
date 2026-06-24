//! The SAFE coordinator implementation (Linux). All decision logic, sequencing, and
//! the control-fd transcript live here; every `unsafe` syscall is delegated to the
//! [`crate::sys`] basement. See `main.rs` for the topology + honesty contract.

use crate::sys::{self, ChildExecPlan, ObservedShape};
use bvisor::linux::protocol::{
    confinement_installed, phase_resolution_consistent, ready_to_exec, validate_table,
    DescriptorKind, DescriptorSlotV1, LauncherState, LinuxLaunchBodyV1, LinuxLaunchPlanV1,
    LoweringWireEntryV1, PhaseResult, RefusalReason, SetupPhase,
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

/// Wire `phase_code` for the scrub action's phase, frozen by
/// `contract::primitive::LoweringPhase::FdHygiene.code()` (== 3): "Sanitize inherited
/// file descriptors (CLOEXEC sweep, handle list)". The skeleton maps this code to
/// [`SetupPhase::AmbientAuthority`].
const PHASE_CODE_SCRUB: u8 = 3;

/// Wire `phase_code` for the exec action's phase, frozen by
/// `contract::primitive::LoweringPhase::Launch.code()` (== 5).
const PHASE_CODE_EXEC: u8 = 5;

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
/// exit code. A boot fault BEFORE the control channel exists is reported to stderr
/// (via `Write`, not the denied print macros) and exits non-zero.
pub(crate) fn run() -> std::process::ExitCode {
    // The control channel must exist before ANY transcript can be emitted.
    let control_fd = match fd_from_env(ENV_CONTROL_FD) {
        Ok(fd) => fd,
        Err(_) => return boot_fault(&BootError::NoControlChannel),
    };
    let mut control = Transcript::new(sys::adopt_fd(control_fd));

    match drive(&mut control) {
        Ok(Verdict::ExecSucceeded) => std::process::ExitCode::SUCCESS,
        Ok(Verdict::Refused(reason)) => {
            // The reason was already noted on the wire by `refuse`; record it on the
            // boot sink too so a refusal is diagnosable even without the control fd.
            let mut sink = std::io::stderr();
            let _ = writeln!(sink, "bvisor-linux-launcher: SetupRefused {reason:?}");
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

    // 4. PlanVerified — table structure + the schedule bucketing (skeleton serves
    //    ONLY scrub@AmbientAuthority + exec; anything else ⇒ MissingPrimitive).
    if validate_table(&body.descriptor_table).is_err() {
        return Ok(refuse(control, RefusalReason::PlanInvalid));
    }
    let scrub_entries = match classify_schedule(body) {
        Ok(scrub) => scrub,
        Err(reason) => return Ok(refuse(control, reason)),
    };
    control.emit(LauncherState::PlanVerified);

    // 5. HandlesVerified — fstat each declared slot against its shape, and confirm
    //    no undeclared fds beyond the known launcher fds.
    let plan_fd = fd_from_env(ENV_PLAN_FD)?;
    let known = KnownFds {
        plan: plan_fd,
        control_present: true,
        error: error_fd,
        error_read: error_read_fd,
        declared: &body.descriptor_table,
    };
    if verify_handles(body, &known)?.is_err() {
        return Ok(refuse(control, RefusalReason::HandleMismatch));
    }
    control.emit(LauncherState::HandlesVerified);

    // 6. Compute the four phase results for the exec-only plan and hold the
    //    ReadyToExec gate BEFORE any child is created.
    let phases = compute_phases(&scrub_entries);
    // Phase-honesty self-check (anti over/under-claim) before we trust the results.
    if !phases_are_honest(&scrub_entries, &phases) {
        return Ok(Verdict::Faulted);
    }
    let phase_results = [
        (SetupPhase::Identity, phases.identity),
        (SetupPhase::Visibility, phases.visibility),
        (SetupPhase::AmbientAuthority, phases.ambient),
        (SetupPhase::Confinement, phases.confinement),
    ];
    // The skeleton advertises NO confinement: this MUST be false.
    debug_assert!(!confinement_installed(0, phases.confinement));
    if !ready_to_exec(true, phase_results, observed_digest, body.h_l) {
        // The decision is fail-closed: refuse NOW, no child.
        return Ok(refuse(control, RefusalReason::PlanInvalid));
    }

    // 7. Build EVERYTHING the child needs BEFORE clone3 (async-signal-safety).
    let exe_fd = exe_slot_fd(body)?;
    let allow = allowlist(&known, exe_fd);
    let close_fds = scrub_close_list(&allow)?;
    let child_plan = match ChildExecPlan::build(
        exe_fd,
        None,
        error_fd,
        &body.target.argv,
        &body.target.envp,
        close_fds,
    ) {
        Ok(p) => p,
        Err(_) => return Ok(Verdict::Faulted),
    };

    // 8. Re-check single-thread, then clone3.
    let tasks = count_self_tasks()?;
    if tasks != 1 {
        return Err(BootError::NotSingleThreaded { observed: tasks });
    }
    let child_pid = sys::clone3_child(&child_plan)?;
    control.emit(LauncherState::ChildCreated);
    let _ = control.note(&format!("mechanism=clone3 child_pid={child_pid}"));

    // Close the COORDINATOR's own copy of the error-pipe WRITE end so only the child
    // holds a write end — then the read end gets EOF the instant the child's
    // successful execve CLOEXEC-closes its copy. A raw best-effort close (the child
    // shares the fd post-clone3; closing the parent's copy must not abort).
    sys::close_fd(error_fd);

    // 9. Wait: read the error pipe (read end), then reap the child.
    let child_outcome = wait_for_child(error_read_fd, child_pid)?;
    match child_outcome {
        ChildOutcome::ExecedToEof => {
            // The deterministic no-branch child sequence ran to exec: resolve the
            // four phases honestly, then ReadyToExec → ExecSucceeded.
            control.emit(LauncherState::IdentityPhaseResolved);
            control.emit(LauncherState::VisibilityPhaseResolved);
            control.emit(LauncherState::AmbientAuthorityPhaseResolved);
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

/// Classify the wire lowering: confirm an `linux.exec.v1` entry exists, collect the
/// scrub (AmbientAuthority) entries, and refuse `MissingPrimitive` on ANY entry the
/// skeleton does not serve (unknown id, or a known id in the wrong phase, or any
/// scheduled action in a phase the skeleton can't serve).
///
/// Returns the scrub entries (in order) on success.
fn classify_schedule(body: &LinuxLaunchBodyV1) -> Result<Vec<LoweringWireEntryV1>, RefusalReason> {
    let mut scrub: Vec<LoweringWireEntryV1> = Vec::new();
    let mut saw_exec = false;
    for entry in &body.lowering.entries {
        match (entry.id.as_str(), entry.phase_code) {
            (ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB) => scrub.push(entry.clone()),
            (ID_EXEC, PHASE_CODE_EXEC) => saw_exec = true,
            // Any other id, or a serviced id in the wrong phase, or any action in a
            // phase the skeleton does not serve ⇒ a primitive we do not implement.
            _ => return Err(RefusalReason::MissingPrimitive),
        }
    }
    if !saw_exec {
        // No launch action: the skeleton has nothing to exec.
        return Err(RefusalReason::MissingPrimitive);
    }
    Ok(scrub)
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

/// Compute the four phase results: Identity/Visibility/Confinement have no scheduled
/// actions ⇒ `NotRequired`; AmbientAuthority has the scrub action the child WILL run
/// ⇒ `Applied`.
fn compute_phases(scrub_entries: &[LoweringWireEntryV1]) -> Phases {
    let ambient = if scrub_entries.is_empty() {
        // No scrub scheduled — but the skeleton REQUIRES the scrub (the mandatory
        // ambient-authority action), so an empty ambient phase is a refusal upstream.
        // `ready_to_exec` enforces ambient==Applied, so NotRequired here fails closed.
        PhaseResult::NotRequired
    } else {
        PhaseResult::Applied
    };
    Phases {
        identity: PhaseResult::NotRequired,
        visibility: PhaseResult::NotRequired,
        ambient,
        confinement: PhaseResult::NotRequired,
    }
}

/// Verify, via the protocol's pure oracle, that each phase result is consistent with
/// what was scheduled vs. what the launcher will observe — the anti over/under-claim
/// self-check. For the exec-only skeleton: the three empty phases must be
/// `NotRequired` (scheduled==observed==∅), and AmbientAuthority must be `Applied`
/// with observed == scheduled (the scrub entries, run deterministically).
fn phases_are_honest(scrub_entries: &[LoweringWireEntryV1], phases: &Phases) -> bool {
    let empty: [LoweringWireEntryV1; 0] = [];
    phase_resolution_consistent(&empty, &empty, phases.identity)
        && phase_resolution_consistent(&empty, &empty, phases.visibility)
        && phase_resolution_consistent(&empty, &empty, phases.confinement)
        // The child runs the EXACT scrub set deterministically: observed == scheduled.
        && phase_resolution_consistent(scrub_entries, scrub_entries, phases.ambient)
}

// ── Handle verification ────────────────────────────────────────────────────────

/// The launcher's own well-known fds, for the no-fd-escape baseline.
struct KnownFds<'a> {
    plan: RawFd,
    control_present: bool,
    /// The child-facing error-pipe WRITE end.
    error: RawFd,
    /// The coordinator-facing error-pipe READ end.
    error_read: RawFd,
    declared: &'a [DescriptorSlotV1],
}

/// `fstat` each declared slot and check kind + writability against its declaration,
/// then confirm no UNDECLARED fds are open beyond the known launcher fds (plan,
/// control, error, stdio, declared slots) — the no-fd-escape baseline.
fn verify_handles(body: &LinuxLaunchBodyV1, known: &KnownFds) -> Result<Result<(), ()>, BootError> {
    for slot in &body.descriptor_table {
        let observed = sys::fstat_shape(raw(slot.slot_index))?;
        if !shape_matches(slot, &observed) {
            return Ok(Err(()));
        }
    }
    if !no_undeclared_fds(known)? {
        return Ok(Err(()));
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

/// Whether every currently-open fd is accounted for: a known launcher fd (plan,
/// control, error, stdio 0/1/2) or a declared descriptor slot. An extra open fd is a
/// no-fd-escape violation (fail closed).
fn no_undeclared_fds(known: &KnownFds) -> Result<bool, BootError> {
    let mut allowed: BTreeSet<RawFd> = BTreeSet::new();
    allowed.insert(0);
    allowed.insert(1);
    allowed.insert(2);
    allowed.insert(known.plan);
    allowed.insert(known.error);
    allowed.insert(known.error_read);
    if known.control_present {
        if let Ok(fd) = fd_from_env(ENV_CONTROL_FD) {
            allowed.insert(fd);
        }
    }
    for slot in known.declared {
        allowed.insert(raw(slot.slot_index));
    }
    for fd in list_open_fds()? {
        // The `/proc/self/fd` read itself holds a transient dir fd; ignore any fd not
        // in `allowed` ONLY if it is the enumeration's own handle. We cannot know its
        // number portably, so we treat the dirfd specially below in `list_open_fds`.
        if !allowed.contains(&fd) {
            return Ok(false);
        }
    }
    Ok(true)
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

/// Convert a (host-assigned, dense) slot index to a `RawFd`. The host opens the
/// descriptor at exactly this number, so the slot index IS the fd number.
fn raw(slot_index: u32) -> RawFd {
    RawFd::try_from(slot_index).unwrap_or(-1)
}

/// The allowlist of fds the child KEEPS across the scrub: the target exe, stdio
/// (0/1/2), and the error-pipe write end. Everything else is closed by the child.
fn allowlist(known: &KnownFds, exe_fd: RawFd) -> BTreeSet<RawFd> {
    let mut allow: BTreeSet<RawFd> = BTreeSet::new();
    allow.insert(0);
    allow.insert(1);
    allow.insert(2);
    allow.insert(exe_fd);
    allow.insert(known.error);
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
/// line to stderr (via `Write`, not the denied `eprintln!`) and exit non-zero.
fn boot_fault(err: &BootError) -> std::process::ExitCode {
    let mut sink = std::io::stderr();
    let _ = writeln!(sink, "bvisor-linux-launcher: {err}");
    std::process::ExitCode::from(4)
}
