#![no_main]
//! GAUNT-FUZZ-1 — `sidx_entry` target.
//!
//! Drives `batpak::__fuzz::__fuzz_sidx_entry(&[u8], segment_id)`, which forwards
//! into `SidxEntry::decode_from` — the fixed-width (`ENTRY_SIZE` == 162 bytes)
//! secondary-index entry decoder. The store reads every SIDX entry off disk through
//! this path during scan/recovery, so a panic on a malformed entry is a durability
//! defect.
//!
//! The extra scalar `segment_id: u64` is derived from the FIRST 8 bytes of `data`
//! so the fuzzer explores it; the rest is the entry buffer. Any length other than
//! exactly 162 bytes is the canonical typed `Err` path.
//!
//! S1 contract: (a) never panic; (b) no unbounded allocation (fixed-width decode,
//! no length prefix); (c) a valid 162-byte buffer decodes `Ok`; (d) any other
//! length / corrupt content returns a typed `StoreError`, never a panic.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_sidx_entry;

fuzz_target!(|data: &[u8]| {
    // Derive `segment_id` from the first 8 bytes so the fuzzer can steer it; the
    // remainder is the entry buffer. Too-short inputs use segment_id 0 and an
    // empty (typed-Err) buffer.
    let (segment_id, buf) = split_u64_prefix(data);

    // (a)+(d): never panic; non-162-byte or corrupt buffers return a typed Err.
    match __fuzz_sidx_entry(buf, segment_id) {
        Ok(()) => {
            // (c): a well-formed 162-byte entry decoded.
        }
        Err(err) => {
            let _ = format!("{err:?}");
        }
    }
});

/// Split an 8-byte little-endian `u64` prefix off the front of `data`, returning
/// the scalar and the remaining slice. When `data` is shorter than 8 bytes the
/// scalar is 0 and the remainder is empty (still a valid, typed-Err input).
fn split_u64_prefix(data: &[u8]) -> (u64, &[u8]) {
    if data.len() < 8 {
        return (0, &[]);
    }
    let (head, rest) = data.split_at(8);
    let scalar = u64::from_le_bytes(head.try_into().expect("8-byte prefix"));
    (scalar, rest)
}
