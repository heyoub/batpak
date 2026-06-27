use std::collections::BTreeMap;
use std::sync::Arc;

use batpak::coordinate::{Coordinate, EventCategory};
use batpak::store::Freshness;

use crate::operation::MAX_DESCRIPTOR_REF_BYTES;
use crate::operation_name::OperationName;
use crate::operation_status_sink::operation_status_entity;

use super::error::SubscriptionRuntimeError;
use super::projector::{ProjectionProjector, ProjectionRouteBinding};

const MAX_SUBSCRIPTION_ID_BYTES: usize = 128;

/// Binding fields needed to open an operation-status subscription session.
#[derive(Clone, Debug)]
pub struct OperationStatusRouteBinding {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Route-declared operation name.
    pub operation: OperationName,
    /// Entity coordinate bound to the operation-status facts.
    pub entity: String,
    /// Wire payload schema ref token.
    pub wire_payload_schema_ref: String,
    /// Optional inner status schema ref.
    pub inner_status_schema_ref: Option<String>,
    /// Freshness mode for status materialization.
    pub freshness: Freshness,
    /// Optional route-specific queue clamp.
    pub backpressure_capacity: Option<usize>,
}

/// Binding fields needed to open an entity-stream subscription session.
#[derive(Clone, Debug)]
pub struct EntityStreamRouteBinding {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Entity coordinate bound to the stream.
    pub entity: String,
    /// Scope coordinate bound to the stream.
    pub scope: String,
    /// Wire payload schema ref token.
    pub wire_payload_schema_ref: String,
    /// Optional inner event payload schema ref.
    pub inner_event_payload_schema_ref: Option<String>,
    /// Optional route-specific queue clamp.
    pub backpressure_capacity: Option<usize>,
}

/// Binding fields needed to open a receipt-stream subscription session.
#[derive(Clone, Debug)]
pub struct ReceiptStreamRouteBinding {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Route-declared receipt kind filter.
    pub receipt_kind: String,
    /// Wire payload schema ref token.
    pub wire_payload_schema_ref: String,
    /// Optional inner receipt schema ref.
    pub inner_receipt_schema_ref: Option<String>,
    /// Optional route-specific queue clamp.
    pub backpressure_capacity: Option<usize>,
}

/// Globally unique subscription id (`orders.open.v1` grammar).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    /// Construct and validate a subscription id.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::InvalidSubscriptionId`].
    pub fn new(id: impl Into<String>) -> Result<Self, SubscriptionRuntimeError> {
        let id = id.into();
        validate_subscription_id(&id)
            .map_err(|reason| SubscriptionRuntimeError::InvalidSubscriptionId { reason })?;
        Ok(Self(id))
    }

    /// Borrow the id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Declared route for one subscription id.
#[derive(Clone)]
pub enum SubscriptionRoute {
    /// Category-wide event stream.
    EventCategory {
        /// Exported 4-bit event category filter.
        category: u8,
        /// Wire `payload_schema_ref` token for stream envelopes.
        wire_payload_schema_ref: String,
        /// Optional inner payload schema ref carried inside the envelope.
        inner_event_payload_schema_ref: Option<String>,
        /// Optional route-specific queue clamp.
        backpressure_capacity: Option<usize>,
    },
    /// Exact coordinate event stream.
    EntityStream {
        /// Entity coordinate bound to the stream.
        entity: String,
        /// Scope coordinate bound to the stream.
        scope: String,
        /// Wire `payload_schema_ref` token for stream envelopes.
        wire_payload_schema_ref: String,
        /// Optional inner payload schema ref carried inside the envelope.
        inner_event_payload_schema_ref: Option<String>,
        /// Optional route-specific queue clamp.
        backpressure_capacity: Option<usize>,
    },
    /// Entity-scoped projection stream.
    Projection {
        /// Route-declared projection id.
        projection_id: String,
        /// Entity coordinate bound to the projection.
        entity: String,
        /// Wire `payload_schema_ref` token for stream envelopes.
        wire_payload_schema_ref: String,
        /// Optional inner projection schema ref carried inside the envelope.
        inner_projection_schema_ref: Option<String>,
        /// Freshness mode for projection materialization.
        freshness: Freshness,
        /// Optional route-specific queue clamp.
        backpressure_capacity: Option<usize>,
        /// syncbat-owned projector that opens typed projection sessions.
        projector: Arc<dyn ProjectionProjector>,
    },
    /// Operation-scoped status stream.
    OperationStatus {
        /// Route-declared operation name.
        operation: OperationName,
        /// Entity coordinate bound to the operation-status facts.
        entity: String,
        /// Wire `payload_schema_ref` token for stream envelopes.
        wire_payload_schema_ref: String,
        /// Optional inner status schema ref carried inside the envelope.
        inner_status_schema_ref: Option<String>,
        /// Freshness mode for status materialization.
        freshness: Freshness,
        /// Optional route-specific queue clamp.
        backpressure_capacity: Option<usize>,
    },
    /// Receipt-kind filtered append stream.
    ReceiptStream {
        /// Receipt kind whose durable append events form the stream.
        receipt_kind: String,
        /// Wire `payload_schema_ref` token for stream envelopes.
        wire_payload_schema_ref: String,
        /// Optional inner receipt schema ref carried inside the envelope.
        inner_receipt_schema_ref: Option<String>,
        /// Optional route-specific queue clamp.
        backpressure_capacity: Option<usize>,
    },
}

impl SubscriptionRoute {
    /// Return the event category for an event-category route.
    #[must_use]
    pub fn event_category(&self) -> Option<u8> {
        match self {
            Self::EventCategory { category, .. } => Some(*category),
            Self::Projection { .. }
            | Self::OperationStatus { .. }
            | Self::ReceiptStream { .. }
            | Self::EntityStream { .. } => None,
        }
    }

    /// Build an entity-stream route binding for session open.
    #[must_use]
    pub fn entity_stream_binding(&self, subscription_id: &str) -> Option<EntityStreamRouteBinding> {
        match self {
            Self::EntityStream {
                entity,
                scope,
                wire_payload_schema_ref,
                inner_event_payload_schema_ref,
                backpressure_capacity,
            } => Some(EntityStreamRouteBinding {
                subscription_id: subscription_id.to_owned(),
                entity: entity.clone(),
                scope: scope.clone(),
                wire_payload_schema_ref: wire_payload_schema_ref.clone(),
                inner_event_payload_schema_ref: inner_event_payload_schema_ref.clone(),
                backpressure_capacity: *backpressure_capacity,
            }),
            Self::EventCategory { .. }
            | Self::Projection { .. }
            | Self::OperationStatus { .. }
            | Self::ReceiptStream { .. } => None,
        }
    }

    /// Build a projection route binding for session open.
    #[must_use]
    pub fn projection_binding(&self, subscription_id: &str) -> Option<ProjectionRouteBinding> {
        match self {
            Self::Projection {
                projection_id,
                entity,
                wire_payload_schema_ref,
                inner_projection_schema_ref,
                freshness,
                backpressure_capacity,
                ..
            } => Some(ProjectionRouteBinding {
                subscription_id: subscription_id.to_owned(),
                projection_id: projection_id.clone(),
                entity: entity.clone(),
                wire_payload_schema_ref: wire_payload_schema_ref.clone(),
                inner_projection_schema_ref: inner_projection_schema_ref.clone(),
                freshness: freshness.clone(),
                backpressure_capacity: *backpressure_capacity,
            }),
            Self::EventCategory { .. } => None,
            Self::EntityStream { .. } => None,
            Self::OperationStatus { .. } | Self::ReceiptStream { .. } => None,
        }
    }

    /// Build an operation-status route binding for session open.
    #[must_use]
    pub fn operation_status_binding(
        &self,
        subscription_id: &str,
    ) -> Option<OperationStatusRouteBinding> {
        match self {
            Self::OperationStatus {
                operation,
                entity,
                wire_payload_schema_ref,
                inner_status_schema_ref,
                freshness,
                backpressure_capacity,
            } => Some(OperationStatusRouteBinding {
                subscription_id: subscription_id.to_owned(),
                operation: operation.clone(),
                entity: entity.clone(),
                wire_payload_schema_ref: wire_payload_schema_ref.clone(),
                inner_status_schema_ref: inner_status_schema_ref.clone(),
                freshness: freshness.clone(),
                backpressure_capacity: *backpressure_capacity,
            }),
            Self::EventCategory { .. }
            | Self::Projection { .. }
            | Self::ReceiptStream { .. }
            | Self::EntityStream { .. } => None,
        }
    }

    /// Build a receipt-stream route binding for session open.
    #[must_use]
    pub fn receipt_stream_binding(
        &self,
        subscription_id: &str,
    ) -> Option<ReceiptStreamRouteBinding> {
        match self {
            Self::ReceiptStream {
                receipt_kind,
                wire_payload_schema_ref,
                inner_receipt_schema_ref,
                backpressure_capacity,
            } => Some(ReceiptStreamRouteBinding {
                subscription_id: subscription_id.to_owned(),
                receipt_kind: receipt_kind.clone(),
                wire_payload_schema_ref: wire_payload_schema_ref.clone(),
                inner_receipt_schema_ref: inner_receipt_schema_ref.clone(),
                backpressure_capacity: *backpressure_capacity,
            }),
            Self::EventCategory { .. }
            | Self::Projection { .. }
            | Self::OperationStatus { .. }
            | Self::EntityStream { .. } => None,
        }
    }
}

impl std::fmt::Debug for SubscriptionRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventCategory {
                category,
                wire_payload_schema_ref,
                inner_event_payload_schema_ref,
                backpressure_capacity,
            } => f
                .debug_struct("EventCategory")
                .field("category", category)
                .field("wire_payload_schema_ref", wire_payload_schema_ref)
                .field(
                    "inner_event_payload_schema_ref",
                    inner_event_payload_schema_ref,
                )
                .field("backpressure_capacity", backpressure_capacity)
                .finish(),
            Self::EntityStream {
                entity,
                scope,
                wire_payload_schema_ref,
                inner_event_payload_schema_ref,
                backpressure_capacity,
            } => f
                .debug_struct("EntityStream")
                .field("entity", entity)
                .field("scope", scope)
                .field("wire_payload_schema_ref", wire_payload_schema_ref)
                .field(
                    "inner_event_payload_schema_ref",
                    inner_event_payload_schema_ref,
                )
                .field("backpressure_capacity", backpressure_capacity)
                .finish(),
            Self::Projection {
                projection_id,
                entity,
                wire_payload_schema_ref,
                inner_projection_schema_ref,
                freshness,
                backpressure_capacity,
                ..
            } => f
                .debug_struct("Projection")
                .field("projection_id", projection_id)
                .field("entity", entity)
                .field("wire_payload_schema_ref", wire_payload_schema_ref)
                .field("inner_projection_schema_ref", inner_projection_schema_ref)
                .field("freshness", freshness)
                .field("backpressure_capacity", backpressure_capacity)
                .field("projector", &"Arc<dyn ProjectionProjector>")
                .finish(),
            Self::OperationStatus {
                operation,
                entity,
                wire_payload_schema_ref,
                inner_status_schema_ref,
                freshness,
                backpressure_capacity,
            } => f
                .debug_struct("OperationStatus")
                .field("operation", operation)
                .field("entity", entity)
                .field("wire_payload_schema_ref", wire_payload_schema_ref)
                .field("inner_status_schema_ref", inner_status_schema_ref)
                .field("freshness", freshness)
                .field("backpressure_capacity", backpressure_capacity)
                .finish(),
            Self::ReceiptStream {
                receipt_kind,
                wire_payload_schema_ref,
                inner_receipt_schema_ref,
                backpressure_capacity,
            } => f
                .debug_struct("ReceiptStream")
                .field("receipt_kind", receipt_kind)
                .field("wire_payload_schema_ref", wire_payload_schema_ref)
                .field("inner_receipt_schema_ref", inner_receipt_schema_ref)
                .field("backpressure_capacity", backpressure_capacity)
                .finish(),
        }
    }
}

impl PartialEq for SubscriptionRoute {
    fn eq(&self, other: &Self) -> bool {
        self.event_category_eq(other)
            || self.entity_stream_eq(other)
            || self.projection_eq(other)
            || self.operation_status_eq(other)
            || self.receipt_stream_eq(other)
    }
}

impl Eq for SubscriptionRoute {}

impl SubscriptionRoute {
    fn event_category_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::EventCategory {
                    category: left_category,
                    wire_payload_schema_ref: left_wire,
                    inner_event_payload_schema_ref: left_inner,
                    backpressure_capacity: left_cap,
                },
                Self::EventCategory {
                    category: right_category,
                    wire_payload_schema_ref: right_wire,
                    inner_event_payload_schema_ref: right_inner,
                    backpressure_capacity: right_cap,
                },
            ) => {
                left_category == right_category
                    && left_wire == right_wire
                    && left_inner == right_inner
                    && left_cap == right_cap
            }
            _ => false,
        }
    }

    fn entity_stream_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::EntityStream {
                    entity: left_entity,
                    scope: left_scope,
                    wire_payload_schema_ref: left_wire,
                    inner_event_payload_schema_ref: left_inner,
                    backpressure_capacity: left_cap,
                },
                Self::EntityStream {
                    entity: right_entity,
                    scope: right_scope,
                    wire_payload_schema_ref: right_wire,
                    inner_event_payload_schema_ref: right_inner,
                    backpressure_capacity: right_cap,
                },
            ) => {
                left_entity == right_entity
                    && left_scope == right_scope
                    && left_wire == right_wire
                    && left_inner == right_inner
                    && left_cap == right_cap
            }
            _ => false,
        }
    }

    fn projection_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Projection {
                    projection_id: left_projection,
                    entity: left_entity,
                    wire_payload_schema_ref: left_wire,
                    inner_projection_schema_ref: left_inner,
                    freshness: left_freshness,
                    backpressure_capacity: left_cap,
                    ..
                },
                Self::Projection {
                    projection_id: right_projection,
                    entity: right_entity,
                    wire_payload_schema_ref: right_wire,
                    inner_projection_schema_ref: right_inner,
                    freshness: right_freshness,
                    backpressure_capacity: right_cap,
                    ..
                },
            ) => {
                left_projection == right_projection
                    && left_entity == right_entity
                    && left_wire == right_wire
                    && left_inner == right_inner
                    && freshness_same(left_freshness, right_freshness)
                    && left_cap == right_cap
            }
            _ => false,
        }
    }

    fn operation_status_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::OperationStatus {
                    operation: left_operation,
                    entity: left_entity,
                    wire_payload_schema_ref: left_wire,
                    inner_status_schema_ref: left_inner,
                    freshness: left_freshness,
                    backpressure_capacity: left_cap,
                },
                Self::OperationStatus {
                    operation: right_operation,
                    entity: right_entity,
                    wire_payload_schema_ref: right_wire,
                    inner_status_schema_ref: right_inner,
                    freshness: right_freshness,
                    backpressure_capacity: right_cap,
                },
            ) => {
                left_operation == right_operation
                    && left_entity == right_entity
                    && left_wire == right_wire
                    && left_inner == right_inner
                    && freshness_same(left_freshness, right_freshness)
                    && left_cap == right_cap
            }
            _ => false,
        }
    }

    fn receipt_stream_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::ReceiptStream {
                    receipt_kind: left_kind,
                    wire_payload_schema_ref: left_wire,
                    inner_receipt_schema_ref: left_inner,
                    backpressure_capacity: left_cap,
                },
                Self::ReceiptStream {
                    receipt_kind: right_kind,
                    wire_payload_schema_ref: right_wire,
                    inner_receipt_schema_ref: right_inner,
                    backpressure_capacity: right_cap,
                },
            ) => {
                left_kind == right_kind
                    && left_wire == right_wire
                    && left_inner == right_inner
                    && left_cap == right_cap
            }
            _ => false,
        }
    }
}

fn freshness_same(left: &Freshness, right: &Freshness) -> bool {
    match (left, right) {
        (Freshness::Consistent, Freshness::Consistent) => true,
        (
            Freshness::MaybeStale {
                max_stale_ms: left_ms,
            },
            Freshness::MaybeStale {
                max_stale_ms: right_ms,
            },
        ) => left_ms == right_ms,
        _ => false,
    }
}

/// Typed subscription route table for the runtime engine.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SubscriptionRegistry {
    routes: BTreeMap<String, SubscriptionRoute>,
}

impl SubscriptionRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            routes: BTreeMap::new(),
        }
    }

    /// Register one route.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::DuplicateSubscription`] or
    /// [`SubscriptionRuntimeError::InvalidRoute`].
    pub fn insert(
        &mut self,
        id: SubscriptionId,
        route: SubscriptionRoute,
    ) -> Result<(), SubscriptionRuntimeError> {
        validate_route(&route)?;
        if self.routes.contains_key(id.as_str()) {
            return Err(SubscriptionRuntimeError::DuplicateSubscription { id: id.0 });
        }
        self.routes.insert(id.0, route);
        Ok(())
    }

    /// Look up a route by subscription id text.
    #[must_use]
    pub fn get(&self, subscription_id: &str) -> Option<&SubscriptionRoute> {
        self.routes.get(subscription_id)
    }
}

fn validate_route(route: &SubscriptionRoute) -> Result<(), SubscriptionRuntimeError> {
    match route {
        SubscriptionRoute::EventCategory {
            category,
            wire_payload_schema_ref,
            backpressure_capacity,
            ..
        } => {
            validate_event_category_route(*category, wire_payload_schema_ref, backpressure_capacity)
        }
        SubscriptionRoute::Projection {
            projection_id,
            entity,
            wire_payload_schema_ref,
            backpressure_capacity,
            ..
        } => validate_projection_route(
            projection_id,
            entity,
            wire_payload_schema_ref,
            backpressure_capacity,
        ),
        SubscriptionRoute::OperationStatus {
            operation,
            entity,
            wire_payload_schema_ref,
            backpressure_capacity,
            ..
        } => validate_operation_status_route(
            operation,
            entity,
            wire_payload_schema_ref,
            backpressure_capacity,
        ),
        SubscriptionRoute::EntityStream {
            entity,
            scope,
            wire_payload_schema_ref,
            backpressure_capacity,
            ..
        } => validate_entity_stream_route(
            entity,
            scope,
            wire_payload_schema_ref,
            backpressure_capacity,
        ),
        SubscriptionRoute::ReceiptStream {
            receipt_kind,
            wire_payload_schema_ref,
            backpressure_capacity,
            ..
        } => validate_receipt_stream_route(
            receipt_kind,
            wire_payload_schema_ref,
            backpressure_capacity,
        ),
    }
}

fn validate_event_category_route(
    category: u8,
    wire_payload_schema_ref: &str,
    backpressure_capacity: &Option<usize>,
) -> Result<(), SubscriptionRuntimeError> {
    EventCategory::new(category).map_err(|_| SubscriptionRuntimeError::InvalidRoute {
        reason: "event category out of range",
    })?;
    validate_wire_and_backpressure(wire_payload_schema_ref, backpressure_capacity)
}

fn validate_projection_route(
    projection_id: &str,
    entity: &str,
    wire_payload_schema_ref: &str,
    backpressure_capacity: &Option<usize>,
) -> Result<(), SubscriptionRuntimeError> {
    if projection_id.is_empty() {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "projection id is empty",
        });
    }
    if entity.is_empty() {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "entity is empty",
        });
    }
    validate_wire_and_backpressure(wire_payload_schema_ref, backpressure_capacity)
}

fn validate_operation_status_route(
    operation: &OperationName,
    entity: &str,
    wire_payload_schema_ref: &str,
    backpressure_capacity: &Option<usize>,
) -> Result<(), SubscriptionRuntimeError> {
    if entity.is_empty() {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "entity is empty",
        });
    }
    let expected = operation_status_entity(operation.as_str()).map_err(|_| {
        SubscriptionRuntimeError::InvalidRoute {
            reason: "operation name produces invalid status entity",
        }
    })?;
    if entity != expected {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "entity does not match operation-status entity helper",
        });
    }
    validate_wire_and_backpressure(wire_payload_schema_ref, backpressure_capacity)
}

fn validate_entity_stream_route(
    entity: &str,
    scope: &str,
    wire_payload_schema_ref: &str,
    backpressure_capacity: &Option<usize>,
) -> Result<(), SubscriptionRuntimeError> {
    Coordinate::new(entity, scope).map_err(|_| SubscriptionRuntimeError::InvalidRoute {
        reason: "entity coordinate is invalid",
    })?;
    validate_wire_and_backpressure(wire_payload_schema_ref, backpressure_capacity)
}

fn validate_receipt_stream_route(
    receipt_kind: &str,
    wire_payload_schema_ref: &str,
    backpressure_capacity: &Option<usize>,
) -> Result<(), SubscriptionRuntimeError> {
    validate_receipt_kind(receipt_kind)?;
    validate_wire_and_backpressure(wire_payload_schema_ref, backpressure_capacity)
}

fn validate_wire_and_backpressure(
    wire_payload_schema_ref: &str,
    backpressure_capacity: &Option<usize>,
) -> Result<(), SubscriptionRuntimeError> {
    if wire_payload_schema_ref.is_empty() {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "wire payload schema ref is empty",
        });
    }
    if matches!(backpressure_capacity, Some(0)) {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "backpressure capacity is zero",
        });
    }
    Ok(())
}

fn validate_receipt_kind(value: &str) -> Result<(), SubscriptionRuntimeError> {
    if value.is_empty() {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "receipt kind is empty",
        });
    }
    if value.len() > MAX_DESCRIPTOR_REF_BYTES {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "receipt kind is too long",
        });
    }
    if value
        .bytes()
        .any(|byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "receipt kind has invalid characters",
        });
    }
    if value.starts_with('.') || value.ends_with('.') || value.contains("..") {
        return Err(SubscriptionRuntimeError::InvalidRoute {
            reason: "receipt kind has invalid dot placement",
        });
    }
    Ok(())
}

/// Validate subscription id grammar:
/// `^[a-z0-9][a-z0-9._-]*\.v[1-9][0-9]*$` with length and dot rules.
fn validate_subscription_id(id: &str) -> Result<(), &'static str> {
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
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("subscription id has characters outside [a-z0-9._-]");
    }
    if version.is_empty() {
        return Err("subscription id missing version digits after .v");
    }
    let first = version.as_bytes()[0];
    if !first.is_ascii_digit() || first == b'0' {
        return Err("subscription id version must start with 1-9");
    }
    if !version.chars().all(|c| c.is_ascii_digit()) {
        return Err("subscription id version must be digits only");
    }
    Ok(())
}
