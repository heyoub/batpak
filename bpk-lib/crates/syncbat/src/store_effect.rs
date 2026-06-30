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

    fn read_event(&mut self, event_category: &str) -> Result<(), EffectError> {
        // `event_category` is the operation's declared read-identity token, not a
        // store filter (the store keys on entity/scope/kind/lane, never on a free
        // category string). The backend is bound to one coordinate, so the
        // mediated read is performed against that coordinate's committed event
        // stream: resolve the stream through the index and read the most recent
        // entry's bytes back. That exercises the real read-by-id path — index
        // lookup, disk read, and decode — so a declared event read is a genuine
        // store read rather than a no-op, and a corrupt-store read surfaces as an
        // `EffectError` instead of silently succeeding.
        let _ = event_category;
        let stream = self.store.by_entity(self.coordinate.entity());
        if let Some(entry) = stream.last() {
            self.store
                .read_raw(entry.event_id())
                .map(|_event| ())
                .map_err(|error| EffectError::new(error.to_string()))?;
        }
        Ok(())
    }

    fn query_projection(&mut self, projection_id: &str) -> Result<(), EffectError> {
        // `projection_id` names the operation's declared projection. A projection
        // folds over the events in this backend's coordinate scope, so the
        // mediated query reads that scope's committed events through the store
        // index — the same substrate a typed projection replays. The query is
        // type-erased here (a trait object cannot name the projection's `T`), so
        // it wires to the untyped scope query rather than `Store::project`.
        let _ = projection_id;
        let _hits = self.store.by_scope(self.coordinate.scope());
        Ok(())
    }

    // `emit_receipt` and `use_host_control` intentionally fall through to the
    // trait's typed fail-closed defaults. The receipt sink is a Core-level
    // concern (`Core::receipt_sink`) this store backend does not hold, and host
    // controls are a host-layer (hostbat) authority a store backend has no way
    // to perform. Backing either here would be a parallel, uncoordinated path;
    // they are wired at their owning layer instead.
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
