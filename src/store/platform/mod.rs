//! Private store platform backend.
//!
//! This module is the narrow room for target-sensitive machine contact. It
//! exposes mechanics only; store/cold-start/frontier code owns admission and
//! durability semantics.

pub(crate) mod clock;
pub(crate) mod evidence;
pub(crate) mod fs;
pub(crate) mod lock;
pub(crate) mod mmap;
pub(crate) mod path_identity;
pub(crate) mod profile;
pub(crate) mod sync;
