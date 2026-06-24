//! Concrete backends. [`inert::InertBackend`] is the honest no-confinement
//! reference; the OS backends (linux/windows/macos/wasm) land here too.
//!
//! HONESTY-FIRST LAYOUT (SCOPE §4, step a): each OS module's `support_matrix()`
//! is PURE DATA and ALWAYS compiled, so the per-platform honesty tables are
//! constructible and unit-testable on ANY host (the honesty is provable on this
//! Linux box, off-target). Only the parts that TOUCH the OS — the backend struct
//! (`probe`/`profile`/`execute`) and the `sys.rs` unsafe basement — are gated
//! behind `feature = "backend-<os>"` + `target_os = "<os>"`, so a Linux build
//! never resolves Windows/macOS crates.
//!
//! The [`crate::Backend`] TRAIT and all contract types stay OS-free. The only
//! host-touching code in the crate lives in the gated backend modules + the
//! Inert reference backend.

pub(crate) mod inert;

// Per-platform honest support matrices (pure data, always compiled). The
// OS-touching struct + unsafe basement inside each are feature/target gated.
pub mod linux;
pub mod macos;
pub mod wasm;
pub mod windows;
