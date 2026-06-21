//! Concrete backends. In C0 only [`inert::InertBackend`] — the honest,
//! no-confinement reference. Real OS backends (linux/windows/macos/wasm) and the
//! `SimBackend` land in later phases.
//!
//! The [`crate::Backend`] TRAIT and all contract types stay OS-free; the only
//! host-touching code in the crate lives here, in the Inert reference backend.

pub(crate) mod inert;
