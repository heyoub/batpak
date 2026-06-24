//! The SANCTIONED unsafe basement for the Linux backend (kernel plan §10.8).
//!
//! This file is the ONE quarantine where the Linux backend's raw-syscall `unsafe`
//! is permitted to live. STEP (b) — REAL filesystem confinement — adds exactly
//! two `unsafe` blocks here, both registered in `traceability/unsafe_ledger.yaml`:
//!
//!   1. `probe_landlock_abi` — the raw `landlock_create_ruleset(NULL, 0,
//!      LANDLOCK_CREATE_RULESET_VERSION)` version query. The landlock crate caps
//!      the ABI it MODELS at its own latest known version, so the honest live
//!      kernel ABI integer is read straight from the kernel here.
//!   2. `spawn_confined` — `std::os::unix::process::CommandExt::pre_exec`, which
//!      runs a closure in the CHILD after `fork` and before `exec`. The ruleset is
//!      BUILT in the parent (all allocation + path-fd opens + add_rule syscalls);
//!      the child closure only APPLIES it via `restrict_self` (an allocation-free,
//!      async-signal-safe pair of syscalls), so confinement is in force the instant
//!      the workload image runs.
//!
//! The safe orchestration in [`super::backend_impl`] NEVER contains `unsafe` — it
//! calls down into this basement through the two narrow wrappers below. The
//! landlock RULESET construction itself is SAFE (the `landlock` crate is pure
//! safe Rust); only the `pre_exec` registration and the raw ABI probe are unsafe.
//!
//! GATING CONTRACT (two interlocking fail-closed gates):
//! 1. The architecture lint (`syncbat_boundary::checks_runtime_shape`) exempts
//!    this `sys.rs` from the blanket sync-first/safe-Rust ban, BUT exempts
//!    NOTHING else under `backend/` — `mod.rs`/`backend_impl.rs` stay covered.
//! 2. The unsafe ledger (`integrity unsafe-ledger`, folded into `structural-check`)
//!    requires EVERY `unsafe` block here to have a matching ledger entry and fails
//!    closed on any unmatched block OR stale entry.

use landlock::{
    Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreated, RulesetCreatedAttr, ABI,
};
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output};

/// `LANDLOCK_CREATE_RULESET_VERSION` (uapi `linux/landlock.h`): asking
/// `landlock_create_ruleset` for the supported ABI version instead of creating a
/// ruleset. Stable kernel ABI constant.
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;

/// One declared confinement root: a path and whether write access is granted.
/// Read+execute is ALWAYS granted under a root (a workload must be able to read
/// the files it was scoped to); `writable` additionally grants the write/create
/// access set. Inert string-free — the path is opened here in the basement.
#[derive(Clone, Debug)]
pub(crate) struct ConfinedRoot {
    /// The declared root path, as a portable string.
    pub(crate) path: String,
    /// Whether the workload may WRITE beneath this root (else read-only).
    pub(crate) writable: bool,
}

/// Probe the LIVE landlock ABI integer straight from the kernel.
///
/// Returns the supported ABI version (`>= 1`), or `0` when landlock is
/// unavailable (old kernel / disabled LSM) — the caller floors `Filesystem` to
/// `Unsupported` below the required ABI, so `plan()` fails closed.
///
/// SAFETY (LEDGER:linux-landlock-abi-probe): `landlock_create_ruleset` is invoked in its
/// documented VERSION-QUERY form — `attr = NULL`, `size = 0`, `flags =
/// LANDLOCK_CREATE_RULESET_VERSION`. In this form the kernel reads NO user
/// memory (the NULL/0 pair is exactly what the version query requires) and only
/// returns the supported ABI as a non-negative `int`, or `-1` with `errno` set.
/// No fd is created, nothing is mutated, no pointer is dereferenced. The call is
/// therefore sound for any caller state.
#[must_use]
pub(crate) fn probe_landlock_abi() -> i64 {
    // SAFETY (LEDGER:linux-landlock-abi-probe): documented version-query form
    // (NULL attr, 0 size); reads no user memory, creates no fd, mutates nothing.
    // See the function-level note.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    // A negative return means landlock is unavailable; report ABI 0 (unsupported)
    // so the safe orchestration fails closed rather than guessing.
    if raw < 0 {
        0
    } else {
        raw
    }
}

/// Build the landlock ruleset restricting FS access to exactly `roots`, at
/// `compat` strictness, IN THE PARENT (before any fork). SAFE: the `landlock`
/// crate is pure safe Rust. ALL heap allocation, the path-fd opens, and the
/// `landlock_create_ruleset`/`landlock_add_rule` syscalls happen HERE — so the
/// post-fork child closure (which only calls `restrict_self`) does not allocate.
/// Building the ruleset does NOT confine the parent: only `restrict_self` (called
/// later, in the child) applies it to a process. A root whose path cannot be
/// opened is a hard error (fail closed — never silently widen the sandbox).
fn build_ruleset(roots: &[ConfinedRoot], compat: CompatLevel) -> io::Result<RulesetCreated> {
    // Handle the full access set the running kernel's modeled ABI exposes, then
    // grant back ONLY what each declared root allows. Anything not granted (every
    // path outside the roots) is therefore denied.
    let abi = ABI::V3;
    let mut ruleset = Ruleset::default()
        .set_compatibility(compat)
        .handle_access(AccessFs::from_all(abi))
        .map_err(to_io)?
        .create()
        .map_err(to_io)?;

    for root in roots {
        let fd = PathFd::new(&root.path).map_err(to_io)?;
        let access = if root.writable {
            AccessFs::from_all(abi)
        } else {
            AccessFs::from_read(abi)
        };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, access))
            .map_err(to_io)?;
    }

    Ok(ruleset)
}

/// Render a landlock error as an `io::Error` so the `pre_exec` closure can return
/// it (the kernel then refuses the exec and the parent observes a spawn failure).
fn to_io(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("landlock: {error}"))
}

/// Launch `exe`+`args` CONFINED to exactly `roots` by landlock, capturing
/// stdout/stderr, and return the captured [`Output`].
///
/// The ruleset is installed in the CHILD via `pre_exec` so confinement is in
/// force before the workload image runs. `compat` selects the strictness:
/// [`CompatLevel::HardRequirement`] makes a kernel that cannot honor the ruleset
/// fail the spawn (fail closed); the caller passes it once the ABI floor is met.
///
/// SAFETY (LEDGER:linux-landlock-pre-exec-apply): `pre_exec` runs the closure in the forked child
/// between `fork` and `exec`, so it must be async-signal-safe. The ruleset —
/// including every heap allocation, path-fd open, and `landlock_add_rule` syscall
/// — is built BEFORE the fork by [`build_ruleset`] in the parent; the created
/// ruleset fd is inherited by the child across `fork`. The post-fork closure
/// therefore performs NO heap allocation and takes no lock: it calls only
/// [`RulesetCreated::restrict_self`], i.e. `prctl(PR_SET_NO_NEW_PRIVS)` plus the
/// `landlock_restrict_self` syscall on the inherited fd — both async-signal-safe
/// syscalls. The closure captures only the owned `Option<RulesetCreated>` (an
/// owned fd), moves it out via `.take()` on its single invocation, and on any
/// error returns an `io::Error` that `pre_exec` propagates as a failed spawn —
/// the workload never runs unconfined.
pub(crate) fn spawn_confined(
    exe: &str,
    args: &[String],
    roots: &[ConfinedRoot],
    compat: CompatLevel,
) -> io::Result<Output> {
    // Build the ruleset (all allocation + path-fd opens + add_rule syscalls) in
    // the PARENT, before the fork. The created ruleset fd is inherited by the
    // child; the child only APPLIES it via restrict_self.
    let ruleset = build_ruleset(roots, compat)?;
    let mut pending = Some(ruleset);

    let mut command = Command::new(exe);
    command.args(args);

    // SAFETY (LEDGER:linux-landlock-pre-exec-apply): see the function-level note —
    // the post-fork closure only applies the parent-built ruleset (no allocation),
    // and fails the spawn rather than running the workload unconfined.
    unsafe {
        command.pre_exec(move || {
            pending
                .take()
                .ok_or_else(|| io::Error::other("landlock: ruleset already consumed"))?
                .restrict_self()
                .map_err(to_io)?;
            Ok(())
        });
    }

    command.output()
}
