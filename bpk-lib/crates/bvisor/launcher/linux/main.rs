//! The single-threaded Linux confinement LAUNCHER binary (kernel plan §10.8).
//!
//! Topology (PERMANENT — the launcher NEVER self-execs):
//! ```text
//! coordinator (this process)  →  workload child (via clone3)  →  exec target
//! ```
//! The COORDINATOR (this `main.rs`, fully SAFE) validates the host-supplied
//! [`LinuxLaunchPlanV1`], decides whether the launch may proceed, and — only if it
//! may — creates ONE child via raw `clone3` (in the [`sys`] basement), placing it INTO
//! the prepared cgroup leaf at birth when a `CgroupDir` slot is present
//! (`CLONE_INTO_CGROUP`). The CHILD, in its async-signal-safe window, scrubs ambient
//! authority (closes the non-allowlisted fds), applies the parent-built landlock ruleset
//! (`restrict_self`) when a confinement action was scheduled, then `fexecve`s the target.
//! The coordinator holds the `ReadyToExec` gate; the child runs ONLY because the
//! coordinator already determined the launch ready.
//!
//! ## No-fd-escape (G6) is enforced by the CHILD SCRUB, not a coordinator refusal
//! The launcher is spawned by FORKING a host process, so it inherits whatever fds the
//! host had open at fork — including a sibling thread's transient non-CLOEXEC fd it
//! never declared. The coordinator does NOT enumerate its own fd table and ABORT on an
//! undeclared inherited fd. Instead the CHILD scrub closes every non-allowlisted fd
//! (raw `SYS_close`) BEFORE `fexecve`, so an unexpected inherited fd is defensively
//! CLOSED in the child and is never visible to the workload. This is the no-fd-escape
//! enforcement: an inherited fd that should not reach the workload is SCRUBBED, never a
//! launch-abort. It is both strictly stronger than a refusal (the scrub PROVES the fd
//! is gone — `EBADF` in the workload — rather than merely declining to launch) and
//! more production-honest (a host that leaks an fd does not break the launch; the
//! workload still cannot see the fd). The coordinator's only handle check is the
//! deterministic declared-slot `fstat` SHAPE verification (kind/writability mismatch ⇒
//! `HandleMismatch`).
//!
//! ## What this launcher serves (and what it deliberately does NOT)
//! Lowering primitives: `linux.ambient.scrub.v1` (AmbientAuthority — the child's fd
//! scrub, mandatory), `linux.landlock.apply.v1` (Confinement — the parent builds the
//! ruleset, the child `restrict_self`s it; optional), and `linux.exec.v1` (the launch,
//! `fexecve`). Resource confinement rides the descriptor table, NOT a lowering action: a
//! `DescriptorRole::CgroupDir` slot ⇒ the child is born into that leaf via
//! `CLONE_INTO_CGROUP`. NOT YET served (a later step) — seccomp and namespace primitives;
//! any such scheduled action ⇒ `SetupRefused{MissingPrimitive}` BEFORE any child is
//! created (the launcher never advertises a confinement it cannot install).
//!
//! ## `IdentityVerified` is the SCHEDULE-DIGEST binding ONLY (not over-claimed)
//! The coordinator binds `observed_schedule_digest = blake3(canonical(body.lowering))`
//! to the body's `h_l`. That is the ENTIRETY of what `IdentityVerified` asserts here:
//! the wire lowering projection it received matches the digest the plan was sealed
//! with. The FULL execution-closure identity (reconstructing the real
//! `LoweringSchedule` through the admission membrane, and the profile-drift check vs
//! `h_p`) is a LATER step (#75) and is NOT claimed by this skeleton.
//!
//! ## Honesty of the phase results
//! Each phase resolves per `phase_resolution_consistent`: `NotRequired` ⟺ 0 scheduled
//! actions, `Applied` ⟺ observed == scheduled. For a scrub+exec plan, Identity /
//! Visibility / Confinement have ZERO scheduled actions ⇒ each `NotRequired`;
//! AmbientAuthority has the scrub the child WILL run ⇒ `Applied`. When a
//! `linux.landlock.apply.v1` action IS scheduled, Confinement resolves `Applied` ONLY
//! when the ruleset was built and the child will `restrict_self` it — so
//! `confinement_installed` is true IFF a confinement action was scheduled AND applied,
//! never an install the launcher did not perform.
//!
//! ## Safety posture
//! This file is SAFE Rust (the 4a runtime-shape gate FAILS the build on any `unsafe`
//! outside the basement). All `unsafe` lives in [`sys`] (`launcher/linux/sys.rs`),
//! each block ledger-anchored. No thread is ever created (the single-thread gate
//! bans `thread::spawn`/`.spawn()`); no async, no tokio, no network.

// The launcher binary's transcript travels over the CONTROL FD, never stdout/stderr
// (the workspace lints deny `print_stdout`/`print_stderr`), so this module writes
// bytes through `File` handles, not the print macros.

#[cfg(target_os = "linux")]
mod sys;

#[cfg(target_os = "linux")]
mod imp;

/// Linux entry point: run the coordinator; map its typed result to an exit code.
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    imp::run()
}

/// Non-Linux stub: the launcher is meaningful only on Linux. Emit a typed error to
/// the standard error handle (via `Write`, not the denied `eprintln!`) and exit
/// non-zero, so the bin still COMPILES cross-platform.
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "bvisor-linux-launcher: unsupported on this platform — the confinement launcher requires Linux"
    );
    std::process::ExitCode::from(2)
}
