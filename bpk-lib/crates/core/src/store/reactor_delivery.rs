//! Key-aware reactor delivery helpers (crypto-shred Stage E2).
//!
//! Split out of `reactor_typed` so that module stays under its size cap. These
//! decrypt an event at the core boundary before it is dispatched to a reactor,
//! and observe a crypto-shredded event as a loud skip. Without
//! `payload-encryption` — or on a store with no keyset — the fetch is exactly
//! [`Store::read_raw`], byte-identical.

use crate::event::StoredEvent;
use crate::store::{Open, Store, StoreError};

/// Raw-lane reactor fetch that is key-aware under `payload-encryption`: an
/// encrypted event is decrypted to its plaintext MessagePack bytes (so the
/// per-kind raw decode sees plaintext, not ciphertext), and a crypto-shredded
/// event surfaces as [`StoreError::PayloadShredded`] so the dispatch loop skips
/// it and advances the cursor.
pub(super) fn fetch_raw_key_aware(
    store: &Store<Open>,
    event_id: crate::id::EventId,
) -> Result<StoredEvent<Vec<u8>>, StoreError> {
    #[cfg(feature = "payload-encryption")]
    if store.key_store.is_some() {
        return match store.read_delivery_payload(event_id)? {
            crate::store::DeliveryPayload::Readable(stored) => Ok(*stored),
            crate::store::DeliveryPayload::Shredded { event_id } => {
                Err(StoreError::PayloadShredded { event_id })
            }
        };
    }
    store.read_raw(event_id)
}

/// Emit the observable, structured warn for a crypto-shredded event skipped
/// during reactor delivery. The reactor is not invoked for the event and the
/// cursor advances past it; the skip is LOUD (this warn), never silent.
#[cfg(feature = "payload-encryption")]
pub(super) fn warn_shredded_reactor_delivery(entity: &str, event_id: crate::id::EventId) {
    use crate::id::EntityIdType;
    tracing::warn!(
        target: "batpak::delivery",
        flow = "reactor",
        entity,
        event_id = event_id.as_u128(),
        "skipping a crypto-shredded event during reactor delivery; the reactor is not \
         invoked for it and the cursor advances past it (payload key destroyed — plaintext gone)"
    );
}
