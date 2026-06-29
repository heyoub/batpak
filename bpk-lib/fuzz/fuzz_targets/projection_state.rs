#![no_main]
//! GAUNT-FUZZ-1 — `projection_state` target.
//!
//! Drives `batpak::__fuzz::__fuzz_projection_state(&[u8])`, which mirrors
//! `decode_cached_state::<T>` (`serde_json::from_slice::<T>`) against the
//! representative `FuzzProjectionState` type. This is the JSON decode path the store
//! takes when warming a projection from its persisted cache; a corrupt cache must
//! warn-and-`None`, never panic.
//!
//! S1 contract: (a) never panic; (b) no unbounded allocation (serde_json bounds
//! reads against the buffer; the run's `-rss_limit_mb` backstops any pathological
//! nesting); (c) a valid JSON `FuzzProjectionState` returns `true`; (d) invalid
//! JSON returns `false` (the warn-and-None path), never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_projection_state;

fuzz_target!(|data: &[u8]| {
    // (a)+(c)+(d): never panic; the bool discriminates Ok (true) / Err (false).
    let decoded = __fuzz_projection_state(data);
    let _ = decoded;
});
