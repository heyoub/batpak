//! The single-threaded Linux confinement LAUNCHER binary (kernel plan §10.8) —
//! NO-CONFINEMENT honest skeleton.
//!
//! Topology (PERMANENT — the launcher NEVER self-execs):
//! ```text
//! coordinator (this process)  →  workload child (via clone3)  →  exec target
//! ```
//! The COORDINATOR (this `main.rs`, fully SAFE) validates the host-supplied
//! [`LinuxLaunchPlanV1`], decides whether the launch may proceed, and — only if it
//! may — creates ONE child via raw `clone3` (in the [`sys`] basement). The CHILD
//! scrubs ambient authority (closes the non-allowlisted fds) and `fexecve`s the
//! target. The coordinator holds the `ReadyToExec` gate; the child runs ONLY because
//! the coordinator already determined the launch ready.
//!
//! ## What this skeleton implements (and what it deliberately does NOT)
//! EXACTLY two lowering primitives — `linux.ambient.scrub.v1` (the AmbientAuthority
//! phase, the child's fd scrub) and `linux.exec.v1` (the launch, `fexecve`). It
//! advertises NO confinement profile. Any OTHER scheduled action (any namespace /
//! landlock / seccomp / cgroup primitive) ⇒ `SetupRefused{MissingPrimitive}` BEFORE
//! any child is created.
//!
//! ## `IdentityVerified` is the SCHEDULE-DIGEST binding ONLY (not over-claimed)
//! The coordinator binds `observed_schedule_digest = blake3(canonical(body.lowering))`
//! to the body's `h_l`. That is the ENTIRETY of what `IdentityVerified` asserts here:
//! the wire lowering projection it received matches the digest the plan was sealed
//! with. The FULL execution-closure identity (reconstructing the real
//! `LoweringSchedule` through the admission membrane, and the profile-drift check vs
//! `h_p`) is a LATER step (#75) and is NOT claimed by this skeleton.
//!
//! ## Honesty of the phase results (exec-only plan)
//! For a scrub+exec plan: Identity / Visibility / Confinement have ZERO scheduled
//! actions ⇒ each resolves [`PhaseResult::NotRequired`] (verified via
//! `phase_resolution_consistent`). AmbientAuthority has the scrub action and the
//! child WILL run it ⇒ [`PhaseResult::Applied`]. `confinement_installed(0, …)` is
//! therefore `false` — the skeleton never claims an install it did not perform.
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
