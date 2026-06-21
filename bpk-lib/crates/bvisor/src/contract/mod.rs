//! The pure contract: types that ALWAYS compile with zero OS deps.
//!
//! Every module here is platform-agnostic. No `std::fs`, no `std::process`, no
//! `std::net`, no OS syscalls. The only host-touching code in the whole crate
//! lives in [`crate::backend::inert`] (the no-confinement reference backend),
//! never in the contract or the [`backend::Backend`] trait.

pub(crate) mod backend;
pub(crate) mod capability;
pub(crate) mod events;
pub(crate) mod host_control;
pub(crate) mod ids;
pub(crate) mod plan;
pub(crate) mod recovery;
pub(crate) mod registry;
pub(crate) mod report;
pub(crate) mod support;
