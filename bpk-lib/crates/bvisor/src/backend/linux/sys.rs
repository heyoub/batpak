//! The SANCTIONED unsafe basement for the Linux backend (kernel plan §10.8).
//!
//! This file is the ONE quarantine where the Linux backend's raw-syscall `unsafe`
//! is permitted to live. Every `unsafe` block below is registered in
//! `traceability/unsafe_ledger.yaml`:
//!
//! STEP (b) — REAL filesystem confinement:
//!   1. `probe_landlock_abi` — the raw `landlock_create_ruleset(NULL, 0,
//!      LANDLOCK_CREATE_RULESET_VERSION)` version query. The landlock crate caps
//!      the ABI it MODELS at its own latest known version, so the honest live
//!      kernel ABI integer is read straight from the kernel here. This is the
//!      backend's ONLY remaining filesystem-confinement `unsafe`: the live ABI
//!      probe that backs `profile()`'s `Filesystem=Enforced` cell. The landlock
//!      ENFORCEMENT (ruleset build + `restrict_self`) now lives in the LAUNCHER's
//!      child window (`launcher/linux/sys.rs`); the backend's old self-spawn
//!      `pre_exec` confinement (`spawn_confined`) was removed in the backend→launcher
//!      rewire (step 7b) so there is exactly ONE confinement path.
//!
//! STEP 7a — the HOST-SIDE launcher harness basement (the launcher-rewire foundation,
//! consumed by the SAFE [`super::launch`] module):
//!   3. `seal_plan_memfd` — `memfd_create(MFD_CLOEXEC|MFD_ALLOW_SEALING)` then
//!      `fcntl(F_ADD_SEALS, …)` to produce a tamper-proof, read-only plan transport
//!      the launcher reads (anchors `linux-backend-memfd-seal` + `-add-seals`).
//!   4. `spawn_launcher_with_fds` — `Command::spawn` of the launcher bin with a
//!      post-fork `pre_exec` that ONLY `dup2`/`fcntl`s a PRE-BUILT fd map onto fixed
//!      target numbers (async-signal-safe: no allocation in the closure); the source
//!      fds are relocated HIGH first by `relocate_high` (anchors
//!      `linux-backend-launcher-relocate` + `-pre-exec`).
//!
//! The safe orchestration in [`super::backend_impl`] and [`super::launch`] NEVER
//! contains `unsafe` — it calls down into this basement through the narrow wrappers
//! below. The landlock RULESET construction itself is SAFE (the `landlock` crate is
//! pure safe Rust); only the `pre_exec`/`memfd`/relocate raw calls are unsafe.
//!
//! GATING CONTRACT (two interlocking fail-closed gates):
//! 1. The architecture lint (`syncbat_boundary::checks_runtime_shape`) exempts
//!    this `sys.rs` from the blanket sync-first/safe-Rust ban, BUT exempts
//!    NOTHING else under `backend/` — `mod.rs`/`backend_impl.rs` stay covered.
//! 2. The unsafe ledger (`integrity unsafe-ledger`, folded into `structural-check`)
//!    requires EVERY `unsafe` block here to have a matching ledger entry and fails
//!    closed on any unmatched block OR stale entry.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

/// `LANDLOCK_CREATE_RULESET_VERSION` (uapi `linux/landlock.h`): asking
/// `landlock_create_ruleset` for the supported ABI version instead of creating a
/// ruleset. Stable kernel ABI constant.
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;

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

// ─────────────────────────────────────────────────────────────────────────────
// HOST-SIDE LAUNCHER HARNESS BASEMENT (step 7a)
//
// Two new raw-syscall surfaces the SAFE harness (`super::launch`) calls down into:
//   1. `seal_plan_memfd` — materialise the encoded launcher plan into a sealed,
//      read-only `memfd` (tamper-proof plan transport). Anchor
//      `LEDGER:linux-backend-memfd-seal`.
//   2. `spawn_launcher_with_fds` — `Command::spawn` the launcher bin with a
//      post-fork `pre_exec` that ONLY `dup2`/`fcntl`s a PRE-BUILT fd map onto the
//      fixed target fd numbers (async-signal-safe: no allocation in the closure).
//      Anchor `LEDGER:linux-backend-launcher-pre-exec`.
//
// `Command::spawn` is permitted HERE: the runtime-shape no-`.spawn()` single-thread
// gate scopes to `crates/bvisor/launcher/` only (the launcher itself uses clone3),
// NOT to this backend basement. This harness is the host coordinator the launcher
// is spawned FROM.
// ─────────────────────────────────────────────────────────────────────────────

/// `MFD_CLOEXEC | MFD_ALLOW_SEALING` for `memfd_create` — a close-on-exec anonymous
/// file that can later be sealed. Stable kernel ABI flags (`linux/memfd.h`).
const MFD_FLAGS: libc::c_uint = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;

/// The full seal set the harness applies once the plan bytes are written:
/// `F_SEAL_SEAL` (no further seals), `F_SEAL_SHRINK`, `F_SEAL_GROW`, and
/// `F_SEAL_WRITE` (the contents are now immutable). After this the launcher's
/// `parse_and_verify` digest check is over BYTES THAT CANNOT CHANGE.
const PLAN_SEALS: libc::c_int =
    libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;

/// Seal `bytes` (the canonically-encoded, framed [`super::protocol::LinuxLaunchPlanV1`])
/// into a read-only `memfd` and return the owned, rewound, fully-sealed descriptor —
/// the tamper-proof plan transport the launcher reads to EOF.
///
/// The returned fd is positioned at offset 0 (the launcher reads from the start) and
/// carries `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`, so its bytes can
/// no longer change AFTER this call — the launcher's envelope digest check is then over
/// immutable content (sealing stops post-write tampering; `parse_and_verify` already
/// stops a forged digest).
///
/// # Errors
/// Any `io::Error` from `memfd_create`, the write, the seek, or `F_ADD_SEALS`. Fails
/// closed — a partial/unsealed memfd is never returned.
///
/// SAFETY (LEDGER:linux-backend-memfd-seal): `memfd_create` is invoked with a valid
/// NUL-terminated name pointer and the documented `MFD_CLOEXEC | MFD_ALLOW_SEALING`
/// flags; it creates a fresh anonymous fd (returned, or -1/errno) and reads no caller
/// memory beyond the name string. The returned fd is immediately wrapped in an
/// `OwnedFd` (single owner, closed on drop, no double-close). The subsequent write /
/// seek / `F_ADD_SEALS` run through the OWNED fd via safe `std::fs::File` + a single
/// `fcntl(F_ADD_SEALS, PLAN_SEALS)` that only sets seal bits and dereferences no
/// pointer. `F_ADD_SEALS` cannot succeed while any writable mapping/handle other than
/// this one exists; here only the just-written owned handle exists, so the seal is
/// total. Sound for any caller state.
pub(crate) fn seal_plan_memfd(bytes: &[u8]) -> io::Result<OwnedFd> {
    use std::io::{Seek, SeekFrom, Write};

    let name = c"bvisor-linux-launch-plan";
    // SAFETY (LEDGER:linux-backend-memfd-seal): see the function-level note — a fresh
    // anonymous fd from `memfd_create` with a valid NUL-terminated name + the documented
    // `MFD_CLOEXEC | MFD_ALLOW_SEALING` flags; on success it is immediately adopted as a
    // single-owner `OwnedFd` (closed on drop, no double-close, no aliasing). The kernel
    // reads no caller memory beyond the name string and dereferences nothing else.
    let owned = unsafe {
        let fd = libc::memfd_create(name.as_ptr(), MFD_FLAGS);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedFd::from_raw_fd(fd)
    };

    // Write the plan through the owned fd, then rewind for the launcher's reader. The
    // File is built from a CLONE of the owned fd so dropping it does NOT close the fd we
    // return (we keep `owned`; the clone closes its own copy on drop).
    let mut file = std::fs::File::from(owned.try_clone()?);
    file.write_all(bytes)?;
    file.flush()?;
    file.seek(SeekFrom::Start(0))?;
    drop(file);

    // Seal the contents immutable. After this the bytes cannot be written/grown/shrunk
    // and no further seal can be added.
    // SAFETY (LEDGER:linux-backend-memfd-add-seals): a single `fcntl(F_ADD_SEALS,
    // PLAN_SEALS)` on the OWNED memfd — it only sets seal bits on this fd and
    // dereferences no pointer; -1/errno on failure (surfaced below). `F_ADD_SEALS`
    // cannot succeed while a writable handle other than this owned one exists; the
    // write-handle clone was already dropped, so the seal is total. The plan bytes are
    // fully written already, so sealing them write/grow/shrink-proof is the tamper-stop.
    let rc = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_ADD_SEALS, PLAN_SEALS) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(owned)
}

/// One inherited fd the harness hands the launcher: the parent-owned source fd, the
/// fixed `target` fd NUMBER the launcher will see (== the descriptor-table slot index),
/// and whether the target keeps `FD_CLOEXEC`. The error-pipe WRITE end keeps CLOEXEC (so
/// a successful workload `fexecve` auto-closes it → the launcher's read end sees EOF);
/// every other inherited fd clears it (so the launcher inherits the fd across its own
/// `execve`).
pub(crate) struct LaunchFd {
    /// The parent-owned source fd to duplicate FROM (relocated HIGH internally).
    pub(crate) src: OwnedFd,
    /// The fixed fd NUMBER the launcher will see (== the descriptor-table slot index).
    pub(crate) target: RawFd,
    /// `true` ⇒ leave `FD_CLOEXEC` SET on the target (the error-pipe write end);
    /// `false` ⇒ clear it so the launcher inherits the fd across its own `execve`.
    pub(crate) keep_cloexec: bool,
}

/// One resolved placement the launcher's `pre_exec` performs: `dup2` `src` onto the fixed
/// `target`, then toggle the target's `FD_CLOEXEC`. Plain `RawFd`/`bool` — the closure
/// indexes a pre-built slice of these, so the post-fork `dup2`/`fcntl` sequence allocates
/// nothing.
#[derive(Clone, Copy)]
struct FdPlacement {
    src: RawFd,
    target: RawFd,
    keep_cloexec: bool,
}

/// Relocate a parent-owned fd to a HIGH number (`>= FD_RELOCATE_BASE`) via
/// `F_DUPFD_CLOEXEC`, returning the relocated `OwnedFd` and consuming the original. This
/// keeps every dup-FROM source ABOVE every fixed dup-TO target, so the `pre_exec` `dup2`
/// sequence can never clobber a not-yet-consumed source mid-sequence (the exact hazard
/// the launcher tests document). CLOEXEC on the high copy is fine — the `pre_exec` `dup2`
/// onto the final fixed fd clears CLOEXEC there (or re-sets it for the error-write end).
///
/// SAFETY (LEDGER:linux-backend-launcher-relocate): `F_DUPFD_CLOEXEC` returns a fresh,
/// exclusively-owned fd `>= FD_RELOCATE_BASE` (or -1/errno) duplicated from the borrowed
/// `fd`; on success it is adopted ONCE as an `OwnedFd` (no aliasing, no double-close), and
/// the low original is dropped (closed) so only the high copy survives. No pointer is
/// dereferenced; this runs in the single-threaded pre-spawn parent.
const FD_RELOCATE_BASE: RawFd = 100;
fn relocate_high(fd: OwnedFd) -> io::Result<OwnedFd> {
    // SAFETY (LEDGER:linux-backend-launcher-relocate): see the note above — a fresh owned
    // fd from `F_DUPFD_CLOEXEC`, adopted once; the borrowed original is dropped after.
    let relocated = unsafe {
        let new = libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, FD_RELOCATE_BASE);
        if new < FD_RELOCATE_BASE {
            return Err(io::Error::last_os_error());
        }
        OwnedFd::from_raw_fd(new)
    };
    drop(fd);
    Ok(relocated)
}

/// Spawn the launcher binary `launcher_path` with the prepared inherited fds placed at
/// their fixed target numbers, returning the live [`Child`]. `env` is the explicit
/// `(name, value)` fd-number environment the launcher reads (`BVISOR_*_FD`); the
/// launcher's process environment is otherwise CLEARED. `placements` lists every fd the
/// child's `pre_exec` must `dup2` into place; the host has already RELOCATED every source
/// HIGH so the fixed targets cannot collide with a not-yet-consumed source.
///
/// The `placements` and the relocated source fds are all built in the PARENT before
/// spawn; the `pre_exec` closure captures them by move and only `dup2`/`fcntl`s — it
/// allocates nothing, so it is async-signal-safe even though the host is multithreaded.
/// The owned source fds are dropped in the PARENT after spawn (the child holds its own
/// dup2'd copies at the fixed numbers).
///
/// # Errors
/// Any `io::Error` from the relocate or from `Command::spawn`.
///
/// SAFETY (LEDGER:linux-backend-launcher-pre-exec): the post-fork `pre_exec` closure runs
/// in the forked child between `fork` and `exec` in a (formerly) multithreaded address
/// space, so it must be async-signal-safe. EVERY allocation — the relocated source fds,
/// the `placements` Vec, the env — happens BEFORE the fork in the single call below; the
/// closure captures the owned `placements` Vec + the raw error-write target by move and
/// performs ONLY `dup2` (place each source onto its fixed target) and `fcntl(F_GETFD/
/// F_SETFD)` (toggle the target's CLOEXEC bit) by INDEXING the already-allocated Vec
/// (copy-on-write read — no allocator), returning the OS errno on a `dup2` failure so the
/// spawn fails closed (the launcher never runs with a half-wired fd table). It performs
/// NO heap allocation and takes NO lock. The source fds are owned by the parent and stay
/// valid until after `spawn` returns (dropped by the caller post-spawn); each `dup2`
/// duplicates into the child's OWN fd table, so there is no cross-process aliasing.
pub(crate) fn spawn_launcher_with_fds(
    launcher_path: &std::path::Path,
    env: &[(&str, String)],
    fds: Vec<LaunchFd>,
) -> io::Result<(Child, Vec<OwnedFd>)> {
    // Relocate EVERY owned source HIGH so the fixed dup-TO targets can never collide with
    // a not-yet-consumed dup-FROM source during the pre_exec sequence, and build the flat
    // placement plan in lockstep. ALL of this is pre-fork allocation (fine); the closure
    // only reads the resulting Vec.
    let mut relocated: Vec<OwnedFd> = Vec::with_capacity(fds.len());
    let mut plan: Vec<FdPlacement> = Vec::with_capacity(fds.len());
    for f in fds {
        let high = relocate_high(f.src)?;
        plan.push(FdPlacement {
            src: high.as_raw_fd(),
            target: f.target,
            keep_cloexec: f.keep_cloexec,
        });
        relocated.push(high);
    }

    let mut command = Command::new(launcher_path);
    command.env_clear();
    for (name, value) in env {
        command.env(name, value);
    }

    // Pipe the launcher's stdout AND stderr so the HOST captures them. The launcher's
    // clone3 child inherits the launcher's fd 0/1/2 (the scrub allowlists stdio), and
    // the launcher is stdio-silent on every workload-running path, so these pipes carry
    // exactly the WORKLOAD's output — the honest backing for `CaptureStreams=Enforced`
    // (the backend→launcher cutover's restored stream capture). The pre_exec `dup2`s
    // only the HIGH channel/authority targets, never fd 0/1/2, so this `Stdio::piped`
    // wiring of the launcher's own stdio is left intact for the child to inherit.
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    // SAFETY (LEDGER:linux-backend-launcher-pre-exec): see the function-level note — this
    // is the ONLY `unsafe` block of the spawn flow; it registers the post-fork closure and
    // (lexically inside it) calls the `unsafe fn apply_fd_placements`, which `dup2`/`fcntl`s
    // the pre-built `plan` (no allocation, async-signal-safe) and fails the spawn (returns
    // the errno) rather than running a half-wired launcher. EVERY allocation (the relocated
    // source fds, the `plan` Vec, the env) happened BEFORE the fork; the closure captures
    // the owned `plan` by move and the child branch only INDEXES it (copy-on-write read).
    unsafe {
        command.pre_exec(move || apply_fd_placements(&plan));
    }

    let child = command.spawn()?;
    Ok((child, relocated))
}

/// Apply each [`FdPlacement`] in the forked child's async-signal-safe window: `dup2` the
/// relocated source onto its fixed target, then toggle the target's `FD_CLOEXEC`. Reads
/// only the borrowed pre-built `plan` slice — NO allocation, NO lock — so it is
/// async-signal-safe. Returns the OS errno on a `dup2` failure so `pre_exec` fails the
/// spawn closed (the launcher never runs with a half-wired fd table).
///
/// # Safety
/// Callable ONLY from the post-fork `pre_exec` window of [`spawn_launcher_with_fds`]
/// (its single call site, lexically inside that `unsafe` block, ledger anchor
/// `linux-backend-launcher-pre-exec`). It indexes the already-allocated `plan` slice
/// (copy-on-write read — no allocator) and issues ONLY async-signal-safe syscalls: `dup2`
/// (place each relocated source onto its fixed target fd number) and `fcntl(F_GETFD/
/// F_SETFD)` (toggle the target's CLOEXEC bit). It performs NO heap allocation, takes NO
/// lock, and dereferences no raw pointer beyond the slice it reads. Each `dup2` duplicates
/// into the CHILD's own fd table, so there is no cross-process aliasing; the parent-owned
/// source fds stay valid until after the parent's `spawn`.
unsafe fn apply_fd_placements(plan: &[FdPlacement]) -> io::Result<()> {
    let mut i = 0usize;
    while i < plan.len() {
        let p = plan[i];
        if libc::dup2(p.src, p.target) < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = libc::fcntl(p.target, libc::F_GETFD);
        if flags >= 0 {
            let next = cloexec_bits(flags, p.keep_cloexec);
            let _ = libc::fcntl(p.target, libc::F_SETFD, next);
        }
        i += 1;
    }
    Ok(())
}

/// The new fd-flags value: SET `FD_CLOEXEC` when `keep` (the error-pipe write end), else
/// CLEAR it (every other inherited fd). Pure integer arithmetic — async-signal-safe.
fn cloexec_bits(flags: libc::c_int, keep: bool) -> libc::c_int {
    if keep {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    }
}
