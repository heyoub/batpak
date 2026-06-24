//! The SANCTIONED unsafe basement for the single-threaded Linux confinement
//! LAUNCHER (kernel plan §10.8). The ONE quarantine where the launcher's
//! raw-syscall `unsafe` is permitted to live; every `unsafe` block here carries a
//! `LEDGER:<id>` anchor reconciled against `traceability/unsafe_ledger.yaml` by the
//! `structural-check` unsafe-ledger gate (fail-closed). The safe orchestration in
//! `main.rs` (sequencing, the transcript, the decision logic) NEVER contains
//! `unsafe` — it calls down through the narrow wrappers below.
//!
//! ## The async-signal-safety contract (the load-bearing invariant)
//! The launcher creates the workload child via raw `clone3` ([`clone3_child`]) —
//! NOT `std::process::Command` (which would `fork`+`exec` behind a `.spawn()` the
//! single-thread gate bans, and is not under our control). After `clone3` the CHILD
//! branch runs in a window where, post-fork in a (formerly) multi-thread-capable
//! address space, ONLY async-signal-safe syscalls are legal: no heap allocation, no
//! lock, no Rust std that allocates. This basement upholds that by BUILDING EVERY
//! pointer / array / fd the child needs IN THE PARENT, before `clone3`, packed into
//! a [`ChildExecPlan`]; the child branch then only INDEXES that already-allocated
//! memory (copy-on-write after fork — reading touches no allocator) and issues the
//! listed async-signal-safe syscalls (`close`, `sigprocmask`, `fchdir`, `fexecve`,
//! `write`, `_exit`). If a step here cannot honestly be made allocation-free it does
//! NOT belong in the child window.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{FromRawFd, IntoRawFd, RawFd};

/// The `fstat`-observed shape of a descriptor: its kind (from `st_mode & S_IFMT`)
/// and whether it was opened writable (from the file-status `O_ACCMODE` flags).
/// Inert plain data — the safe orchestration compares it to the declared
/// `DescriptorShape` without ever touching `unsafe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObservedShape {
    /// The `st_mode & S_IFMT` file-type bits (`S_IFDIR`/`S_IFREG`/`S_IFSOCK`/`S_IFIFO`/…),
    /// kept in the platform `mode_t` so the caller compares against the raw libc
    /// constants directly (no lossy conversion).
    pub(crate) file_type: libc::mode_t,
    /// Whether the handle is writable (access mode is `O_WRONLY` or `O_RDWR`).
    pub(crate) writable: bool,
}

/// A fully pre-built child-execution plan: EVERYTHING the post-`clone3` child needs,
/// allocated in the single-threaded parent BEFORE the fork. The child branch only
/// reads these fields; it never allocates, locks, or grows any of them.
///
/// `argv`/`envp` are NUL-terminated arrays of pointers into the `CString`s held in
/// `_argv_storage`/`_envp_storage` (kept alive for the plan's lifetime so the
/// pointers stay valid). `close_fds` is the scrub close-list. `error_fd` is the
/// `O_CLOEXEC` write end of the error pipe — successful `fexecve` auto-closes it, so
/// the coordinator observes EOF; any failure writes the errno here before `_exit`.
pub(crate) struct ChildExecPlan {
    exe_fd: RawFd,
    cwd_fd: Option<RawFd>,
    error_fd: RawFd,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
    close_fds: Vec<libc::c_int>,
    _argv_storage: Vec<CString>,
    _envp_storage: Vec<CString>,
}

/// Why a [`ChildExecPlan`] could not be built (all in the PARENT, before any fork —
/// allocation here is fine and these are ordinary fallible-build errors).
#[derive(Debug)]
pub(crate) enum PlanBuildError {
    /// An `argv`/`envp` string contained an interior NUL, so it cannot become a
    /// C string for `fexecve`.
    InteriorNul,
}

impl std::fmt::Display for PlanBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InteriorNul => {
                write!(f, "argv/envp entry contains an interior NUL byte")
            }
        }
    }
}

impl std::error::Error for PlanBuildError {}

impl ChildExecPlan {
    /// Build the plan IN THE PARENT (single-threaded, pre-fork). All allocation —
    /// the `CString` conversions, the pointer arrays, the close-list `Vec` — happens
    /// HERE; the child branch only reads the result. `argv`/`envp` are the explicit
    /// vectors from the verified plan (no inherited env). `close_fds` is the scrub
    /// list the caller computed (every open fd EXCEPT the exec/stdio/error allowlist).
    ///
    /// # Errors
    /// [`PlanBuildError::InteriorNul`] if any `argv`/`envp` entry has an interior NUL.
    pub(crate) fn build(
        exe_fd: RawFd,
        cwd_fd: Option<RawFd>,
        error_fd: RawFd,
        argv: &[String],
        envp: &[(String, String)],
        close_fds: Vec<libc::c_int>,
    ) -> Result<Self, PlanBuildError> {
        let mut argv_storage: Vec<CString> = Vec::with_capacity(argv.len());
        for arg in argv {
            argv_storage
                .push(CString::new(arg.as_bytes()).map_err(|_| PlanBuildError::InteriorNul)?);
        }
        let mut envp_storage: Vec<CString> = Vec::with_capacity(envp.len());
        for (name, value) in envp {
            let mut joined = Vec::with_capacity(name.len() + value.len() + 1);
            joined.extend_from_slice(name.as_bytes());
            joined.push(b'=');
            joined.extend_from_slice(value.as_bytes());
            envp_storage.push(CString::new(joined).map_err(|_| PlanBuildError::InteriorNul)?);
        }
        let argv_ptrs: Vec<*const libc::c_char> = argv_storage
            .iter()
            .map(|c| c.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        let envp_ptrs: Vec<*const libc::c_char> = envp_storage
            .iter()
            .map(|c| c.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        Ok(Self {
            exe_fd,
            cwd_fd,
            error_fd,
            argv: argv_ptrs,
            envp: envp_ptrs,
            close_fds,
            _argv_storage: argv_storage,
            _envp_storage: envp_storage,
        })
    }
}

/// Read ALL bytes from an inherited raw fd into an owned `Vec` — used by the
/// COORDINATOR (single-threaded, pre-`clone3`), where heap allocation is fine.
///
/// The fd is adopted into a temporary [`File`], drained, then released WITHOUT
/// closing (`into_raw_fd`) so the caller still owns the underlying descriptor and
/// the launcher fd-accounting stays exact.
///
/// # Errors
/// Any `io::Error` from the read.
pub(crate) fn read_fd_to_vec(fd: RawFd) -> io::Result<Vec<u8>> {
    // SAFETY (LEDGER:linux-launcher-read-fd): `fd` is an inherited descriptor the
    // host opened and handed the launcher via the documented env-named slot; we
    // adopt it into a `File` only to drain it. `into_raw_fd` releases ownership
    // WITHOUT closing, so the descriptor is neither double-closed nor leaked — the
    // caller retains exactly the fd it passed in. No pointer is dereferenced and no
    // raw memory is touched; only safe `File` reads run between adopt and release.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let mut buf = Vec::new();
    let result = file.read_to_end(&mut buf);
    let _ = file.into_raw_fd();
    result?;
    Ok(buf)
}

/// `fstat` an inherited descriptor and return its observed shape (kind + writable),
/// for the COORDINATOR's handle-verification step. Pure observation — no fd is
/// created, consumed, or mutated.
///
/// # Errors
/// An `io::Error` carrying the `fstat`/`fcntl` errno on failure.
pub(crate) fn fstat_shape(fd: RawFd) -> io::Result<ObservedShape> {
    // SAFETY (LEDGER:linux-launcher-fstat): `stat` is zero-initialised and passed
    // by `&mut` to `fstat`, which only WRITES the struct and reads nothing from
    // user memory beyond that out-pointer; `fcntl(F_GETFL)` reads only the kernel's
    // file-status flags. `fd` is borrowed, not consumed — no fd is created or
    // closed. On error each returns -1 and we surface the OS errno. Sound for any
    // valid open `fd` the host handed us.
    let (mode, flags) = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) != 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        (st.st_mode, flags)
    };
    let access = flags & libc::O_ACCMODE;
    Ok(ObservedShape {
        file_type: mode & libc::S_IFMT,
        writable: access == libc::O_WRONLY || access == libc::O_RDWR,
    })
}

/// Adopt an inherited raw fd as an owned [`File`] for the COORDINATOR to write its
/// transcript (control fd) or read the child's error report (error-pipe read end).
/// The returned `File` OWNS the descriptor and closes it on drop — the caller must
/// pass an fd it intends the launcher to own for the rest of the run.
pub(crate) fn adopt_fd(fd: RawFd) -> File {
    // SAFETY (LEDGER:linux-launcher-adopt-fd): `fd` is an inherited descriptor the
    // host opened for the launcher (control channel / error-pipe end), named via a
    // documented env slot. We take exclusive ownership of exactly this one fd; the
    // caller passes each such fd here AT MOST ONCE, so there is no aliasing and no
    // double-close. No pointer is dereferenced and no raw memory is touched.
    unsafe { File::from_raw_fd(fd) }
}

/// Create the workload child via raw `clone3` and, IN THE CHILD, run the
/// deterministic async-signal-safe `scrub → (optional fchdir) → fexecve` sequence on
/// the PRE-BUILT [`ChildExecPlan`]. Returns the child pid to the PARENT.
///
/// Topology (PERMANENT): coordinator (this process) → workload child → exec target.
/// The launcher NEVER self-execs: `clone3` makes a real child and the parent
/// returns. On success the child's image is replaced by the target; on any child
/// failure the child writes the errno to the error pipe and `_exit(127)`s, and the
/// parent observes the failure via the error pipe + `waitid`.
///
/// # Errors
/// An `io::Error` carrying the `clone3` errno if the fork itself fails (the child
/// never exists, so nothing ran).
pub(crate) fn clone3_child(plan: &ChildExecPlan) -> io::Result<libc::pid_t> {
    // Build the clone3 argument IN THE PARENT. flags=0 for the skeleton (the
    // MECHANISM is clone3; pidfd / CLONE_INTO_CGROUP are later steps). exit_signal
    // = SIGCHLD so the parent can `waitid` the child normally.
    let mut args: libc::clone_args = ChildArgs::zeroed();
    // exit_signal = SIGCHLD (a small positive constant) so the parent reaps via the
    // normal child-signal path; widen without a lossy `as` cast.
    args.exit_signal = u64::try_from(libc::SIGCHLD).unwrap_or(0);
    let size = u64::try_from(std::mem::size_of::<libc::clone_args>()).unwrap_or(0);

    // SAFETY (LEDGER:linux-launcher-clone3-child): `clone3` is invoked with a
    // properly sized `clone_args` (flags=0, exit_signal=SIGCHLD) built in the
    // single-threaded parent. The PARENT branch (rc>0) only returns the pid. The
    // CHILD branch (rc==0) is the async-signal-safe window: it touches ONLY the
    // PRE-BUILT `plan` (argv/envp pointer arrays, close-list, fds — all allocated
    // by `ChildExecPlan::build` BEFORE this call) by INDEXING already-mapped
    // copy-on-write memory, and issues ONLY async-signal-safe syscalls —
    // `close`, `sigprocmask`, `fchdir`, `fexecve`, `write`, `_exit`. It performs NO
    // heap allocation, takes NO lock, and calls NO allocating Rust std. On ANY
    // child-side failure it writes the errno to the `O_CLOEXEC` error-pipe fd and
    // `_exit(127)`s WITHOUT unwinding, so the target never runs and the parent
    // observes the fault. On success `fexecve` replaces the image and CLOEXEC closes
    // the error fd, which the parent reads as EOF. No memory is freed across the
    // fork and no destructor runs in the child (the `_exit`/`fexecve` paths never
    // return into Rust).
    let rc = unsafe { libc::syscall(libc::SYS_clone3, std::ptr::addr_of!(args), size) };

    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc == 0 {
        // SAFETY (LEDGER:linux-launcher-child-window): the `rc == 0` clone3 CHILD
        // branch — the async-signal-safe window. `run_child` is an `unsafe fn` that
        // performs ONLY async-signal-safe syscalls (sigprocmask/close/fchdir/fexecve/
        // write/_exit) on the PRE-BUILT `plan` (argv/envp pointer arrays into parent-
        // owned CStrings, the scrub close-list, and the fds — all allocated by
        // ChildExecPlan::build BEFORE clone3). It indexes already-mapped copy-on-write
        // memory, performs NO heap allocation, takes NO lock, and DIVERGES (it either
        // fexecve-replaces the image or _exit(127)s after writing the errno) — so no
        // destructor runs and no unwinding crosses the fork. This call site is reached
        // ONLY in the child branch, satisfying `run_child`'s contract.
        unsafe { run_child(plan) }
    }
    // PARENT — return the child pid. `rc` is the pid (> 0).
    let pid = libc::pid_t::try_from(rc).unwrap_or(-1);
    Ok(pid)
}

/// The CHILD branch body: the deterministic async-signal-safe sequence. Diverges —
/// it either `fexecve`s (image replaced) or `_exit`s. NEVER returns into Rust, so no
/// destructor runs and no unwinding crosses the fork. Marked `unsafe` because it
/// dereferences the pre-built raw pointer arrays and issues raw syscalls.
///
/// SAFETY: callable ONLY from the `rc == 0` child branch of [`clone3_child`], with a
/// `plan` whose `argv`/`envp`/`close_fds` were fully built in the parent. It indexes
/// only that already-allocated memory and calls only async-signal-safe syscalls.
unsafe fn run_child(plan: &ChildExecPlan) -> ! {
    // 1. Normalise the signal mask to empty (async-signal-safe). The set is a
    //    stack `sigset_t`; `sigemptyset`/`sigprocmask` allocate nothing.
    let mut empty: libc::sigset_t = std::mem::zeroed();
    if libc::sigemptyset(&mut empty) == 0 {
        let _ = libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
    }

    // 2. Scrub: close every fd in the pre-built close-list (async-signal-safe). A
    //    failing close (already-closed fd) is ignored — the list is the parent's
    //    allowlist complement, computed before the fork. The raw `SYS_close` syscall
    //    is used (NOT `libc::close`) to bypass std's owned-fd close guard: in the
    //    forked child these fds are still tracked as owned by parent-built `File`s,
    //    and std's `close` shim would abort on them; the raw syscall closes the fd
    //    in the child's own fd table without touching that guard.
    let mut i = 0usize;
    while i < plan.close_fds.len() {
        libc::syscall(libc::SYS_close, plan.close_fds[i]);
        i += 1;
    }

    // 3. Optional cwd normalisation to a declared directory fd (async-signal-safe).
    if let Some(cwd) = plan.cwd_fd {
        if libc::fchdir(cwd) != 0 {
            child_fail(plan.error_fd);
        }
    }

    // 4. Replace the image. exec rides the fd, never a path (no reopen race). On
    //    return fexecve FAILED — report and _exit.
    libc::fexecve(plan.exe_fd, plan.argv.as_ptr(), plan.envp.as_ptr());
    child_fail(plan.error_fd)
}

/// Report the current errno to the error pipe and `_exit(127)` — async-signal-safe.
/// Diverges. SAFETY: callable only from the child window with a valid `error_fd`.
unsafe fn child_fail(error_fd: RawFd) -> ! {
    let errno = *libc::__errno_location();
    let bytes = errno.to_ne_bytes();
    // A single `write` of the fixed-width errno; partial writes are irrelevant —
    // the parent only needs to distinguish EOF (success) from any bytes (failure).
    libc::write(error_fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len());
    libc::_exit(127)
}

/// Close one inherited raw fd in the COORDINATOR via the raw `close` syscall. Used
/// to drop the coordinator's own copy of the error-pipe WRITE end after clone3 so
/// the read end can reach EOF. The raw syscall is used (NOT a `File` drop) because
/// std's owned-fd close path aborts the process if the fd is already closed, whereas
/// here a best-effort close is wanted (the child may or may not still share it).
pub(crate) fn close_fd(fd: RawFd) {
    // SAFETY (LEDGER:linux-launcher-close-fd): a single raw `close` on an inherited
    // descriptor the launcher owns. No pointer is dereferenced and no Rust value
    // wraps this fd (it is passed as a plain RawFd), so there is no aliasing and no
    // double-free of an owned handle. A failure (already-closed fd) is ignored.
    unsafe {
        libc::syscall(libc::SYS_close, fd);
    }
}

/// Reap a child pid via `waitid(P_PID, …, WEXITED)` in the COORDINATOR (the parent,
/// single-threaded). Best-effort: the launch outcome is decided by the error pipe,
/// not the child's exit code; this only prevents a zombie. Errors are swallowed.
pub(crate) fn reap_child(pid: libc::pid_t) {
    // A child pid is non-negative; widen to id_t via try_from (no lossy `as`).
    let Ok(id) = libc::id_t::try_from(pid) else {
        return;
    };
    // SAFETY (LEDGER:linux-launcher-waitid): `siginfo_t` is zero-initialised and
    // passed by `&mut` to `waitid`, which only WRITES it; we pass `WEXITED` to wait
    // for the child's terminal state. `pid` is the child this launcher just created
    // via clone3, so the parent is entitled to reap it. No user memory is read
    // through a raw pointer beyond the out-`siginfo`, and the call cannot corrupt
    // launcher state. Best-effort — a failure is ignored (the pipe already decided
    // the outcome).
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let _ = libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED);
    }
}

/// Zero-initialise a `clone_args` without naming every per-arch field. A tiny
/// helper so the basement stays arch-portable.
trait ChildArgs {
    fn zeroed() -> Self;
}

impl ChildArgs for libc::clone_args {
    fn zeroed() -> Self {
        // SAFETY (LEDGER:linux-launcher-clone-args-zero): `clone_args` is a
        // C-layout struct of plain integer fields (no references, no `NonNull`, no
        // padding-sensitive invariants), so the all-zero bit pattern is a valid
        // inhabitant. We immediately overwrite the fields we use before passing it
        // to `clone3`. No pointer is dereferenced here.
        unsafe { std::mem::zeroed() }
    }
}
