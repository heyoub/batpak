//! The SANCTIONED unsafe basement for the Windows backend (kernel plan §10.8).
//!
//! This file is the ONE quarantine where the Windows backend's `windows-sys` FFI
//! `unsafe` is permitted to live: `NtCreateLowBoxToken`, AppContainer capability
//! SIDs, `CreateJobObject` / `AssignProcessToJobObject` / `TerminateJobObject`
//! (atomic Kill), DACL construction, redirected handles, and WFP filters. The
//! safe orchestration in [`super`] (`mod.rs`) NEVER contains `unsafe`.
//!
//! GATING CONTRACT: identical to the Linux basement — the architecture lint
//! exempts only this `sys.rs`, and the unsafe ledger
//! (`traceability/unsafe_ledger.yaml`) reconciles every `unsafe` block here,
//! failing closed on any unmatched block or stale entry.
//!
//! STEP (a) scaffolding: INTENTIONALLY EMPTY — real FFI lands in step (d).
