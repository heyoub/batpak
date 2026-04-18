/// Positional types for locating events within a DAG chain.
pub mod position;
pub use position::DagPosition;

use crate::event::EventKind;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Hard cap for each coordinate component. Prevents accidental or hostile
/// cardinality bombs from turning entity/scope keys into unbounded memory sinks.
pub const MAX_COORDINATE_COMPONENT_LEN: usize = 1024;

/// Coordinate: WHO (entity) + WHERE (scope). The address of an event stream.

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(into = "CoordinateWire")]
pub struct Coordinate {
    entity: Arc<str>, // WHO — stream key, hash chain anchor
    scope: Arc<str>,  // WHERE — isolation boundary
}

/// Wire form of [`Coordinate`] used by serde so that every deserialised
/// value routes back through [`Coordinate::new`] and picks up the same
/// validation as in-process construction.
#[derive(Serialize, Deserialize)]
struct CoordinateWire {
    entity: String,
    scope: String,
}

impl From<Coordinate> for CoordinateWire {
    fn from(coord: Coordinate) -> Self {
        Self {
            entity: coord.entity.as_ref().to_owned(),
            scope: coord.scope.as_ref().to_owned(),
        }
    }
}

impl<'de> Deserialize<'de> for Coordinate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CoordinateWire::deserialize(deserializer)?;
        Coordinate::new(&wire.entity, &wire.scope).map_err(serde::de::Error::custom)
    }
}

/// Errors returned when constructing a [`Coordinate`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoordinateError {
    /// The entity string was empty.
    EmptyEntity,
    /// The scope string was empty.
    EmptyScope,
    /// The entity string exceeded the maximum supported length.
    EntityTooLong {
        /// Actual entity string length.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },
    /// The scope string exceeded the maximum supported length.
    ScopeTooLong {
        /// Actual scope string length.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },
    /// A coordinate component contained a NUL byte (`'\0'`).
    NulByte,
    /// A coordinate component contained a forbidden ASCII control character.
    ControlChar,
    /// A coordinate component contained a path-traversal substring (`..` or `/`).
    PathTraversal,
}

/// Region: the ONE predicate type for query, subscription, cursor, traversal.
#[derive(Clone, Debug, Default)]
pub struct Region {
    /// Optional entity name prefix; matches any entity whose name starts with this string.
    pub(crate) entity_prefix: Option<Arc<str>>,
    /// Optional exact scope to match.
    pub(crate) scope: Option<Arc<str>>,
    /// Optional event-kind filter applied to matched events.
    pub(crate) fact: Option<KindFilter>,
    /// Optional inclusive per-entity clock range; does not apply to live filtering.
    pub(crate) clock_range: Option<(u32, u32)>, // per-entity clock, not global_sequence
}

/// Filter on [`EventKind`] used within a [`Region`] query.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum KindFilter {
    /// Matches only events with this exact kind.
    Exact(EventKind),
    /// Matches any event whose kind falls within this 4-bit category.
    Category(u8), // matches any EventKind in this 4-bit category
    /// Matches events of any kind.
    Any,
}

impl Coordinate {
    /// Creates a new `Coordinate` from an entity and scope string.
    ///
    /// # Errors
    /// Returns `CoordinateError::EmptyEntity` if the entity string is empty.
    /// Returns `CoordinateError::EmptyScope` if the scope string is empty.
    pub fn new(entity: impl AsRef<str>, scope: impl AsRef<str>) -> Result<Self, CoordinateError> {
        let entity = entity.as_ref();
        let scope = scope.as_ref();
        Self::validate_parts(entity, scope)?;
        Ok(Self {
            entity: Arc::from(entity),
            scope: Arc::from(scope),
        })
    }

    /// Returns the entity string.
    pub fn entity(&self) -> &str {
        &self.entity
    }
    /// Returns the scope string.
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub(crate) fn entity_arc(&self) -> Arc<str> {
        Arc::clone(&self.entity)
    }
    pub(crate) fn scope_arc(&self) -> Arc<str> {
        Arc::clone(&self.scope)
    }

    pub(crate) fn from_shared_parts(
        entity: Arc<str>,
        scope: Arc<str>,
    ) -> Result<Self, CoordinateError> {
        Self::validate_parts(entity.as_ref(), scope.as_ref())?;
        Ok(Self { entity, scope })
    }

    /// Revalidate an existing coordinate against the current validation rules.
    ///
    /// Used at API boundaries (e.g. `submit_batch`) to defend against
    /// coordinates constructed through internal routes that bypass `new`,
    /// or produced by older on-disk data under tightened rules.
    ///
    /// # Errors
    /// Returns any [`CoordinateError`] that [`Coordinate::new`] would produce
    /// if called with the same entity/scope strings.
    pub fn validate(&self) -> Result<(), CoordinateError> {
        Self::validate_parts(self.entity.as_ref(), self.scope.as_ref())
    }

    fn validate_parts(entity: &str, scope: &str) -> Result<(), CoordinateError> {
        if entity.is_empty() {
            return Err(CoordinateError::EmptyEntity);
        }
        if scope.is_empty() {
            return Err(CoordinateError::EmptyScope);
        }
        if entity.len() > MAX_COORDINATE_COMPONENT_LEN {
            return Err(CoordinateError::EntityTooLong {
                len: entity.len(),
                max: MAX_COORDINATE_COMPONENT_LEN,
            });
        }
        if scope.len() > MAX_COORDINATE_COMPONENT_LEN {
            return Err(CoordinateError::ScopeTooLong {
                len: scope.len(),
                max: MAX_COORDINATE_COMPONENT_LEN,
            });
        }
        Self::validate_component_bytes(entity)?;
        Self::validate_component_bytes(scope)?;
        Ok(())
    }

    fn validate_component_bytes(value: &str) -> Result<(), CoordinateError> {
        for byte in value.bytes() {
            if byte == 0 {
                return Err(CoordinateError::NulByte);
            }
            // ASCII control range 0x00..=0x1F and DEL 0x7F. NUL is handled
            // above for a more specific error; the rest fall through here.
            if byte < 0x20 || byte == 0x7F {
                return Err(CoordinateError::ControlChar);
            }
        }
        if value.contains('/') || value.contains("..") {
            return Err(CoordinateError::PathTraversal);
        }
        Ok(())
    }
}

impl fmt::Display for Coordinate {
    /// "entity@scope"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.entity, self.scope)
    }
}

impl fmt::Display for CoordinateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEntity => write!(f, "entity cannot be empty"),
            Self::EmptyScope => write!(f, "scope cannot be empty"),
            Self::EntityTooLong { len, max } => {
                write!(f, "entity length {len} exceeds maximum {max}")
            }
            Self::ScopeTooLong { len, max } => {
                write!(f, "scope length {len} exceeds maximum {max}")
            }
            Self::NulByte => write!(f, "coordinate component contains a NUL byte"),
            Self::ControlChar => write!(
                f,
                "coordinate component contains a forbidden ASCII control character"
            ),
            Self::PathTraversal => write!(
                f,
                "coordinate component contains a forbidden path-traversal substring"
            ),
        }
    }
}
impl std::error::Error for CoordinateError {}

/// Region builder with method chaining.
impl Region {
    /// Returns a region that matches all events.
    pub fn all() -> Self {
        Self::default()
    }

    /// Returns a region scoped to entities whose names start with `prefix`.
    pub fn entity(prefix: impl AsRef<str>) -> Self {
        Self {
            entity_prefix: Some(Arc::from(prefix.as_ref())),
            ..Self::default()
        }
    }

    /// Returns a region scoped to a specific scope string.
    pub fn scope(scope: impl AsRef<str>) -> Self {
        Self {
            scope: Some(Arc::from(scope.as_ref())),
            ..Self::default()
        }
    }

    /// Chainable setters
    pub fn with_scope(mut self, scope: impl AsRef<str>) -> Self {
        self.scope = Some(Arc::from(scope.as_ref()));
        self
    }

    /// Filters events by the given kind filter.
    pub fn with_fact(mut self, filter: KindFilter) -> Self {
        self.fact = Some(filter);
        self
    }

    /// Filters events to those whose kind matches the given category.
    pub fn with_fact_category(mut self, cat: u8) -> Self {
        self.fact = Some(KindFilter::Category(cat));
        self
    }

    /// Filters events to those within the given per-entity clock range.
    pub fn with_clock_range(mut self, range: (u32, u32)) -> Self {
        self.clock_range = Some(range);
        self
    }

    /// Returns the configured entity prefix, if any.
    pub fn entity_prefix(&self) -> Option<&str> {
        self.entity_prefix.as_deref()
    }

    /// Returns the configured exact scope, if any.
    pub fn scope_value(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// Returns the configured kind filter, if any.
    pub fn fact(&self) -> Option<&KindFilter> {
        self.fact.as_ref()
    }

    /// Returns the configured inclusive per-entity clock range, if any.
    pub fn clock_range(&self) -> Option<(u32, u32)> {
        self.clock_range
    }

    /// Match against individual fields — avoids circular dep on store::Notification.
    /// Called by Subscription::recv() to filter events. [FILE:src/store/delivery/subscription.rs]
    pub fn matches_event(&self, entity: &str, scope: &str, kind: EventKind) -> bool {
        if let Some(ref prefix) = self.entity_prefix {
            if !entity.starts_with(prefix.as_ref()) {
                return false;
            }
        }
        if let Some(ref s) = self.scope {
            if scope != s.as_ref() {
                return false;
            }
        }
        if let Some(ref fact) = self.fact {
            match fact {
                KindFilter::Exact(k) => {
                    if kind != *k {
                        return false;
                    }
                }
                KindFilter::Category(c) => {
                    if kind.category() != *c {
                        return false;
                    }
                }
                KindFilter::Any => {}
            }
        }
        // clock_range is not checked here — it's for index queries, not live filtering.
        true
    }
}
