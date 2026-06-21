use serde::{Deserialize, Serialize};
use std::fmt;

/// EventKind wraps a private u16. Products cannot construct arbitrary system kinds.
/// Use [`EventKind::try_custom`] for runtime input and [`EventKind::custom`]
/// for const/internal call sites that should panic on invalid shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventKind(u16); // PRIVATE inner field — not pub

/// Validation error returned by [`EventKind::try_custom`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventKindError {
    /// The category does not fit in the supported 4-bit namespace.
    CategoryOutOfRange {
        /// Rejected category value.
        category: u8,
    },
    /// Category `0x0` is reserved for system kinds.
    ReservedSystemCategory,
    /// Category `0xD` is reserved for effect kinds.
    ReservedEffectCategory,
    /// The type id exceeds the supported 12-bit namespace.
    TypeIdOutOfRange {
        /// Rejected type identifier.
        type_id: u16,
    },
}

impl EventKind {
    /// Fallible custom kind constructor for public input.
    ///
    /// Use this when the category/type pair originates from user input,
    /// configuration, or any other runtime boundary that must not panic.
    ///
    /// # Errors
    /// Returns [`EventKindError`] when the caller supplies a reserved category
    /// or a value that does not fit the supported 4-bit/12-bit custom namespace.
    pub fn try_custom(category: u8, type_id: u16) -> Result<Self, EventKindError> {
        if category >= 16 {
            return Err(EventKindError::CategoryOutOfRange { category });
        }
        if category == 0 {
            return Err(EventKindError::ReservedSystemCategory);
        }
        if category == 0xD {
            return Err(EventKindError::ReservedEffectCategory);
        }
        if type_id >= 0x1000 {
            return Err(EventKindError::TypeIdOutOfRange { type_id });
        }
        Ok(Self(((category as u16) << 12) | type_id))
    }

    /// category:type encoding. Upper 4 bits = category, lower 12 = type.
    /// Products use categories 0x1-0xC, 0xE-0xF. System reserves 0x0 and 0xD.
    ///
    /// This constructor is intentionally strict: invalid inputs panic instead
    /// of silently truncating into a different namespace. That keeps the
    /// `const fn` surface honest and prevents accidental cross-category
    /// collisions from being smuggled through as "best effort" encoding.
    /// Runtime callers should prefer [`EventKind::try_custom`] so invalid user
    /// input becomes a typed error instead of a process abort.
    ///
    /// # Example
    /// ```
    /// use batpak::prelude::*;
    /// let kind = EventKind::custom(0xF, 1);
    /// assert!(!kind.is_system());
    /// assert!(!kind.is_effect());
    /// ```
    ///
    /// # Panics
    /// Panics when `category` or `type_id` fall outside the supported custom
    /// namespace. This is intentional for const/static/internal use only.
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
        assert!(
            type_id < 0x1000,
            "EventKind type_id must fit in 12 bits without truncation"
        );
        Self(((category as u16) << 12) | type_id)
    }

    /// Reconstruct a kind from its raw `u16` encoding.
    ///
    /// This is the inverse of [`as_raw_u16`](Self::as_raw_u16) and performs no
    /// validation: it is for round-tripping an already-encoded `u16` (e.g. the
    /// packed `kind_bits` carried by a registry collision record) back into a
    /// kind so its [`category`](Self::category)/[`type_id`](Self::type_id)
    /// accessors can narrow the nibbles without an unchecked cast.
    #[inline]
    pub(crate) const fn from_raw_u16(raw: u16) -> Self {
        Self(raw)
    }

    /// Returns the canonical on-disk and over-the-wire `u16` encoding.
    ///
    /// This value must stay byte-for-byte equal to `(category << 12) | type_id`.
    /// Receipt signing covers, projection cache keys, cold-start rows, SIDX
    /// footers, mmap rows, and writer notifications all depend on this stable
    /// encoding. See ADR-0019.
    ///
    /// The derive macro intentionally inlines the same formula while expanding
    /// user code, before this type is available in the generated expression.
    #[inline]
    pub const fn as_raw_u16(self) -> u16 {
        self.0
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

    /// Returns `true` if this kind is reserved for internal substrate use and
    /// must not be appended through the public raw-`kind` write surface.
    ///
    /// Reserved kinds are the system category (0x0) and the effect category
    /// (0xD). Note that `TOMBSTONE` (0x0FFE) and `SYSTEM_DENIAL` (0x000F) live
    /// in category 0x0, so `is_system()` already covers them — no extra
    /// disjunct is required here.
    pub const fn is_reserved(self) -> bool {
        self.is_system() || self.is_effect()
    }

    /// Library constants. Products NEVER define these — they use custom().
    /// User-defined data event. Uses product category 0x1, not system category 0x0.
    pub const DATA: Self = Self(0x1000);
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
    /// Store lifecycle receipt emitted after a successful mutable open.
    pub const SYSTEM_OPEN_COMPLETED: Self = Self(0x0008);
    /// Store lifecycle receipt emitted during an explicit graceful close.
    pub const SYSTEM_CLOSE_COMPLETED: Self = Self(0x0009);
    /// Persisted gate-denial audit receipt.
    pub const SYSTEM_DENIAL: Self = Self(0x000F);
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

    /// Fixture-only: NETBAT/1 wire-parity heartbeat request payload used by
    /// the `refbat` reference host. Category `0xF`, type_id `0xA01`.
    ///
    /// This constant lives in `batpak::event::kind` so the substrate owns
    /// the numeric registry naming, but the payload struct itself
    /// (`refbat::heartbeat::SystemHeartbeatRequest`) lives in `refbat` — it is
    /// a fixture, not a substrate-promoted public event. The constant
    /// remains in sync with the `#[batpak(category = 0xF, type_id =
    /// 0xA01)]` attribute on the struct via a compile-time alignment test
    /// in `crates/refbat/src/heartbeat.rs`.
    pub const SYSTEM_HEARTBEAT_REQUEST: Self = Self::custom(0xF, 0xA01);

    /// Fixture-only: NETBAT/1 wire-parity heartbeat ack payload used by
    /// the `refbat` reference host. Category `0xF`, type_id `0xA02`. See
    /// [`SYSTEM_HEARTBEAT_REQUEST`](Self::SYSTEM_HEARTBEAT_REQUEST) for
    /// the substrate-vs-fixture placement rationale.
    pub const SYSTEM_HEARTBEAT_ACK: Self = Self::custom(0xF, 0xA02);
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:04X}", self.0)
    }
}

impl fmt::Display for EventKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CategoryOutOfRange { category } => {
                write!(f, "EventKind category {category:#X} must fit in 4 bits")
            }
            Self::ReservedSystemCategory => {
                write!(f, "EventKind category 0x0 is reserved for system kinds")
            }
            Self::ReservedEffectCategory => {
                write!(f, "EventKind category 0xD is reserved for effect kinds")
            }
            Self::TypeIdOutOfRange { type_id } => {
                write!(f, "EventKind type_id {type_id:#X} must fit in 12 bits")
            }
        }
    }
}

impl std::error::Error for EventKindError {}
