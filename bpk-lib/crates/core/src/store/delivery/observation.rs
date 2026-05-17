//! Delivery-observation witnesses.
//!
//! This module expresses the composition required for exactly-once effects:
//! a substrate-supplied at-least-once checkpoint witness plus a
//! consumer-supplied idempotency key.

use crate::coordinate::MAX_COORDINATE_COMPONENT_LEN;
use std::fmt;

/// Maximum byte length for a durable cursor checkpoint identifier.
///
/// Kept equal to coordinate component length so durable witness identities and
/// logical stream identifiers share one bounded string envelope.
pub const MAX_CHECKPOINT_ID_LEN: usize = MAX_COORDINATE_COMPONENT_LEN;

/// Typed durable cursor checkpoint identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointId(String);

impl CheckpointId {
    /// Construct a checkpoint identity from owned string content.
    ///
    /// # Errors
    /// Returns [`CheckpointIdError`] when the identifier is empty, too long,
    /// path-shaped, contains forbidden control bytes, or contains identity
    /// separator characters reserved by cursor region identities.
    pub fn new(value: impl Into<String>) -> Result<Self, CheckpointIdError> {
        let value = value.into();
        validate_checkpoint_id(&value)?;
        Ok(Self(value))
    }

    /// Borrow the underlying checkpoint identifier as `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Substrate witness that delivery is at-least-once for the given checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtLeastOnce(CheckpointId);

impl AtLeastOnce {
    /// Create a new at-least-once witness from a typed checkpoint identifier.
    #[must_use]
    pub(crate) fn new(checkpoint_id: CheckpointId) -> Self {
        Self(checkpoint_id)
    }

    /// Wrap the raw cursor callback checkpoint identifier.
    pub(crate) fn from_cursor_callback(raw: impl Into<String>) -> Result<Self, CheckpointIdError> {
        Ok(Self::new(CheckpointId::new(raw)?))
    }

    /// Borrow the checkpoint identity that minted this witness.
    #[must_use]
    pub fn checkpoint_id(&self) -> &CheckpointId {
        &self.0
    }
}

/// Error returned when constructing a [`CheckpointId`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckpointIdError {
    /// The identifier was empty.
    Empty,
    /// The identifier exceeded [`MAX_CHECKPOINT_ID_LEN`].
    TooLong {
        /// Actual identifier length.
        len: usize,
        /// Maximum permitted length.
        max: usize,
    },
    /// The identifier contained a NUL byte (`'\0'`).
    NulByte,
    /// The identifier contained a forbidden ASCII control character.
    ControlChar,
    /// The identifier contained a path-traversal substring (`..` or `/`).
    PathTraversal,
    /// The identifier contained a cursor identity separator (`|` or `=`).
    ForbiddenSeparator,
}

impl fmt::Display for CheckpointIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "checkpoint id cannot be empty"),
            Self::TooLong { len, max } => {
                write!(f, "checkpoint id length {len} exceeds maximum {max}")
            }
            Self::NulByte => write!(f, "checkpoint id contains a NUL byte"),
            Self::ControlChar => write!(
                f,
                "checkpoint id contains a forbidden ASCII control character"
            ),
            Self::PathTraversal => write!(
                f,
                "checkpoint id contains a forbidden path-traversal substring (`..` or `/`)"
            ),
            Self::ForbiddenSeparator => write!(
                f,
                "checkpoint id contains a forbidden identity-separator character (`|` or `=`)"
            ),
        }
    }
}

impl std::error::Error for CheckpointIdError {}

fn validate_checkpoint_id(value: &str) -> Result<(), CheckpointIdError> {
    if value.is_empty() {
        return Err(CheckpointIdError::Empty);
    }
    if value.len() > MAX_CHECKPOINT_ID_LEN {
        return Err(CheckpointIdError::TooLong {
            len: value.len(),
            max: MAX_CHECKPOINT_ID_LEN,
        });
    }
    for byte in value.bytes() {
        if byte == 0 {
            return Err(CheckpointIdError::NulByte);
        }
        if byte < 0x20 || byte == 0x7F {
            return Err(CheckpointIdError::ControlChar);
        }
    }
    if value.contains('/') || value.contains("..") {
        return Err(CheckpointIdError::PathTraversal);
    }
    if value.contains('|') || value.contains('=') {
        return Err(CheckpointIdError::ForbiddenSeparator);
    }
    Ok(())
}

/// Fixed-width consumer idempotency digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IdempotencyKey([u8; 32]);

impl IdempotencyKey {
    /// Construct an idempotency key from raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[must_use = "ObservedOnce must be consumed via into_parts() to retain the exactly-once witness"]
/// Exactly-once witness formed by composing substrate delivery with consumer
/// idempotency.
pub struct ObservedOnce {
    _seal: seal::Token,
    at_least_once: AtLeastOnce,
    idempotency_key: IdempotencyKey,
}

mod seal {
    pub(super) struct Token;
}

impl ObservedOnce {
    /// Create an exactly-once witness from the required substrate and consumer
    /// proofs.
    pub fn new(at_least_once: AtLeastOnce, idempotency_key: IdempotencyKey) -> Self {
        Self {
            _seal: seal::Token,
            at_least_once,
            idempotency_key,
        }
    }

    /// Consume the exactly-once witness into its component proofs.
    #[must_use]
    pub fn into_parts(self) -> (AtLeastOnce, IdempotencyKey) {
        (self.at_least_once, self.idempotency_key)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AtLeastOnce, CheckpointId, CheckpointIdError, IdempotencyKey, ObservedOnce,
        MAX_CHECKPOINT_ID_LEN,
    };

    #[test]
    fn observed_once_round_trips_into_parts() {
        let at_least_once =
            AtLeastOnce::new(CheckpointId::new("cursor-checkpoint").expect("valid checkpoint id"));
        let idempotency = IdempotencyKey::from_bytes([7; 32]);

        let observed = ObservedOnce::new(at_least_once.clone(), idempotency);
        let (actual_at_least_once, actual_idempotency) = observed.into_parts();

        assert_eq!(actual_at_least_once, at_least_once);
        assert_eq!(actual_idempotency, idempotency);
    }

    #[test]
    fn at_least_once_from_cursor_callback_wraps_checkpoint_identity() {
        let at_least_once =
            AtLeastOnce::from_cursor_callback("cursor-checkpoint").expect("valid checkpoint id");

        assert_eq!(at_least_once.checkpoint_id().as_str(), "cursor-checkpoint");
    }

    #[test]
    fn checkpoint_id_rejects_path_shapes_and_control_bytes() {
        assert_eq!(
            CheckpointId::new("../../etc/passwd"),
            Err(CheckpointIdError::PathTraversal)
        );
        assert_eq!(CheckpointId::new(""), Err(CheckpointIdError::Empty));
        assert_eq!(
            CheckpointId::new("with/slash"),
            Err(CheckpointIdError::PathTraversal)
        );
        assert_eq!(
            CheckpointId::new("with\0nul"),
            Err(CheckpointIdError::NulByte)
        );
        assert_eq!(
            CheckpointId::new("with\x01ctrl"),
            Err(CheckpointIdError::ControlChar)
        );
    }

    #[test]
    fn checkpoint_id_rejects_overlong_and_identity_separator_values() {
        let too_long = "x".repeat(MAX_CHECKPOINT_ID_LEN + 1);
        assert_eq!(
            CheckpointId::new(too_long),
            Err(CheckpointIdError::TooLong {
                len: MAX_CHECKPOINT_ID_LEN + 1,
                max: MAX_CHECKPOINT_ID_LEN
            })
        );
        assert_eq!(
            CheckpointId::new("scope|fact"),
            Err(CheckpointIdError::ForbiddenSeparator)
        );
        assert_eq!(
            CheckpointId::new("scope=fact"),
            Err(CheckpointIdError::ForbiddenSeparator)
        );
    }

    #[test]
    fn checkpoint_id_error_is_displayable_std_error() {
        fn assert_error_trait(_: &dyn std::error::Error) {}

        let error = CheckpointIdError::PathTraversal;
        assert_error_trait(&error);
        assert!(error.to_string().contains("path-traversal"));
    }
}
