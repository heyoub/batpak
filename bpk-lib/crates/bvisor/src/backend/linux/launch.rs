//! The HOST-SIDE launcher harness (kernel plan §10.8, backend→launcher rewire step
//! 7a). A REUSABLE, SAFE orchestration that produces+seals a [`LinuxLaunchPlanV1`]
//! over a memfd, spawns the single-threaded Linux confinement launcher binary with
//! controlled inherited fds, and collects its transcript + terminal outcome into a
//! structured [`LaunchObservation`]. Step 7b wires this into `backend_impl::execute()`;
//! THIS step only BUILDS it and re-proves G1/G3 confinement THROUGH it.
//!
//! ## Safety posture
//! This module is SAFE Rust (the runtime-shape gate fails the build on any `unsafe`
//! outside the `sys.rs` basement). EXACTLY two basement calls do the raw work:
//!   - [`sys::seal_plan_memfd`] — seal the encoded plan into a read-only memfd
//!     (`LEDGER:linux-backend-memfd-seal`);
//!   - [`sys::spawn_launcher_with_fds`] — `Command::spawn` the launcher with a
//!     post-fork `pre_exec` that only `dup2`/`fcntl`s a PRE-BUILT fd map
//!     (`LEDGER:linux-backend-launcher-pre-exec`).
//!
//! The control socketpair + error pipe are created with SAFE std (`UnixStream::pair`,
//! `std::io::pipe`), which return CLOEXEC fds — no `unsafe` here.
//!
//! ## fd directions (from the LAUNCHER's point of view)
//! - PLAN (`BVISOR_LAUNCH_PLAN_FD`): the launcher READS the sealed memfd to EOF.
//! - CONTROL (`BVISOR_CONTROL_FD`): the launcher WRITES its state-machine transcript;
//!   the host keeps the READ end of the socketpair.
//! - ERROR WRITE (`BVISOR_ERROR_FD`): the child WRITES its errno here on failure; a
//!   successful `fexecve` CLOEXEC-closes it (the launcher owns this end + passes it to
//!   the child). It KEEPS `FD_CLOEXEC` so the EOF signal is honest.
//! - ERROR READ (`BVISOR_ERROR_READ_FD`): the launcher READS this to distinguish EOF
//!   (exec success) from errno bytes (a scrub/exec fault). A pipe is two fds; the host
//!   passes BOTH to the launcher.
//! - AUTHORITY (exe / read+write roots / stdio): pre-opened handles placed at their
//!   declared descriptor-table slot fd numbers (slot_index == fd number).
//!
//! ## Launcher binary identity
//! The launcher bin is a `[[bin]]` in this package. The harness locates it via the
//! `BVISOR_LAUNCHER_BIN` env override, else the test/dev compile-time path the caller
//! passes (`env!("CARGO_BIN_EXE_bvisor-linux-launcher")`). The launcher is now
//! content-addressed: `backend_impl::attest_launcher` records the BLAKE3 digest of the
//! bin observed at the resolved path. REMAINING (F2): the digest is computed from the
//! file AT THE PATH and the launcher is then exec'd FROM the path — a TOCTOU window;
//! true fd-pinning (open once, hash THAT fd, `fexecve` the SAME fd) is the follow-on.

use crate::backend::linux::protocol::{LauncherState, LinuxLaunchPlanV1};
use crate::backend::linux::sys::{self, LaunchFd};
use std::io::Read;
use std::os::fd::{OwnedFd, RawFd};
use std::path::PathBuf;

/// The env var that overrides the launcher binary path. When set, the harness spawns
/// exactly this path; otherwise it uses the `default_path` the caller supplies (the
/// compile-time `CARGO_BIN_EXE_bvisor-linux-launcher`). Content-addressed identity is
/// step 12.
pub const ENV_LAUNCHER_BIN: &str = "BVISOR_LAUNCHER_BIN";

/// The env var names the launcher reads to learn its inherited fd NUMBERS. Frozen by the
/// launcher (`launcher/linux/imp.rs`); mirrored here so the harness and the launcher
/// agree without a shared constant module.
const ENV_PLAN_FD: &str = "BVISOR_LAUNCH_PLAN_FD";
const ENV_CONTROL_FD: &str = "BVISOR_CONTROL_FD";
const ENV_ERROR_FD: &str = "BVISOR_ERROR_FD";
const ENV_ERROR_READ_FD: &str = "BVISOR_ERROR_READ_FD";

/// `dangerous-test-hooks` ONLY: the env flag the harness forwards to the launcher to
/// force its userns map-write to fail (exercising the fail-closed reap-and-fault branch
/// on a host that DOES support unprivileged userns). The launcher reads it only under the
/// same feature. Public so the S8 teeth test names exactly this key.
#[cfg(feature = "dangerous-test-hooks")]
pub const ENV_FORCE_USERNS_MAP_FAIL: &str = "BVISOR_TEST_FORCE_USERNS_MAP_FAIL";

/// One pre-opened authority handle the launcher inherits at a fixed fd number. The
/// `slot_index` MUST equal the matching [`crate::backend::linux::protocol::DescriptorSlotV1`]
/// slot index in the plan (the launcher reads the fd at exactly the slot number), and the
/// `handle` is the host-owned descriptor placed there. Read-only roots / the exe / stdio
/// all ride a handle here — authority NEVER rides a re-opened path.
pub struct AuthorityFd {
    /// The fixed fd number the launcher will see (== the plan slot index).
    pub slot_index: RawFd,
    /// The host-owned descriptor to place at `slot_index`.
    pub handle: OwnedFd,
}

/// Why the harness could not even run the launcher (a host-side wiring fault, before any
/// launcher verdict). Distinct from a launcher REFUSAL/FAULT, which is carried in the
/// [`LaunchObservation`] the launcher itself produced.
#[derive(Debug)]
#[non_exhaustive]
pub enum HarnessError {
    /// The plan could not be canonically encoded for the memfd.
    Encode(crate::backend::linux::protocol::EncodeError),
    /// An authority slot index does not fit a `u32`/`RawFd`, or two slots collide.
    BadSlot {
        /// The offending slot index.
        slot_index: RawFd,
    },
    /// An OS error sealing the plan, setting up the channels, spawning, or collecting.
    Os(std::io::Error),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "could not encode the launch plan: {e}"),
            Self::BadSlot { slot_index } => {
                write!(
                    f,
                    "authority slot index {slot_index} is invalid or collides"
                )
            }
            Self::Os(e) => write!(f, "launcher harness OS fault: {e}"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl From<std::io::Error> for HarnessError {
    fn from(e: std::io::Error) -> Self {
        Self::Os(e)
    }
}

/// What the harness collected from one launcher run — the structured observation step 7b
/// maps to an `Outcome` + evidence. The TERMINAL is parsed from the launcher's own
/// transcript (its honest state machine); the harness NEVER grades confinement itself
/// (an independent on-disk oracle does that — see the G1/G3 test).
#[derive(Clone, Debug)]
pub struct LaunchObservation {
    /// The terminal [`LauncherState`] the launcher reached, parsed from the transcript's
    /// last state line. `None` ⇒ the launcher emitted no terminal (it died before
    /// resolving — a harness/launcher fault).
    pub terminal: Option<LauncherState>,
    /// Every state-machine line the launcher emitted, in order (the `# ...` note lines
    /// are dropped — they are free-form mechanism annotations, kept in [`Self::notes`]).
    pub transcript: Vec<LauncherState>,
    /// The free-form mechanism notes the launcher emitted (`# mechanism=clone3 ...`,
    /// `# confinement=Applied installed=true`, `# refusal=...`), in order. These carry
    /// the launcher's own honest mechanism attestation the report can surface.
    pub notes: Vec<String>,
    /// Whether the launcher recorded `installed=true` — its REAL confinement evidence
    /// (true IFF a landlock action was scheduled AND its ruleset built+applied).
    pub confinement_installed: bool,
    /// The launcher process's exit status (its own exit code, NOT the workload's).
    pub launcher_exit: std::process::ExitStatus,
    /// The WORKLOAD's captured stdout bytes. The launcher's clone3 child inherits the
    /// launcher's fd 1 (the scrub allowlists stdio), and the launcher is stdio-silent on
    /// every path where a workload runs, so the launcher process's own piped stdout
    /// carries ONLY the workload's output — the host reads it here. This is the honest
    /// backing for `CaptureStreams=Enforced` the backend→launcher cutover must restore.
    pub captured_stdout: Vec<u8>,
    /// The WORKLOAD's captured stderr bytes (same mechanism as [`Self::captured_stdout`]
    /// over the launcher's inherited fd 2). The launcher writes NO diagnostics to its own
    /// stderr on any workload-running path, so this carries only the workload's stderr.
    pub captured_stderr: Vec<u8>,
}

impl LaunchObservation {
    /// Whether the workload ran confined to success: the terminal is
    /// [`LauncherState::ExecSucceeded`]. Step 7b maps this to the workload-ran outcome.
    #[must_use]
    pub fn exec_succeeded(&self) -> bool {
        self.terminal == Some(LauncherState::ExecSucceeded)
    }

    /// The canonical run-time `Outcome` the terminal maps to (via the protocol's
    /// `outcome_class`), or `None` if no terminal was reached. Provided so 7b's
    /// `execute()` maps HONESTLY (ExecSucceeded→ran-confined; refused/faulted→matching).
    #[must_use]
    pub fn outcome(&self) -> Option<crate::contract::report::Outcome> {
        self.terminal
            .and_then(crate::backend::linux::protocol::outcome_class)
    }
}

/// Locate the launcher binary: the `BVISOR_LAUNCHER_BIN` override if set, else
/// `default_path` (the caller's compile-time `CARGO_BIN_EXE_bvisor-linux-launcher`).
/// Content-addressed identity is step 12 — the path is trusted as supplied here.
#[must_use]
pub fn resolve_launcher_path(default_path: &str) -> PathBuf {
    match std::env::var(ENV_LAUNCHER_BIN) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from(default_path),
    }
}

/// Whether this host permits an UNPRIVILEGED process to create a new user namespace —
/// the S8 userns-rendezvous prerequisite. SAFE host-side probe (reads `/proc/sys` only).
///
/// Returns `false` (⇒ the userns test must SKIP, never silently pass) when EITHER:
///   - `/proc/sys/user/max_user_namespaces` reads `0` (user namespaces are globally
///     disabled, or this namespace's quota is exhausted), OR
///   - the Debian/Ubuntu `kernel.unprivileged_userns_clone` sysctl EXISTS and reads `0`
///     (unprivileged userns creation is explicitly disabled by that knob).
///
/// A MISSING `unprivileged_userns_clone` (most distros) is NOT a denial — the mainline
/// default permits it, so absence ⇒ allowed. This mirrors the landlock ABI-floor SKIP:
/// the test refuses to claim a pass it could not actually exercise.
#[must_use]
pub fn unprivileged_userns_available() -> bool {
    // Global quota: a literal "0" means no userns may be created here.
    if let Ok(text) = std::fs::read_to_string("/proc/sys/user/max_user_namespaces") {
        if text.trim() == "0" {
            return false;
        }
    }
    // Debian/Ubuntu knob: present-and-"0" ⇒ explicitly disabled. Absent ⇒ allowed.
    if let Ok(text) = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone") {
        if text.trim() == "0" {
            return false;
        }
    }
    true
}

/// Run the launcher over `plan` with the pre-opened `authority` handles, collecting the
/// transcript + terminal into a [`LaunchObservation`]. SAFE end-to-end except the two
/// ledgered basement calls (memfd seal + spawn pre_exec).
///
/// The harness:
///   1. seals the encoded plan into a read-only memfd ([`sys::seal_plan_memfd`]);
///   2. creates a control socketpair (host keeps the read end) + an error pipe;
///   3. assigns the launcher-channel fds at numbers ABOVE every authority slot (so they
///      never collide), builds the [`LaunchFd`] table matching what the launcher expects;
///   4. spawns the launcher with the explicit `BVISOR_*_FD` env ([`sys::spawn_launcher_with_fds`]);
///   5. drains the control transcript, then waits the launcher.
///
/// # Errors
/// A [`HarnessError`] for an encode/slot/OS failure BEFORE the launcher produced a
/// verdict. A launcher refusal/fault is NOT an error — it is the [`LaunchObservation`]'s
/// terminal.
pub fn run_launcher(
    launcher_path: &std::path::Path,
    plan: &LinuxLaunchPlanV1,
    authority: Vec<AuthorityFd>,
) -> Result<LaunchObservation, HarnessError> {
    run_launcher_inner(launcher_path, plan, authority, &[])
}

/// `dangerous-test-hooks` ONLY: run the launcher with EXTRA env entries forwarded into
/// its otherwise `env_clear()`ed environment. The S8 fail-closed teeth uses this to set
/// [`ENV_FORCE_USERNS_MAP_FAIL`] for a SINGLE launch WITHOUT mutating the test process's
/// own (process-global, clippy-disallowed) environment — so concurrent tests are
/// unaffected. Production never compiles this.
///
/// # Errors
/// As [`run_launcher`].
#[cfg(feature = "dangerous-test-hooks")]
pub fn run_launcher_with_env(
    launcher_path: &std::path::Path,
    plan: &LinuxLaunchPlanV1,
    authority: Vec<AuthorityFd>,
    extra_env: &[(&str, String)],
) -> Result<LaunchObservation, HarnessError> {
    run_launcher_inner(launcher_path, plan, authority, extra_env)
}

fn run_launcher_inner(
    launcher_path: &std::path::Path,
    plan: &LinuxLaunchPlanV1,
    authority: Vec<AuthorityFd>,
    extra_env: &[(&str, String)],
) -> Result<LaunchObservation, HarnessError> {
    use std::os::unix::net::UnixStream;

    // 1. Seal the encoded plan into a read-only memfd (basement, ledgered).
    let bytes = plan.encode().map_err(HarnessError::Encode)?;
    let plan_fd = sys::seal_plan_memfd(&bytes)?;

    // 2. Control socketpair: the launcher WRITES the transcript on its end; the host
    //    READS the transcript on its end. The error pipe: the child WRITES its errno on
    //    the write end; the launcher READS the read end (it owns BOTH).
    let (control_host, control_launcher) = UnixStream::pair()?;
    let (error_read, error_write) = std::io::pipe()?;

    // 3. Pick launcher-channel fd numbers strictly ABOVE every authority slot, so a
    //    fixed channel fd can never collide with a declared descriptor slot. Authority
    //    slot indices are the fd numbers the launcher reads each handle at.
    let mut max_slot: RawFd = 2; // stdio floor
    for a in &authority {
        if a.slot_index < 0 {
            return Err(HarnessError::BadSlot {
                slot_index: a.slot_index,
            });
        }
        max_slot = max_slot.max(a.slot_index);
    }
    let base = max_slot.checked_add(1).ok_or(HarnessError::BadSlot {
        slot_index: max_slot,
    })?;
    let plan_target = base;
    let control_target = base + 1;
    let error_write_target = base + 2;
    let error_read_target = base + 3;

    // 4. Build the LaunchFd table: each authority handle at its slot fd, plus the four
    //    launcher channels at the channel fds. Only the error-WRITE end keeps CLOEXEC
    //    (so a successful workload fexecve closes it → the launcher's read end sees EOF);
    //    every other inherited fd clears CLOEXEC so the launcher inherits it.
    let mut fds: Vec<LaunchFd> = Vec::with_capacity(authority.len() + 4);
    for a in authority {
        fds.push(LaunchFd {
            src: a.handle,
            target: a.slot_index,
            keep_cloexec: false,
        });
    }
    fds.push(LaunchFd {
        src: plan_fd,
        target: plan_target,
        keep_cloexec: false,
    });
    // The host keeps `control_host`; the launcher gets the OTHER end.
    fds.push(LaunchFd {
        src: OwnedFd::from(control_launcher),
        target: control_target,
        keep_cloexec: false,
    });
    fds.push(LaunchFd {
        src: OwnedFd::from(error_write),
        target: error_write_target,
        keep_cloexec: true,
    });
    fds.push(LaunchFd {
        src: OwnedFd::from(error_read),
        target: error_read_target,
        keep_cloexec: false,
    });

    let mut env: Vec<(&str, String)> = vec![
        (ENV_PLAN_FD, plan_target.to_string()),
        (ENV_CONTROL_FD, control_target.to_string()),
        (ENV_ERROR_FD, error_write_target.to_string()),
        (ENV_ERROR_READ_FD, error_read_target.to_string()),
    ];
    // The launcher spawns with `env_clear()`, so any extra (test-only) env entry must be
    // forwarded EXPLICITLY here. In production `extra_env` is always empty.
    for (name, value) in extra_env {
        env.push((name, value.clone()));
    }

    // 5. Spawn the launcher (basement, ledgered pre_exec). The relocated source fds come
    //    back so the parent can drop them AFTER spawn (the child holds its own copies).
    //    The launcher's own stdout/stderr are piped (set in `spawn_launcher_with_fds`) so
    //    the host captures the WORKLOAD's inherited stdout/stderr here.
    let (mut child, relocated) = sys::spawn_launcher_with_fds(launcher_path, &env, fds)?;
    // Drop the parent's copies of every launcher-side fd so the launcher solely owns its
    // channels (the error read end then reaches EOF when the child's fexecve closes the
    // child write copy). `control_host` is intentionally KEPT (the host reads it).
    drop(relocated);

    // 6. Drain the workload's piped stdout/stderr CONCURRENTLY with the run (on scoped
    //    threads) while THIS thread drains the control transcript, THEN wait the launcher.
    //
    //    Reading the workload streams AFTER `wait` would DEADLOCK a workload that floods a
    //    pipe past its kernel buffer: it blocks writing to the full pipe, never exits, so
    //    `wait` never returns and the post-wait read is never reached. Concurrent draining
    //    keeps all three pipes (control + stdout + stderr) emptying as the workload runs,
    //    so the workload always makes progress regardless of output size. The scoped
    //    threads own the stream handles and their join() yields the captured bytes — no
    //    channel needed. We do NOT cap the capture — every byte is read to EOF.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut transcript_text = String::new();
    let (captured_stdout, captured_stderr) =
        std::thread::scope(|scope| -> Result<(Vec<u8>, Vec<u8>), HarnessError> {
            let out = scope.spawn(move || drain_owned(stdout));
            let err = scope.spawn(move || drain_owned(stderr));
            let mut control = control_host;
            control.read_to_string(&mut transcript_text)?;
            let captured_stdout = out.join().map_err(|_| drain_panicked("stdout"))??;
            let captured_stderr = err.join().map_err(|_| drain_panicked("stderr"))??;
            Ok((captured_stdout, captured_stderr))
        })?;
    let launcher_exit = child.wait()?;

    Ok(parse_observation(
        &transcript_text,
        launcher_exit,
        captured_stdout,
        captured_stderr,
    ))
}

/// Read an owned piped child stream fully to EOF, returning the captured bytes. `None`
/// (the stream handle was not present) yields empty bytes — never a fault. Owns the
/// stream so it can be moved onto a scoped drain thread; generic so the same drain serves
/// both `ChildStdout` and `ChildStderr`.
fn drain_owned<R: Read>(stream: Option<R>) -> Result<Vec<u8>, HarnessError> {
    let mut buf = Vec::new();
    if let Some(mut s) = stream {
        s.read_to_end(&mut buf)?;
    }
    Ok(buf)
}

/// Map a panicked drain thread to an OS fault (a drain thread only panics on a bug, never
/// in normal operation — a pipe read errors via `io::Error`, which propagates as `Os`).
fn drain_panicked(which: &str) -> HarnessError {
    HarnessError::Os(std::io::Error::other(format!(
        "{which} drain thread panicked"
    )))
}

/// Parse the launcher's newline-delimited transcript into a structured observation.
/// State lines are the `LauncherState` `Debug` names the launcher emits; note lines are
/// prefixed `# `. The terminal is the last recognised state line. The captured workload
/// `stdout`/`stderr` (read from the launcher's piped stdio) ride into the observation
/// unchanged — they are the workload's output, NOT part of the control transcript.
fn parse_observation(
    text: &str,
    launcher_exit: std::process::ExitStatus,
    captured_stdout: Vec<u8>,
    captured_stderr: Vec<u8>,
) -> LaunchObservation {
    let mut transcript: Vec<LauncherState> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut installed = false;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(note) = line.strip_prefix("# ") {
            if note.contains("installed=true") {
                installed = true;
            }
            notes.push(note.to_string());
        } else if let Some(state) = state_from_name(line) {
            transcript.push(state);
        }
    }
    let terminal = transcript.iter().rev().copied().find(|s| s.is_terminal());
    LaunchObservation {
        terminal,
        transcript,
        notes,
        confinement_installed: installed,
        launcher_exit,
        captured_stdout,
        captured_stderr,
    }
}

/// Map a transcript state name (the launcher's `LauncherState` `Debug` spelling) back to
/// the enum. Unknown lines (free text, blank) return `None` and are ignored.
fn state_from_name(name: &str) -> Option<LauncherState> {
    let state = match name {
        "LauncherStarted" => LauncherState::LauncherStarted,
        "IdentityVerified" => LauncherState::IdentityVerified,
        "PlanVerified" => LauncherState::PlanVerified,
        "HandlesVerified" => LauncherState::HandlesVerified,
        "ChildCreated" => LauncherState::ChildCreated,
        "IdentityPhaseResolved" => LauncherState::IdentityPhaseResolved,
        "VisibilityPhaseResolved" => LauncherState::VisibilityPhaseResolved,
        "AmbientAuthorityPhaseResolved" => LauncherState::AmbientAuthorityPhaseResolved,
        "ConfinementPhaseResolved" => LauncherState::ConfinementPhaseResolved,
        "ReadyToExec" => LauncherState::ReadyToExec,
        "ExecSucceeded" => LauncherState::ExecSucceeded,
        "SetupRefused" => LauncherState::SetupRefused,
        "SetupFaulted" => LauncherState::SetupFaulted,
        _ => return None,
    };
    Some(state)
}
