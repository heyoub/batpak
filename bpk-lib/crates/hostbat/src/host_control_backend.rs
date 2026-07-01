//! The host-layer [`EffectBackend`] that PERFORMS an operation's declared
//! host-control effect.
//!
//! A `Control` operation reaches host authority only through its `Ctx`
//! host-control handle, which performs the identified control through the
//! runtime-owned effect backend. `syncbat`'s store backend fails that axis
//! closed (a store is not a host); this is where the host actually performs it.
//! [`HostControlEffectBackend`] wraps an optional inner store backend so the
//! same composed backend still mediates the store axes (event append/read,
//! projection query, receipt emit) when one is bound, and falls through to the
//! typed fail-closed defaults when it is not.

use batpak::event::EventKind;
use syncbat::effect_backend::{EffectBackend, EffectError};

/// Performs the host control identified by an operation's declared control-id.
///
/// The blanket impl lets any `FnMut(&str) -> Result<(), HostControlError>` act
/// as a controller, mirroring [`crate::module::LifecycleHook`].
pub trait HostController {
    /// Perform the control identified by `control`.
    ///
    /// # Errors
    /// Returns [`HostControlError`] when the controller refuses or fails to
    /// perform the identified control.
    fn perform(&mut self, control: &str) -> Result<(), HostControlError>;
}

impl<F> HostController for F
where
    F: FnMut(&str) -> Result<(), HostControlError>,
{
    fn perform(&mut self, control: &str) -> Result<(), HostControlError> {
        self(control)
    }
}

/// Failure performing a host control through a [`HostController`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostControlError {
    message: String,
}

impl HostControlError {
    /// Construct a host-control error with a human-readable message.
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

impl std::fmt::Display for HostControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host control failure: {}", self.message)
    }
}

impl std::error::Error for HostControlError {}

/// An [`EffectBackend`] that performs host controls through a [`HostController`]
/// and delegates the store-effect axes to an optional inner backend.
///
/// The host-control axis is always performed by the `controller`. The store
/// axes delegate to `inner` when one is bound and fall through to the
/// [`EffectBackend`] typed fail-closed defaults when it is `None`, so a
/// host-control-only composition can never silently perform a store effect.
pub struct HostControlEffectBackend {
    inner: Option<Box<dyn EffectBackend>>,
    controller: Box<dyn HostController>,
}

impl HostControlEffectBackend {
    /// Compose a host-control backend over an optional inner store backend.
    #[must_use]
    pub fn new(inner: Option<Box<dyn EffectBackend>>, controller: Box<dyn HostController>) -> Self {
        Self { inner, controller }
    }
}

/// Zero-effect backend used when no inner store backend is bound: every store
/// axis resolves to the [`EffectBackend`] typed fail-closed default, so the
/// fallback reuses those defaults rather than duplicating their messages.
struct NoInnerBackend;

impl EffectBackend for NoInnerBackend {
    fn append_event(&mut self, _kind: EventKind, _payload: &[u8]) -> Result<(), EffectError> {
        Err(EffectError::new(
            "event appends are not supported without an inner effect backend",
        ))
    }
}

impl EffectBackend for HostControlEffectBackend {
    fn append_event(&mut self, kind: EventKind, payload: &[u8]) -> Result<(), EffectError> {
        match self.inner.as_deref_mut() {
            Some(inner) => inner.append_event(kind, payload),
            None => NoInnerBackend.append_event(kind, payload),
        }
    }

    fn read_event(&mut self, event_category: &str) -> Result<(), EffectError> {
        match self.inner.as_deref_mut() {
            Some(inner) => inner.read_event(event_category),
            None => NoInnerBackend.read_event(event_category),
        }
    }

    fn query_projection(&mut self, projection_id: &str) -> Result<(), EffectError> {
        match self.inner.as_deref_mut() {
            Some(inner) => inner.query_projection(projection_id),
            None => NoInnerBackend.query_projection(projection_id),
        }
    }

    fn emit_receipt(&mut self, receipt_kind: &str) -> Result<(), EffectError> {
        match self.inner.as_deref_mut() {
            Some(inner) => inner.emit_receipt(receipt_kind),
            None => NoInnerBackend.emit_receipt(receipt_kind),
        }
    }

    fn use_host_control(&mut self, control: &str) -> Result<(), EffectError> {
        self.controller
            .perform(control)
            .map_err(|error| EffectError::new(error.to_string()))
    }
}
