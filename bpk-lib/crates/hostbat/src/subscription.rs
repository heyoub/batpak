//! Subscription descriptor types for the client-visible host interface.
//!
//! Subscriptions are declared on modules, aggregated globally by subscription id,
//! and folded into `H_interface`.
//! Packet A is declaration-only: descriptors require `NETBAT/2-streaming` transport
//! but the host runtime continues to serve calls on `NETBAT/1`.

use batpak::coordinate::Coordinate;
use serde::ser::SerializeStruct;
use serde::Serialize;

use crate::error::HostError;
use crate::schema::SchemaRole;

/// Maximum bytes accepted for a [`SubscriptionId`].
const MAX_SUBSCRIPTION_ID_BYTES: usize = 128;

/// Maximum bytes accepted for a [`ProjectionId`].
const MAX_PROJECTION_ID_BYTES: usize = 128;

/// Wire transport required to serve declared subscriptions (not yet implemented).
pub const SUBSCRIPTION_WIRE_REQUIRES: &str = "NETBAT/2-streaming";

/// Globally unique subscription name within a host composition, e.g. `orders.open.v1`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    /// Construct a subscription id, validating grammar and length.
    ///
    /// # Errors
    /// [`HostError::SubscriptionInvalidId`] when grammar or length checks fail.
    pub fn new(id: impl Into<String>) -> Result<Self, HostError> {
        let id = id.into();
        validate_subscription_id(&id).map_err(|detail| HostError::SubscriptionInvalidId {
            id: id.clone(),
            detail: detail.to_owned(),
        })?;
        Ok(Self(id))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Exported subscription event category (4-bit namespace), not a concrete [`batpak::event::EventKind`].
///
/// Rejects category `>= 16`, `0x0`, and `0xD`, matching [`batpak::event::EventKind::try_custom`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EventCategory(u8);

impl EventCategory {
    /// Construct an exported subscription category.
    ///
    /// # Errors
    /// [`HostError::SubscriptionReservedCategory`] when the category is reserved or out of range.
    pub fn new(category: u8) -> Result<Self, HostError> {
        if category >= 16 {
            return Err(HostError::SubscriptionReservedCategory { category });
        }
        if category == 0 {
            return Err(HostError::SubscriptionReservedCategory { category });
        }
        if category == 0xD {
            return Err(HostError::SubscriptionReservedCategory { category });
        }
        Ok(Self(category))
    }

    /// The raw 4-bit category value.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }
}

/// Stable projection identity referenced by [`SubscriptionSource::Projection`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectionId(String);

impl ProjectionId {
    /// Construct a projection id with module-id grammar.
    ///
    /// # Errors
    /// [`HostError::SubscriptionInvalidProjectionId`] when grammar checks fail.
    pub fn new(id: impl Into<String>) -> Result<Self, HostError> {
        let id = id.into();
        validate_component_id(&id, MAX_PROJECTION_ID_BYTES, "projection id").map_err(|detail| {
            HostError::SubscriptionInvalidProjectionId {
                id: id.clone(),
                detail: detail.to_owned(),
            }
        })?;
        Ok(Self(id))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Receipt-stream filter for [`SubscriptionSource::ReceiptStream`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ReceiptFilter {
    /// Receipt kind whose append events form the stream.
    pub receipt_kind: String,
}

impl ReceiptFilter {
    /// Filter receipt append events by receipt kind.
    #[must_use]
    pub fn new(receipt_kind: impl Into<String>) -> Self {
        Self {
            receipt_kind: receipt_kind.into(),
        }
    }
}

/// Operation-status selector for [`SubscriptionSource::OperationStatus`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct OperationStatusSelector {
    /// Operation whose checkout/status facts are streamed.
    pub operation: String,
}

impl OperationStatusSelector {
    /// Select status facts for one exported operation name.
    #[must_use]
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
        }
    }
}

/// Source axis of a declared subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionSource {
    /// Category-wide event stream.
    EventCategory(EventCategory),
    /// Coordinate-scoped entity event stream.
    EntityStream(Coordinate),
    /// Projection frontier stream.
    Projection(ProjectionId),
    /// Receipt append stream filtered by kind.
    ReceiptStream(ReceiptFilter),
    /// Operation checkout/status stream.
    OperationStatus(OperationStatusSelector),
}

impl SubscriptionSource {
    /// Required payload schema role for this source variant.
    #[must_use]
    pub fn required_payload_role(&self) -> SchemaRole {
        match self {
            Self::EventCategory(_) | Self::EntityStream(_) => SchemaRole::EventPayload,
            Self::ReceiptStream(_) => SchemaRole::ReceiptPayload,
            Self::Projection(_) | Self::OperationStatus(_) => SchemaRole::SubscriptionPayload,
        }
    }
}

impl Serialize for SubscriptionSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::EventCategory(category) => {
                let mut state = serializer.serialize_struct("SubscriptionSource", 2)?;
                state.serialize_field("kind", "event-category")?;
                state.serialize_field("category", &category.get())?;
                state.end()
            }
            Self::EntityStream(coordinate) => {
                let mut state = serializer.serialize_struct("SubscriptionSource", 2)?;
                state.serialize_field("kind", "entity-stream")?;
                state.serialize_field("coordinate", coordinate)?;
                state.end()
            }
            Self::Projection(projection) => {
                let mut state = serializer.serialize_struct("SubscriptionSource", 2)?;
                state.serialize_field("kind", "projection")?;
                state.serialize_field("projection_id", projection.as_str())?;
                state.end()
            }
            Self::ReceiptStream(filter) => {
                let mut state = serializer.serialize_struct("SubscriptionSource", 2)?;
                state.serialize_field("kind", "receipt-stream")?;
                state.serialize_field("receipt_kind", &filter.receipt_kind)?;
                state.end()
            }
            Self::OperationStatus(selector) => {
                let mut state = serializer.serialize_struct("SubscriptionSource", 2)?;
                state.serialize_field("kind", "operation-status")?;
                state.serialize_field("operation", &selector.operation)?;
                state.end()
            }
        }
    }
}

/// Delivery semantics for a declared subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubscriptionDelivery {
    /// Cursor-backed at-least-once delivery (default for v1 declarations).
    CursorAtLeastOnce,
}

impl SubscriptionDelivery {
    /// Stable lowercase spelling used in canonical encodings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CursorAtLeastOnce => "cursor-at-least-once",
        }
    }
}

/// Backpressure policy for a declared subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BackpressurePolicy {
    /// Bounded queue with cumulative ACK and slow-consumer close.
    BoundedQueue {
        /// Maximum queued deliveries before backpressure applies.
        capacity: u32,
    },
}

impl BackpressurePolicy {
    /// Stable lowercase spelling used in canonical encodings.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Self::BoundedQueue { .. } => "bounded-queue",
        }
    }
}

/// Declarative subscription exported through the host interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SubscriptionDescriptor {
    id: SubscriptionId,
    source: SubscriptionSource,
    payload_schema_ref: String,
    delivery: SubscriptionDelivery,
    backpressure: BackpressurePolicy,
}

impl SubscriptionDescriptor {
    /// Declare one subscription with validated id and source-specific payload role.
    ///
    /// # Errors
    /// [`HostError::SubscriptionInvalidId`] or [`HostError::SubscriptionReservedCategory`].
    pub fn new(
        id: SubscriptionId,
        source: SubscriptionSource,
        payload_schema_ref: impl Into<String>,
        delivery: SubscriptionDelivery,
        backpressure: BackpressurePolicy,
    ) -> Self {
        Self {
            id,
            source,
            payload_schema_ref: payload_schema_ref.into(),
            delivery,
            backpressure,
        }
    }

    /// Globally unique subscription id.
    #[must_use]
    pub fn id(&self) -> &SubscriptionId {
        &self.id
    }

    /// Source axis for the subscription stream.
    #[must_use]
    pub fn source(&self) -> &SubscriptionSource {
        &self.source
    }

    /// Referenced payload schema id (role depends on [`SubscriptionSource`]).
    #[must_use]
    pub fn payload_schema_ref(&self) -> &str {
        &self.payload_schema_ref
    }

    /// Declared delivery semantics.
    #[must_use]
    pub fn delivery(&self) -> SubscriptionDelivery {
        self.delivery
    }

    /// Declared backpressure policy.
    #[must_use]
    pub fn backpressure(&self) -> BackpressurePolicy {
        self.backpressure
    }

    /// Required payload schema role for this descriptor's source.
    #[must_use]
    pub fn required_payload_role(&self) -> SchemaRole {
        self.source.required_payload_role()
    }
}

/// Validate subscription id grammar:
/// `^[a-z0-9][a-z0-9._-]*\.v[1-9][0-9]*$` with length and dot rules.
pub(crate) fn validate_subscription_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() {
        return Err("empty subscription id");
    }
    if id.len() > MAX_SUBSCRIPTION_ID_BYTES {
        return Err("subscription id longer than 128 bytes");
    }
    if id.starts_with('.') || id.ends_with('.') {
        return Err("subscription id has a leading or trailing '.'");
    }
    if id.contains("..") {
        return Err("subscription id has a doubled '.'");
    }
    let Some(dot_v) = id.rfind(".v") else {
        return Err("subscription id must contain a .v version suffix");
    };
    let name = &id[..dot_v];
    let version = &id[dot_v + 2..];
    if name.is_empty() {
        return Err("subscription id name prefix is empty");
    }
    if !name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err("subscription id must start with [a-z0-9]");
    }
    validate_component_id(name, MAX_SUBSCRIPTION_ID_BYTES, "subscription id name")?;
    if version.is_empty() {
        return Err("subscription id missing version digits after .v");
    }
    let Some(first) = version.chars().next() else {
        return Err("subscription id missing version digits after .v");
    };
    if !first.is_ascii_digit() || first == '0' {
        return Err("subscription id version must start with 1-9");
    }
    if !version.chars().all(|c| c.is_ascii_digit()) {
        return Err("subscription id version must be digits only");
    }
    Ok(())
}

fn validate_component_id(id: &str, max_len: usize, label: &str) -> Result<(), &'static str> {
    if id.len() > max_len {
        return Err("component id longer than maximum");
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("component id has characters outside [a-z0-9._-]");
    }
    let _ = label;
    Ok(())
}

#[cfg(test)]
#[path = "subscription_tests.rs"]
mod subscription_tests;
