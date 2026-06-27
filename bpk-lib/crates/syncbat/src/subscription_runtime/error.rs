use std::error::Error;
use std::fmt;

/// Stable stream terminal codes emitted through netbat `SUB_ERR` / `SUB_END`.
pub mod stream_code {
    /// Subscription id is not registered.
    pub const UNKNOWN_SUBSCRIPTION: &str = "unknown_subscription";
    /// Resume cursor bytes failed decode or semantic checks.
    pub const CURSOR_INVALID: &str = "cursor_invalid";
    /// Resume cursor does not match subscription/category binding.
    pub const CURSOR_MISMATCH: &str = "cursor_mismatch";
    /// Client exceeded bounded delivery window without cumulative ACK.
    pub const SLOW_CONSUMER: &str = "slow_consumer";
    /// Post-subscribe control frame was malformed.
    pub const MALFORMED_STREAM_FRAME: &str = "malformed_stream_frame";
    /// Client cancelled the subscription stream.
    pub const CLIENT_CANCELLED: &str = "client_cancelled";
}

/// Error returned by the subscription runtime engine.
#[derive(Debug)]
pub enum SubscriptionRuntimeError {
    /// Subscription id grammar or length is invalid.
    InvalidSubscriptionId {
        /// Stable reason token.
        reason: &'static str,
    },
    /// Subscription id is already present in the registry.
    DuplicateSubscription {
        /// Duplicated subscription id.
        id: String,
    },
    /// Subscription route declaration is invalid.
    InvalidRoute {
        /// Stable reason token.
        reason: &'static str,
    },
    /// Runtime configuration cannot support a valid stream.
    InvalidConfig {
        /// Stable reason token.
        reason: &'static str,
    },
    /// Subscription id is not present in the registry.
    UnknownSubscription {
        /// Requested subscription id.
        id: String,
    },
    /// Resume cursor decode or validation failed.
    CursorInvalid {
        /// Stable reason token.
        reason: &'static str,
    },
    /// Resume cursor does not match the subscription route binding.
    CursorMismatch {
        /// Stable reason token.
        reason: &'static str,
    },
    /// Store read or envelope encoding failed.
    Store(batpak::store::StoreError),
    /// Canonical envelope encoding failed.
    EnvelopeEncoding(String),
    /// Cumulative ACK referenced an unknown delivery index or cursor.
    AckInvalid {
        /// Stable reason token.
        reason: &'static str,
    },
}

impl fmt::Display for SubscriptionRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSubscriptionId { reason } => {
                write!(f, "invalid subscription id: {reason}")
            }
            Self::DuplicateSubscription { id } => {
                write!(f, "duplicate subscription route: {id}")
            }
            Self::InvalidRoute { reason } => write!(f, "invalid subscription route: {reason}"),
            Self::InvalidConfig { reason } => write!(f, "invalid subscription config: {reason}"),
            Self::UnknownSubscription { id } => write!(f, "unknown subscription: {id}"),
            Self::CursorInvalid { reason } => write!(f, "cursor invalid: {reason}"),
            Self::CursorMismatch { reason } => write!(f, "cursor mismatch: {reason}"),
            Self::Store(error) => write!(f, "store error: {error}"),
            Self::EnvelopeEncoding(detail) => write!(f, "envelope encoding failed: {detail}"),
            Self::AckInvalid { reason } => write!(f, "ack invalid: {reason}"),
        }
    }
}

impl Error for SubscriptionRuntimeError {}

impl From<batpak::store::StoreError> for SubscriptionRuntimeError {
    fn from(error: batpak::store::StoreError) -> Self {
        Self::Store(error)
    }
}
