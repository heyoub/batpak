//! Schema-validating wrapper around a [`syncbat::EffectBackend`].
//!
//! Host-mediated event appends fail closed when the kind is unbound or the
//! payload bytes do not satisfy the bound event-payload schema. Raw store
//! appends remain outside this boundary.

use std::collections::BTreeMap;

use batpak::event::EventKind;
use syncbat::effect_backend::{EffectBackend, EffectError};

use crate::schema::{SchemaRegistry, SchemaRole};

/// Wraps an inner effect backend with append-time event-payload schema validation.
pub struct ValidatingEffectBackend {
    inner: Box<dyn EffectBackend>,
    bindings: BTreeMap<u16, String>,
    registry: SchemaRegistry,
}

impl ValidatingEffectBackend {
    /// Construct a validating wrapper over `inner` using `bindings` and `registry`.
    #[must_use]
    pub fn new(
        inner: Box<dyn EffectBackend>,
        bindings: BTreeMap<u16, String>,
        registry: SchemaRegistry,
    ) -> Self {
        Self {
            inner,
            bindings,
            registry,
        }
    }
}

impl EffectBackend for ValidatingEffectBackend {
    fn append_event(&mut self, kind: EventKind, payload: &[u8]) -> Result<(), EffectError> {
        let kind_raw = kind.as_raw_u16();
        let schema_ref = self.bindings.get(&kind_raw).ok_or_else(|| {
            EffectError::new(format!(
                "event kind 0x{kind_raw:04x} has no payload schema binding"
            ))
        })?;
        self.registry
            .validate(schema_ref, SchemaRole::EventPayload, payload)
            .map_err(|error| {
                EffectError::new(format!(
                    "event kind 0x{kind_raw:04x} payload schema validation failed: {error}"
                ))
            })?;
        self.inner.append_event(kind, payload)
    }
}
