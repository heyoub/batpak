use serde::{Deserialize, Serialize};
use std::fmt;

/// EventKind wraps a private u16. Products cannot construct arbitrary system kinds.
/// Products use EventKind::custom(category, type_id) which validates the range.
/// [SPEC:src/event/kind.rs]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventKind(u16); // PRIVATE inner field — not pub

impl EventKind {
    /// category:type encoding. Upper 4 bits = category, lower 12 = type.
    /// Products use categories 0x1-0xC, 0xE-0xF. System reserves 0x0 and 0xD.
    ///
    /// # Example
    /// ```
    /// use batpak::prelude::*;
    /// let kind = EventKind::custom(0xF, 1);
    /// assert!(!kind.is_system());
    /// assert!(!kind.is_effect());
    /// ```
    pub const fn custom(category: u8, type_id: u16) -> Self {
        // Validate: only lower 4 bits of category survive the shift.
        // category >= 16 would silently overflow into wrong namespace.
        assert!(category < 16, "EventKind category must be 0-15 (4 bits)");
        // Categories 0x0 (system) and 0xD (effect) are reserved for library constants.
        // Products must use 0x1-0xC or 0xE-0xF.
        assert!(
            category != 0,
            "EventKind category 0x0 is reserved for system kinds (SYSTEM_INIT, etc.)"
        );
        assert!(
            category != 0xD,
            "EventKind category 0xD is reserved for effect kinds (EFFECT_ERROR, etc.)"
        );
        Self(((category as u16) << 12) | (type_id & 0x0FFF))
    }

    /// Returns the upper 4-bit category of this kind.
    pub const fn category(self) -> u8 {
        (self.0 >> 12) as u8
    }

    /// Returns the lower 12-bit type identifier within the category.
    pub const fn type_id(self) -> u16 {
        self.0 & 0x0FFF
    }

    /// Returns `true` if this kind is in the reserved system category (0x0).
    pub const fn is_system(self) -> bool {
        self.category() == 0x0
    }

    /// Returns `true` if this kind is in the reserved effect category (0xD).
    pub const fn is_effect(self) -> bool {
        self.category() == 0xD
    }

    /// Library constants. Products NEVER define these — they use custom().
    /// User-defined data event.
    pub const DATA: Self = Self(0x0000);
    /// System initialisation event.
    pub const SYSTEM_INIT: Self = Self(0x0001);
    /// System shutdown event.
    pub const SYSTEM_SHUTDOWN: Self = Self(0x0002);
    /// System heartbeat event.
    pub const SYSTEM_HEARTBEAT: Self = Self(0x0003);
    /// System configuration-change event.
    pub const SYSTEM_CONFIG_CHANGE: Self = Self(0x0004);
    /// System checkpoint event.
    pub const SYSTEM_CHECKPOINT: Self = Self(0x0005);
    /// Batch envelope marker. Internal only—never visible to queries.
    pub const SYSTEM_BATCH_BEGIN: Self = Self(0x0006);
    /// Batch commit marker. Internal only—never visible to queries.
    /// Paired with SYSTEM_BATCH_BEGIN for two-phase commit semantics.
    pub const SYSTEM_BATCH_COMMIT: Self = Self(0x0007);
    /// Effect: an error was observed during processing.
    pub const EFFECT_ERROR: Self = Self(0xD001);
    /// Effect: a retry is being attempted.
    pub const EFFECT_RETRY: Self = Self(0xD002);
    /// Effect: acknowledgement of a prior event.
    pub const EFFECT_ACK: Self = Self(0xD004);
    /// Effect: backpressure signal to slow producers.
    pub const EFFECT_BACKPRESSURE: Self = Self(0xD005);
    /// Effect: cancellation of a prior request.
    pub const EFFECT_CANCEL: Self = Self(0xD006);
    /// Effect: a conflict was detected.
    pub const EFFECT_CONFLICT: Self = Self(0xD007);
    /// Used by compact() for tombstone markers. Public so consumers can detect them.
    pub const TOMBSTONE: Self = Self(0x0FFE);
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:04X}", self.0)
    }
}
