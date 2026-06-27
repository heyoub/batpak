//! Event-kind to event-payload schema bindings for host-mediated appends.
//!
//! Bindings are declared per module, aggregated globally by event kind at host
//! composition, and folded into `H_module`, `H_interface`, and
//! [`crate::client_manifest::ClientManifest`].

use batpak::event::EventKind;
use serde::Serialize;

use crate::error::HostError;

/// Binds one event kind to a declared event-payload schema identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct EventPayloadBinding {
    kind: EventKind,
    payload_schema_ref: String,
}

impl EventPayloadBinding {
    /// Declare a binding from `kind` to `payload_schema_ref`.
    ///
    /// # Errors
    /// [`HostError::EventPayloadBindingInvalid`] when the schema reference is empty.
    pub fn new(kind: EventKind, payload_schema_ref: impl Into<String>) -> Result<Self, HostError> {
        let payload_schema_ref = payload_schema_ref.into();
        if payload_schema_ref.is_empty() {
            return Err(HostError::EventPayloadBindingInvalid {
                kind: kind.as_raw_u16(),
                detail: "payload schema reference is empty".to_owned(),
            });
        }
        Ok(Self {
            kind,
            payload_schema_ref,
        })
    }

    /// The bound event kind.
    #[must_use]
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// Canonical raw kind encoding used as the global binding key.
    #[must_use]
    pub fn kind_raw(&self) -> u16 {
        self.kind.as_raw_u16()
    }

    /// Referenced event-payload schema id resolved at host build.
    #[must_use]
    pub fn payload_schema_ref(&self) -> &str {
        &self.payload_schema_ref
    }

    /// Identity-bearing manifest view for module digest sealing.
    pub(crate) fn manifest_view(&self) -> EventPayloadBindingView<'_> {
        EventPayloadBindingView {
            kind: self.kind_raw(),
            payload_schema_ref: &self.payload_schema_ref,
        }
    }
}

/// Serializable manifest view of one [`EventPayloadBinding`].
#[derive(Clone, Copy, Serialize)]
pub(crate) struct EventPayloadBindingView<'a> {
    kind: u16,
    payload_schema_ref: &'a str,
}
