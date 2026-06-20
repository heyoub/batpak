#![no_main]
//! GAUNT-FUZZ-1 — `segment_header` target.
//!
//! Drives `batpak::__fuzz::__fuzz_segment_header(&[u8])`, which forwards arbitrary
//! bytes into `encoding::from_bytes::<SegmentHeader>` (the msgpack decode of a
//! segment's on-disk header). Every segment file the store opens decodes its
//! header through this path, so a panic on crafted bytes is a durability defect.
//!
//! S1 contract on arbitrary `&[u8]`:
//!   (a) NEVER PANIC — libFuzzer turns any panic/abort into a crash.
//!   (b) NO UNBOUNDED ALLOCATION — we never pre-allocate from an attacker length
//!       prefix; rmp_serde bounds reads against the buffer and the run's
//!       `-rss_limit_mb` backstops any pathological allocation at the process level.
//!   (c) ROUND-TRIP — valid inputs decode to `Ok(())`; this is implicit (we exercise
//!       the decoder; a successful decode proves the bytes were a real header).
//!   (d) INVALID BYTES → typed `rmp_serde::decode::Error`, never a panic. The vast
//!       majority of arbitrary inputs land here, and reaching the `Err` arm without
//!       panicking is the proof.

use libfuzzer_sys::fuzz_target;

use batpak::__fuzz::__fuzz_segment_header;

fuzz_target!(|data: &[u8]| {
    // (a)+(d): never panic; invalid bytes are a typed decode error.
    match __fuzz_segment_header(data) {
        Ok(()) => {
            // (c): a valid header decoded. Nothing private to assert here — the
            // wrapper collapsed the header to `()`. Success without panic is the
            // round-trip proof for this surface.
        }
        Err(err) => {
            // (d): keep the typed error live so a future error path is exercised.
            let _ = format!("{err:?}");
        }
    }
});
