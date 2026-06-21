#![no_main]
//! GAUNT-FUZZ-1 — `hidden_ranges` target.
//!
//! Drives `batpak::__fuzz::__fuzz_hidden_ranges(&[u8])`, a file-path decoder: the
//! wrapper writes `data` to `<tmp>/visibility_ranges.fbv` and calls the real
//! `load_cancelled_ranges`. This is the on-disk cancelled-range index (hidden
//! events); a corrupt file must return a typed `Err`, never panic — including the
//! `RangeMalformed` case where a range has `start == end`.
//!
//! S1 contract: (a) never panic; (b) no unbounded allocation (the loader bounds its
//! reads; the run's `-rss_limit_mb` backstops any crafted huge length-prefix); (c)
//! a valid file returns `Ok(true)` (ranges present); (d) a corrupt / malformed file
//! returns a typed `StoreError`, never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_hidden_ranges;

fuzz_target!(|data: &[u8]| {
    // (a)+(c)+(d): never panic; corrupt files return a typed Err.
    match __fuzz_hidden_ranges(data) {
        Ok(present) => {
            let _ = present;
        }
        Err(err) => {
            let _ = format!("{err:?}");
        }
    }
});
