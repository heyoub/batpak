//! The runtime-owned capability backend that makes operation effects real.
//!
//! This is what turns the effect row from a declaration into an enforced
//! boundary. An operation reaches durable effects ONLY through a `Ctx`-owned
//! capability handle, and every handle performs its effect through this backend.
//! Because the runtime owns the backend and hands it out only per-invocation
//! through `Ctx`, a handler cannot append an event (or touch any other declared
//! effect) the runtime did not mediate — so the observed effect row is
//! authoritative, not cooperative. A handler with no backend bound simply cannot
//! perform the effect; the attempt is a typed error.

use batpak::event::EventKind;

/// Durable-effect backend the runtime injects into each invocation context.
///
/// Implementations are store-backed (see `StoreEffectBackend`). Trait-object
/// safe: every method takes already-encoded bytes so it can be held as
/// `Box<dyn EffectBackend>`.
pub trait EffectBackend {
    /// Append one event of `kind` carrying `payload` (already canonically
    /// encoded) to the runtime's durable store.
    ///
    /// # Errors
    /// Returns [`EffectError`] when the backend cannot perform the append.
    fn append_event(&mut self, kind: EventKind, payload: &[u8]) -> Result<(), EffectError>;

    /// Mediate one declared event-category read for this invocation.
    ///
    /// # Errors
    /// Returns [`EffectError`] when this backend does not support event reads or
    /// rejects the read.
    fn read_event(&mut self, event_category: &str) -> Result<(), EffectError> {
        let _ = event_category;
        Err(EffectError::new(
            "event reads are not supported by this effect backend",
        ))
    }

    /// Mediate one declared projection query for this invocation.
    ///
    /// # Errors
    /// Returns [`EffectError`] when this backend does not support projection
    /// queries or rejects the query.
    fn query_projection(&mut self, projection_id: &str) -> Result<(), EffectError> {
        let _ = projection_id;
        Err(EffectError::new(
            "projection queries are not supported by this effect backend",
        ))
    }

    /// Mediate one declared receipt emission for this invocation.
    ///
    /// # Errors
    /// Returns [`EffectError`] when this backend does not support receipt
    /// emission or rejects the emission.
    fn emit_receipt(&mut self, receipt_kind: &str) -> Result<(), EffectError> {
        let _ = receipt_kind;
        Err(EffectError::new(
            "receipt emission is not supported by this effect backend",
        ))
    }

    /// Mediate one declared host-control use for this invocation.
    ///
    /// # Errors
    /// Returns [`EffectError`] when this backend does not support host controls
    /// or rejects the use.
    fn use_host_control(&mut self) -> Result<(), EffectError> {
        Err(EffectError::new(
            "host controls are not supported by this effect backend",
        ))
    }
}

/// Failure performing a durable effect through an [`EffectBackend`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectError {
    message: String,
}

impl EffectError {
    /// Construct an effect error with a human-readable message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The failure message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for EffectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "effect backend failure: {}", self.message)
    }
}

impl std::error::Error for EffectError {}
