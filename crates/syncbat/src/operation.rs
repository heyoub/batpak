//! Generic operation metadata for syncbat handlers.

/// Runtime-facing side-effect classification for an operation receipt.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum EffectClass {
    /// Reads or inspects data without intending to mutate durable state.
    Inspect,
    /// Computes output from input without intending to touch durable state.
    Compute,
    /// Mutates local durable state.
    Persist,
    /// Produces an externally visible side effect.
    Emit,
    /// Changes runtime control flow or runtime-owned bookkeeping.
    Control,
}

/// Byte input passed into a syncbat handler.
pub type OperationInput = Vec<u8>;

/// Byte output returned by a syncbat handler.
pub type OperationOutput = Vec<u8>;

/// Stable metadata that describes a byte-oriented operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OperationDescriptor {
    /// Stable operation name used for routing and receipts.
    pub name: &'static str,
    /// Optional human-readable title.
    pub title: Option<&'static str>,
    /// Runtime-facing effect class for receipt classification.
    pub effect: EffectClass,
    /// Stable string reference for the operation input schema.
    pub input_schema_ref: &'static str,
    /// Stable string reference for the operation output schema.
    pub output_schema_ref: &'static str,
    /// Stable receipt kind emitted for this operation.
    pub receipt_kind: &'static str,
}

impl OperationDescriptor {
    /// Construct an operation descriptor from stable string references.
    #[must_use]
    pub const fn new(
        name: &'static str,
        effect: EffectClass,
        input_schema_ref: &'static str,
        output_schema_ref: &'static str,
        receipt_kind: &'static str,
    ) -> Self {
        Self {
            name,
            title: None,
            effect,
            input_schema_ref,
            output_schema_ref,
            receipt_kind,
        }
    }

    /// Return a copy of this descriptor with a human-readable title attached.
    #[must_use]
    pub const fn with_title(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }

    /// Stable operation name used for routing and receipts.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}
