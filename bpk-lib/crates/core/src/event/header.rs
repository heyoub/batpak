use crate::coordinate::DagPosition;
use crate::event::EventKind;
use crate::id::{CausationId, CorrelationId, EventId};
use serde::{Deserialize, Serialize};

/// EventHeader: metadata for every event. Store generates this — users don't call new directly.
/// repr(C) for deterministic field ordering (NOT a wire format — msgpack handles serialization).
#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventHeader {
    /// Globally unique identifier for this event, assigned by the store.
    ///
    /// Wire format is the typed id's own serde impl (16 big-endian bytes) — the
    /// bytes are byte-identical to the prior raw `u128` encoding so MessagePack
    /// goldens do not move.
    pub event_id: EventId,
    /// Groups related events that share a single originating request or saga.
    pub correlation_id: CorrelationId,
    /// Identifies the direct predecessor event that caused this one, if any.
    pub causation_id: Option<CausationId>,
    /// Wall-clock timestamp in microseconds when the event was appended.
    pub timestamp_us: i64,
    /// Logical position of this event within its stream DAG.
    pub position: DagPosition,
    /// Byte length of the serialized payload.
    pub payload_size: u32,
    /// Category discriminant describing what kind of domain event this is.
    pub event_kind: EventKind,
    /// Bit flags encoding delivery and transaction semantics.
    pub flags: u8,
    /// Content hash of the serialized payload. Enables automatic projection cache
    /// invalidation when event schemas evolve. Computed from payload bytes during
    /// writer step 5 (reuses the blake3 computation).
    #[serde(default)]
    pub content_hash: [u8; 32],
    /// Wire schema version of the payload bytes (`EventPayload::PAYLOAD_VERSION`).
    ///
    /// Stamped at the typed-append seam; untyped / legacy paths leave it `0`,
    /// which the decode seam treats as "tolerant decode as current". This field
    /// rides INSIDE the frame msgpack but OUTSIDE the hashed/signed region: the
    /// content hash covers payload bytes only and the signature cover is
    /// `event_id + sequence + coord + kind + prev_hash + content_hash +
    /// extensions` (see `store/signing.rs::cover_bytes`), so adding it moves
    /// neither any event content hash nor any signature. `#[serde(default)]`
    /// lets pre-versioning frames decode as `0`.
    #[serde(default)]
    pub payload_version: u16,
    /// Optional crypto-shred payload-encryption metadata (opt-in
    /// `payload-encryption`).
    ///
    /// `Some` only for events whose ON-DISK payload is ciphertext; `None` — and,
    /// via `skip_serializing_if`, entirely ABSENT from the frame msgpack — for
    /// every plaintext event, so a plaintext frame is byte-identical to a build
    /// compiled without this field (the encoder is `to_vec_named`, a msgpack MAP,
    /// so an omitted trailing key leaves the map key set unchanged). Like
    /// [`payload_version`](Self::payload_version) this field rides INSIDE the
    /// frame msgpack but OUTSIDE the hashed/signed region: `content_hash` /
    /// `event_hash` cover the (cipher)payload bytes only and the signing cover is
    /// `event_id + sequence + coord + kind + prev_hash + content_hash +
    /// extensions` (see `store/signing.rs::cover_bytes`), so stamping it moves
    /// neither any content hash nor any signature. `#[serde(default)]` lets a
    /// frame without the field decode as `None`.
    #[cfg(feature = "payload-encryption")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_encryption: Option<PayloadEncryption>,
}

/// On-disk encryption metadata for a crypto-shred payload (opt-in
/// `payload-encryption`).
///
/// Present on the [`EventHeader`] of every event whose stored payload is
/// ciphertext, absent for plaintext events. It carries only what the read path
/// needs to find the key and open the ciphertext — never any key material:
///
/// * `keyscope_id` — the raw bytes of the
///   [`KeyScope`](crate::store::KeyScope) the payload key is filed under; the
///   read path rebuilds the scope from these bytes to look the key up (or
///   observe its absence, i.e. a shred). Derived from non-secret
///   coordinates/kinds/ids.
/// * `nonce` — the 192-bit XChaCha20-Poly1305 nonce the payload was sealed with;
///   public by construction.
///
/// Both fields are non-secret, so deriving [`Debug`] here exposes nothing
/// sensitive (key bytes live only in [`PayloadKey`](crate::store::PayloadKey),
/// which never renders its material).
#[cfg(feature = "payload-encryption")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "payload-encryption"))
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadEncryption {
    /// Raw [`KeyScope`](crate::store::KeyScope) bytes the payload key is filed
    /// under. Non-secret (derived from coordinates/kinds/ids).
    pub keyscope_id: Vec<u8>,
    /// The 192-bit XChaCha20-Poly1305 nonce the payload was sealed with. Public.
    pub nonce: [u8; 24],
}

/// Flag bit constants for EventHeader.flags
/// Signals that the consumer must explicitly acknowledge this event before the next is delivered.
pub const FLAG_REQUIRES_ACK: u8 = 0x01;
/// Marks this event as part of an atomic transaction group.
pub const FLAG_TRANSACTIONAL: u8 = 0x02;
/// Marks this event as a replay of a previously persisted event rather than a live emission.
pub const FLAG_REPLAY: u8 = 0x08;

impl EventHeader {
    /// Constructs an `EventHeader` with all fields; flags and content hash default to zero.
    ///
    /// The parameters accept raw `u128` values to preserve the existing
    /// zero-ceremony call shape used by wire-decode paths. Public call-sites
    /// that already hold typed ids ([`crate::id::EventId`],
    /// [`crate::id::CorrelationId`], [`crate::id::CausationId`]) should use
    /// [`EventHeader::new_typed`] instead — the typed variant cannot
    /// accidentally swap an event id for a correlation id at the call site.
    pub fn new(
        event_id: u128,
        correlation_id: u128,
        causation_id: Option<u128>,
        timestamp_us: i64,
        position: DagPosition,
        payload_size: u32,
        event_kind: EventKind,
    ) -> Self {
        Self {
            event_id: EventId::from(event_id),
            correlation_id: CorrelationId::from(correlation_id),
            causation_id: causation_id.map(CausationId::from),
            timestamp_us,
            position,
            payload_size,
            event_kind,
            flags: 0,
            content_hash: [0u8; 32],
            payload_version: 0,
            #[cfg(feature = "payload-encryption")]
            payload_encryption: None,
        }
    }

    /// Typed-id constructor. See [`EventHeader::new`] for the raw-id
    /// escape hatch used by wire-decode paths.
    ///
    /// `From<u128>` / `as_u128()` on each newtype form the internal-only
    /// wire-serde boundary (G10) — downstream crates should reach for this
    /// method, not the raw-id form.
    pub fn new_typed(
        event_id: crate::id::EventId,
        correlation_id: crate::id::CorrelationId,
        causation_id: Option<crate::id::CausationId>,
        timestamp_us: i64,
        position: DagPosition,
        payload_size: u32,
        event_kind: EventKind,
    ) -> Self {
        Self {
            event_id,
            correlation_id,
            causation_id,
            timestamp_us,
            position,
            payload_size,
            event_kind,
            flags: 0,
            content_hash: [0u8; 32],
            payload_version: 0,
            #[cfg(feature = "payload-encryption")]
            payload_encryption: None,
        }
    }

    /// Sets the flags byte on this header.
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    /// Stamps the payload schema version (`EventPayload::PAYLOAD_VERSION`).
    ///
    /// Only the typed-append lowerings call this; untyped/legacy paths leave the
    /// default `0`. The value lives outside the hashed/signed region, so
    /// stamping it does not move any content hash or signature.
    pub fn with_payload_version(mut self, payload_version: u16) -> Self {
        self.payload_version = payload_version;
        self
    }

    /// Returns `true` if the consumer must acknowledge this event.
    pub fn requires_ack(&self) -> bool {
        self.flags & FLAG_REQUIRES_ACK != 0
    }

    /// Returns `true` if this event is part of a transaction.
    pub fn is_transactional(&self) -> bool {
        self.flags & FLAG_TRANSACTIONAL != 0
    }

    /// Returns `true` if this event is being replayed rather than emitted live.
    pub fn is_replay(&self) -> bool {
        self.flags & FLAG_REPLAY != 0
    }

    /// Returns the age of this event in microseconds relative to `now_us`.
    pub fn age_us(&self, now_us: i64) -> u64 {
        now_us
            .saturating_sub(self.timestamp_us)
            .max(0)
            .cast_unsigned()
    }
}
