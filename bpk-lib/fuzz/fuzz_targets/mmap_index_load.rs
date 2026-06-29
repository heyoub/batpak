#![no_main]
//! GAUNT-FUZZ-1 — `mmap_index_load` target.
//!
//! Drives `batpak::__fuzz::__fuzz_mmap_index_load(&[u8])`, a file-path decoder: the
//! wrapper writes `data` to `<tmp>/index.fbati` and calls the real `load_mmap_index`
//! (with a `SystemClock`). This is the whole cold-start mmap-index load path — one
//! of the store's largest, most-nested parse surfaces. A corrupt mapped index must
//! be a typed outcome (`"invalid"` / `"future_version"`), never a panic.
//!
//! The wrapper returns a stable `&'static str` discriminant:
//! `"missing" | "loaded" | "invalid" | "future_version" | "io_error"`.
//!
//! S1 contract: (a) never panic; (b) no unbounded allocation (the run's
//! `-rss_limit_mb` backstops a crafted huge offset/length in the header); (c) a
//! valid index returns `"loaded"`; (d) a corrupt / future-version index returns the
//! typed `"invalid"` / `"future_version"` discriminant, never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_mmap_index_load;

fuzz_target!(|data: &[u8]| {
    // (a)+(c)+(d): never panic; the discriminant must be one of the known outcomes.
    let outcome = __fuzz_mmap_index_load(data);
    assert!(
        matches!(
            outcome,
            "missing" | "loaded" | "invalid" | "future_version" | "io_error"
        ),
        "mmap_index_load returned an unknown discriminant: {outcome:?}"
    );
});
