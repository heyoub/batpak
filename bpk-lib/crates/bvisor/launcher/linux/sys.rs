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

use landlock::{
    Access, AccessFs, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr,
    RulesetCreatedAttr, ABI,
};
// The compiled BPF the coordinator builds in the PARENT (via the bvisor seccomp model)
// and the child installs LAST in its window. `sock_filter` is the kernel-ABI BPF
// instruction; `BpfProgram = Vec<sock_filter>` is the assembled stream. The child only
// READS this pre-built slice (no allocation) when building the stack `sock_fprog`.
pub(crate) use seccompiler::{sock_filter, BpfProgram};
// Re-export so the SAFE coordinator (`imp.rs`) can name the owned ruleset type it
// carries from `build_landlock_ruleset` into `clone3_child` without itself depending
// on the `landlock` crate surface.
pub(crate) use landlock::RulesetCreated;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{BorrowedFd, FromRawFd, IntoRawFd, RawFd};

/// `LANDLOCK_CREATE_RULESET_VERSION` (uapi `linux/landlock.h`): asks
/// `landlock_create_ruleset` for the supported ABI version instead of creating a
/// ruleset. Stable kernel ABI constant.
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;

/// The landlock ABI floor the launcher confines at. `ABI::V3` is the access set the
/// parent-side ruleset is built from; the launcher refuses to advertise confinement
/// when the live kernel ABI is below this floor (see [`build_landlock_ruleset`]).
const LANDLOCK_ABI_FLOOR: ABI = ABI::V3;

/// The same floor as the raw kernel ABI integer the live probe returns, so the SAFE
/// coordinator can compare [`probe_landlock_abi`]'s result without depending on the
/// `landlock` crate's `ABI` enum. Kept in lockstep with [`LANDLOCK_ABI_FLOOR`].
pub(crate) const LANDLOCK_ABI_FLOOR_RAW: i64 = ABI::V3 as i64;

/// `CLONE_INTO_CGROUP` (uapi `linux/sched.h`, kernel ≥ 5.7): a `clone3` flag asking
/// the kernel to place the new child DIRECTLY into the cgroup whose fd is in
/// `clone_args.cgroup`, at birth — eliminating the post-fork
/// write-pid-to-`cgroup.procs` migration race. Named here as an explicit `u64`
/// because the value `0x2_0000_0000` is 2^33 (wider than `i32`), while libc types the
/// gnu-linux constant as `c_int`; `clone_args.flags`/`.cgroup` are both `c_ulonglong`.
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;

/// `CLONE_NEWUSER` (uapi `linux/sched.h`): a `clone3`/`clone` flag asking the kernel to
/// create the child in a NEW user namespace. Named here as an explicit `u64` (its value
/// `0x1000_0000` fits `i32`, but `clone_args.flags` is `c_ulonglong`, so we keep it wide
/// to OR it into `flags` without a lossy cast). Set ONLY when the plan opts into the
/// userns rendezvous (S8) — the child is born unmapped (overflow uid) and BLOCKS until
/// the parent writes its uid/gid maps and releases it (then it is uid 0 in the userns).
const CLONE_NEWUSER: u64 = 0x1000_0000;

/// `CLONE_NEWNET` (uapi `linux/sched.h`): a `clone3`/`clone` flag asking the kernel to
/// create the child in a NEW, EMPTY network namespace (proof-spine S9 / D3 — the
/// `NetworkDenyAll` mechanism). Named here as an explicit `u64` (its value `0x4000_0000`
/// fits `i32`, but `clone_args.flags` is `c_ulonglong`, so we keep it wide to OR it into
/// `flags` without a lossy cast). Set ONLY when the plan opts into the empty netns — and
/// ONLY ALONGSIDE `CLONE_NEWUSER` (an unprivileged process may create a new netns only when
/// it is also root in a new userns; the caller enforces the pairing). The child is born into
/// an empty netns (only `lo`, with no address + no routes => unreachable, no external interface)
/// so it is structurally unable to reach any network. This is just a FLAG BIT — it adds NO new syscall.
const CLONE_NEWNET: u64 = 0x4000_0000;

/// One declared confinement root the launcher restricts FS access TO: a pre-opened,
/// fstat-validated descriptor (NEVER a path — exec/landlock rides the inherited fd,
/// avoiding the CVE-2019-5736 reopen race) and whether the workload may write beneath
/// it. Read+execute is ALWAYS granted under a root; `writable` additionally grants the
/// write/create access set. Inert plain data the SAFE coordinator fills in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LandlockRoot {
    /// The inherited root directory fd (a `DescriptorRole::ReadRoot`/`WriteRoot`
    /// slot the coordinator already `fstat`-validated as a writable/readable dir).
    pub(crate) fd: RawFd,
    /// Whether the workload may WRITE beneath this root (else read-only).
    pub(crate) writable: bool,
}

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
    /// OPT-IN user-namespace rendezvous (S8): the READ end of the parent→child sync
    /// pipe. When `Some`, the child (born in a new userns via `CLONE_NEWUSER`, initially
    /// unmapped/overflow-uid) BLOCKS on a 1-byte `read()` of this fd as the FIRST step of
    /// the child window — waiting for the parent to write the uid/gid maps and release
    /// it. `None` ⇒ no rendezvous (the existing no-userns path, byte-for-byte unchanged).
    sync_read_fd: Option<RawFd>,
    /// OPT-IN user-namespace rendezvous (S8): the child's INHERITED copy of the sync
    /// pipe's WRITE end. The child MUST close this copy (raw `SYS_close`) as the very
    /// FIRST thing in its window — BEFORE blocking on the read — so that when the PARENT
    /// later closes ITS write copy (the fail-closed branch) the child's blocking `read()`
    /// sees EOF instead of DEADLOCKING on a write end it itself holds open. `None` when no
    /// rendezvous.
    sync_write_fd: Option<RawFd>,
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

/// The OPT-IN user-namespace rendezvous fds the child window needs (S8). Bundled so the
/// plan builder carries one optional value instead of two correlated fds: `read` is the
/// sync-pipe READ end the child blocks on; `write` is the child's inherited copy of the
/// WRITE end, which the child closes FIRST (so the parent's fail-closed close yields a
/// clean EOF rather than a deadlock). `None` ⇒ no rendezvous (no-userns path unchanged).
#[derive(Clone, Copy)]
pub(crate) struct UsernsSyncPipe {
    /// The sync-pipe READ end the child blocks on for the parent's release byte.
    pub(crate) read: RawFd,
    /// The child's inherited copy of the sync-pipe WRITE end (closed first in the child).
    pub(crate) write: RawFd,
}

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
        sync_pipe: Option<UsernsSyncPipe>,
        argv: &[String],
        envp: &[(String, String)],
        close_fds: Vec<libc::c_int>,
    ) -> Result<Self, PlanBuildError> {
        let sync_read_fd = sync_pipe.map(|p| p.read);
        let sync_write_fd = sync_pipe.map(|p| p.write);
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
            sync_read_fd,
            sync_write_fd,
            argv: argv_ptrs,
            envp: envp_ptrs,
            close_fds,
            _argv_storage: argv_storage,
            _envp_storage: envp_storage,
        })
    }
}

/// Probe the LIVE landlock ABI integer straight from the kernel.
///
/// Returns the supported ABI version (`>= 1`), or `0` when landlock is unavailable
/// (old kernel / disabled LSM). The COORDINATOR floors the confinement at
/// [`LANDLOCK_ABI_FLOOR`]: a probe below that ⇒ the launcher refuses the landlock
/// action (`SetupRefused{MissingPrimitive}`) rather than advertising a confinement it
/// cannot deliver. Pure observation, run in the single-threaded parent before clone3.
#[must_use]
pub(crate) fn probe_landlock_abi() -> i64 {
    // SAFETY (LEDGER:linux-launcher-landlock-abi): `landlock_create_ruleset` is
    // invoked in its documented VERSION-QUERY form (attr=NULL, size=0, flags=
    // LANDLOCK_CREATE_RULESET_VERSION). In this form the kernel reads NO user memory,
    // creates NO fd, and mutates nothing — it only returns the supported ABI integer
    // (>=0) or -1/errno. No pointer is dereferenced. Sound for any caller state; the
    // NULL/0 pair is exactly what the version query requires.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if raw < 0 {
        0
    } else {
        raw
    }
}

/// Build the landlock ruleset restricting FS access to exactly `roots`, IN THE PARENT
/// (before clone3) — async-signal-safety: ALL heap allocation, the
/// `landlock_create_ruleset`/`landlock_add_rule` syscalls, and the rule construction
/// happen HERE; the post-clone3 child only calls `restrict_self` (allocation-free).
///
/// Each rule is built from a [`BorrowedFd`] of the INHERITED root fd — NOT by
/// reopening a path (the CVE-2019-5736 / Leaky-Vessels reopen race the protocol
/// forbids, and strictly better than the backend's `PathFd::new(path)`). Read-only
/// roots get the read access set; writable roots get read+write. Built at
/// [`CompatLevel::HardRequirement`] so a kernel that cannot honor the ruleset fails
/// CLOSED (the caller has already probed the ABI floor, so the requirement is met).
///
/// The `roots` slice is the coordinator-resolved, already-`fstat`-validated root
/// descriptors. Building the ruleset does NOT confine the parent: only `restrict_self`
/// (in the child) applies it. SAFE: the `landlock` crate is pure safe Rust.
///
/// # Errors
/// An `io::Error` if the ruleset cannot be created (e.g. the ABI floor is not met at
/// `HardRequirement`, or a root fd cannot be borrowed) — fail closed, never widen.
pub(crate) fn build_landlock_ruleset(roots: &[LandlockRoot]) -> io::Result<RulesetCreated> {
    let abi = LANDLOCK_ABI_FLOOR;
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(landlock_to_io)?
        .create()
        .map_err(landlock_to_io)?;

    for root in roots {
        // SAFETY (LEDGER:linux-launcher-landlock-root-fd): `root.fd` is an inherited
        // directory descriptor the host opened and the coordinator already
        // `fstat`-validated as the declared read/write ROOT (a directory of the
        // declared writability). We borrow it for exactly the duration of this
        // `add_rule` call — `BorrowedFd` neither closes nor takes ownership, so the
        // coordinator's fd accounting is unchanged and there is no double-close. The
        // borrow does not outlive the loop iteration. No raw memory is touched.
        let borrowed = unsafe { BorrowedFd::borrow_raw(root.fd) };
        let access = if root.writable {
            AccessFs::from_all(abi)
        } else {
            AccessFs::from_read(abi)
        };
        ruleset = ruleset
            .add_rule(PathBeneath::new(borrowed, access))
            .map_err(landlock_to_io)?;
    }

    Ok(ruleset)
}

/// Render a landlock error as an `io::Error` (coordinator-side, pre-clone3 — the
/// allocation in the message is fine here, never in the child window).
fn landlock_to_io(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("landlock: {error}"))
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
/// `confinement` is the OPTIONAL parent-built landlock ruleset (`None` ⇒ no landlock
/// action scheduled). It is built (all allocation + add_rule syscalls) BEFORE this
/// call by [`build_landlock_ruleset`]; the child applies it via `restrict_self` after
/// the fd scrub and before `fexecve`. The parent branch never touches it (it drops at
/// return, closing only the parent's copy of the ruleset fd — the child holds its own
/// post-clone3 copy, so the parent drop does not affect the child's confinement).
///
/// `cgroup_fd` is the OPTIONAL inherited [`DescriptorRole::CgroupDir`] directory fd
/// (`None` ⇒ no cgroup placement). When `Some`, `clone3` is asked (via
/// `CLONE_INTO_CGROUP`) to place the child DIRECTLY into that prepared leaf at birth,
/// so the workload is resource-confined the instant it exists — no post-fork migration
/// window. The kernel consumes the fd DURING the syscall in the parent; the child never
/// touches it (so the scrub may close its inherited copy harmlessly).
///
/// # Errors
/// An `io::Error` carrying the `clone3` errno if the fork itself fails (the child
/// never exists, so nothing ran) — including an invalid/forbidden cgroup fd, which
/// fails the syscall rather than running the child uncgrouped.
pub(crate) fn clone3_child(
    plan: &ChildExecPlan,
    confinement: Option<RulesetCreated>,
    seccomp: Option<&BpfProgram>,
    cgroup_fd: Option<RawFd>,
    userns: bool,
    netns: bool,
) -> io::Result<libc::pid_t> {
    // Build the clone3 argument IN THE PARENT. exit_signal = SIGCHLD so the parent can
    // `waitid` the child normally; the MECHANISM is clone3 (NEVER Command::spawn).
    let mut args: libc::clone_args = ChildArgs::zeroed();
    // exit_signal = SIGCHLD (a small positive constant) so the parent reaps via the
    // normal child-signal path; widen without a lossy `as` cast.
    args.exit_signal = u64::try_from(libc::SIGCHLD).unwrap_or(0);
    // OPT-IN user-namespace rendezvous (S8): add CLONE_NEWUSER ONLY when the plan
    // requested it. When off this OR is never reached, so the flags are EXACTLY what
    // the pre-S8 no-userns path produced (0, or CLONE_INTO_CGROUP) — the existing
    // PROVEN oracles run through an unchanged environment.
    if userns {
        args.flags |= CLONE_NEWUSER;
    }
    // OPT-IN empty network namespace = NetworkDenyAll (S9 / D3): add CLONE_NEWNET ONLY
    // when the plan requested it — and the caller guarantees `netns ⇒ userns` (unprivileged
    // CLONE_NEWNET needs the child root-in-userns). The child is born into an EMPTY netns
    // (only `lo`, no address + no routes => unreachable, no external interface) at clone3 time, BEFORE the userns rendezvous
    // releases it. This adds NO new syscall — only a flag bit. When off this OR is never
    // reached, so the no-netns flags are unchanged.
    if netns {
        args.flags |= CLONE_NEWNET;
    }
    // Optional cgroup placement: if the coordinator resolved a CgroupDir slot, ask the
    // kernel to birth the child INSIDE that leaf (no migration race). A fd that does not
    // fit a u64 leaves the flag UNSET (never a truncated cgroup field) — the child then
    // simply runs in the launcher's cgroup, an honest no-placement, not a wrong one.
    if let Some(fd) = cgroup_fd {
        if let Ok(cg) = u64::try_from(fd) {
            args.flags |= CLONE_INTO_CGROUP;
            args.cgroup = cg;
        }
    }
    let size = u64::try_from(std::mem::size_of::<libc::clone_args>()).unwrap_or(0);

    // SAFETY (LEDGER:linux-launcher-clone3-child): `clone3` is invoked with a
    // properly sized `clone_args` (exit_signal=SIGCHLD, and flags drawn from {0,
    // CLONE_INTO_CGROUP, CLONE_NEWUSER, CLONE_NEWNET} — CLONE_INTO_CGROUP with `cgroup` set
    // to the inherited, fstat-validated CgroupDir directory fd, CLONE_NEWUSER ONLY when the
    // plan opted into the userns rendezvous, and CLONE_NEWNET ONLY alongside CLONE_NEWUSER
    // when it opted into the empty netns — a plain flag bit, no new syscall) built in the
    // single-threaded parent. The
    // kernel consumes the cgroup fd DURING this syscall in the parent (placing the child
    // into the leaf at birth); an invalid fd only makes clone3 fail with errno (handled
    // below — no child runs). CLONE_NEWUSER births the child in a NEW user namespace
    // initially UNMAPPED (overflow uid); the child then BLOCKS in its window on the sync
    // pipe until the parent writes the uid/gid maps and releases it — no extra memory or
    // unsafe is needed for the flag itself. CLONE_NEWNET (only alongside CLONE_NEWUSER)
    // likewise births the child into a NEW, EMPTY network namespace at the same syscall —
    // again just a flag bit, no extra memory/unsafe. The PARENT branch (rc>0) only returns the pid. The
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
        // performs ONLY async-signal-safe syscalls (the optional userns-rendezvous
        // blocking read/sigprocmask/close/fchdir/fexecve/write/_exit) on the PRE-BUILT
        // `plan` (argv/envp pointer arrays into parent-
        // owned CStrings, the scrub close-list, and the fds — all allocated by
        // ChildExecPlan::build BEFORE clone3). It indexes already-mapped copy-on-write
        // memory, performs NO heap allocation, takes NO lock, and DIVERGES (it either
        // fexecve-replaces the image or _exit(127)s after writing the errno) — so no
        // destructor runs and no unwinding crosses the fork. The optional `confinement`
        // ruleset was fully BUILT in the parent (every allocation + add_rule syscall);
        // the child only APPLIES it via the async-signal-safe `restrict_self`. This
        // call site is reached ONLY in the child branch, satisfying `run_child`'s
        // contract. The optional seccomp `program` was assembled ENTIRELY in the parent
        // (the bvisor seccomp model's compile()); the child only reads that slice.
        unsafe { run_child(plan, confinement, seccomp) }
    }
    // PARENT — return the child pid. `rc` is the pid (> 0). `confinement` (if any)
    // drops here, closing ONLY the parent's copy of the ruleset fd; the child holds
    // its own inherited copy, so its confinement is unaffected.
    let pid = libc::pid_t::try_from(rc).unwrap_or(-1);
    Ok(pid)
}

/// The CHILD branch body: the deterministic async-signal-safe sequence. Diverges —
/// it either `fexecve`s (image replaced) or `_exit`s. NEVER returns into Rust, so no
/// destructor runs and no unwinding crosses the fork. Marked `unsafe` because it
/// dereferences the pre-built raw pointer arrays and issues raw syscalls.
///
/// SAFETY: callable ONLY from the `rc == 0` child branch of [`clone3_child`], with a
/// `plan` whose `argv`/`envp`/`close_fds`/`sync_read_fd` were fully built in the parent,
/// an OPTIONAL `confinement` ruleset whose every allocation + `add_rule` syscall ran in
/// the parent, and an OPTIONAL `seccomp` BPF program assembled ENTIRELY in the parent. It
/// indexes only that already-allocated memory and calls only async-signal-safe syscalls —
/// the optional userns-rendezvous blocking `read` (raw `SYS_read` into a stack byte),
/// `restrict_self` (`prctl` + `landlock_restrict_self`), the STANDALONE
/// `prctl(PR_SET_NO_NEW_PRIVS)`, and `seccomp(SECCOMP_SET_MODE_FILTER, ..)` on the pre-built
/// BPF (a fixed stack `sock_fprog`).
unsafe fn run_child(
    plan: &ChildExecPlan,
    confinement: Option<RulesetCreated>,
    seccomp: Option<&BpfProgram>,
) -> ! {
    // 0. USER-NAMESPACE RENDEZVOUS (S8, async-signal-safe): if a sync-pipe read end was
    //    packed in, the child was born in a NEW userns (CLONE_NEWUSER) and is currently
    //    UNMAPPED (overflow uid). BLOCK on a 1-byte `read()` of the sync pipe until the
    //    parent has written uid_map / setgroups=deny / gid_map and writes the release
    //    byte — after which the child is uid 0 inside the userns. The raw `SYS_read`
    //    syscall is async-signal-safe and allocates nothing; the read target is a fixed
    //    stack byte. A read that returns <=0 (parent closed the pipe WITHOUT releasing
    //    ⇒ a fail-closed map-write failure, or EOF) means the rendezvous did not complete
    //    ⇒ the child reports + `_exit`s, so the target NEVER runs unmapped.
    if let Some(sync_fd) = plan.sync_read_fd {
        // First close the child's INHERITED copy of the sync WRITE end (raw SYS_close —
        // async-signal-safe). The child must NOT keep a write end open across the blocking
        // read: if it did, the parent's fail-closed close of ITS write copy would never
        // bring the pipe to EOF (the child's own copy keeps it open) and the read would
        // DEADLOCK. After this, the parent is the sole writer, so its release-byte arrives
        // and its fail-closed close produces a clean EOF.
        if let Some(write_fd) = plan.sync_write_fd {
            libc::syscall(libc::SYS_close, write_fd);
        }
        let mut byte: u8 = 0;
        let n = libc::syscall(
            libc::SYS_read,
            sync_fd,
            std::ptr::addr_of_mut!(byte).cast::<libc::c_void>(),
            1usize,
        );
        if n != 1 {
            child_fail(plan.error_fd);
        }
    }

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

    // 4. CONFINEMENT (the landlock-restrict step, backed by the child-window unsafe
    //    dispatch — ledger anchor `linux-launcher-child-window`): apply the
    //    parent-built landlock ruleset, AFTER the fd scrub and BEFORE fexecve, so the
    //    workload image runs already confined (fail-closed: any restrict_self error
    //    _exits before the target ever runs). `restrict_self` is itself a SAFE call —
    //    the async-signal-safe `prctl(PR_SET_NO_NEW_PRIVS)` + `landlock_restrict_self`
    //    pair on the inherited ruleset fd — with NO allocation and NO rule construction
    //    (all of that ran in the parent's `build_landlock_ruleset`).
    if let Some(ruleset) = confinement {
        if ruleset.restrict_self().is_err() {
            // The ruleset never installed; the kernel left errno set. Report + _exit so
            // the target NEVER runs unconfined. (On the Init/No/Dummy compat states
            // restrict_self returns Ok without enforcing — but we built it at
            // HardRequirement above the probed ABI floor, so a real enforce is reached
            // or create() already failed in the parent.)
            child_fail(plan.error_fd);
        }
    }

    // 4b. NO_NEW_PRIVS, STANDALONE (S10): set it explicitly here so the seccomp filter can
    //     install WITHOUT/BEFORE landlock and in the load-bearing order — NNP MUST precede
    //     an unprivileged seccomp filter (the kernel refuses SECCOMP_SET_MODE_FILTER
    //     otherwise). It is idempotent with landlock's own NNP. Run it ONLY when a seccomp
    //     filter is scheduled (a no-seccomp plan leaves the child window byte-for-byte
    //     unchanged); fail-closed on a prctl error so the target never runs without it.
    //     `set_no_new_privs` is the async-signal-safe basement wrapper (prctl, no alloc).
    if seccomp.is_some() && !set_no_new_privs() {
        child_fail(plan.error_fd);
    }

    // 4c. SECCOMP FILTER, LAST (S10): install the parent-built BPF denylist AFTER the fd
    //     scrub + landlock (so landlock's own syscalls already ran) and IMMEDIATELY before
    //     fexecve (the filter allows execve/execveat so the exec survives + write/exit_group
    //     so an error can still be reported). The program was assembled ENTIRELY in the
    //     parent; `install_seccomp_filter` only builds a fixed stack `sock_fprog` over the
    //     borrowed slice and issues the async-signal-safe `SYS_seccomp` syscall — NO
    //     allocation, NO lock. Fail-closed: a failed install _exits so the target NEVER runs
    //     un-filtered (e.g. a ChildSpawn::DenyNewTasks workload must not run able to fork).
    if let Some(program) = seccomp {
        if !install_seccomp_filter(program) {
            child_fail(plan.error_fd);
        }
    }

    // 5. Replace the image. exec rides the fd, never a path (no reopen race). On
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

/// Set `FD_CLOEXEC` on an inherited raw fd in the COORDINATOR (parent, single-threaded,
/// pre-clone3). Used on the landlock ruleset fd(s) so a successful workload `fexecve`
/// auto-closes them (no ruleset fd leaks into the workload); the fd stays open across
/// the child's `restrict_self` because CLOEXEC only acts at exec, not before. A failure
/// is ignored — the ruleset is still applied; at worst the fd would leak (the scrub
/// already closes everything else, and the workload cannot misuse a ruleset fd with
/// `NO_NEW_PRIVS` already set).
pub(crate) fn set_cloexec(fd: RawFd) {
    // SAFETY (LEDGER:linux-launcher-set-cloexec): a coordinator-side `fcntl` pair on an
    // inherited descriptor the launcher owns (a landlock ruleset fd it just created).
    // `F_GETFD` only READS the fd flags; `F_SETFD` only WRITES the CLOEXEC bit. The fd
    // is passed as a plain RawFd with no Rust value wrapping it, so there is no aliasing
    // and no double-close. No pointer is dereferenced and no raw memory is touched. A
    // failure (returned -1) is ignored — best-effort.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// Create the parent→child user-namespace RENDEZVOUS sync pipe in the COORDINATOR
/// (single-threaded, pre-clone3) and return `(read_end, write_end)` as raw fds, both
/// `O_CLOEXEC`. The READ end is packed into the [`ChildExecPlan`] (the child blocks on
/// it post-clone3, inside its new userns); the WRITE end stays with the parent, which
/// writes one byte to RELEASE the child AFTER it has written the uid/gid maps. Both are
/// CLOEXEC so a successful workload `fexecve` cannot leak the pipe — the child reads
/// from the read end BEFORE exec (CLOEXEC acts only at exec), and the parent closes its
/// write end explicitly once the child is released or fail-closed.
///
/// Returned as plain `RawFd`s (NOT owned handles) so the coordinator can place the read
/// end into the inherited-fd-numbered plan and best-effort-close each end with the same
/// raw discipline as the rest of the launcher's inherited fds.
///
/// # Errors
/// An `io::Error` carrying the `pipe2` errno on failure (the userns launch then refuses
/// fail-closed — no child is created).
pub(crate) fn make_sync_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds: [libc::c_int; 2] = [-1, -1];
    // SAFETY (LEDGER:linux-launcher-userns-sync-pipe): coordinator-side (parent,
    // single-threaded, pre-clone3). `pipe2` writes EXACTLY two fresh fd numbers into the
    // `fds` out-array (a fixed stack `[c_int; 2]`) and reads no other user memory; we
    // pass `O_CLOEXEC` so both ends are close-on-exec. On failure it returns -1 and sets
    // errno, which we surface as an io::Error (no fd is created). The two returned fds
    // are brand-new, exclusively owned by the launcher, and each is closed at most once
    // (the read end by the child scrub / CLOEXEC, the write end by the coordinator's
    // explicit close); they are passed as plain RawFds with no Rust value wrapping them,
    // so there is no aliasing and no double-close. No pointer beyond the out-array is
    // dereferenced.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

/// The launcher's effective uid/gid, observed in the COORDINATOR (parent, pre-clone3),
/// for building the userns uid/gid maps (`0 <euid> 1` / `0 <egid> 1`): the child uid 0
/// maps to exactly the unprivileged identity the launcher already runs as.
#[must_use]
pub(crate) fn effective_ids() -> (libc::uid_t, libc::gid_t) {
    // SAFETY (LEDGER:linux-launcher-effective-ids): coordinator-side pure observation.
    // `geteuid`/`getegid` take NO arguments, read NO user memory, dereference NO pointer,
    // create/close NO fd, and CANNOT fail (POSIX guarantees they always succeed). They
    // only return the calling process's effective uid/gid. Sound for any caller state.
    unsafe { (libc::geteuid(), libc::getegid()) }
}

/// The kernel-ABI `struct sock_fprog` (uapi `linux/filter.h`): a `{ len, filter }` pair
/// pointing at the BPF instruction stream `seccomp(SECCOMP_SET_MODE_FILTER, ..)` installs.
/// libc does not expose it and seccompiler keeps its own copy private, so the launcher
/// basement declares its own `#[repr(C)]` mirror (two plain fields — a `u16` count and a
/// pointer to the pre-built `sock_filter` slice). Built ON THE STACK in the child window
/// (no allocation) from the PARENT-built filter; the kernel `copy_from_user`s the program
/// during the syscall and leaves the memory untouched, so a borrowed pointer is sound.
#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const sock_filter,
}

/// Set `PR_SET_NO_NEW_PRIVS` STANDALONE in the CHILD window (async-signal-safe, S10).
///
/// EXTRACTED from landlock's `restrict_self` (which sets NNP internally) so the seccomp
/// filter can be installed WITHOUT/BEFORE landlock and in the right order: NNP must be set
/// before any unprivileged seccomp filter (the kernel refuses `SECCOMP_SET_MODE_FILTER`
/// from an unprivileged caller otherwise). `prctl` is async-signal-safe and allocates
/// nothing; it is idempotent (landlock setting it again later is harmless). Returns `true`
/// on success; a non-zero `prctl` return ⇒ `false` (the caller fails closed before the
/// filter install, so the target never runs without NNP).
///
#[must_use]
fn set_no_new_privs() -> bool {
    // SAFETY (LEDGER:linux-launcher-no-new-privs): child-window (async-signal-safe)
    // `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)`. The four trailing args are the documented
    // ignored-zero arguments for this prctl option; it reads NO user memory and
    // dereferences NO pointer — it only sets the calling thread's no_new_privs bit (so a
    // subsequent unprivileged seccomp filter is permitted, and no exec can gain privilege).
    // It allocates nothing and takes no lock, so it is sound in the post-clone3 child
    // window. A non-zero return (failure) is surfaced to the caller, which fails closed
    // BEFORE the filter install so the target never runs without NNP. Idempotent with
    // landlock's own NNP.
    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0 }
}

/// Install the PARENT-built seccomp BPF filter in the CHILD window via
/// `seccomp(SECCOMP_SET_MODE_FILTER, 0, &fprog)` (async-signal-safe, S10).
///
/// The `program` slice was assembled ENTIRELY in the parent (the bvisor seccomp model's
/// `compile()`); the child only READS it. A fixed `SockFprog` is built ON THE STACK
/// (no heap, no lock) pointing at that slice, and the raw `SYS_seccomp` syscall installs
/// it. The kernel `copy_from_user`s the program during the syscall, so the borrowed
/// pointer needs no ownership. PRECONDITION: `PR_SET_NO_NEW_PRIVS` is already set (see
/// [`set_no_new_privs`]) — call this LAST, after landlock, immediately before `fexecve`.
/// Returns `true` on a successful install; any non-zero return ⇒ `false` (the caller fails
/// closed so the target never runs without the filter).
///
#[must_use]
fn install_seccomp_filter(program: &[sock_filter]) -> bool {
    // An empty program would install a no-op filter — refuse it (the caller built a real
    // denylist; an empty stream means the parent compile silently produced nothing).
    if program.is_empty() {
        return false;
    }
    let Ok(len) = u16::try_from(program.len()) else {
        return false;
    };
    let fprog = SockFprog {
        len,
        filter: program.as_ptr(),
    };
    // SAFETY (LEDGER:linux-launcher-seccomp-install): child-window (async-signal-safe)
    // `seccomp(SECCOMP_SET_MODE_FILTER, 0, &fprog)`. `fprog` is a FIXED stack
    // `SockFprog { len, filter }` built HERE (no heap) over the PARENT-built `program`
    // slice (assembled entirely in the parent by the bvisor seccomp model's compile()); the
    // child only READS that already-mapped slice. The kernel `copy_from_user`s the BPF
    // program DURING the syscall and leaves the memory untouched, so the borrowed pointer
    // needs no ownership and is sound for the call's duration. `len` is the bounded
    // instruction count (try_from-guarded). NNP is already set (the caller ran
    // set_no_new_privs first), so the unprivileged install is permitted. It allocates
    // nothing, takes no lock, and runs LAST (after the scrub + landlock, immediately before
    // fexecve — the filter allows execve/execveat/write/exit_group). A non-zero return
    // (failure) is surfaced so the caller fails closed (the target never runs un-filtered).
    unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0usize,
            std::ptr::addr_of!(fprog).cast::<libc::c_void>(),
        ) == 0
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
