use crate::coordinate::DagPosition;
use crate::event::EventKind;
use serde::{Deserialize, Serialize};

/// EventHeader: metadata for every event. Store generates this — users don't call new directly.
/// repr(C) for deterministic field ordering (NOT a wire format — msgpack handles serialization).
/// [SPEC:src/event/header.rs]
#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventHeader {
    #[serde(with = "crate::wire::u128_bytes")]
    pub event_id: u128,
    #[serde(with = "crate::wire::u128_bytes")]
    pub correlation_id: u128,
    #[serde(with = "crate::wire::option_u128_bytes")]
    pub causation_id: Option<u128>,
    pub timestamp_us: i64,
    pub position: DagPosition,
    pub payload_size: u32,
    pub event_kind: EventKind,
    pub flags: u8,
    /// Content hash of the serialized payload. Enables automatic projection cache
    /// invalidation when event schemas evolve. Computed from payload bytes during
    /// writer step 5 (reuses the blake3 computation). [0u8; 32] when blake3 is off.
    /// [CROSS-POLLINATION:czap/typed-ref.ts — content addressing for auto-invalidation]
    #[serde(default)]
    pub content_hash: [u8; 32],
}

/// Flag bit constants for EventHeader.flags
pub const FLAG_REQUIRES_ACK: u8 = 0x01;
pub const FLAG_TRANSACTIONAL: u8 = 0x02;
pub const FLAG_REPLAY: u8 = 0x08;

impl EventHeader {
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
            event_id,
            correlation_id,
            causation_id,
            timestamp_us,
            position,
            payload_size,
            event_kind,
            flags: 0,
            content_hash: [0u8; 32],
        }
    }

    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    pub fn requires_ack(&self) -> bool {
        self.flags & FLAG_REQUIRES_ACK != 0
    }

    pub fn is_transactional(&self) -> bool {
        self.flags & FLAG_TRANSACTIONAL != 0
    }

    pub fn is_replay(&self) -> bool {
        self.flags & FLAG_REPLAY != 0
    }

    pub fn age_us(&self, now_us: i64) -> u64 {
        now_us.saturating_sub(self.timestamp_us).max(0) as u64
    }
}
