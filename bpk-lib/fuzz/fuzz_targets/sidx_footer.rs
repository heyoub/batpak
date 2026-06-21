#![no_main]
//! GAUNT-FUZZ-1 — `sidx_footer` target.
//!
//! Drives `batpak::__fuzz::__fuzz_sidx_footer(&[u8], segment_id)`, a file-path
//! decoder: the wrapper writes `data` to a temp file and drives BOTH SIDX footer
//! parse paths on it — `read_layout` (via `authenticated_string_table_offset`) and
//! `read_entries_unauthenticated`. The SIDX footer is the trailer the store seeks to
//! when locating entries; a crafted footer (bogus offsets/counts) must be a typed
//! `Err`, never a panic.
//!
//! The extra scalar `segment_id: u64` is derived from the FIRST 8 bytes of `data`
//! so the fuzzer explores it; the rest is the file body.
//!
//! S1 contract: (a) never panic; (b) NO UNBOUNDED ALLOCATION — a crafted entry-count
//! in the footer must not drive a pre-allocation; we assert the parsed entry count
//! never exceeds the file length (one entry is many bytes, so `<= len` is a generous
//! upper bound that a length-prefix amplification attack would violate). The run's
//! `-rss_limit_mb` backstops at the process level. (c) a valid footer parses `Ok`;
//! (d) a corrupt footer returns a typed `StoreError`, never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_sidx_footer;

fuzz_target!(|data: &[u8]| {
    // Derive `segment_id` from the first 8 bytes; the remainder is the file body.
    let (segment_id, body) = split_u64_prefix(data);

    // (a)+(d): never panic; corrupt footers return a typed Err.
    match __fuzz_sidx_footer(body, segment_id) {
        Ok((authenticated, entry_count)) => {
            let _ = authenticated;
            // (b): a footer cannot honestly declare more entries than there are
            // bytes in the file. A length-prefix amplification (huge count over a
            // tiny file) would violate this before any pre-allocation could OOM.
            assert!(
                entry_count <= body.len(),
                "parsed SIDX entry count must never exceed the file length (no amplification)"
            );
        }
        Err(err) => {
            let _ = format!("{err:?}");
        }
    }
});

/// Split an 8-byte little-endian `u64` prefix off the front of `data`. Short inputs
/// yield segment_id 0 and an empty body (still a valid, typed-Err input).
fn split_u64_prefix(data: &[u8]) -> (u64, &[u8]) {
    if data.len() < 8 {
        return (0, &[]);
    }
    let (head, rest) = data.split_at(8);
    let scalar = u64::from_le_bytes(head.try_into().expect("8-byte prefix"));
    (scalar, rest)
}
