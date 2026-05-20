//! `system.heartbeat` payloads and handler.
//!
//! The two payload structs ([`SystemHeartbeatRequest`] and
//! [`SystemHeartbeatAck`]) are user-defined `EventPayload` types living
//! in category `0xF`, distinct from the substrate-owned raw kind
//! `EventKind::SYSTEM_HEARTBEAT` (`0x0003`, category `0x0`) which has no
//! payload struct. Heartbeat is a fixture, not substrate vocabulary; it
//! lives in this crate so the substrate (`batpak`) does not gain a public
//! example surface.

use std::time::{SystemTime, UNIX_EPOCH};

use batpak::EventPayload;
use serde::{Deserialize, Serialize};
use syncbat::{EffectClass, Handler, HandlerError, HandlerResult, OperationDescriptor};

use crate::EventPayloadFixture;

/// Stable operation name used for NETBAT/1 dispatch and receipts.
pub const HEARTBEAT_OPERATION_NAME: &str = "system.heartbeat";
/// Schema reference advertised by the operation descriptor for the request
/// payload. Mirrored into the exported manifest.
pub const HEARTBEAT_INPUT_SCHEMA_REF: &str = "system.heartbeat.request";
/// Schema reference advertised by the operation descriptor for the ack
/// payload. Mirrored into the exported manifest.
pub const HEARTBEAT_OUTPUT_SCHEMA_REF: &str = "system.heartbeat.ack";
/// Receipt kind emitted by the operation. Mirrored into the exported
/// manifest.
pub const HEARTBEAT_RECEIPT_KIND: &str = "receipt.system.heartbeat.v1";

/// Stable operation descriptor for `system.heartbeat`.
pub const HEARTBEAT_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    HEARTBEAT_OPERATION_NAME,
    EffectClass::Inspect,
    HEARTBEAT_INPUT_SCHEMA_REF,
    HEARTBEAT_OUTPUT_SCHEMA_REF,
    HEARTBEAT_RECEIPT_KIND,
);

/// NETBAT input payload for `system.heartbeat`.
///
/// Wire field names match the Rust field names byte-for-byte; the Phase 0
/// invariant is `wireName === tsName` (no camelCase / snake_case transform
/// anywhere on the canonical path).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA01)]
pub struct SystemHeartbeatRequest {
    /// Echoed back verbatim in the ack to prove request/response binding.
    pub nonce: String,
}

/// NETBAT output payload for `system.heartbeat`.
///
/// `server_ts_ms` is a Unix-epoch millisecond integer. In the manifest
/// fixture and golden bytes it is the deterministic constant
/// [`crate::manifest::FIXTURE_SERVER_TS_MS`]; the live handler uses
/// `SystemTime::now()`. Live roundtrip tests do not assert byte-equality
/// against the OK frame goldens — they assert shape, nonce echo, and
/// safe-integer bound only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA02)]
pub struct SystemHeartbeatAck {
    /// Echo of the request nonce.
    pub nonce: String,
    /// Server-recorded clock value when the ack was produced. Wire token
    /// `u64-millis`; bounded to `<= Number.MAX_SAFE_INTEGER` in the
    /// TS-binding so the field can ride as `S.Int` not `S.String`.
    pub server_ts_ms: u64,
}

impl EventPayloadFixture for SystemHeartbeatRequest {
    fn fixture_value() -> Self {
        Self {
            nonce: crate::manifest::FIXTURE_NONCE.to_owned(),
        }
    }
}

impl EventPayloadFixture for SystemHeartbeatAck {
    fn fixture_value() -> Self {
        Self {
            nonce: crate::manifest::FIXTURE_NONCE.to_owned(),
            server_ts_ms: crate::manifest::FIXTURE_SERVER_TS_MS,
        }
    }
}

/// Decode a `system.heartbeat` request from canonical bytes and produce
/// an ack with `server_ts_ms` set to the current wall clock.
///
/// # Errors
/// Returns [`HandlerError::InvalidInput`] if the input bytes do not decode
/// to a `SystemHeartbeatRequest`, or [`HandlerError::Failed`] if the
/// ack cannot be encoded.
pub fn handle_heartbeat(input: &[u8]) -> HandlerResult {
    let request: SystemHeartbeatRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(error.to_string()))?;
    let ack = SystemHeartbeatAck {
        nonce: request.nonce,
        server_ts_ms: current_unix_ms(),
    };
    batpak::encoding::to_bytes(&ack).map_err(|error| HandlerError::failed(error.to_string()))
}

/// Convenience newtype implementing [`syncbat::Handler`] over
/// [`handle_heartbeat`].
pub struct HeartbeatHandler;

impl Handler for HeartbeatHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        handle_heartbeat(input)
    }
}

fn current_unix_ms() -> u64 {
    // Clocks before the Unix epoch saturate to zero rather than panic.
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

// Force the `EventPayload` impl for both heartbeat types to remain
// referenced from this module, since their kind constants are consumed by
// the manifest registry via [`SystemHeartbeatRequest::KIND`] /
// [`SystemHeartbeatAck::KIND`].
#[allow(dead_code)]
const _REQUEST_KIND_LINK: batpak::event::EventKind = SystemHeartbeatRequest::KIND;
#[allow(dead_code)]
const _ACK_KIND_LINK: batpak::event::EventKind = SystemHeartbeatAck::KIND;

// ─── Manifest registry submissions ──────────────────────────────────────────
//
// One `inventory::submit!` per `EventPayload`-deriving type. The fixture
// closures defer encoding/JSON work to manifest export time so library
// callers that only need the schema-ref constants do not pay it.

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::heartbeat::SystemHeartbeatRequest",
        ts_name: "SystemHeartbeatRequest",
        schema_ref: HEARTBEAT_INPUT_SCHEMA_REF,
        kind_bits: SystemHeartbeatRequest::KIND.as_raw_u16(),
        fields: &[
            crate::manifest::FieldRow { wire_name: "nonce", type_token: "string", order: 0 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&SystemHeartbeatRequest::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(SystemHeartbeatRequest::fixture_value()).ok(),
    }
}

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::heartbeat::SystemHeartbeatAck",
        ts_name: "SystemHeartbeatAck",
        schema_ref: HEARTBEAT_OUTPUT_SCHEMA_REF,
        kind_bits: SystemHeartbeatAck::KIND.as_raw_u16(),
        fields: &[
            crate::manifest::FieldRow { wire_name: "nonce", type_token: "string", order: 0 },
            crate::manifest::FieldRow { wire_name: "server_ts_ms", type_token: "u64-millis", order: 1 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&SystemHeartbeatAck::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(SystemHeartbeatAck::fixture_value()).ok(),
    }
}

#[cfg(test)]
// justifies: INV-ALLOW-IS-DESIGN; tests in this module assert fixture invariants
// using panic!/assert! patterns. Suppressing these workspace-level denies for
// the test module only matches the precedent set by other syncbat tests.
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn fixture_request_roundtrips_through_canonical_encoding() {
        let value = SystemHeartbeatRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value).expect("encode request");
        let decoded: SystemHeartbeatRequest =
            batpak::encoding::from_bytes(&bytes).expect("decode request");
        assert_eq!(decoded, value);
    }

    #[test]
    fn fixture_ack_roundtrips_through_canonical_encoding() {
        let value = SystemHeartbeatAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value).expect("encode ack");
        let decoded: SystemHeartbeatAck = batpak::encoding::from_bytes(&bytes).expect("decode ack");
        assert_eq!(decoded, value);
    }

    #[test]
    fn handler_echoes_nonce_and_stamps_safe_integer_clock() {
        let request_bytes =
            batpak::encoding::to_bytes(&SystemHeartbeatRequest::fixture_value()).expect("encode");
        let output_bytes = handle_heartbeat(&request_bytes).expect("handle");
        let ack: SystemHeartbeatAck =
            batpak::encoding::from_bytes(&output_bytes).expect("decode ack");
        assert_eq!(ack.nonce, crate::manifest::FIXTURE_NONCE);
        // Safe-JS upper bound; mirrors the parity test on the TS side.
        const NUMBER_MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
        assert!(
            ack.server_ts_ms <= NUMBER_MAX_SAFE_INTEGER,
            "server_ts_ms {} exceeds Number.MAX_SAFE_INTEGER",
            ack.server_ts_ms
        );
    }

    #[test]
    fn descriptor_advertises_stable_strings() {
        assert_eq!(HEARTBEAT_DESCRIPTOR.name(), HEARTBEAT_OPERATION_NAME);
        assert_eq!(
            HEARTBEAT_DESCRIPTOR.input_schema_ref(),
            HEARTBEAT_INPUT_SCHEMA_REF
        );
        assert_eq!(
            HEARTBEAT_DESCRIPTOR.output_schema_ref(),
            HEARTBEAT_OUTPUT_SCHEMA_REF
        );
        assert_eq!(HEARTBEAT_DESCRIPTOR.receipt_kind(), HEARTBEAT_RECEIPT_KIND);
        HEARTBEAT_DESCRIPTOR
            .validate()
            .expect("descriptor must validate");
    }

    #[test]
    fn request_and_ack_have_distinct_kinds() {
        assert_ne!(
            SystemHeartbeatRequest::KIND.as_raw_u16(),
            SystemHeartbeatAck::KIND.as_raw_u16(),
        );
        // Neither collides with the substrate-owned raw SYSTEM_HEARTBEAT kind.
        assert_ne!(
            SystemHeartbeatRequest::KIND.as_raw_u16(),
            batpak::event::EventKind::SYSTEM_HEARTBEAT.as_raw_u16(),
        );
        assert_ne!(
            SystemHeartbeatAck::KIND.as_raw_u16(),
            batpak::event::EventKind::SYSTEM_HEARTBEAT.as_raw_u16(),
        );
    }

    #[test]
    fn derive_macro_kinds_match_substrate_named_constants() {
        // The derive macro produces KIND from #[batpak(category=N, type_id=M)].
        // The substrate exposes named constants that must stay byte-equal so
        // downstream consumers can reference the canonical name without having
        // to import the hbat struct.
        assert_eq!(
            SystemHeartbeatRequest::KIND.as_raw_u16(),
            batpak::event::EventKind::SYSTEM_HEARTBEAT_REQUEST.as_raw_u16(),
        );
        assert_eq!(
            SystemHeartbeatAck::KIND.as_raw_u16(),
            batpak::event::EventKind::SYSTEM_HEARTBEAT_ACK.as_raw_u16(),
        );
    }
}
