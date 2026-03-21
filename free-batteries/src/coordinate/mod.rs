pub mod position;
pub use position::DagPosition;

use crate::event::EventKind;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Coordinate: WHO (entity) + WHERE (scope). The address of an event stream.
/// [SPEC:src/coordinate/mod.rs]

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Coordinate {
    entity: Arc<str>, // WHO — stream key, hash chain anchor
    scope: Arc<str>,  // WHERE — isolation boundary
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinateError {
    EmptyEntity,
    EmptyScope,
}

/// Region: the ONE predicate type for query, subscription, cursor, traversal.
/// [SPEC:src/coordinate/mod.rs — Region replaces SubscriptionPattern]
#[derive(Clone, Debug, Default)]
pub struct Region {
    pub entity_prefix: Option<Arc<str>>,
    pub scope: Option<Arc<str>>,
    pub fact: Option<KindFilter>,
    pub clock_range: Option<(u32, u32)>, // per-entity clock, NOT global_sequence [SPEC:IMPLEMENTATION NOTES item 12]
}

#[derive(Clone, Debug)]
pub enum KindFilter {
    Exact(EventKind),
    Category(u8), // matches any EventKind in this 4-bit category
    Any,
}

impl Coordinate {
    pub fn new(entity: impl AsRef<str>, scope: impl AsRef<str>) -> Result<Self, CoordinateError> {
        let entity = entity.as_ref();
        let scope = scope.as_ref();
        if entity.is_empty() {
            return Err(CoordinateError::EmptyEntity);
        }
        if scope.is_empty() {
            return Err(CoordinateError::EmptyScope);
        }
        Ok(Self {
            entity: Arc::from(entity),
            scope: Arc::from(scope),
        })
    }

    pub fn entity(&self) -> &str {
        &self.entity
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub(crate) fn entity_arc(&self) -> Arc<str> {
        Arc::clone(&self.entity)
    }
    pub(crate) fn scope_arc(&self) -> Arc<str> {
        Arc::clone(&self.scope)
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
        }
    }
}
impl std::error::Error for CoordinateError {}

/// Region builder — method chaining. [SPEC:src/coordinate/mod.rs — Region builder]
impl Region {
    pub fn all() -> Self {
        Self::default()
    }

    pub fn entity(prefix: impl AsRef<str>) -> Self {
        Self {
            entity_prefix: Some(Arc::from(prefix.as_ref())),
            ..Self::default()
        }
    }

    pub fn scope(scope: impl AsRef<str>) -> Self {
        Self {
            scope: Some(Arc::from(scope.as_ref())),
            ..Self::default()
        }
    }

    pub fn coordinate(coord: &Coordinate) -> Self {
        Self {
            entity_prefix: Some(coord.entity_arc()),
            scope: Some(coord.scope_arc()),
            ..Self::default()
        }
    }

    /// Chainable setters
    pub fn with_scope(mut self, scope: impl AsRef<str>) -> Self {
        self.scope = Some(Arc::from(scope.as_ref()));
        self
    }

    pub fn with_fact(mut self, filter: KindFilter) -> Self {
        self.fact = Some(filter);
        self
    }

    pub fn with_fact_category(mut self, cat: u8) -> Self {
        self.fact = Some(KindFilter::Category(cat));
        self
    }

    pub fn with_clock_range(mut self, range: (u32, u32)) -> Self {
        self.clock_range = Some(range);
        self
    }

    /// Match against individual fields — avoids circular dep on store::Notification.
    /// Called by Subscription::recv() to filter events. [FILE:src/store/subscription.rs]
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
