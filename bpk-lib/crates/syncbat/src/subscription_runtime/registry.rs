use std::collections::BTreeMap;

use batpak::coordinate::EventCategory;

use super::error::SubscriptionRuntimeError;

const MAX_SUBSCRIPTION_ID_BYTES: usize = 128;

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
#[derive(Clone, Debug, Eq, PartialEq)]
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
}

impl SubscriptionRoute {
    /// Return the event category for an event-category route.
    ///
    /// # Errors
    /// Returns `None` when the route is not event-category (future source kinds).
    #[must_use]
    pub fn event_category(&self) -> Option<u8> {
        match self {
            Self::EventCategory { category, .. } => Some(*category),
        }
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
            EventCategory::new(*category).map_err(|_| SubscriptionRuntimeError::InvalidRoute {
                reason: "event category out of range",
            })?;
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
    }
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
