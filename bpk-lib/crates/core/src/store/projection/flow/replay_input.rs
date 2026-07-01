#[cfg(feature = "payload-encryption")]
use crate::event::EventKind;
use crate::event::{Event, JsonValueInput, ProjectionInput, RawMsgpackInput};
use crate::store::index::DiskPos;
use crate::store::StoreError;

/// Internal projection-replay machinery. Exposed as `pub` (behind
/// `#[doc(hidden)]`) only to satisfy the public bound on
/// `Store::project` / `project_if_changed` / `watch_projection` without
/// tripping the `private_bounds` lint. External callers cannot implement
/// this trait (its `Reader` parameter is a `#[doc(hidden)]` internal
/// type) and must not rely on it being stable.
#[doc(hidden)]
pub trait ReplayInput: ProjectionInput {
    fn read_batch(
        reader: &crate::store::segment::scan::Reader,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<Self::Payload>>, StoreError>;

    fn read_one(
        reader: &crate::store::segment::scan::Reader,
        pos: &DiskPos,
    ) -> Result<Event<Self::Payload>, StoreError>;

    /// Decode already-plaintext payload BYTES into this replay lane's payload
    /// type (opt-in `payload-encryption`).
    ///
    /// The key-aware projection replay path decrypts an encrypted event's
    /// ciphertext to plaintext bytes via the Stage C primitive, then hands those
    /// bytes here — so the encrypted-event decode ends in EXACTLY the lane the
    /// plaintext path uses. Passing a PLAINTEXT event's on-disk bytes here must
    /// also match the plaintext read seam byte-for-byte (hence the
    /// system-batch-marker carve-out on the value lane), so a mixed
    /// plaintext/encrypted stream folds identically to the pre-encryption path.
    #[cfg(feature = "payload-encryption")]
    fn payload_from_plaintext_bytes(
        bytes: Vec<u8>,
        event_kind: EventKind,
    ) -> Result<Self::Payload, StoreError>;
}

impl ReplayInput for JsonValueInput {
    fn read_batch(
        reader: &crate::store::segment::scan::Reader,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<Self::Payload>>, StoreError> {
        reader.read_events_batch(positions)
    }

    fn read_one(
        reader: &crate::store::segment::scan::Reader,
        pos: &DiskPos,
    ) -> Result<Event<Self::Payload>, StoreError> {
        reader.read_event_only(pos)
    }

    #[cfg(feature = "payload-encryption")]
    fn payload_from_plaintext_bytes(
        bytes: Vec<u8>,
        event_kind: EventKind,
    ) -> Result<Self::Payload, StoreError> {
        // Mirror the plaintext value seam (`decode_frame_payload_value`): the
        // in-band batch markers carry no user payload and decode to `Null`;
        // everything else is MessagePack-decoded into a `serde_json::Value`.
        match event_kind {
            EventKind::SYSTEM_BATCH_BEGIN | EventKind::SYSTEM_BATCH_COMMIT => {
                Ok(serde_json::Value::Null)
            }
            _ => crate::encoding::from_bytes(&bytes)
                .map_err(|error| StoreError::Serialization(Box::new(error))),
        }
    }
}

impl ReplayInput for RawMsgpackInput {
    fn read_batch(
        reader: &crate::store::segment::scan::Reader,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<Self::Payload>>, StoreError> {
        reader.read_raw_events_batch(positions)
    }

    fn read_one(
        reader: &crate::store::segment::scan::Reader,
        pos: &DiskPos,
    ) -> Result<Event<Self::Payload>, StoreError> {
        reader.read_event_raw_only(pos)
    }

    #[cfg(feature = "payload-encryption")]
    fn payload_from_plaintext_bytes(
        bytes: Vec<u8>,
        _event_kind: EventKind,
    ) -> Result<Self::Payload, StoreError> {
        // The raw lane keeps payloads as their MessagePack bytes verbatim — for a
        // decrypted event that is the recovered plaintext bytes, exactly as
        // `read_event_raw_only` returns a plaintext event's on-disk bytes.
        Ok(bytes)
    }
}
