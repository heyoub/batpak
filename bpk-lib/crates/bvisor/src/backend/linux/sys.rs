//! The SANCTIONED unsafe basement for the Linux backend (kernel plan §10.8).
//!
//! This file is the ONE quarantine where the Linux backend's raw-syscall `unsafe`
//! is permitted to live: `unshare` / `clone3` / `pivot_root` / `mount` /
//! `prctl(NO_NEW_PRIVS)` / `pipe2` / `dup2` / `pidfd_open` / `waitid`, plus the
//! landlock ABI floor logic and the cgroup-v2 `cgroup.kill` file writes. The safe
//! orchestration in [`super`] (`mod.rs`) NEVER contains `unsafe` — it calls down
//! into this basement through narrow, documented wrappers.
//!
//! GATING CONTRACT (two interlocking gates, both fail-closed):
//! 1. The architecture lint (`syncbat_boundary::checks_runtime_shape`) exempts
//!    this `sys.rs` from the blanket sync-first/safe-Rust ban, BUT exempts
//!    NOTHING else under `backend/` — `mod.rs` stays covered.
//! 2. The unsafe ledger (`integrity unsafe-ledger`, folded into `structural-check`)
//!    requires EVERY `unsafe` block here to have a matching entry in
//!    `traceability/unsafe_ledger.yaml` (file, line, syscall, safety-invariant,
//!    requirement-kind) and fails closed on any unmatched block OR stale entry.
//!
//! STEP (a) scaffolding: this basement is INTENTIONALLY EMPTY — there is no real
//! syscall code yet (it lands in step (b)). The ledger is therefore empty too;
//! the gate is wired so step (b)'s first `unsafe` block is FORCED to register.
