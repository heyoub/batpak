//! Explicit descriptor registry consumed by both `xtask export-ts-manifest`
//! and the `hbat` binary.
//!
//! `#[derive(EventPayload)]` registers payloads via the `inventory` crate,
//! which collects per-binary. `xtask` cannot see registrations linked into
//! the `hbat` binary. The function [`descriptors`] is the Phase 0 shim
//! that lets both callers materialize the same descriptor set without
//! requiring a cross-binary discovery story. Once inventory can span
//! binaries (or descriptor metadata is emitted by the derive macro into
//! a different mechanism), this shim retires.

use batpak::event::EventPayload;
use netbat::{encode_request, encode_response, NetbatError};
use serde::Serialize;
use syncbat::RuntimeError;

use crate::heartbeat::{
    HEARTBEAT_INPUT_SCHEMA_REF, HEARTBEAT_OPERATION_NAME, HEARTBEAT_OUTPUT_SCHEMA_REF,
    HEARTBEAT_RECEIPT_KIND,
};
use crate::{EventPayloadFixture, SystemHeartbeatAck, SystemHeartbeatRequest};

/// Wire/manifest version emitted in the JSON envelope.
pub const MANIFEST_VERSION: u32 = 1;

/// Deterministic nonce used in the manifest fixture for both request and
/// ack. The live `hbat` handler echoes whatever nonce the caller sent; the
/// fixture-locked value here is only used to compute golden hex.
pub const FIXTURE_NONCE: &str = "heartbeat-fixture-0001";

/// Deterministic `server_ts_ms` used in the manifest fixture for the ack.
/// The live `hbat` handler stamps real wall-clock time; tests that need
/// byte-exact ack goldens use this constant instead.
///
/// Value: `1_700_000_000_000` — well under `Number.MAX_SAFE_INTEGER`
/// (`2^53 - 1 = 9_007_199_254_740_991`).
pub const FIXTURE_SERVER_TS_MS: u64 = 1_700_000_000_000;

/// Operation name used in the deterministic `unknown_operation` error
/// fixture. The grammar-valid `system.heartbeat.nope` name is not
/// registered in the runtime, so the request always fails before reaching
/// a handler.
pub const ERROR_FIXTURE_OPERATION: &str = "system.heartbeat.nope";

/// Snapshot returned by [`descriptors`]. Consumed by `xtask` to assemble
/// the BatPAK TS manifest JSON, and by `hbat` if it ever wants to inspect
/// its own catalog.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSnapshot {
    /// Event payload descriptors registered with this snapshot.
    pub events: Vec<EventDescriptor>,
    /// Operation descriptors registered with this snapshot.
    pub operations: Vec<OperationDescriptorRecord>,
}

/// Descriptor for one `EventPayload`-deriving struct exposed to the
/// TS-binding manifest.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDescriptor {
    /// Stable schema reference (e.g. `"system.heartbeat.request"`).
    pub name: String,
    /// Fully-qualified Rust type path for the payload struct.
    pub rust_type: String,
    /// PascalCase TypeScript symbol the generator emits.
    pub ts_name: String,
    /// `EventKind` category (upper 4 bits).
    pub category: u8,
    /// `EventKind` type id (lower 12 bits).
    pub type_id: u16,
    /// Field metadata in serde declaration order.
    pub fields: Vec<FieldDescriptor>,
    /// JSON form of the Phase 0 deterministic fixture value.
    pub fixture_value: serde_json::Value,
    /// Lowercase hex of `batpak::encoding::to_bytes(fixture_value)`.
    pub golden_payload_hex: String,
}

/// Field metadata for one payload field.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDescriptor {
    /// Exact serde key on the wire.
    pub wire_name: String,
    /// TS field name. **Phase 0 invariant: `ts_name == wire_name`.** No
    /// camelCase / snake_case transform is performed on the canonical
    /// encode/decode path; a presentational adapter outside the canonical
    /// boundary is a follow-on concern.
    pub ts_name: String,
    /// Canonical type token consumed by the codegen
    /// (`"string"`, `"u64-millis"`, ...).
    pub type_token: String,
    /// Declaration order, starting at 0.
    pub order: usize,
}

/// Descriptor for one syncbat operation exposed to the TS-binding manifest.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationDescriptorRecord {
    /// Stable operation name used on the NETBAT/1 wire.
    pub name: String,
    /// Schema name of the input event.
    pub input_event: String,
    /// Schema name of the output event.
    pub output_event: String,
    /// Mirrors `OperationDescriptor::input_schema_ref()`.
    pub input_schema_ref: String,
    /// Mirrors `OperationDescriptor::output_schema_ref()`.
    pub output_schema_ref: String,
    /// Mirrors `OperationDescriptor::receipt_kind()`.
    pub receipt_kind: String,
    /// Mirrors `events[input_event].golden_payload_hex`.
    pub golden_input_hex: String,
    /// Mirrors `events[output_event].golden_payload_hex`.
    pub golden_output_hex: String,
    /// Lowercase hex of the complete NETBAT/1 CALL frame including `\n`.
    pub golden_request_frame_hex: String,
    /// Lowercase hex of the complete `OK <hex>\n` frame for the fixture
    /// output. Live roundtrip tests do not assert against this — only the
    /// fixture-path tests do — because the live ack uses a real clock.
    pub golden_ok_frame_hex: String,
    /// Deterministic ERR-frame fixture produced by an unknown-operation
    /// call. Stable across handler edits because the request never reaches
    /// a handler.
    pub error_fixture: ManifestErrorFixture,
}

/// Deterministic NETBAT/1 ERR frame fixture for the manifest.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestErrorFixture {
    /// Short identifier (`"unknown_operation"`). Matches `code` below.
    pub name: String,
    /// Lowercase hex of the CALL frame that triggers this error.
    pub request_frame_hex: String,
    /// Lowercase hex of the resulting ERR frame.
    pub err_frame_hex: String,
    /// Stable ASCII token reported on the wire by
    /// `netbat::NetbatError::code()`.
    pub code: String,
    /// UTF-8 text the hex-encoded message portion decodes to.
    pub message_utf8: String,
}

/// Build the Phase 0 descriptor snapshot for `system.heartbeat`.
///
/// # Panics
/// Panics if canonical encoding of the fixture values fails. The encoder
/// is the same `rmp-serde`-named codec used substrate-wide; both fixture
/// values are simple `{ string, u64 }` shapes that the encoder is
/// fixture-proven to handle, so a failure here would indicate a substrate
/// regression that should fail loud at build time.
#[must_use]
pub fn descriptors() -> ManifestSnapshot {
    let request_event = describe_request_event();
    let ack_event = describe_ack_event();

    let golden_input_hex = request_event.golden_payload_hex.clone();
    let golden_output_hex = ack_event.golden_payload_hex.clone();

    let request_input_bytes =
        decode_hex(&golden_input_hex).expect("manifest request hex is internally produced");
    let request_frame = encode_request(HEARTBEAT_OPERATION_NAME, &request_input_bytes);

    let ack_output_bytes =
        decode_hex(&golden_output_hex).expect("manifest ack hex is internally produced");
    let ok_frame = encode_response(Ok(&ack_output_bytes));

    let error_fixture = build_error_fixture(&request_input_bytes);

    let operation = OperationDescriptorRecord {
        name: HEARTBEAT_OPERATION_NAME.to_owned(),
        input_event: HEARTBEAT_INPUT_SCHEMA_REF.to_owned(),
        output_event: HEARTBEAT_OUTPUT_SCHEMA_REF.to_owned(),
        input_schema_ref: HEARTBEAT_INPUT_SCHEMA_REF.to_owned(),
        output_schema_ref: HEARTBEAT_OUTPUT_SCHEMA_REF.to_owned(),
        receipt_kind: HEARTBEAT_RECEIPT_KIND.to_owned(),
        golden_input_hex,
        golden_output_hex,
        golden_request_frame_hex: encode_hex(&request_frame),
        golden_ok_frame_hex: encode_hex(&ok_frame),
        error_fixture,
    };

    ManifestSnapshot {
        events: vec![request_event, ack_event],
        operations: vec![operation],
    }
}

fn describe_request_event() -> EventDescriptor {
    let fixture = SystemHeartbeatRequest::fixture_value();
    let payload_bytes =
        batpak::encoding::to_bytes(&fixture).expect("encode SystemHeartbeatRequest fixture");
    let fixture_json =
        serde_json::to_value(&fixture).expect("SystemHeartbeatRequest fixture is JSON-shaped");
    EventDescriptor {
        name: HEARTBEAT_INPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::heartbeat::SystemHeartbeatRequest".to_owned(),
        ts_name: "SystemHeartbeatRequest".to_owned(),
        category: SystemHeartbeatRequest::KIND.category(),
        type_id: SystemHeartbeatRequest::KIND.type_id(),
        fields: vec![FieldDescriptor {
            wire_name: "nonce".to_owned(),
            ts_name: "nonce".to_owned(),
            type_token: "string".to_owned(),
            order: 0,
        }],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn describe_ack_event() -> EventDescriptor {
    let fixture = SystemHeartbeatAck::fixture_value();
    let payload_bytes =
        batpak::encoding::to_bytes(&fixture).expect("encode SystemHeartbeatAck fixture");
    let fixture_json =
        serde_json::to_value(&fixture).expect("SystemHeartbeatAck fixture is JSON-shaped");
    EventDescriptor {
        name: HEARTBEAT_OUTPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::heartbeat::SystemHeartbeatAck".to_owned(),
        ts_name: "SystemHeartbeatAck".to_owned(),
        category: SystemHeartbeatAck::KIND.category(),
        type_id: SystemHeartbeatAck::KIND.type_id(),
        fields: vec![
            FieldDescriptor {
                wire_name: "nonce".to_owned(),
                ts_name: "nonce".to_owned(),
                type_token: "string".to_owned(),
                order: 0,
            },
            FieldDescriptor {
                wire_name: "server_ts_ms".to_owned(),
                ts_name: "server_ts_ms".to_owned(),
                type_token: "u64-millis".to_owned(),
                order: 1,
            },
        ],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn build_error_fixture(request_input_bytes: &[u8]) -> ManifestErrorFixture {
    let request_frame = encode_request(ERROR_FIXTURE_OPERATION, request_input_bytes);
    let runtime_err = RuntimeError::unknown_operation(ERROR_FIXTURE_OPERATION);
    let netbat_err = NetbatError::Runtime(runtime_err);
    let message_utf8 = netbat_err.to_string();
    let code = netbat_err.code().to_owned();
    let err_frame = encode_response(Err(&netbat_err));

    ManifestErrorFixture {
        name: code.clone(),
        request_frame_hex: encode_hex(&request_frame),
        err_frame_hex: encode_hex(&err_frame),
        code,
        message_utf8,
    }
}

/// Lowercase-hex encode the given bytes.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[(*byte >> 4) as usize]));
        out.push(char::from(HEX[(*byte & 0x0F) as usize]));
    }
    out
}

/// Decode a hex string (lowercase or uppercase) back to bytes.
fn decode_hex(hex: &str) -> Result<Vec<u8>, &'static str> {
    if !hex.len().is_multiple_of(2) {
        return Err("hex string has odd length");
    }
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Result<u8, &'static str> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("non-hex character in hex string"),
    }
}

#[cfg(test)]
// justifies: INV-ALLOW-IS-DESIGN; tests in this module assert fixture invariants
// using assert!/panic! patterns. Suppressing these workspace-level denies for
// the test module only matches the precedent set by syncbat/tests/runtime.rs
// and store_sink.rs from bpk-lib/crates/syncbat/.
#[allow(clippy::panic, clippy::unwrap_used, clippy::assertions_on_constants)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_has_two_events_and_one_operation() {
        let snap = descriptors();
        assert_eq!(snap.events.len(), 2);
        assert_eq!(snap.operations.len(), 1);
    }

    #[test]
    fn event_golden_hex_decodes_back_to_fixture_value() {
        let snap = descriptors();
        let request = &snap.events[0];
        let bytes = decode_hex(&request.golden_payload_hex).expect("decode hex");
        let decoded: SystemHeartbeatRequest =
            batpak::encoding::from_bytes(&bytes).expect("decode msgpack");
        assert_eq!(decoded.nonce, FIXTURE_NONCE);

        let ack = &snap.events[1];
        let bytes = decode_hex(&ack.golden_payload_hex).expect("decode hex");
        let decoded: SystemHeartbeatAck =
            batpak::encoding::from_bytes(&bytes).expect("decode msgpack");
        assert_eq!(decoded.nonce, FIXTURE_NONCE);
        assert_eq!(decoded.server_ts_ms, FIXTURE_SERVER_TS_MS);
    }

    #[test]
    fn fixture_field_metadata_matches_phase0_invariants() {
        let snap = descriptors();
        for event in &snap.events {
            for field in &event.fields {
                assert_eq!(
                    field.wire_name, field.ts_name,
                    "Phase 0 invariant: wireName must equal tsName"
                );
            }
        }
    }

    #[test]
    fn error_fixture_code_is_unknown_operation() {
        let snap = descriptors();
        let op = &snap.operations[0];
        assert_eq!(op.error_fixture.code, "unknown_operation");
        assert_eq!(op.error_fixture.name, "unknown_operation");
    }

    #[test]
    fn error_fixture_message_is_plain_utf8_not_msgpack() {
        let snap = descriptors();
        let message = &snap.operations[0].error_fixture.message_utf8;
        assert!(
            message.contains(ERROR_FIXTURE_OPERATION),
            "error message {message:?} must mention the failed operation"
        );
        // No leading MessagePack map/array markers — this is plain text.
        let first_byte = message.as_bytes().first().copied().unwrap_or(0);
        assert!(
            first_byte.is_ascii_graphic(),
            "error message first byte {first_byte:#x} should be plain ASCII"
        );
    }

    #[test]
    fn fixture_value_is_json_subset() {
        let snap = descriptors();
        for event in &snap.events {
            assert_json_phase0_subset(&event.fixture_value);
        }
    }

    fn assert_json_phase0_subset(value: &serde_json::Value) {
        const SAFE_MAX_U: u64 = (1_u64 << 53) - 1;
        const SAFE_MAX_I: i64 = (1_i64 << 53) - 1;
        const SAFE_MIN_I: i64 = -SAFE_MAX_I;
        match value {
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_u64() {
                    assert!(
                        i <= SAFE_MAX_U,
                        "fixture integer {i} exceeds Number.MAX_SAFE_INTEGER"
                    );
                } else if let Some(i) = n.as_i64() {
                    assert!(
                        (SAFE_MIN_I..=SAFE_MAX_I).contains(&i),
                        "fixture integer {i} outside safe range"
                    );
                } else {
                    panic!("Phase 0 fixture JSON forbids non-integer numbers");
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    assert_json_phase0_subset(item);
                }
            }
            serde_json::Value::Object(map) => {
                for (_, item) in map {
                    assert_json_phase0_subset(item);
                }
            }
        }
    }
}
