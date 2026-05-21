//! `bank.commit` and `event.get` operation surface.
//!
//! These are the irreducible verbs a NETBAT/1 client needs to do real work:
//! commit a typed event into the underlying [`batpak::store::Store`] and
//! retrieve a previously-committed event by its `event_id`.
//!
//! Both verbs are exposed via [`OperationDescriptor`] constants here, with
//! request/response payload types deriving [`batpak::EventPayload`] so the
//! TypeScript SDK manifest carries their full shape. The actual handlers
//! capture a runtime `Arc<batpak::store::Store>` and live in
//! [`crate::handlers`].

use std::collections::BTreeMap;

use batpak::EventPayload;
use serde::{Deserialize, Serialize};
use syncbat::{EffectClass, OperationDescriptor};

use crate::EventPayloadFixture;

// ─── bank.commit ────────────────────────────────────────────────────────────

/// Stable operation name for committing a typed event into the BatPAK store.
pub const BANK_COMMIT_OPERATION_NAME: &str = "bank.commit";
/// Schema reference for the request payload.
pub const BANK_COMMIT_INPUT_SCHEMA_REF: &str = "bank.commit.request";
/// Schema reference for the ack payload.
pub const BANK_COMMIT_OUTPUT_SCHEMA_REF: &str = "bank.commit.ack";
/// Receipt kind emitted for `bank.commit` calls.
pub const BANK_COMMIT_RECEIPT_KIND: &str = "receipt.bank.commit.v1";

/// Operation descriptor for `bank.commit`.
pub const BANK_COMMIT_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    BANK_COMMIT_OPERATION_NAME,
    EffectClass::Persist,
    BANK_COMMIT_INPUT_SCHEMA_REF,
    BANK_COMMIT_OUTPUT_SCHEMA_REF,
    BANK_COMMIT_RECEIPT_KIND,
);

/// Wire input for [`BANK_COMMIT_DESCRIPTOR`].
///
/// The client supplies the target coordinate (`entity` + `scope`), the
/// numeric kind discriminants (4-bit category + 12-bit type_id), and the
/// already-canonically-encoded payload bytes as lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA10)]
pub struct BankCommitRequest {
    /// Coordinate entity name. Validated against
    /// `batpak::coordinate::Coordinate::new`.
    pub entity: String,
    /// Coordinate scope name.
    pub scope: String,
    /// Event-kind upper 4 bits (1..=15, excluding 0 and 0xD which are
    /// substrate-reserved).
    pub kind_category: u8,
    /// Event-kind lower 12 bits (0..=0xFFF).
    pub kind_type_id: u16,
    /// Lowercase hex of the canonically-encoded payload bytes.
    pub payload_hex: String,
}

/// Wire output for [`BANK_COMMIT_DESCRIPTOR`]. Mirrors
/// [`batpak::store::AppendReceipt`] with all binary fields rendered as
/// lowercase hex so the TS side stays Number.MAX_SAFE_INTEGER-safe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA11)]
pub struct BankCommitAck {
    /// `event_id` as 32-char lowercase hex (`u128` rendered big-endian).
    pub event_id_hex: String,
    /// Monotonic global sequence number assigned at commit. Bounded to
    /// `Number.MAX_SAFE_INTEGER` on the wire.
    pub sequence: u64,
    /// Blake3-32 content hash of the payload, as 64-char lowercase hex.
    pub content_hash_hex: String,
    /// Signing-key identity, as 64-char lowercase hex. All zeros when
    /// receipt signing is disabled.
    pub key_id_hex: String,
    /// Detached Ed25519 signature over the receipt fields, as 128-char
    /// lowercase hex. `None` when receipt signing is disabled.
    pub signature_hex: Option<String>,
    /// Receipt-extension map. Keys are the full extension key strings
    /// (e.g. `"syncbat.descriptor"`); values are lowercase hex of the
    /// raw extension bytes.
    pub extensions: BTreeMap<String, String>,
}

impl EventPayloadFixture for BankCommitRequest {
    fn fixture_value() -> Self {
        Self {
            entity: "fixture:bank".to_owned(),
            scope: "fixture-scope".to_owned(),
            kind_category: 0xF,
            kind_type_id: 0xA01,
            // Matches the SystemHeartbeatRequest fixture payload exactly.
            payload_hex: "81a56e6f6e6365b66865617274626561742d666978747572652d30303031".to_owned(),
        }
    }
}

impl EventPayloadFixture for BankCommitAck {
    fn fixture_value() -> Self {
        Self {
            event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
            sequence: 42,
            content_hash_hex: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
            key_id_hex: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_owned(),
            signature_hex: None,
            extensions: {
                let mut m = BTreeMap::new();
                m.insert(
                    "syncbat.descriptor".to_owned(),
                    netbat::encode_hex_str(b"bank.commit"),
                );
                m
            },
        }
    }
}

// ─── event.get ──────────────────────────────────────────────────────────────

/// Stable operation name for fetching a stored event by its `event_id`.
pub const EVENT_GET_OPERATION_NAME: &str = "event.get";
/// Schema reference for the request payload.
pub const EVENT_GET_INPUT_SCHEMA_REF: &str = "event.get.request";
/// Schema reference for the ack payload.
pub const EVENT_GET_OUTPUT_SCHEMA_REF: &str = "event.get.ack";
/// Receipt kind emitted for `event.get` calls.
pub const EVENT_GET_RECEIPT_KIND: &str = "receipt.event.get.v1";

/// Operation descriptor for `event.get`.
pub const EVENT_GET_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    EVENT_GET_OPERATION_NAME,
    EffectClass::Inspect,
    EVENT_GET_INPUT_SCHEMA_REF,
    EVENT_GET_OUTPUT_SCHEMA_REF,
    EVENT_GET_RECEIPT_KIND,
);

/// Wire input for [`EVENT_GET_DESCRIPTOR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA20)]
pub struct EventGetRequest {
    /// `event_id` as 32-char lowercase hex.
    pub event_id_hex: String,
}

/// Wire output for [`EVENT_GET_DESCRIPTOR`].
///
/// Combines the substrate's [`batpak::event::EventHeader`] view with the
/// raw canonical payload bytes so the client can re-decode under any
/// type it has bindings for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA21)]
pub struct EventGetAck {
    /// `event_id` as 32-char lowercase hex.
    pub event_id_hex: String,
    /// Monotonic global sequence number at commit time. Number.MAX_SAFE_INTEGER
    /// bounded.
    pub sequence: u64,
    /// Wall-clock timestamp in microseconds since Unix epoch.
    pub timestamp_us: i64,
    /// Correlation id as 32-char lowercase hex (zero when unset).
    pub correlation_id_hex: String,
    /// Causation id as 32-char lowercase hex, or `None` when no causation
    /// is recorded.
    pub causation_id_hex: Option<String>,
    /// Event-kind upper 4 bits.
    pub kind_category: u8,
    /// Event-kind lower 12 bits.
    pub kind_type_id: u16,
    /// Coordinate entity at commit time.
    pub entity: String,
    /// Coordinate scope at commit time.
    pub scope: String,
    /// Lowercase hex of the canonical payload bytes.
    pub payload_hex: String,
    /// Lowercase hex of the blake3 content hash of the payload bytes.
    pub content_hash_hex: String,
}

impl EventPayloadFixture for EventGetRequest {
    fn fixture_value() -> Self {
        Self {
            event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
        }
    }
}

impl EventPayloadFixture for EventGetAck {
    fn fixture_value() -> Self {
        Self {
            event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
            sequence: 42,
            timestamp_us: 1_700_000_000_000_000,
            correlation_id_hex: "00000000000000000000000000000000".to_owned(),
            causation_id_hex: None,
            kind_category: 0xF,
            kind_type_id: 0xA01,
            entity: "fixture:bank".to_owned(),
            scope: "fixture-scope".to_owned(),
            payload_hex: "81a56e6f6e6365b66865617274626561742d666978747572652d30303031".to_owned(),
            content_hash_hex: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
        }
    }
}

// ─── Manifest registry submissions ──────────────────────────────────────────
//
// One `inventory::submit!` per `EventPayload`-deriving type. The
// `manifest::descriptors()` runtime walker materializes each entry into
// a full `EventDescriptor`. Field rows mirror the serde declaration
// order on the struct above.

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::bank::BankCommitRequest",
        ts_name: "BankCommitRequest",
        schema_ref: BANK_COMMIT_INPUT_SCHEMA_REF,
        kind_bits: BankCommitRequest::KIND.as_raw_u16(),
        fields: &[
            crate::manifest::FieldRow { wire_name: "entity", type_token: "string", order: 0 },
            crate::manifest::FieldRow { wire_name: "scope", type_token: "string", order: 1 },
            crate::manifest::FieldRow { wire_name: "kind_category", type_token: "u8", order: 2 },
            crate::manifest::FieldRow { wire_name: "kind_type_id", type_token: "u16", order: 3 },
            // payload is a free-form hex blob (variable length, lowercase).
            // Branded as HexBlob on the TS side so callers cannot
            // accidentally pass an event_id or content hash here.
            crate::manifest::FieldRow { wire_name: "payload_hex", type_token: "hex-blob", order: 4 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&BankCommitRequest::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(BankCommitRequest::fixture_value()).ok(),
    }
}

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::bank::BankCommitAck",
        ts_name: "BankCommitAck",
        schema_ref: BANK_COMMIT_OUTPUT_SCHEMA_REF,
        kind_bits: BankCommitAck::KIND.as_raw_u16(),
        fields: &[
            // Branded hex tokens prevent passing the wrong hex shape
            // (e.g. a content hash where an event id was expected).
            crate::manifest::FieldRow { wire_name: "event_id_hex", type_token: "u128-hex", order: 0 },
            crate::manifest::FieldRow { wire_name: "sequence", type_token: "u64-safe", order: 1 },
            crate::manifest::FieldRow { wire_name: "content_hash_hex", type_token: "blake3-32-hex", order: 2 },
            crate::manifest::FieldRow { wire_name: "key_id_hex", type_token: "key-id-hex", order: 3 },
            crate::manifest::FieldRow { wire_name: "signature_hex", type_token: "option<ed25519-sig-hex>", order: 4 },
            crate::manifest::FieldRow { wire_name: "extensions", type_token: "map<string,hex-blob>", order: 5 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&BankCommitAck::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(BankCommitAck::fixture_value()).ok(),
    }
}

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::bank::EventGetRequest",
        ts_name: "EventGetRequest",
        schema_ref: EVENT_GET_INPUT_SCHEMA_REF,
        kind_bits: EventGetRequest::KIND.as_raw_u16(),
        fields: &[
            crate::manifest::FieldRow { wire_name: "event_id_hex", type_token: "u128-hex", order: 0 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&EventGetRequest::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(EventGetRequest::fixture_value()).ok(),
    }
}

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::bank::EventGetAck",
        ts_name: "EventGetAck",
        schema_ref: EVENT_GET_OUTPUT_SCHEMA_REF,
        kind_bits: EventGetAck::KIND.as_raw_u16(),
        fields: &[
            crate::manifest::FieldRow { wire_name: "event_id_hex", type_token: "u128-hex", order: 0 },
            crate::manifest::FieldRow { wire_name: "sequence", type_token: "u64-safe", order: 1 },
            crate::manifest::FieldRow { wire_name: "timestamp_us", type_token: "i64-microseconds", order: 2 },
            crate::manifest::FieldRow { wire_name: "correlation_id_hex", type_token: "u128-hex", order: 3 },
            // causation_id is optional u128 hex — keep option<string>
            // for now to avoid a third option-of-brand token; brand
            // emission for option<u128-hex> can come in a follow-on
            // patch once the codegen test coverage proves the pattern.
            crate::manifest::FieldRow { wire_name: "causation_id_hex", type_token: "option<string>", order: 4 },
            crate::manifest::FieldRow { wire_name: "kind_category", type_token: "u8", order: 5 },
            crate::manifest::FieldRow { wire_name: "kind_type_id", type_token: "u16", order: 6 },
            crate::manifest::FieldRow { wire_name: "entity", type_token: "string", order: 7 },
            crate::manifest::FieldRow { wire_name: "scope", type_token: "string", order: 8 },
            crate::manifest::FieldRow { wire_name: "payload_hex", type_token: "hex-blob", order: 9 },
            crate::manifest::FieldRow { wire_name: "content_hash_hex", type_token: "blake3-32-hex", order: 10 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&EventGetAck::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(EventGetAck::fixture_value()).ok(),
    }
}

#[cfg(test)]
// justifies: INV-TEST-PANIC-AS-ASSERTION; hbat fixture tests use panic and unwrap as assertion signals for canonical encoding roundtrips.
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bank_commit_fixture_request_roundtrips() {
        let v = BankCommitRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&v).expect("encode");
        let decoded: BankCommitRequest = batpak::encoding::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, v);
    }

    #[test]
    fn bank_commit_fixture_ack_roundtrips() {
        let v = BankCommitAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&v).expect("encode");
        let decoded: BankCommitAck = batpak::encoding::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, v);
    }

    #[test]
    fn event_get_fixture_request_roundtrips() {
        let v = EventGetRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&v).expect("encode");
        let decoded: EventGetRequest = batpak::encoding::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, v);
    }

    #[test]
    fn event_get_fixture_ack_roundtrips() {
        let v = EventGetAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&v).expect("encode");
        let decoded: EventGetAck = batpak::encoding::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, v);
    }

    #[test]
    fn descriptors_validate() {
        BANK_COMMIT_DESCRIPTOR
            .validate()
            .expect("bank.commit descriptor must validate");
        EVENT_GET_DESCRIPTOR
            .validate()
            .expect("event.get descriptor must validate");
    }

    #[test]
    fn kinds_are_distinct() {
        let kinds = [
            BankCommitRequest::KIND.as_raw_u16(),
            BankCommitAck::KIND.as_raw_u16(),
            EventGetRequest::KIND.as_raw_u16(),
            EventGetAck::KIND.as_raw_u16(),
        ];
        let mut sorted = kinds;
        sorted.sort_unstable();
        for w in sorted.windows(2) {
            assert!(w[0] != w[1], "duplicate kind: {kinds:?}");
        }
    }
}
