//! Delivery-observation witnesses.
//!
//! This module expresses the composition required for exactly-once effects:
//! a substrate-supplied at-least-once checkpoint witness plus a
//! consumer-supplied idempotency key.

/// Typed durable cursor checkpoint identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckpointId(String);

impl CheckpointId {
    /// Construct a checkpoint identity from owned string content.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
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
    #[cfg(test)]
    pub(crate) fn new(checkpoint_id: CheckpointId) -> Self {
        Self(checkpoint_id)
    }

    /// Wrap the raw cursor callback checkpoint identifier.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn from_cursor_callback(raw: impl Into<String>) -> Self {
        Self::new(CheckpointId::new(raw))
    }
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
    use super::{AtLeastOnce, CheckpointId, IdempotencyKey, ObservedOnce};

    #[test]
    fn observed_once_round_trips_into_parts() {
        let at_least_once = AtLeastOnce::new(CheckpointId::new("cursor-checkpoint"));
        let idempotency = IdempotencyKey::from_bytes([7; 32]);

        let observed = ObservedOnce::new(at_least_once.clone(), idempotency);
        let (actual_at_least_once, actual_idempotency) = observed.into_parts();

        assert_eq!(actual_at_least_once, at_least_once);
        assert_eq!(actual_idempotency, idempotency);
    }

    #[test]
    fn at_least_once_from_cursor_callback_wraps_checkpoint_identity() {
        let at_least_once = AtLeastOnce::from_cursor_callback("cursor-checkpoint");
        let (checkpoint_id,) = (at_least_once.0,);

        assert_eq!(checkpoint_id.as_str(), "cursor-checkpoint");
    }
}
