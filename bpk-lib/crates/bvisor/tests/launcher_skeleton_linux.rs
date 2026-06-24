// Integration test for the single-threaded Linux confinement LAUNCHER skeleton
// (kernel plan §10.8). Real-OS: spawns the `bvisor-linux-launcher` bin with
// inherited fds, so it is gated to Linux + the backend-linux feature. Tests MAY use
// unsafe + `Command::pre_exec` (tests are NOT basement-checked) to wire the fds.
#![cfg(all(target_os = "linux", feature = "backend-linux"))]
//! Skeleton coordinator↔child behaviour: the happy path (real child execs, EOF on
//! the error pipe, coordinator pid ≠ target pid → NOT self-exec), and the three
//! refusals (MissingPrimitive, HandleMismatch, bad plan). The PURE phase-honesty
//! fixtures live in `launcher_protocol.rs` and are not duplicated here.

use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

// Frozen ids/phase-codes the skeleton serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3; // LoweringPhase::FdHygiene.code()
const PHASE_CODE_EXEC: u8 = 5; // LoweringPhase::Launch.code()

// Fixed fd numbers the launcher inherits (chosen well above stdio + the exe slot).
const FIXED_EXE_FD: RawFd = 10;
const FIXED_CONTROL_FD: RawFd = 11;
const FIXED_ERROR_WRITE_FD: RawFd = 12;
const FIXED_PLAN_FD: RawFd = 13;
const FIXED_ERROR_READ_FD: RawFd = 14;

// ── Wire helpers ───────────────────────────────────────────────────────────────

fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

/// A body whose `h_l` is `blake3(canonical(lowering))` so the skeleton's
/// schedule-digest binding passes (the REAL H_L binding is #75 — note it).
fn body_with(
    lowering: LoweringWireV1,
    table: Vec<DescriptorSlotV1>,
    argv: Vec<String>,
) -> LinuxLaunchBodyV1 {
    let bytes = batpak::canonical::to_bytes(&lowering).expect("encode lowering");
    let h_l = batpak::event::hash::compute_hash(&bytes);
    LinuxLaunchBodyV1 {
        attempt_id: AttemptId([7u8; 32]),
        plan_id: BoundaryPlanHash([1u8; 32]),
        h_a: AdmissionProgramHash([2u8; 32]),
        h_p: BackendProfileHash([3u8; 32]),
        h_l,
        lowering,
        descriptor_table: table,
        target: TargetSpecV1 {
            argv,
            envp: vec![("PATH".to_owned(), "/usr/bin".to_owned())],
            exe_slot: u32::try_from(FIXED_EXE_FD).expect("fd fits u32"),
        },
    }
}

/// The exe-slot descriptor declaration (regular, read-only) at the fixed exe fd.
fn exe_slot() -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: u32::try_from(FIXED_EXE_FD).expect("fd"),
        role: DescriptorRole::TargetExe,
        expected: DescriptorShape {
            kind: DescriptorKind::Regular,
            writable: false,
        },
    }
}

/// A valid scrub+exec plan launching `/bin/true`.
fn happy_plan() -> LinuxLaunchPlanV1 {
    let lowering = LoweringWireV1 {
        entries: vec![
            entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB),
            entry(ID_EXEC, PHASE_CODE_EXEC),
        ],
    };
    LinuxLaunchPlanV1 {
        body: body_with(lowering, vec![exe_slot()], vec!["true".to_owned()]),
    }
}

// ── fd setup ───────────────────────────────────────────────────────────────────

/// Open `/bin/true` read-only as an OwnedFd (the target exe rides this fd).
fn open_true() -> OwnedFd {
    let file = std::fs::File::open("/bin/true").expect("open /bin/true");
    OwnedFd::from(file)
}

/// A Unix socketpair, BOTH ends O_CLOEXEC so the parent-numbered originals close on
/// the launcher's execve — only the fixed-numbered dups (CLOEXEC-cleared by dup2)
/// survive into the launcher, keeping its fd table clean for the no-fd-escape check.
/// Returns (launcher control end, test reader end).
fn socketpair() -> (OwnedFd, OwnedFd) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: test-only fd setup; `fds` is a valid 2-element out-array for
    // socketpair, which writes exactly two fds on success.
    let rc = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    assert_eq!(rc, 0, "socketpair");
    // SAFETY: socketpair just produced two fresh, owned fds.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// A pipe with BOTH ends O_CLOEXEC. The fixed-numbered dup of the write end (made in
/// pre_exec) is RE-SET to CLOEXEC explicitly so a successful target execve inside the
/// launcher's child auto-closes it (coordinator sees EOF); the parent-numbered
/// originals close on the launcher's own execve. Returns (read end, write end).
fn error_pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: test-only; `fds` is a valid 2-element out-array for pipe2.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert_eq!(rc, 0, "pipe2");
    // SAFETY: pipe2 just produced two fresh, owned fds.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// Materialise a plan into an OwnedFd holding its framed bytes (a sealed temp file,
/// rewound to offset 0 — the launcher reads it to EOF).
fn plan_fd(plan: &LinuxLaunchPlanV1) -> OwnedFd {
    use std::io::{Seek, SeekFrom, Write};
    let bytes = plan.encode().expect("encode plan");
    let mut f = tempfile::tempfile().expect("tempfile");
    f.write_all(&bytes).expect("write plan");
    f.seek(SeekFrom::Start(0)).expect("rewind");
    OwnedFd::from(f)
}

/// `dup2` `src` onto `target` in the CHILD (async-signal-safe-ish test setup) and
/// clear CLOEXEC on the target unless `keep_cloexec`. Returns the raw errno on
/// failure so `pre_exec` can propagate it.
fn dup_to(src: RawFd, target: RawFd, keep_cloexec: bool) -> std::io::Result<()> {
    // SAFETY: runs in the forked child via pre_exec; dup2/fcntl are async-signal-safe
    // and operate on inherited fds. Test-only.
    unsafe {
        if libc::dup2(src, target) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if !keep_cloexec {
            let flags = libc::fcntl(target, libc::F_GETFD);
            if flags >= 0 {
                let _ = libc::fcntl(target, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
    Ok(())
}

/// Spawn the launcher with the given plan + descriptor exe fd, returning the child
/// process handle and the test's control-read end. `exe_fd` is what gets duped to
/// `FIXED_EXE_FD` (vary it to force a HandleMismatch). The launcher owns BOTH error-
/// pipe ends (write end for the child, read end for itself); the test reads the
/// transcript, which reports the EOF-vs-errno outcome honestly.
fn spawn_launcher(plan: &LinuxLaunchPlanV1, exe_fd: OwnedFd) -> (std::process::Child, OwnedFd) {
    spawn_with_plan_fd(plan_fd(plan), exe_fd)
}

/// The shared spawn core, taking an already-materialised plan fd (so the bad-plan
/// test can supply a corrupt frame). Wires the four launcher fds + the error read
/// end to their fixed numbers via a test-only `pre_exec`.
fn spawn_with_plan_fd(pfd: OwnedFd, exe_fd: OwnedFd) -> (std::process::Child, OwnedFd) {
    // Serialise the fd-setup + fork window across parallel test threads: the launcher
    // enforces a no-undeclared-fd baseline, and a sibling thread's fds (open at the
    // instant of `fork`) could otherwise leak into this child and trip that check.
    // The whole setup→spawn region is the critical section; reads happen after.
    static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SPAWN_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let (control_launcher, control_test) = socketpair();
    let (error_read, error_write) = error_pipe();

    // Raw fds the child will dup FROM (parent-owned; valid until after spawn).
    let exe_raw = exe_fd.as_raw_fd();
    let control_raw = control_launcher.as_raw_fd();
    let error_w_raw = error_write.as_raw_fd();
    let error_r_raw = error_read.as_raw_fd();
    let plan_raw = pfd.as_raw_fd();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bvisor-linux-launcher"));
    // Explicit env only — the launcher reads just its fd-number vars.
    cmd.env_clear()
        .env("BVISOR_LAUNCH_PLAN_FD", FIXED_PLAN_FD.to_string())
        .env("BVISOR_CONTROL_FD", FIXED_CONTROL_FD.to_string())
        .env("BVISOR_ERROR_FD", FIXED_ERROR_WRITE_FD.to_string())
        .env("BVISOR_ERROR_READ_FD", FIXED_ERROR_READ_FD.to_string());

    // SAFETY: test-only pre_exec — dup the inherited fds to fixed numbers. The
    // error-WRITE fd is re-set O_CLOEXEC (so a successful target execve inside the
    // launcher's child auto-closes it → coordinator read end sees EOF); the others
    // clear CLOEXEC so the launcher inherits them.
    unsafe {
        cmd.pre_exec(move || {
            dup_to(exe_raw, FIXED_EXE_FD, false)?;
            dup_to(control_raw, FIXED_CONTROL_FD, false)?;
            dup_to(plan_raw, FIXED_PLAN_FD, false)?;
            dup_to(error_r_raw, FIXED_ERROR_READ_FD, false)?;
            if libc::dup2(error_w_raw, FIXED_ERROR_WRITE_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(FIXED_ERROR_WRITE_FD, libc::F_GETFD);
            if flags >= 0 {
                let _ = libc::fcntl(
                    FIXED_ERROR_WRITE_FD,
                    libc::F_SETFD,
                    flags | libc::FD_CLOEXEC,
                );
            }
            Ok(())
        });
    }

    let child = cmd.spawn().expect("spawn launcher");
    // Drop ALL launcher-side ends in the parent so the launcher owns them solely.
    drop(control_launcher);
    drop(error_write);
    drop(error_read);
    drop(pfd);
    drop(exe_fd);
    (child, control_test)
}

/// Read all of an OwnedFd to a String (the control transcript).
fn read_all(fd: OwnedFd) -> String {
    let mut f = std::fs::File::from(fd);
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s
}

/// Materialise raw (possibly corrupt) framed bytes into a rewound temp-file fd.
fn raw_plan_fd(bytes: &[u8]) -> OwnedFd {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = tempfile::tempfile().expect("tempfile");
    f.write_all(bytes).expect("write plan bytes");
    f.seek(SeekFrom::Start(0)).expect("rewind");
    OwnedFd::from(f)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// HAPPY PATH: a real child execs `/bin/true`; the transcript ends `ExecSucceeded`
/// (which the coordinator emits IFF the error pipe gave EOF — no errno); and the
/// launcher coordinator pid ≠ the child pid (a real clone3 child, NOT a self-exec).
#[test]
fn happy_path_execs_a_real_child_and_reports_success() {
    let plan = happy_plan();
    let exe = open_true();
    let (mut child, control) = spawn_launcher(&plan, exe);

    let transcript = read_all(control);
    let status = child.wait().expect("wait launcher");
    let coordinator_pid = i64::from(child.id());

    assert!(
        transcript.contains("LauncherStarted"),
        "transcript should start: {transcript}"
    );
    let child_pid = child_pid_from(&transcript);
    assert!(
        transcript.contains("ChildCreated"),
        "a real child was created: {transcript}"
    );
    assert!(
        child_pid != coordinator_pid && child_pid > 0,
        "the workload child pid ({child_pid}) must differ from the coordinator pid \
         ({coordinator_pid}) — proves a real clone3 child, NOT a self-exec"
    );
    assert!(
        transcript.trim_end().ends_with("ExecSucceeded"),
        "transcript must end ExecSucceeded (⟺ error pipe EOF, no errno): {transcript}"
    );
    assert!(
        !transcript.contains("errno="),
        "no child errno on exec success: {transcript}"
    );
    assert!(status.success(), "launcher exit 0 on success: {status:?}");
}

/// MISSING PRIMITIVE: a plan scheduling a confinement action the skeleton does not
/// implement ⇒ SetupRefused, NO child/exec.
#[test]
fn missing_primitive_refuses_before_any_child() {
    // A landlock-ish action in the PolicyInstall phase (code 4) — unimplemented.
    let lowering = LoweringWireV1 {
        entries: vec![
            entry("linux.landlock.v1", 4),
            entry(ID_EXEC, PHASE_CODE_EXEC),
        ],
    };
    let plan = LinuxLaunchPlanV1 {
        body: body_with(lowering, vec![exe_slot()], vec!["true".to_owned()]),
    };
    let exe = open_true();
    let (mut child, control) = spawn_launcher(&plan, exe);

    let transcript = read_all(control);
    let _ = child.wait();

    assert!(
        transcript.contains("SetupRefused"),
        "must refuse: {transcript}"
    );
    assert!(
        transcript.contains("MissingPrimitive"),
        "reason MissingPrimitive: {transcript}"
    );
    assert!(
        !transcript.contains("ChildCreated"),
        "NO child on refusal: {transcript}"
    );
}

/// HANDLE MISMATCH: a descriptor declared `Regular` but the passed fd is a directory
/// ⇒ SetupRefused{HandleMismatch}, no exec.
#[test]
fn handle_mismatch_refuses() {
    let plan = happy_plan(); // declares the exe slot as Regular
                             // Pass a DIRECTORY fd where a Regular file is declared.
    let dir_fd = OwnedFd::from(std::fs::File::open("/").expect("open /"));
    let (mut child, control) = spawn_launcher(&plan, dir_fd);

    let transcript = read_all(control);
    let _ = child.wait();

    assert!(
        transcript.contains("SetupRefused"),
        "must refuse: {transcript}"
    );
    assert!(
        transcript.contains("HandleMismatch"),
        "reason HandleMismatch: {transcript}"
    );
    assert!(
        !transcript.contains("ChildCreated"),
        "NO child on handle mismatch: {transcript}"
    );
}

/// BAD PLAN: a tampered/bad-magic frame ⇒ SetupRefused (PlanInvalid), no exec.
#[test]
fn bad_plan_refuses() {
    let plan = happy_plan();
    let mut bytes = plan.encode().expect("encode");
    bytes[0] ^= 0xFF; // smash the magic
    let exe = open_true();
    let (mut child, control) = spawn_with_plan_fd(raw_plan_fd(&bytes), exe);

    let transcript = read_all(control);
    let _ = child.wait();

    assert!(
        transcript.contains("SetupRefused") || transcript.contains("SetupFaulted"),
        "bad plan must refuse/fault: {transcript}"
    );
    assert!(
        !transcript.contains("ChildCreated"),
        "NO child on a bad plan: {transcript}"
    );
}

/// Parse the `child_pid=<n>` the coordinator notes after ChildCreated; -1 if absent.
fn child_pid_from(transcript: &str) -> i64 {
    for line in transcript.lines() {
        if let Some(rest) = line.split("child_pid=").nth(1) {
            if let Ok(pid) = rest.trim().parse::<i64>() {
                return pid;
            }
        }
    }
    -1
}
