//! The SANCTIONED unsafe basement for the macOS backend (kernel plan §10.8).
//!
//! This file is the ONE quarantine where the macOS backend's `libc` extern FFI
//! `unsafe` is permitted to live: the Seatbelt `sandbox_init` / `sandbox_free`
//! externs (deprecated-but-shipped) and the `killpg` pgid teardown. The safe
//! orchestration in [`super`] (`mod.rs`) NEVER contains `unsafe`.
//!
//! GATING CONTRACT: identical to the Linux basement — the architecture lint
//! exempts only this `sys.rs`, and the unsafe ledger
//! (`traceability/unsafe_ledger.yaml`) reconciles every `unsafe` block here,
//! failing closed on any unmatched block or stale entry.
//!
//! STEP (a) scaffolding: INTENTIONALLY EMPTY — real FFI lands in step (e).
