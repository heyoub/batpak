#![no_main]
//! GAUNT-FUZZ-1 — `checkpoint_data` target.
//!
//! Drives `batpak::__fuzz::__fuzz_checkpoint_data(version, body)`, which forwards
//! into `decode_checkpoint_data` — the versioned cold-start checkpoint decoder. A
//! corrupt checkpoint must be IGNORED (typed `None`), never panic, since the
//! checkpoint is an optimization rebuildable from the log.
//!
//! The `version: u16` selector is derived from the FIRST 2 bytes of `data` so the
//! fuzzer explores every version branch (including `u16::MAX`); the rest is the
//! msgpack body.
//!
//! S1 contract: (a) never panic; (b) no unbounded allocation; (c) a valid body for
//! a known version returns `true`; (d) an unknown version or corrupt body returns
//! `false` (the "ignore corrupt checkpoint" path), never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_checkpoint_data;

fuzz_target!(|data: &[u8]| {
    let (version, body) = split_u16_prefix(data);

    // (a)+(c)+(d): never panic; the bool discriminates Some (true) / None (false).
    let decoded = __fuzz_checkpoint_data(version, body);
    let _ = decoded;
});

/// Split a 2-byte little-endian `u16` prefix off the front of `data`. Short inputs
/// yield version 0 and an empty body (still a valid, false-returning input).
fn split_u16_prefix(data: &[u8]) -> (u16, &[u8]) {
    if data.len() < 2 {
        return (0, &[]);
    }
    let (head, rest) = data.split_at(2);
    let version = u16::from_le_bytes(head.try_into().expect("2-byte prefix"));
    (version, rest)
}
