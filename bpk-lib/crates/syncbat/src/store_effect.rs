//! Batpak-backed [`EffectBackend`] for syncbat operation effects.
//!
//! This is the production capability backend: an operation's `Ctx` event-append
//! handle performs through this, which appends the event to one batpak store
//! coordinate. The runtime owns this backend and exposes it to a handler only
//! through `Ctx`, so an operation's appends are mediated and observed.

use std::sync::Arc;

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{Open, Store};
use serde::{Serialize, Serializer};

use crate::effect_backend::{EffectBackend, EffectError};

/// Batpak store-backed effect backend bound to one append coordinate.
pub struct StoreEffectBackend {
    store: Arc<Store<Open>>,
    coordinate: Coordinate,
}

impl StoreEffectBackend {
    /// Construct a backend that appends operation events to `coordinate`.
    #[must_use]
    pub fn new(store: Arc<Store<Open>>, coordinate: Coordinate) -> Self {
        Self { store, coordinate }
    }
}

impl EffectBackend for StoreEffectBackend {
    fn append_event(&mut self, kind: EventKind, payload: &[u8]) -> Result<(), EffectError> {
        self.store
            .append(&self.coordinate, kind, &RawPayload(payload))
            .map(|_receipt| ())
            .map_err(|error| EffectError::new(error.to_string()))
    }
}

/// Serializes a pre-encoded payload verbatim as a MessagePack `bin` so the
/// handler's canonical event-body bytes are stored opaquely rather than
/// re-encoded as an array of integers.
struct RawPayload<'a>(&'a [u8]);

impl Serialize for RawPayload<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(self.0)
    }
}
