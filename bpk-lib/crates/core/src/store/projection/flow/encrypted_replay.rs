//! Key-aware projection replay for the crypto-shred payload path (Stage E1).
//!
//! Stage C made the Value-decode read seam REFUSE to decode ciphertext
//! (fail-closed), so nothing misdecodes an encrypted payload without its key.
//! The cost: projection replay, which folds over decoded event payloads, would
//! fail closed the moment it met an encrypted event. This module restores replay
//! for encrypted entities by decrypting each event through the shared Stage C
//! primitive ([`Store::open_encrypted_payload_bytes`]) BEFORE the fold, then
//! decoding the recovered plaintext bytes into the projection's replay lane.
//!
//! Only reached when a keyset is configured (`store.key_store.is_some()`); the
//! plaintext (`None`) path stays on the byte-identical pre-encryption reads.
//!
//! # Shredded-in-projection semantics (skip-with-awareness)
//!
//! An event whose payload key has been crypto-shredded has NO recoverable
//! plaintext, so the fold cannot apply it. We SKIP it — the projected state is
//! honestly aware it may be incomplete post-erasure — rather than misdecode
//! ciphertext or panic. The skip is OBSERVABLE (a `tracing::warn`), never a
//! silent state corruption. Skipping (not erroring) is the right default because
//! erasure inherently means the plaintext is gone: the replay watermark still
//! advances past the shredded event (it WAS seen), so a later incremental replay
//! agrees with a full replay — both skip the same event.

use super::ReplayInput;
use crate::event::Event;
use crate::id::EntityIdType;
use crate::store::read_api::PayloadPlaintext;
use crate::store::{Store, StoreError};

/// Key-aware batch read for full replay: decrypt each encrypted event, decode
/// into the replay lane, and SKIP crypto-shredded events (observably).
pub(super) fn read_batch_key_aware<I, State>(
    store: &Store<State>,
    entity: &str,
    positions: &[&crate::store::index::DiskPos],
) -> Result<Vec<Event<I::Payload>>, StoreError>
where
    I: ReplayInput,
    State: crate::store::StoreState,
{
    let mut events = Vec::with_capacity(positions.len());
    for pos in positions {
        if let Some(event) = read_one_key_aware::<I, State>(store, entity, pos)? {
            events.push(event);
        }
    }
    Ok(events)
}

/// Key-aware single-event read: `Ok(Some(event))` for a readable (plaintext or
/// decryptable) event, `Ok(None)` for a crypto-shredded one (skip-with-awareness).
pub(super) fn read_one_key_aware<I, State>(
    store: &Store<State>,
    entity: &str,
    pos: &crate::store::index::DiskPos,
) -> Result<Option<Event<I::Payload>>, StoreError>
where
    I: ReplayInput,
    State: crate::store::StoreState,
{
    // Read the RAW frame (ciphertext + header + coordinate); never route
    // ciphertext through the Value-decode seam.
    let raw = store.reader.read_entry_raw(pos)?;
    let coordinate = raw.coordinate;
    let header = raw.event.header;
    let hash_chain = raw.event.hash_chain;
    let payload_bytes = raw.event.payload;

    // Cloned so `header` can be moved into the returned `Event` without the
    // `meta` borrow outliving it.
    let Some(meta) = header.payload_encryption.clone() else {
        // Plaintext event in an encryption-enabled store: decode exactly as the
        // plaintext replay seam would (the lane's `payload_from_plaintext_bytes`
        // mirrors it), so a mixed stream folds identically.
        let payload = I::payload_from_plaintext_bytes(payload_bytes, header.event_kind)?;
        return Ok(Some(Event {
            header,
            payload,
            hash_chain,
        }));
    };

    match store.open_encrypted_payload_bytes(
        &coordinate,
        header.event_kind,
        header.event_id,
        &meta,
        &payload_bytes,
    )? {
        PayloadPlaintext::Shredded => {
            tracing::warn!(
                target: "batpak::projection",
                flow = "project",
                entity,
                event_id = header.event_id.as_u128(),
                "skipping a crypto-shredded event during projection replay; the projected \
                 state omits its effect (the payload key has been destroyed — plaintext gone)"
            );
            Ok(None)
        }
        PayloadPlaintext::Plaintext(plaintext) => {
            let payload = I::payload_from_plaintext_bytes(plaintext, header.event_kind)?;
            Ok(Some(Event {
                header,
                payload,
                hash_chain,
            }))
        }
    }
}
