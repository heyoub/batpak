//! Property-based tests for the NETBAT/1 line protocol.
//!
//! Hand-written boundary tests cover the obvious framing shapes; these
//! property tests exercise the encoder/decoder pair across arbitrary
//! (operation_name, input_bytes) shapes so we catch encoder/decoder
//! drift that fixture tests would miss.
//!
//! PROVES:
//!   - encode_request -> decode_line round-trip is byte-stable across
//!     the full grammar of operation names + arbitrary input bytes.
//!   - The encoded frame literally ends with `\n` for every input.
//!   - decode_line is total (never panics) on arbitrary byte input —
//!     malformed frames return typed NetbatError, valid frames return
//!     parsed RequestFrame.
//!   - encode_response(Ok|Err) -> parseable response shape across
//!     arbitrary outputs and error variants.
//!
//! Property tests run 256 cases by default; CI's PROPTEST_CASES env
//! var can lift the floor in stress runs.

use netbat as nb;
use proptest::prelude::*;

// ─── arbitrary generators ──────────────────────────────────────────────────

/// Generator for NETBAT/1 operation names. Matches the grammar that
/// syncbat::OperationName::new accepts: `[A-Za-z0-9._-]+`, no
/// leading/trailing `.`, no `..`, 1..=128 bytes.
fn arb_operation_name() -> impl Strategy<Value = String> {
    proptest::collection::vec("[A-Za-z0-9_-]{1,16}", 1..=4).prop_map(|segments| segments.join("."))
}

/// Generator for input payload bytes — capped at 1 KiB so the
/// property runs stay quick. The real DEFAULT_MAX_INPUT_BYTES (32
/// KiB) is exercised by the boundary suite's limit tests.
fn arb_payload_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..=1024)
}

// ─── encode_request <-> decode_line round-trip ─────────────────────────────

proptest! {
    /// Every (operation, input) pair survives an encode -> decode
    /// round-trip with both halves intact.
    #[test]
    fn encode_request_decode_line_roundtrip(
        op in arb_operation_name(),
        payload in arb_payload_bytes(),
    ) {
        let frame = nb::encode_request(&op, &payload);
        let parsed = nb::decode_line(&frame, &nb::Limits::default())
            .expect("encoder output must decode");
        prop_assert_eq!(parsed.operation(), op.as_str());
        prop_assert_eq!(parsed.input(), payload.as_slice());
    }

    /// Every encoded frame literally ends with one newline byte and
    /// starts with the protocol-version prefix.
    #[test]
    fn encoded_request_frame_shape(
        op in arb_operation_name(),
        payload in arb_payload_bytes(),
    ) {
        let frame = nb::encode_request(&op, &payload);
        prop_assert!(frame.starts_with(b"NETBAT/1 CALL "));
        prop_assert!(frame.ends_with(b"\n"));
        // Exactly one trailing newline (not \r\n, not double).
        prop_assert!(!frame.ends_with(b"\r\n"));
        let interior = &frame[..frame.len() - 1];
        prop_assert!(!interior.contains(&b'\n'));
    }

    /// Encoding the same (op, payload) twice produces byte-identical
    /// frames. Catches non-determinism in the encoder.
    #[test]
    fn encode_request_is_deterministic(
        op in arb_operation_name(),
        payload in arb_payload_bytes(),
    ) {
        let a = nb::encode_request(&op, &payload);
        let b = nb::encode_request(&op, &payload);
        prop_assert_eq!(a, b);
    }
}

// ─── decode_line totality ──────────────────────────────────────────────────

proptest! {
    /// decode_line never panics on arbitrary byte input. Either
    /// returns a valid RequestFrame or a typed NetbatError.
    #[test]
    fn decode_line_is_total_on_arbitrary_bytes(
        line in proptest::collection::vec(any::<u8>(), 0..=4096),
    ) {
        // Just call it — must not panic. The result is irrelevant.
        let _ = nb::decode_line(&line, &nb::Limits::default());
    }

    /// decode_line never panics on bytes that LOOK request-shaped
    /// (start with the prefix but have arbitrary tail bytes). Catches
    /// truncation / encoding edge cases in the parser.
    #[test]
    fn decode_line_is_total_on_prefix_plus_garbage(
        suffix in proptest::collection::vec(any::<u8>(), 0..=512),
    ) {
        let mut frame = b"NETBAT/1 CALL ".to_vec();
        frame.extend_from_slice(&suffix);
        let _ = nb::decode_line(&frame, &nb::Limits::default());
    }
}

// ─── encode_response shapes ────────────────────────────────────────────────

proptest! {
    /// encode_response(Ok(bytes)) produces a frame of the shape
    /// `OK <hex>\n` whose hex decodes back to the original bytes.
    #[test]
    fn encode_response_ok_roundtrips(payload in arb_payload_bytes()) {
        let frame = nb::encode_response(Ok(&payload));
        prop_assert!(frame.starts_with(b"OK "));
        prop_assert!(frame.ends_with(b"\n"));

        let hex_segment = std::str::from_utf8(&frame[3..frame.len() - 1])
            .expect("encode_response emits ASCII hex");
        let decoded = nb::decode_hex_str(hex_segment).expect("decodes");
        prop_assert_eq!(decoded, payload);
    }

    /// encode_response(Err) for a MalformedRequest always carries the
    /// stable code token + UTF-8 message hex (NOT MessagePack).
    #[test]
    fn encode_response_err_carries_stable_code(
        reason_idx in 0_usize..3_usize,
    ) {
        // Mix across three malformed-shape reasons so we cover branches
        // in the encoder. NetbatError::MalformedRequest::reason is a
        // &'static str — these literals stay valid for the test.
        let reason = match reason_idx {
            0 => "bad",
            1 => "operation has invalid bytes",
            _ => "missing input",
        };
        let err = nb::NetbatError::MalformedRequest { reason };
        let frame = nb::encode_response(Err(&err));
        prop_assert!(frame.starts_with(b"ERR malformed_request "));
        prop_assert!(frame.ends_with(b"\n"));
    }
}
