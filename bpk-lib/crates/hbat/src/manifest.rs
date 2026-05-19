//! Explicit descriptor registry consumed by both `xtask export-ts-manifest`
//! and the `hbat` binary.
//!
//! `#[derive(EventPayload)]` registers payloads via the `inventory` crate,
//! which collects per-binary. `xtask` cannot see registrations linked into
//! the `hbat` binary. The function [`descriptors`] is the Phase 0 shim
//! that lets both callers materialize the same descriptor set without
//! requiring a cross-binary discovery story.

use batpak::event::EventPayload;
use netbat::{encode_request, encode_response, NetbatError};
use serde::Serialize;
use syncbat::RuntimeError;

use crate::bank::{
    BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, BANK_COMMIT_INPUT_SCHEMA_REF,
    BANK_COMMIT_OPERATION_NAME, BANK_COMMIT_OUTPUT_SCHEMA_REF, BANK_COMMIT_RECEIPT_KIND,
    EVENT_GET_INPUT_SCHEMA_REF, EVENT_GET_OPERATION_NAME, EVENT_GET_OUTPUT_SCHEMA_REF,
    EVENT_GET_RECEIPT_KIND,
};
use crate::heartbeat::{
    HEARTBEAT_INPUT_SCHEMA_REF, HEARTBEAT_OPERATION_NAME, HEARTBEAT_OUTPUT_SCHEMA_REF,
    HEARTBEAT_RECEIPT_KIND,
};
use crate::{EventPayloadFixture, SystemHeartbeatAck, SystemHeartbeatRequest};

/// Wire/manifest version emitted in the JSON envelope.
pub const MANIFEST_VERSION: u32 = 1;

/// Deterministic nonce used in the manifest fixture for both heartbeat
/// request and ack.
pub const FIXTURE_NONCE: &str = "heartbeat-fixture-0001";

/// Deterministic `server_ts_ms` used in the manifest fixture for the ack.
pub const FIXTURE_SERVER_TS_MS: u64 = 1_700_000_000_000;

/// Operation name used in the deterministic `unknown_operation` error
/// fixture.
pub const ERROR_FIXTURE_OPERATION: &str = "system.heartbeat.nope";

/// Snapshot returned by [`descriptors`].
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
    /// Stable schema reference.
    pub name: String,
    /// Fully-qualified Rust type path.
    pub rust_type: String,
    /// PascalCase TypeScript symbol.
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
    /// TS field name. Phase 0 invariant: `ts_name == wire_name`.
    pub ts_name: String,
    /// Canonical type token consumed by the codegen.
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
    /// Mirrors the input event's golden_payload_hex.
    pub golden_input_hex: String,
    /// Mirrors the output event's golden_payload_hex.
    pub golden_output_hex: String,
    /// Lowercase hex of the complete NETBAT/1 CALL frame including `\n`.
    pub golden_request_frame_hex: String,
    /// Lowercase hex of the complete `OK <hex>\n` frame for the fixture
    /// output.
    pub golden_ok_frame_hex: String,
    /// Deterministic ERR-frame fixture produced by an unknown-operation
    /// call. Each operation gets one to prove the ERR-path is consistent
    /// regardless of which verb the client probes.
    pub error_fixture: ManifestErrorFixture,
}

/// Deterministic NETBAT/1 ERR frame fixture for the manifest.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestErrorFixture {
    /// Short identifier matching `code` below.
    pub name: String,
    /// Lowercase hex of the CALL frame that triggers this error.
    pub request_frame_hex: String,
    /// Lowercase hex of the resulting ERR frame.
    pub err_frame_hex: String,
    /// Stable ASCII token from `NetbatError::code()`.
    pub code: String,
    /// UTF-8 text the hex-encoded message portion decodes to.
    pub message_utf8: String,
}

/// Build the 0.7.6 descriptor snapshot for hbat's operation surface.
#[must_use]
pub fn descriptors() -> ManifestSnapshot {
    let request_event = describe_heartbeat_request_event();
    let ack_event = describe_heartbeat_ack_event();
    let bank_commit_request_event = describe_bank_commit_request_event();
    let bank_commit_ack_event = describe_bank_commit_ack_event();
    let event_get_request_event = describe_event_get_request_event();
    let event_get_ack_event = describe_event_get_ack_event();

    let heartbeat_op = build_operation(
        HEARTBEAT_OPERATION_NAME,
        HEARTBEAT_INPUT_SCHEMA_REF,
        HEARTBEAT_OUTPUT_SCHEMA_REF,
        HEARTBEAT_RECEIPT_KIND,
        &request_event.golden_payload_hex,
        &ack_event.golden_payload_hex,
    );

    let bank_commit_op = build_operation(
        BANK_COMMIT_OPERATION_NAME,
        BANK_COMMIT_INPUT_SCHEMA_REF,
        BANK_COMMIT_OUTPUT_SCHEMA_REF,
        BANK_COMMIT_RECEIPT_KIND,
        &bank_commit_request_event.golden_payload_hex,
        &bank_commit_ack_event.golden_payload_hex,
    );

    let event_get_op = build_operation(
        EVENT_GET_OPERATION_NAME,
        EVENT_GET_INPUT_SCHEMA_REF,
        EVENT_GET_OUTPUT_SCHEMA_REF,
        EVENT_GET_RECEIPT_KIND,
        &event_get_request_event.golden_payload_hex,
        &event_get_ack_event.golden_payload_hex,
    );

    ManifestSnapshot {
        events: vec![
            request_event,
            ack_event,
            bank_commit_request_event,
            bank_commit_ack_event,
            event_get_request_event,
            event_get_ack_event,
        ],
        operations: vec![heartbeat_op, bank_commit_op, event_get_op],
    }
}

fn build_operation(
    op_name: &str,
    input_schema_ref: &str,
    output_schema_ref: &str,
    receipt_kind: &str,
    golden_input_hex: &str,
    golden_output_hex: &str,
) -> OperationDescriptorRecord {
    let input_bytes = decode_hex(golden_input_hex).expect("hex internally produced");
    let output_bytes = decode_hex(golden_output_hex).expect("hex internally produced");
    let request_frame = encode_request(op_name, &input_bytes);
    let ok_frame = encode_response(Ok(&output_bytes));
    let error_fixture = build_error_fixture(&input_bytes);
    OperationDescriptorRecord {
        name: op_name.to_owned(),
        input_event: input_schema_ref.to_owned(),
        output_event: output_schema_ref.to_owned(),
        input_schema_ref: input_schema_ref.to_owned(),
        output_schema_ref: output_schema_ref.to_owned(),
        receipt_kind: receipt_kind.to_owned(),
        golden_input_hex: golden_input_hex.to_owned(),
        golden_output_hex: golden_output_hex.to_owned(),
        golden_request_frame_hex: encode_hex(&request_frame),
        golden_ok_frame_hex: encode_hex(&ok_frame),
        error_fixture,
    }
}

fn describe_heartbeat_request_event() -> EventDescriptor {
    let fixture = SystemHeartbeatRequest::fixture_value();
    let payload_bytes = batpak::encoding::to_bytes(&fixture).expect("encode heartbeat request");
    let fixture_json = serde_json::to_value(&fixture).expect("json-shaped");
    EventDescriptor {
        name: HEARTBEAT_INPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::heartbeat::SystemHeartbeatRequest".to_owned(),
        ts_name: "SystemHeartbeatRequest".to_owned(),
        category: SystemHeartbeatRequest::KIND.category(),
        type_id: SystemHeartbeatRequest::KIND.type_id(),
        fields: vec![field("nonce", "string", 0)],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn describe_heartbeat_ack_event() -> EventDescriptor {
    let fixture = SystemHeartbeatAck::fixture_value();
    let payload_bytes = batpak::encoding::to_bytes(&fixture).expect("encode heartbeat ack");
    let fixture_json = serde_json::to_value(&fixture).expect("json-shaped");
    EventDescriptor {
        name: HEARTBEAT_OUTPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::heartbeat::SystemHeartbeatAck".to_owned(),
        ts_name: "SystemHeartbeatAck".to_owned(),
        category: SystemHeartbeatAck::KIND.category(),
        type_id: SystemHeartbeatAck::KIND.type_id(),
        fields: vec![
            field("nonce", "string", 0),
            field("server_ts_ms", "u64-millis", 1),
        ],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn describe_bank_commit_request_event() -> EventDescriptor {
    let fixture = BankCommitRequest::fixture_value();
    let payload_bytes = batpak::encoding::to_bytes(&fixture).expect("encode bank.commit request");
    let fixture_json = serde_json::to_value(&fixture).expect("json-shaped");
    EventDescriptor {
        name: BANK_COMMIT_INPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::bank::BankCommitRequest".to_owned(),
        ts_name: "BankCommitRequest".to_owned(),
        category: BankCommitRequest::KIND.category(),
        type_id: BankCommitRequest::KIND.type_id(),
        fields: vec![
            field("entity", "string", 0),
            field("scope", "string", 1),
            field("kind_category", "u8", 2),
            field("kind_type_id", "u16", 3),
            field("payload_hex", "string", 4),
        ],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn describe_bank_commit_ack_event() -> EventDescriptor {
    let fixture = BankCommitAck::fixture_value();
    let payload_bytes = batpak::encoding::to_bytes(&fixture).expect("encode bank.commit ack");
    let fixture_json = serde_json::to_value(&fixture).expect("json-shaped");
    EventDescriptor {
        name: BANK_COMMIT_OUTPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::bank::BankCommitAck".to_owned(),
        ts_name: "BankCommitAck".to_owned(),
        category: BankCommitAck::KIND.category(),
        type_id: BankCommitAck::KIND.type_id(),
        fields: vec![
            field("event_id_hex", "string", 0),
            field("sequence", "u64-safe", 1),
            field("content_hash_hex", "string", 2),
            field("key_id_hex", "string", 3),
            field("signature_hex", "option<string>", 4),
            field("extensions", "map<string,string>", 5),
        ],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn describe_event_get_request_event() -> EventDescriptor {
    let fixture = EventGetRequest::fixture_value();
    let payload_bytes = batpak::encoding::to_bytes(&fixture).expect("encode event.get request");
    let fixture_json = serde_json::to_value(&fixture).expect("json-shaped");
    EventDescriptor {
        name: EVENT_GET_INPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::bank::EventGetRequest".to_owned(),
        ts_name: "EventGetRequest".to_owned(),
        category: EventGetRequest::KIND.category(),
        type_id: EventGetRequest::KIND.type_id(),
        fields: vec![field("event_id_hex", "string", 0)],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn describe_event_get_ack_event() -> EventDescriptor {
    let fixture = EventGetAck::fixture_value();
    let payload_bytes = batpak::encoding::to_bytes(&fixture).expect("encode event.get ack");
    let fixture_json = serde_json::to_value(&fixture).expect("json-shaped");
    EventDescriptor {
        name: EVENT_GET_OUTPUT_SCHEMA_REF.to_owned(),
        rust_type: "hbat::bank::EventGetAck".to_owned(),
        ts_name: "EventGetAck".to_owned(),
        category: EventGetAck::KIND.category(),
        type_id: EventGetAck::KIND.type_id(),
        fields: vec![
            field("event_id_hex", "string", 0),
            field("sequence", "u64-safe", 1),
            field("timestamp_us", "i64-microseconds", 2),
            field("correlation_id_hex", "string", 3),
            field("causation_id_hex", "option<string>", 4),
            field("kind_category", "u8", 5),
            field("kind_type_id", "u16", 6),
            field("entity", "string", 7),
            field("scope", "string", 8),
            field("payload_hex", "string", 9),
            field("content_hash_hex", "string", 10),
        ],
        fixture_value: fixture_json,
        golden_payload_hex: encode_hex(&payload_bytes),
    }
}

fn field(name: &str, type_token: &str, order: usize) -> FieldDescriptor {
    FieldDescriptor {
        wire_name: name.to_owned(),
        ts_name: name.to_owned(),
        type_token: type_token.to_owned(),
        order,
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

/// Decode a hex string back to bytes.
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
#[allow(clippy::panic, clippy::unwrap_used, clippy::assertions_on_constants)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_has_all_0_7_6_events_and_operations() {
        let snap = descriptors();
        // 6 events: heartbeat request/ack, bank.commit request/ack, event.get request/ack.
        assert_eq!(snap.events.len(), 6, "event count: {snap:?}");
        // 3 operations: heartbeat, bank.commit, event.get.
        assert_eq!(snap.operations.len(), 3);
        let names: Vec<&str> = snap.operations.iter().map(|o| o.name.as_str()).collect();
        assert!(names.contains(&"system.heartbeat"));
        assert!(names.contains(&"bank.commit"));
        assert!(names.contains(&"event.get"));
    }

    #[test]
    fn fixture_field_metadata_matches_phase0_invariants() {
        let snap = descriptors();
        for event in &snap.events {
            for field in &event.fields {
                assert_eq!(
                    field.wire_name, field.ts_name,
                    "wireName/tsName drift on {}.{}",
                    event.name, field.wire_name
                );
            }
        }
    }

    #[test]
    fn all_operations_carry_an_error_fixture() {
        let snap = descriptors();
        for op in &snap.operations {
            assert_eq!(op.error_fixture.code, "unknown_operation");
            assert_eq!(op.error_fixture.name, "unknown_operation");
            assert!(op
                .error_fixture
                .message_utf8
                .contains(ERROR_FIXTURE_OPERATION));
        }
    }

    #[test]
    fn each_event_decodes_back_to_its_fixture_value() {
        let snap = descriptors();
        for event in &snap.events {
            let bytes = decode_hex(&event.golden_payload_hex).expect("decode golden hex");
            // Re-encode the fixture value through serde_json roundtrip to
            // confirm the JSON form is structurally consistent with what
            // msgpack decode would produce.
            let payload_json: serde_json::Value =
                rmp_serde::from_slice(&bytes).expect("decode msgpack");
            assert_eq!(payload_json, event.fixture_value, "event {}", event.name);
        }
    }
}
