//! Private store platform backend.
//!
//! This module is the narrow room for target-sensitive machine contact. It
//! exposes mechanics only; store/cold-start/frontier code owns admission and
//! durability semantics.

/// Test-only global-allocator shims (counting / fault injection). Re-exported
/// publicly by `store` so a dedicated single-test binary can install one as
/// `#[global_allocator]`. Compiled out entirely unless `alloc-count` or
/// `fault-alloc` is enabled.
#[cfg(any(feature = "alloc-count", feature = "fault-alloc"))]
pub mod alloc;

pub(crate) mod clock;
pub(crate) mod evidence;
pub(crate) mod fs;
pub(crate) mod lock;
pub(crate) mod mmap;
pub(crate) mod path_identity;
pub(crate) mod profile;
pub(crate) mod sync;
