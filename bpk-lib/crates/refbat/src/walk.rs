//! `event.walk` operation surface.
//!
//! Bounded hash-chain ancestry from a starting `event_id`. Relation-ordered
//! output only — not commit-order pagination.

use batpak::EventPayload;
use serde::{Deserialize, Serialize};
use syncbat::{EffectClass, OperationDescriptor};

use crate::bank::EventSummary;
use crate::EventPayloadFixture;

/// Stable operation name for walking hash-chain ancestors.
pub const EVENT_WALK_OPERATION_NAME: &str = "event.walk";
/// Schema reference for the request payload.
pub const EVENT_WALK_INPUT_SCHEMA_REF: &str = "event.walk.request";
/// Schema reference for the ack payload.
pub const EVENT_WALK_OUTPUT_SCHEMA_REF: &str = "event.walk.ack";
/// Receipt kind emitted for `event.walk` calls.
pub const EVENT_WALK_RECEIPT_KIND: &str = "receipt.event.walk.v1";
/// Maximum number of ancestry summaries returned by one `event.walk` call.
pub const EVENT_WALK_MAX_LIMIT: u64 = 1024;

/// Operation descriptor for `event.walk`.
pub const EVENT_WALK_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    EVENT_WALK_OPERATION_NAME,
    EffectClass::Inspect,
    EVENT_WALK_INPUT_SCHEMA_REF,
    EVENT_WALK_OUTPUT_SCHEMA_REF,
    EVENT_WALK_RECEIPT_KIND,
);

static EVENT_WALK_DESCRIPTOR_STORAGE: OperationDescriptor = EVENT_WALK_DESCRIPTOR;

fn event_walk_descriptor() -> &'static OperationDescriptor {
    &EVENT_WALK_DESCRIPTOR_STORAGE
}

inventory::submit! {
    crate::manifest::OperationDescriptorRegistration {
        descriptor: event_walk_descriptor,
    }
}

/// Wire input for [`EVENT_WALK_DESCRIPTOR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA40)]
pub struct EventWalkRequest {
    /// Starting `event_id` as 32-char lowercase hex.
    pub event_id_hex: String,
    /// Maximum number of ancestry summaries to return.
    pub limit: u64,
}

/// Wire output for [`EVENT_WALK_DESCRIPTOR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA41)]
pub struct EventWalkAck {
    /// Metadata-only summaries in hash-chain ancestry order (anchor first).
    pub entries: Vec<EventSummary>,
}

impl EventPayloadFixture for EventWalkRequest {
    fn fixture_value() -> Self {
        Self {
            event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
            limit: 3,
        }
    }
}

impl EventPayloadFixture for EventWalkAck {
    fn fixture_value() -> Self {
        Self {
            entries: vec![EventSummary::fixture_value()],
        }
    }
}

crate::refbat_event_descriptor! {
    type = EventWalkRequest,
    schema_ref = EVENT_WALK_INPUT_SCHEMA_REF,
    ts_name = "EventWalkRequest",
    fields = [
        ("event_id_hex", "u128-hex"),
        ("limit", "u64-safe-positive"),
    ],
}

crate::refbat_event_descriptor! {
    type = EventWalkAck,
    schema_ref = EVENT_WALK_OUTPUT_SCHEMA_REF,
    ts_name = "EventWalkAck",
    fields = [
        ("entries", "array<EventSummary>"),
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn fixture_request_roundtrips_through_canonical_encoding() -> Result<()> {
        let value = EventWalkRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: EventWalkRequest = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn fixture_ack_roundtrips_through_canonical_encoding() -> Result<()> {
        let value = EventWalkAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: EventWalkAck = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }
}
