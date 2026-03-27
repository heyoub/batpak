use serde::{Deserialize, Serialize};
use std::fmt;

/// EventKind wraps a private u16. Products cannot construct arbitrary system kinds.
/// Products use EventKind::custom(category, type_id) which validates the range.
/// [SPEC:src/event/kind.rs]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventKind(u16); // PRIVATE inner field — not pub

impl EventKind {
    /// category:type encoding. Upper 4 bits = category, lower 12 = type.
    /// Products use categories 0x1-0xF. System uses 0x0 and 0xD.
    pub const fn custom(category: u8, type_id: u16) -> Self {
        // Validate: only lower 4 bits of category survive the shift.
        // category >= 16 would silently overflow into wrong namespace.
        assert!(category < 16, "EventKind category must be 0-15 (4 bits)");
        Self(((category as u16) << 12) | (type_id & 0x0FFF))
    }

    pub const fn category(self) -> u8 {
        (self.0 >> 12) as u8
    }

    pub const fn type_id(self) -> u16 {
        self.0 & 0x0FFF
    }

    pub const fn is_system(self) -> bool {
        self.category() == 0x0
    }

    pub const fn is_effect(self) -> bool {
        self.category() == 0xD
    }

    /// Library constants. Products NEVER define these — they use custom().
    pub const DATA: Self = Self(0x0000);
    pub const SYSTEM_INIT: Self = Self(0x0001);
    pub const SYSTEM_SHUTDOWN: Self = Self(0x0002);
    pub const SYSTEM_HEARTBEAT: Self = Self(0x0003);
    pub const SYSTEM_CONFIG_CHANGE: Self = Self(0x0004);
    pub const SYSTEM_CHECKPOINT: Self = Self(0x0005);
    pub const EFFECT_ERROR: Self = Self(0xD001);
    pub const EFFECT_RETRY: Self = Self(0xD002);
    pub const EFFECT_ACK: Self = Self(0xD004);
    pub const EFFECT_BACKPRESSURE: Self = Self(0xD005);
    pub const EFFECT_CANCEL: Self = Self(0xD006);
    pub const EFFECT_CONFLICT: Self = Self(0xD007);
    /// Used by compact() for tombstone markers. Public so consumers can detect them.
    pub const TOMBSTONE: Self = Self(0x0FFE);
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:04X}", self.0)
    }
}
