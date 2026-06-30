//! PROVES: INV-NETBAT-LINE-PROTOCOL-STABLE (the "deterministic error-code
//!         mapping" clause) — every token `NetbatError::code()` can emit is
//!         frozen, byte-for-byte, to its on-wire spelling.
//! CATCHES: a silent rename/drift of ANY `ERR <code> ...` token (including the
//!          less-trafficked ones that `boundary.rs` only prefix-asserts), a
//!          renamed/removed error variant (compile error here), and any framing
//!          drift around the token in the `ERR <code> <hex>\n` response shape.
//! SEEDED: one constructed instance of every `NetbatError` variant and every
//!         `syncbat::RuntimeError` mapping, each paired with its frozen token.
//!
//! Relationship to `boundary.rs`: that suite pins the FULL response bytes for
//! the two hot paths (`response_err_malformed.hex` for `malformed_request`, and
//! the asserted `unknown_operation` frame) and prefix-asserts a couple of
//! others. This file does NOT touch or duplicate those full-frame goldens; it
//! adds the missing piece — an EXHAUSTIVE table that freezes the token field for
//! ALL code() outputs, so a drift in a rarely-exercised token (e.g.
//! `cursor_too_large`, `receipt_sink`) can no longer slip through.
//!
//! Exhaustive-by-construction — what forces a new variant to need a pin:
//!  * `frozen_token` below names EVERY `NetbatError` variant and EVERY
//!    `syncbat::RuntimeError` variant explicitly. RENAMING or REMOVING a variant
//!    breaks COMPILATION of this file (a hard, compile-time signal).
//!  * Each frozen string literal is asserted byte-for-byte against the live
//!    `code()` result, so changing a token's spelling turns the table RED.
//!  * The `EXPECTED_*` counts pin how many variants/tokens the table covers; the
//!    `samples()` table is the single source of truth (one row per variant).
//!  * LIMITATION (stated honestly): `NetbatError` and `syncbat::RuntimeError` are
//!    both `#[non_exhaustive]`, so an EXTERNAL test crate cannot make the
//!    compiler reject a newly-ADDED variant — a `_` arm is mandatory. A brand-new
//!    variant therefore lands in the `_ => UNPINNED` arm and is only caught once a
//!    sample row for it is added here. The `EXPECTED_*` count tripwire plus this
//!    note are the mitigation; compiler-forced add-detection would require an
//!    in-crate `#[cfg(test)]` match, which is out of scope for a tests/-only
//!    change. Token RENAMES and variant RENAMES/REMOVALS — the actual drift gap
//!    this gate closes — are caught hard.

use netbat::{self as nb, NetbatError};
use std::collections::HashSet;
use std::mem::discriminant;
use syncbat::RuntimeError;

/// Sentinel returned by [`frozen_token`] for any variant not explicitly pinned.
/// Reaching it means a new `#[non_exhaustive]` variant exists without a golden
/// token — see the module-header limitation note.
const UNPINNED: &str = "<<UNPINNED>>";

/// Number of rows in [`samples`] (one per `NetbatError` variant plus the full
/// `RuntimeError` fan-out). Bump this — and add the corresponding row — when a
/// variant is added.
const EXPECTED_SAMPLES: usize = 19;
/// Distinct `NetbatError` discriminants the table covers (all 14 variants; the
/// six `Runtime` rows share one discriminant, so 13 + 1).
const EXPECTED_NETBAT_VARIANTS: usize = 14;
/// Distinct `syncbat::RuntimeError` discriminants the table covers (all six
/// current variants).
const EXPECTED_RUNTIME_VARIANTS: usize = 6;
/// Distinct on-wire tokens the table pins. 13 direct `NetbatError` tokens plus
/// the five the `Runtime` path can produce (`unknown_operation`,
/// `missing_handler`, `handler`, `receipt_sink`, and the `runtime` catch-all).
const EXPECTED_DISTINCT_TOKENS: usize = 18;

/// Frozen on-wire token for every error this transport can surface.
///
/// This mirrors `NetbatError::code()` with FROZEN string literals so that any
/// drift in the production mapping turns the table red. Every variant is named
/// explicitly: a renamed/removed variant fails to COMPILE here, and a renamed
/// token value fails the byte-for-byte assertion in [`code_table_pins_every_token`].
fn frozen_token(err: &NetbatError) -> &'static str {
    match err {
        NetbatError::Io { .. } => "io",
        NetbatError::EmptyStream => "empty_stream",
        NetbatError::LineTooLong { .. } => "line_too_long",
        NetbatError::MalformedRequest { .. } => "malformed_request",
        NetbatError::UnsupportedProtocolVersion { .. } => "unsupported_protocol_version",
        NetbatError::OperationNameTooLong { .. } => "operation_name_too_long",
        NetbatError::InputTooLarge { .. } => "input_too_large",
        NetbatError::OutputTooLarge { .. } => "output_too_large",
        NetbatError::MalformedStreamFrame { .. } => "malformed_stream_frame",
        NetbatError::SubscriptionIdTooLong { .. } => "subscription_id_too_long",
        NetbatError::CursorTooLarge { .. } => "cursor_too_large",
        NetbatError::StreamPayloadTooLarge { .. } => "stream_payload_too_large",
        NetbatError::StreamMessageTooLarge { .. } => "stream_message_too_large",
        NetbatError::Runtime(runtime) => match runtime {
            RuntimeError::UnknownOperation { .. } => "unknown_operation",
            RuntimeError::MissingHandler { .. } => "missing_handler",
            RuntimeError::Handler { .. } => "handler",
            RuntimeError::ReceiptSink { .. } => "receipt_sink",
            // `code()` folds `Denied`, `StatusSink`, and any future
            // `RuntimeError` variant into the generic `runtime` token via its
            // `Runtime(_)` arm. Naming `Denied`/`StatusSink` explicitly here
            // makes a rename/removal a compile error and pins the CURRENT
            // fold-to-runtime behavior; a brand-new RuntimeError variant lands
            // in the `_` arm below (see module-header limitation note).
            RuntimeError::Denied { .. } | RuntimeError::StatusSink { .. } => "runtime",
            _ => UNPINNED,
        },
        // Mandatory wildcard: `NetbatError` is `#[non_exhaustive]`, so an
        // external crate cannot match it exhaustively. A newly added variant
        // surfaces here.
        _ => UNPINNED,
    }
}

/// The golden table: every error case paired with its frozen on-wire token.
///
/// One row per `NetbatError` variant, with the `Runtime` variant expanded to one
/// row per `syncbat::RuntimeError` mapping (including the two — `Denied`,
/// `StatusSink` — that currently fold into the `runtime` catch-all).
fn samples() -> Vec<(NetbatError, &'static str)> {
    vec![
        (
            NetbatError::Io {
                kind: std::io::ErrorKind::BrokenPipe,
            },
            "io",
        ),
        (NetbatError::EmptyStream, "empty_stream"),
        (NetbatError::LineTooLong { max: 7 }, "line_too_long"),
        (
            NetbatError::MalformedRequest { reason: "bad" },
            "malformed_request",
        ),
        (
            NetbatError::UnsupportedProtocolVersion {
                version: "NETBAT/9".to_owned(),
            },
            "unsupported_protocol_version",
        ),
        (
            NetbatError::OperationNameTooLong { max: 3 },
            "operation_name_too_long",
        ),
        (NetbatError::InputTooLarge { max: 1 }, "input_too_large"),
        (NetbatError::OutputTooLarge { max: 1 }, "output_too_large"),
        (
            NetbatError::MalformedStreamFrame { reason: "bad" },
            "malformed_stream_frame",
        ),
        (
            NetbatError::SubscriptionIdTooLong { max: 4 },
            "subscription_id_too_long",
        ),
        (NetbatError::CursorTooLarge { max: 5 }, "cursor_too_large"),
        (
            NetbatError::StreamPayloadTooLarge { max: 6 },
            "stream_payload_too_large",
        ),
        (
            NetbatError::StreamMessageTooLarge { max: 8 },
            "stream_message_too_large",
        ),
        (
            NetbatError::Runtime(RuntimeError::unknown_operation("missing")),
            "unknown_operation",
        ),
        (
            NetbatError::Runtime(RuntimeError::missing_handler("op")),
            "missing_handler",
        ),
        (
            NetbatError::Runtime(RuntimeError::handler("op", "invalid_input", "bad payload")),
            "handler",
        ),
        (
            NetbatError::Runtime(RuntimeError::receipt_sink("op", "sink unavailable")),
            "receipt_sink",
        ),
        // `Denied` and `StatusSink` both currently surface as the `runtime`
        // catch-all token. Pinning them documents that fold and turns RED the
        // day a more specific token is introduced (forcing a deliberate pin).
        (
            NetbatError::Runtime(RuntimeError::denied("op", "policy", "not allowed")),
            "runtime",
        ),
        (
            NetbatError::Runtime(RuntimeError::status_sink("op", "status sink unavailable")),
            "runtime",
        ),
    ]
}

#[test]
fn code_table_pins_every_token() {
    let mut failures: Vec<String> = Vec::new();

    for (err, expected) in samples() {
        let live = err.code();
        if live != expected {
            failures.push(format!(
                "code() drift: golden pinned `{expected}`, code() returned `{live}`"
            ));
        }
        let frozen = frozen_token(&err);
        if frozen == UNPINNED {
            failures.push(format!(
                "UNPINNED reached while pinning `{expected}` — a new \
                 #[non_exhaustive] variant needs a golden token row"
            ));
        } else if frozen != expected {
            failures.push(format!(
                "mirror drift: golden pinned `{expected}`, frozen_token returned `{frozen}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "ERR code-token table drifted:\n{}",
        failures.join("\n")
    );
}

#[test]
fn err_frame_carries_the_pinned_token_in_stable_shape() {
    let mut failures: Vec<String> = Vec::new();

    for (err, expected) in samples() {
        let frame = nb::encode_response(Err(&err));
        let Ok(text) = std::str::from_utf8(&frame) else {
            failures.push(format!("token `{expected}`: ERR frame is not ASCII/UTF-8"));
            continue;
        };
        let Some(rest) = text.strip_prefix("ERR ") else {
            failures.push(format!(
                "token `{expected}`: frame missing `ERR ` keyword: {text:?}"
            ));
            continue;
        };
        let Some(body) = rest.strip_suffix('\n') else {
            failures.push(format!(
                "token `{expected}`: frame missing trailing newline: {text:?}"
            ));
            continue;
        };
        let mut fields = body.splitn(2, ' ');
        let token = fields.next().unwrap_or_default();
        let Some(hex_message) = fields.next() else {
            failures.push(format!(
                "token `{expected}`: frame has no hex message field: {text:?}"
            ));
            continue;
        };

        if token != expected {
            failures.push(format!(
                "wire token drift: frame carried `{token}`, golden pinned `{expected}`"
            ));
        }

        let hex_is_lowercase_even = !hex_message.is_empty()
            && hex_message.len().is_multiple_of(2)
            && hex_message
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !hex_is_lowercase_even {
            failures.push(format!(
                "token `{expected}`: message field is not even-length lowercase hex: {hex_message:?}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "ERR frame shape/token drifted:\n{}",
        failures.join("\n")
    );
}

#[test]
fn code_table_is_exhaustive_by_construction() {
    let samples = samples();

    assert_eq!(
        samples.len(),
        EXPECTED_SAMPLES,
        "sample-row count changed: add/remove a row AND update EXPECTED_SAMPLES \
         (and the related EXPECTED_* counts) when a variant changes"
    );

    let netbat_variants: HashSet<_> = samples.iter().map(|(err, _)| discriminant(err)).collect();
    assert_eq!(
        netbat_variants.len(),
        EXPECTED_NETBAT_VARIANTS,
        "distinct NetbatError variants covered by the table changed"
    );

    let runtime_variants: HashSet<_> = samples
        .iter()
        .filter_map(|(err, _)| {
            if let NetbatError::Runtime(runtime) = err {
                Some(discriminant(runtime))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        runtime_variants.len(),
        EXPECTED_RUNTIME_VARIANTS,
        "distinct syncbat::RuntimeError variants covered by the table changed"
    );

    let tokens: HashSet<_> = samples.iter().map(|(_, token)| *token).collect();
    assert_eq!(
        tokens.len(),
        EXPECTED_DISTINCT_TOKENS,
        "distinct on-wire tokens pinned by the table changed"
    );

    let unpinned: Vec<&str> = samples
        .iter()
        .filter(|(err, _)| frozen_token(err) == UNPINNED)
        .map(|(_, token)| *token)
        .collect();
    assert!(
        unpinned.is_empty(),
        "these sampled variants reached the UNPINNED sentinel (need a golden token): {unpinned:?}"
    );
}
