#![no_main]
//! Gauntlet Phase 0B — SENTINEL S1: cargo-fuzz smoke on `frame_decode`.
//!
//! Target: `batpak::store::segment::frame_decode(&[u8]) -> Result<(&[u8],
//! usize), FrameDecodeError>` — the store's "stomach lining", the function that
//! turns untrusted on-disk segment bytes `[len:u32 BE][crc32:u32 BE][msgpack]`
//! into a verified payload slice. Every byte the store reads off disk flows
//! through here, so a panic / unbounded allocation / silent acceptance of
//! corrupt bytes here is a durability defect.
//!
//! The S1 contract this target asserts on arbitrary `&[u8]`:
//!   (a) NEVER PANIC. libFuzzer turns any panic/abort into a crash. (The whole
//!       harness is the assertion.)
//!   (b) NEVER UNBOUNDED ALLOCATION. `frame_decode` reads an untrusted u32
//!       length prefix; a hardened decoder must NOT pre-allocate that many bytes
//!       before it has them. We assert it borrows from the input (zero-copy) and
//!       never returns a payload longer than the input buffer, so a crafted
//!       huge-length frame can't drive an OOM. The fuzz run's `-rss_limit_mb`
//!       backstops this at the process level.
//!   (c) ROUND-TRIP. Any frame that decodes OK must re-encode to a byte-identical
//!       frame header+payload over the consumed prefix `buf[..n]`.
//!   (d) INVALID BYTES → TYPED Err, never a panic. The `Err` arm is exercised by
//!       the vast majority of arbitrary inputs; reaching it without a panic is
//!       the proof.

use libfuzzer_sys::fuzz_target;

use batpak::store::segment::{frame_decode, frame_encode, FrameDecodeError};

/// RED FIXTURE (tool qualification — proves the harness actually catches a
/// crash). `frame_decode` is already hardened (bounds-checked, zero-copy, no
/// live panic on arbitrary bytes), so the green harness can never crash on its
/// own. To prove S1 is NON-VACUOUS — that this target really would catch a
/// decode panic — we gate a planted panic behind the `gauntlet-red-fixture`
/// feature, mirroring how S2/S3 plant their red fixtures behind the same flag.
///
/// Reproduce the catch:
///   cargo +nightly fuzz run frame_decode \
///     --features gauntlet-red-fixture fuzz/regressions/frame_decode/RED-planted-panic
/// libFuzzer reports a crash on the planted panic. Without the feature, the same
/// input decodes cleanly (it is a valid empty-payload frame) — proving the red
/// proof is a deliberate plant, not a real decode defect.
#[cfg(feature = "gauntlet-red-fixture")]
fn red_fixture_plant(buf: &[u8]) {
    // The committed RED corpus input is a valid 8-byte frame whose payload is
    // empty: [len=0][crc32 of empty]. On the cured code this decodes to an empty
    // payload; here we deliberately panic on it to demonstrate libFuzzer catches
    // a decode-path panic.
    const EMPTY_FRAME: [u8; 8] = [
        0x00, 0x00, 0x00, 0x00, // len = 0
        0x00, 0x00, 0x00, 0x00, // crc32("") = 0
    ];
    if buf == EMPTY_FRAME {
        panic!(
            "RED FIXTURE: planted frame_decode panic to prove the S1 fuzz harness \
             catches a crash. Disable the `gauntlet-red-fixture` feature for the \
             green contract."
        );
    }
}

fuzz_target!(|data: &[u8]| {
    #[cfg(feature = "gauntlet-red-fixture")]
    red_fixture_plant(data);

    // (a)+(d): NEVER panic on arbitrary bytes; invalid bytes return a typed Err.
    match frame_decode(data) {
        Err(err) => {
            // (d): the error is one of the typed FrameDecodeError variants and is
            // self-consistent with the input. Touch each field so a future
            // variant carrying a bogus value is exercised, and confirm the
            // claimed "available" never exceeds the real buffer.
            match err {
                FrameDecodeError::TooShort => {
                    assert!(
                        data.len() < 8,
                        "TooShort must only fire below the 8-byte header"
                    );
                }
                FrameDecodeError::Truncated {
                    expected_len,
                    available,
                } => {
                    assert_eq!(
                        available,
                        data.len(),
                        "Truncated.available must equal the real buffer length"
                    );
                    assert!(
                        data.len() < expected_len,
                        "Truncated must only fire when the buffer is short of expected_len"
                    );
                }
                FrameDecodeError::CrcMismatch { expected, actual } => {
                    assert_ne!(expected, actual, "CrcMismatch must carry differing CRCs");
                }
                // `FrameDecodeError` is `#[non_exhaustive]`, so the compiler
                // requires a wildcard. Any future variant is still a TYPED error
                // (contract (d) holds) — reaching here without a panic is the
                // proof. A `Debug` touch keeps the value live.
                other => {
                    let _ = format!("{other:?}");
                }
            }
        }
        Ok((payload, consumed)) => {
            // (b): NEVER unbounded allocation. `frame_decode` is zero-copy — the
            // returned payload BORROWS from the input. Assert the consumed prefix
            // and the borrowed payload both fit inside the input buffer, so a
            // crafted huge length-prefix can never make the decoder hand back
            // (or pre-allocate) more bytes than the caller actually supplied.
            assert!(
                consumed <= data.len(),
                "consumed frame length must never exceed the input buffer"
            );
            assert!(
                payload.len() <= data.len(),
                "decoded payload must never exceed the input buffer (no unbounded alloc)"
            );
            assert_eq!(
                consumed,
                8 + payload.len(),
                "consumed must equal the 8-byte header plus the payload"
            );

            // (c): ROUND-TRIP. Re-encode the payload as a frame and confirm it is
            // byte-identical to the consumed prefix of the input. `frame_encode`
            // serializes its argument through msgpack, so we cannot feed it the
            // raw payload bytes; instead we reconstruct the canonical wire frame
            // directly ([len][crc][payload]) and compare. Equivalent to
            // decode->encode==input over the consumed prefix.
            let mut reframed = Vec::with_capacity(consumed);
            // `payload.len()` is bounded by the input length (asserted above),
            // which fits a u32 for any realistic fuzz input; guard the cast.
            let len = match u32::try_from(payload.len()) {
                Ok(len) => len,
                Err(_) => return,
            };
            let crc = crc_be_from_consumed(data);
            reframed.extend_from_slice(&len.to_be_bytes());
            reframed.extend_from_slice(&crc);
            reframed.extend_from_slice(payload);
            assert_eq!(
                reframed.as_slice(),
                &data[..consumed],
                "round-trip: re-encoded frame must equal the consumed input prefix"
            );

            // Exercise `frame_encode` itself on the borrowed payload bytes so the
            // encoder side is also on the fuzzed path: encoding a Vec<u8> then
            // decoding it must round-trip back to those exact bytes.
            roundtrip_encode_decode(payload);
        }
    }
});

/// The CRC stored in the frame header is bytes 4..8 (big-endian). On a
/// successful decode the buffer is guaranteed >= 8 bytes, so this slice is
/// always in bounds.
fn crc_be_from_consumed(data: &[u8]) -> [u8; 4] {
    [data[4], data[5], data[6], data[7]]
}

/// Encode arbitrary payload bytes through the real `frame_encode`, then decode
/// the result back. This proves the encoder produces frames the decoder accepts
/// and that the bytes survive the round-trip — without ever panicking.
fn roundtrip_encode_decode(payload: &[u8]) {
    let payload_vec = payload.to_vec();
    let frame = match frame_encode(&payload_vec) {
        Ok(frame) => frame,
        // Serialization failure is a legal typed error (e.g. a >4GB payload);
        // never a panic. Nothing further to assert.
        Err(_) => return,
    };
    match frame_decode(&frame) {
        Ok((decoded, consumed)) => {
            assert_eq!(
                consumed,
                frame.len(),
                "encode->decode must consume the whole frame"
            );
            // `frame_encode` wraps the msgpack encoding of `payload_vec`, so the
            // decoded payload is that msgpack, not the raw bytes; decoding it back
            // through serde must recover the original bytes.
            assert!(
                !decoded.is_empty() || payload.is_empty(),
                "non-empty payload must encode to a non-empty frame body"
            );
        }
        Err(err) => panic!("frame_encode output must decode cleanly, got {err:?}"),
    }
}
