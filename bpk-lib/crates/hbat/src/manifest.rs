//! Inventory-driven descriptor registry consumed by both
//! `xtask export-ts-manifest` and the `hbat` binary.
//!
//! Each `EventPayload`-deriving module submits an
//! [`EventDescriptorRegistration`] via the `inventory` crate. Each
//! operation module submits an [`OperationDescriptorRegistration`] next
//! to its `*_DESCRIPTOR` const. The function [`descriptors`] iterates both
//! registries at runtime and materializes the [`ManifestSnapshot`] — no
//! centralized hand-rolled describe table for events or operations.

use netbat::{encode_request, encode_response, NetbatError};
use serde::Serialize;
use syncbat::{OperationDescriptor, RuntimeError};

/// Manifest construction failure.
#[derive(Debug)]
pub enum ManifestBuildError {
    /// A registered event failed to provide canonical fixture bytes.
    FixtureBytes {
        /// Fully-qualified Rust type that failed.
        rust_type: &'static str,
    },
    /// A registered event failed to provide its JSON fixture view.
    FixtureJson {
        /// Fully-qualified Rust type that failed.
        rust_type: &'static str,
    },
    /// An operation refers to an event schema ref that was not registered.
    MissingSchemaRef {
        /// Missing schema reference.
        schema_ref: String,
    },
    /// A manifest-owned golden hex string did not decode.
    GoldenHex {
        /// Golden field being decoded.
        field: &'static str,
        /// Decode error text.
        error: String,
    },
}

impl std::fmt::Display for ManifestBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FixtureBytes { rust_type } => {
                write!(f, "encode fixture bytes for {rust_type}")
            }
            Self::FixtureJson { rust_type } => write!(f, "encode fixture json for {rust_type}"),
            Self::MissingSchemaRef { schema_ref } => {
                write!(f, "event descriptor missing for schema ref {schema_ref}")
            }
            Self::GoldenHex { field, error } => {
                write!(f, "decode manifest golden hex field {field}: {error}")
            }
        }
    }
}

impl std::error::Error for ManifestBuildError {}

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

/// One static field row submitted alongside an
/// [`EventDescriptorRegistration`]. Mirrors [`FieldDescriptor`] but uses
/// `&'static str` so the row can live in static memory next to the
/// `inventory::submit!` call.
#[derive(Clone, Copy, Debug)]
pub struct FieldRow {
    /// Exact serde key on the wire. `ts_name` equals this string today
    /// per the Phase 0 invariant.
    pub wire_name: &'static str,
    /// Canonical type token consumed by the codegen.
    pub type_token: &'static str,
    /// Declaration order, starting at 0.
    pub order: usize,
}

/// Compile-time inventory entry for one `EventPayload`-deriving struct.
///
/// Each event-defining module emits one of these via `inventory::submit!`
/// next to its `#[derive(EventPayload)]`. The [`descriptors`] function
/// iterates them at runtime and produces the full [`EventDescriptor`]
/// list. The fixture closures defer encoding/JSON work to manifest
/// export time so callers that only consume the schema-ref constants do
/// not pay it.
pub struct EventDescriptorRegistration {
    /// Fully-qualified Rust type path (e.g. `hbat::heartbeat::SystemHeartbeatRequest`).
    pub rust_type: &'static str,
    /// PascalCase TypeScript symbol for the type.
    pub ts_name: &'static str,
    /// Stable schema reference advertised by the operation descriptor.
    pub schema_ref: &'static str,
    /// Packed `(category << 12) | type_id` — equals `EventKind::as_raw_u16()`.
    pub kind_bits: u16,
    /// Field rows in declaration order.
    pub fields: &'static [FieldRow],
    /// Returns the canonically-encoded fixture payload bytes. Returns
    /// `None` if encoding fails; manifest construction reports the
    /// `rust_type` so the failing event is named in the diagnostic.
    pub fixture_bytes: fn() -> Option<Vec<u8>>,
    /// Returns the JSON view of the fixture value.
    pub fixture_json: fn() -> Option<serde_json::Value>,
}

inventory::collect!(EventDescriptorRegistration);

/// Compile-time inventory entry for one syncbat operation exposed to the
/// TS-binding manifest.
///
/// Each operation-defining module emits one of these via `inventory::submit!`
/// next to its `*_DESCRIPTOR` const. The [`descriptors`] function iterates
/// them at runtime and materializes [`OperationDescriptorRecord`] rows.
pub struct OperationDescriptorRegistration {
    /// Returns the syncbat descriptor — single source of truth for name/schema refs.
    pub descriptor: fn() -> &'static OperationDescriptor,
}

inventory::collect!(OperationDescriptorRegistration);

impl OperationDescriptorRegistration {
    fn materialize(
        &self,
        index: &EventIndex<'_>,
    ) -> Result<OperationDescriptorRecord, ManifestBuildError> {
        let desc = (self.descriptor)();
        build_operation(
            desc.name(),
            desc.input_schema_ref(),
            desc.output_schema_ref(),
            desc.receipt_kind(),
            index.golden_for(desc.input_schema_ref())?,
            index.golden_for(desc.output_schema_ref())?,
        )
    }
}

impl EventDescriptorRegistration {
    fn materialize(&self) -> Result<EventDescriptor, ManifestBuildError> {
        let payload_bytes = (self.fixture_bytes)().ok_or(ManifestBuildError::FixtureBytes {
            rust_type: self.rust_type,
        })?;
        let fixture_value = (self.fixture_json)().ok_or(ManifestBuildError::FixtureJson {
            rust_type: self.rust_type,
        })?;
        // justifies: ADR-0010, src/event/kind.rs; kind_bits upper nibble fits in u8 by construction so narrowing into u8 cannot truncate
        #[allow(clippy::cast_possible_truncation)]
        let category = (self.kind_bits >> 12) as u8;
        let type_id = self.kind_bits & 0x0FFF;
        Ok(EventDescriptor {
            name: self.schema_ref.to_owned(),
            rust_type: self.rust_type.to_owned(),
            ts_name: self.ts_name.to_owned(),
            category,
            type_id,
            fields: self
                .fields
                .iter()
                .map(|row| FieldDescriptor {
                    wire_name: row.wire_name.to_owned(),
                    ts_name: row.wire_name.to_owned(),
                    type_token: row.type_token.to_owned(),
                    order: row.order,
                })
                .collect(),
            fixture_value,
            golden_payload_hex: encode_hex(&payload_bytes),
        })
    }
}

/// Build the descriptor snapshot for hbat's operation surface.
///
/// Walks the `inventory` registry, materializes one [`EventDescriptor`]
/// per submitted [`EventDescriptorRegistration`], sorts by
/// `(category, type_id)` for link-order stability, then materializes the
/// operation table from [`OperationDescriptorRegistration`] entries sorted
/// by operation name.
///
/// # Errors
/// Returns [`ManifestBuildError`] if a registered fixture cannot be
/// materialized or if an operation references a missing event descriptor.
pub fn descriptors() -> Result<ManifestSnapshot, ManifestBuildError> {
    let mut events: Vec<EventDescriptor> = inventory::iter::<EventDescriptorRegistration>
        .into_iter()
        .map(EventDescriptorRegistration::materialize)
        .collect::<Result<_, _>>()?;
    // Link order is implementation-defined; sort by the wire-stable
    // (category, type_id) pair so the manifest JSON is byte-identical
    // across builds.
    events.sort_by_key(|event| (event.category, event.type_id));

    let index = EventIndex::new(&events);

    let mut operations: Vec<OperationDescriptorRecord> =
        inventory::iter::<OperationDescriptorRegistration>
            .into_iter()
            .map(|registration| registration.materialize(&index))
            .collect::<Result<_, _>>()?;
    operations.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(ManifestSnapshot { events, operations })
}

/// Internal lookup from schema-ref to materialized golden-payload hex.
/// Each operation row carries the same hex string already on its event
/// descriptor; the index just lets `build_operation` find it without
/// duplicating the descriptor walk.
struct EventIndex<'a> {
    by_schema_ref: std::collections::HashMap<&'a str, &'a str>,
}

impl<'a> EventIndex<'a> {
    fn new(events: &'a [EventDescriptor]) -> Self {
        let mut by_schema_ref = std::collections::HashMap::with_capacity(events.len());
        for event in events {
            by_schema_ref.insert(event.name.as_str(), event.golden_payload_hex.as_str());
        }
        Self { by_schema_ref }
    }

    fn golden_for(&self, schema_ref: &str) -> Result<&'a str, ManifestBuildError> {
        self.by_schema_ref.get(schema_ref).copied().ok_or_else(|| {
            ManifestBuildError::MissingSchemaRef {
                schema_ref: schema_ref.to_owned(),
            }
        })
    }
}

fn build_operation(
    op_name: &str,
    input_schema_ref: &str,
    output_schema_ref: &str,
    receipt_kind: &str,
    golden_input_hex: &str,
    golden_output_hex: &str,
) -> Result<OperationDescriptorRecord, ManifestBuildError> {
    let input_bytes =
        decode_hex(golden_input_hex).map_err(|error| ManifestBuildError::GoldenHex {
            field: "golden_input_hex",
            error: error.to_string(),
        })?;
    let output_bytes =
        decode_hex(golden_output_hex).map_err(|error| ManifestBuildError::GoldenHex {
            field: "golden_output_hex",
            error: error.to_string(),
        })?;
    let request_frame = encode_request(op_name, &input_bytes);
    let ok_frame = encode_response(Ok(&output_bytes));
    let error_fixture = build_error_fixture(&input_bytes);
    Ok(OperationDescriptorRecord {
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
    })
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

// Hex codec lives in `netbat::transport::hex`. The canonical
// implementation is re-exported via `netbat::encode_hex_str` /
// `netbat::decode_hex_str`. The `pub fn encode_hex` that used to live
// here is preserved as a re-export below so existing callers continue
// to compile.

/// Lowercase-hex encode the given bytes.
///
/// Deprecated wrapper kept for backward compatibility with internal
/// callers that imported `crate::manifest::encode_hex`. New code
/// should call [`netbat::encode_hex_str`] directly.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    netbat::encode_hex_str(bytes)
}

/// Decode a hex string back to bytes via `netbat::decode_hex_str`.
fn decode_hex(hex: &str) -> Result<Vec<u8>, netbat::NetbatError> {
    netbat::decode_hex_str(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn snapshot_has_all_reference_events_and_operations() -> Result<()> {
        let snap = descriptors()?;
        // 9 events: heartbeat request/ack, bank.commit request/ack,
        // event.get request/ack, event.query request/summary/ack.
        assert_eq!(snap.events.len(), 9, "event count: {snap:?}");
        // 4 operations: heartbeat, bank.commit, event.get, event.query.
        assert_eq!(snap.operations.len(), 4);
        let names: Vec<&str> = snap.operations.iter().map(|o| o.name.as_str()).collect();
        assert!(names.contains(&"system.heartbeat"));
        assert!(names.contains(&"bank.commit"));
        assert!(names.contains(&"event.get"));
        assert!(names.contains(&"event.query"));
        Ok(())
    }

    #[test]
    fn fixture_field_metadata_matches_phase0_invariants() -> Result<()> {
        let snap = descriptors()?;
        for event in &snap.events {
            for field in &event.fields {
                assert_eq!(
                    field.wire_name, field.ts_name,
                    "wireName/tsName drift on {}.{}",
                    event.name, field.wire_name
                );
            }
        }
        Ok(())
    }

    #[test]
    fn all_operations_carry_an_error_fixture() -> Result<()> {
        let snap = descriptors()?;
        for op in &snap.operations {
            assert_eq!(op.error_fixture.code, "unknown_operation");
            assert_eq!(op.error_fixture.name, "unknown_operation");
            assert!(op
                .error_fixture
                .message_utf8
                .contains(ERROR_FIXTURE_OPERATION));
        }
        Ok(())
    }

    #[test]
    fn each_event_decodes_back_to_its_fixture_value() -> Result<()> {
        let snap = descriptors()?;
        for event in &snap.events {
            let bytes = decode_hex(&event.golden_payload_hex)?;
            // Re-encode the fixture value through serde_json roundtrip to
            // confirm the JSON form is structurally consistent with what
            // msgpack decode would produce.
            let payload_json: serde_json::Value = batpak::encoding::from_bytes(&bytes)?;
            assert_eq!(payload_json, event.fixture_value, "event {}", event.name);
        }
        Ok(())
    }

    #[test]
    fn events_sort_by_kind_for_link_order_stability() -> Result<()> {
        let snap = descriptors()?;
        let pairs: Vec<(u8, u16)> = snap
            .events
            .iter()
            .map(|e| (e.category, e.type_id))
            .collect();
        let mut sorted = pairs.clone();
        sorted.sort();
        assert_eq!(
            pairs, sorted,
            "events must be sorted by (category, type_id)"
        );
        Ok(())
    }

    #[test]
    fn operations_sort_by_name_for_link_order_stability() -> Result<()> {
        let snap = descriptors()?;
        let names: Vec<&str> = snap.operations.iter().map(|op| op.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "operations must be sorted by name");
        Ok(())
    }
}
