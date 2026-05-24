//! `receipt.verify` operation surface.
//!
//! Ack-shaped append receipt fields can be checked against the current store
//! index and signing registry without reconstructing a full [`AppendReceipt`]
//! on the wire.

use std::collections::BTreeMap;

use batpak::EventPayload;
use serde::{Deserialize, Serialize};
use syncbat::{EffectClass, OperationDescriptor};

use crate::EventPayloadFixture;

/// Stable operation name for verifying ack-shaped append receipt fields.
pub const RECEIPT_VERIFY_OPERATION_NAME: &str = "receipt.verify";
/// Schema reference for the request payload.
pub const RECEIPT_VERIFY_INPUT_SCHEMA_REF: &str = "receipt.verify.request";
/// Schema reference for the ack payload.
pub const RECEIPT_VERIFY_OUTPUT_SCHEMA_REF: &str = "receipt.verify.ack";
/// Receipt kind emitted for `receipt.verify` calls.
pub const RECEIPT_VERIFY_RECEIPT_KIND: &str = "receipt.receipt.verify.v1";

/// Operation descriptor for `receipt.verify`.
pub const RECEIPT_VERIFY_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    RECEIPT_VERIFY_OPERATION_NAME,
    EffectClass::Inspect,
    RECEIPT_VERIFY_INPUT_SCHEMA_REF,
    RECEIPT_VERIFY_OUTPUT_SCHEMA_REF,
    RECEIPT_VERIFY_RECEIPT_KIND,
);

static RECEIPT_VERIFY_DESCRIPTOR_STORAGE: OperationDescriptor = RECEIPT_VERIFY_DESCRIPTOR;

fn receipt_verify_descriptor() -> &'static OperationDescriptor {
    &RECEIPT_VERIFY_DESCRIPTOR_STORAGE
}

inventory::submit! {
    crate::manifest::OperationDescriptorRegistration {
        descriptor: receipt_verify_descriptor,
    }
}

/// Wire input for [`RECEIPT_VERIFY_DESCRIPTOR`].
///
/// Field shape mirrors [`crate::bank::BankCommitAck`] so a commit ack can be
/// round-tripped into verification without translation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA30)]
pub struct ReceiptVerifyRequest {
    /// `event_id` as 32-char lowercase hex.
    pub event_id_hex: String,
    /// Monotonic global sequence number assigned at commit.
    pub sequence: u64,
    /// Blake3-32 content hash of the payload, as 64-char lowercase hex.
    pub content_hash_hex: String,
    /// Signing-key identity, as 64-char lowercase hex.
    pub key_id_hex: String,
    /// Detached Ed25519 signature over the receipt fields, as 128-char
    /// lowercase hex. `None` when receipt signing is disabled.
    pub signature_hex: Option<String>,
    /// Receipt-extension map. Keys are extension key strings; values are
    /// lowercase hex of the raw extension bytes.
    pub extensions: BTreeMap<String, String>,
}

/// Wire output for [`RECEIPT_VERIFY_DESCRIPTOR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA31)]
pub struct ReceiptVerifyAck {
    /// True when the receipt matches the committed index and signing state.
    pub valid: bool,
    /// `"signed"`, `"unsigned_accepted"`, or `"invalid"`.
    pub outcome: String,
    /// Stable snake-case rejection reason when `outcome` is `"invalid"`.
    pub reason_code: Option<String>,
}

impl EventPayloadFixture for ReceiptVerifyRequest {
    fn fixture_value() -> Self {
        Self {
            event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
            sequence: 42,
            content_hash_hex: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
            key_id_hex: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_owned(),
            signature_hex: None,
            extensions: BTreeMap::new(),
        }
    }
}

impl EventPayloadFixture for ReceiptVerifyAck {
    fn fixture_value() -> Self {
        Self {
            valid: true,
            outcome: "unsigned_accepted".to_owned(),
            reason_code: None,
        }
    }
}

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::receipt::ReceiptVerifyRequest",
        ts_name: "ReceiptVerifyRequest",
        schema_ref: RECEIPT_VERIFY_INPUT_SCHEMA_REF,
        kind_bits: ReceiptVerifyRequest::KIND.as_raw_u16(),
        fields: &[
            crate::manifest::FieldRow { wire_name: "event_id_hex", type_token: "u128-hex", order: 0 },
            crate::manifest::FieldRow { wire_name: "sequence", type_token: "u64-safe", order: 1 },
            crate::manifest::FieldRow { wire_name: "content_hash_hex", type_token: "blake3-32-hex", order: 2 },
            crate::manifest::FieldRow { wire_name: "key_id_hex", type_token: "key-id-hex", order: 3 },
            crate::manifest::FieldRow { wire_name: "signature_hex", type_token: "option<ed25519-sig-hex>", order: 4 },
            crate::manifest::FieldRow { wire_name: "extensions", type_token: "map<string,hex-blob>", order: 5 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&ReceiptVerifyRequest::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(ReceiptVerifyRequest::fixture_value()).ok(),
    }
}

inventory::submit! {
    crate::manifest::EventDescriptorRegistration {
        rust_type: "hbat::receipt::ReceiptVerifyAck",
        ts_name: "ReceiptVerifyAck",
        schema_ref: RECEIPT_VERIFY_OUTPUT_SCHEMA_REF,
        kind_bits: ReceiptVerifyAck::KIND.as_raw_u16(),
        fields: &[
            crate::manifest::FieldRow { wire_name: "valid", type_token: "bool", order: 0 },
            crate::manifest::FieldRow { wire_name: "outcome", type_token: "string", order: 1 },
            crate::manifest::FieldRow { wire_name: "reason_code", type_token: "option<string>", order: 2 },
        ],
        fixture_bytes: || batpak::encoding::to_bytes(&ReceiptVerifyAck::fixture_value()).ok(),
        fixture_json: || serde_json::to_value(ReceiptVerifyAck::fixture_value()).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn fixture_request_roundtrips_through_canonical_encoding() -> Result<()> {
        let value = ReceiptVerifyRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ReceiptVerifyRequest = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn fixture_ack_roundtrips_through_canonical_encoding() -> Result<()> {
        let value = ReceiptVerifyAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ReceiptVerifyAck = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }
}
