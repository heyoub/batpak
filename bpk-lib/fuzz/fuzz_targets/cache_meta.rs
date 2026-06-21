#![no_main]
//! GAUNT-FUZZ-1 — `cache_meta` target.
//!
//! Drives `batpak::__fuzz::__fuzz_cache_meta(&[u8])`, which forwards into
//! `CacheMeta::decode_from_bytes` — the projection-cache trailer decoder. It splits
//! a fixed-size meta trailer off the END of the buffer (current or legacy format,
//! disambiguated by a trailing magic) and returns the leading state bytes alongside
//! the parsed meta. A misread is a cache MISS (typed `Err`), never a panic.
//!
//! S1 contract: (a) never panic; (b) NO UNBOUNDED ALLOCATION — the returned `value`
//! is the leading slice of the input, so it can never exceed the input length; we
//! assert that here as the explicit no-amplification check. (c) a valid trailer
//! decodes `Ok`; (d) a too-short / corrupt buffer returns a typed `StoreError`.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_cache_meta;

fuzz_target!(|data: &[u8]| {
    // (a)+(d): never panic; too-short / corrupt buffers return a typed Err.
    match __fuzz_cache_meta(data) {
        Ok((value, meta)) => {
            // (b): the leading state bytes are a prefix of the input — decoding can
            // never hand back (or allocate) more bytes than the caller supplied.
            assert!(
                value.len() <= data.len(),
                "decoded cache value must never exceed the input buffer (no amplification)"
            );
            // Keep the parsed meta live so its fields are exercised.
            let _ = format!("{meta:?}");
        }
        Err(err) => {
            let _ = format!("{err:?}");
        }
    }
});
