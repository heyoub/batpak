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
