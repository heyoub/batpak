use super::BootError;
use crate::sys::{self, ObservedShape};
use bvisor::linux::protocol::{DescriptorKind, DescriptorSlotV1, LauncherState, LinuxLaunchBodyV1};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::RawFd;

// ── Seccomp (S10): compile the parent-built denylist BPF ───────────────────────

/// Compile the default-allow seccomp DENYLIST the child installs, from the plan's
/// `target.seccomp` request, IN THE PARENT (so the child-window install is allocation-free).
/// The request composes into ONE denylist: `deny_new_tasks` denies the
/// `clone`/`clone3`/`fork`/`vfork` family (`ChildSpawn::DenyNewTasks`), `deny_inet_sockets`
/// denies `socket(2)` (the NetworkDenyAll DiD). The deny terminal is `Errno(EPERM)` so the
/// workload's `fork()` / `socket()` FAILS OBSERVABLY (the §4 oracle reads the failure) rather
/// than the harder SIGSYS kill — the filter ALWAYS allows execve/execveat/write/exit_group so
/// the following fexecve + error reporting survive (seccomp's compile() enforces that).
///
/// Returns `Err(())` (⇒ the launcher refuses, the target never runs) when: a seccomp action
/// was scheduled but the plan carries NO `target.seccomp` request, the request denies nothing
/// (a no-op), or the seccomp model rejects the policy / the compile fails. A denylist is ONE
/// composed layer — the broad confinement is landlock/cgroup/netns/fd-scrub.
pub(super) fn build_seccomp_filter(body: &LinuxLaunchBodyV1) -> Result<sys::BpfProgram, ()> {
    use bvisor::linux::seccomp::{DefaultAction, SeccompPolicy};
    let Some(request) = body.target.seccomp else {
        // A seccomp action was scheduled but no request describes it ⇒ fail closed.
        return Err(());
    };
    if !request.denies_anything() {
        // An all-false request would compile a no-op (empty) denylist ⇒ fail closed.
        return Err(());
    }
    // Compose the deny set from the request flags (EPERM so the deny is OBSERVABLE).
    let mut deny = Vec::new();
    if request.deny_new_tasks {
        deny.extend(SeccompPolicy::task_creation_syscalls());
    }
    if request.deny_inet_sockets {
        deny.push(SeccompPolicy::socket_syscall());
    }
    let policy = SeccompPolicy::denylist(DefaultAction::Errno(eperm()), deny);
    // Compile for the launcher's OWN arch (the install is always same-arch).
    let compiled = policy.compile(current_seccomp_arch()).map_err(|_| ())?;
    Ok(compiled.program().clone())
}

/// `EPERM` as the seccomp deny errno (so the denied syscall fails with a standard,
/// observable error rather than a SIGSYS kill). Cast through `u32` for the errno field.
fn eperm() -> u32 {
    u32::try_from(libc::EPERM).unwrap_or(1)
}

/// The launcher's OWN seccomp target arch (the install is always same-arch). The seccomp
/// model supports LE x86_64/aarch64/riscv64; on any OTHER build arch the launcher cannot
/// assemble a correct filter, so a seccomp-bearing plan would have failed to compile a
/// usable filter — we map the three supported arches and fall through to x86_64 only as a
/// compile-time impossibility guard (the binary is built for exactly one arch).
fn current_seccomp_arch() -> bvisor::SeccompArch {
    use bvisor::SeccompArch;
    #[cfg(target_arch = "x86_64")]
    {
        SeccompArch::X86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        SeccompArch::Aarch64
    }
    #[cfg(target_arch = "riscv64")]
    {
        SeccompArch::Riscv64
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )))]
    {
        SeccompArch::X86_64
    }
}

// ── Handle verification ────────────────────────────────────────────────────────

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
pub(super) fn verify_handles(body: &LinuxLaunchBodyV1) -> Result<Result<(), ()>, BootError> {
    for slot in &body.descriptor_table {
        let observed = sys::fstat_shape(super::raw(slot.slot_index))?;
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

// ── User namespace rendezvous ─────────────────────────────────────────────────

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
pub(super) fn user_namespace_rendezvous(
    child_pid: libc::pid_t,
    sync_write_fd: RawFd,
) -> Result<(), ()> {
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

// ── Child wait ─────────────────────────────────────────────────────────────────

/// What the child reported.
pub(super) enum ChildOutcome {
    /// The error pipe gave EOF (write end CLOEXEC-closed by a successful execve) ⇒
    /// the child's deterministic scrub→fexecve ran to exec.
    ExecedToEof,
    /// The error pipe yielded errno bytes ⇒ scrub or fexecve failed; the target
    /// never ran.
    Errno(i32),
}

/// Read the error pipe to EOF or an errno, then reap the child. EOF ⇒ exec success;
/// any bytes ⇒ the reported errno.
pub(super) fn wait_for_child(
    error_read_fd: RawFd,
    child_pid: libc::pid_t,
) -> Result<ChildOutcome, BootError> {
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
pub(super) struct Transcript {
    sink: File,
}

impl Transcript {
    pub(super) fn new(sink: File) -> Self {
        Self { sink }
    }

    /// Emit one launcher state as a line (e.g. `LauncherStarted`).
    pub(super) fn emit(&mut self, state: LauncherState) {
        let _ = writeln!(self.sink, "{state:?}");
        let _ = self.sink.flush();
    }

    /// Emit a free-form note line, prefixed `# ` so a reader can separate notes from
    /// state lines.
    pub(super) fn note(&mut self, text: &str) -> std::io::Result<()> {
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
pub(super) fn boot_fault(err: &BootError) -> std::process::ExitCode {
    let mut sink = std::io::stderr();
    let _ = writeln!(sink, "bvisor-linux-launcher: {err}");
    std::process::ExitCode::from(4)
}
