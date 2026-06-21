#![no_main]
//! GAUNT-FUZZ-1 — `mmap_entry` target.
//!
//! Drives `batpak::__fuzz::__fuzz_mmap_entry(&[u8], version)`, which forwards into
//! `MmapIndexEntry::decode_from` — the fixed-width mmap-index entry decoder used
//! when the store memory-maps its index on cold start. A panic on a malformed
//! mapped entry is a durability defect.
//!
//! The `version: u16` selector is derived from the FIRST 2 bytes of `data` (so the
//! fuzzer reaches every version branch, including `u16::MAX`); the rest is the
//! entry buffer.
//!
//! S1 contract: (a) never panic; (b) no unbounded allocation (fixed-width, no
//! length prefix); (c) a valid entry for a known version returns `true`; (d) a
//! wrong-width buffer or unknown version returns `false`, never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_mmap_entry;

fuzz_target!(|data: &[u8]| {
    let (version, buf) = split_u16_prefix(data);

    // (a)+(c)+(d): never panic; the bool discriminates Ok (true) / Err (false).
    let decoded = __fuzz_mmap_entry(buf, version);
    let _ = decoded;
});

/// Split a 2-byte little-endian `u16` prefix off the front of `data`. Short inputs
/// yield version 0 and an empty buffer (still a valid, false-returning input).
fn split_u16_prefix(data: &[u8]) -> (u16, &[u8]) {
    if data.len() < 2 {
        return (0, &[]);
    }
    let (head, rest) = data.split_at(2);
    let version = u16::from_le_bytes(head.try_into().expect("2-byte prefix"));
    (version, rest)
}
