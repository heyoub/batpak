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

/// Maximum bytes accepted for a stable operation name.
pub const MAX_OPERATION_NAME_BYTES: usize = 128;
/// Maximum bytes accepted for schema and receipt string references.
pub const MAX_DESCRIPTOR_REF_BYTES: usize = 256;

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

    /// Validate descriptor fields before insertion into a live runtime catalog.
    ///
    /// # Errors
    /// Returns [`DescriptorValidationError`] when any stable identifier is
    /// empty, too long, or contains bytes outside syncbat's descriptor grammar.
    pub fn validate(&self) -> Result<(), DescriptorValidationError> {
        validate_stable_token("name", self.name, MAX_OPERATION_NAME_BYTES)?;
        validate_stable_ref(
            self.name,
            "input_schema_ref",
            self.input_schema_ref,
            MAX_DESCRIPTOR_REF_BYTES,
        )?;
        validate_stable_ref(
            self.name,
            "output_schema_ref",
            self.output_schema_ref,
            MAX_DESCRIPTOR_REF_BYTES,
        )?;
        validate_stable_ref(
            self.name,
            "receipt_kind",
            self.receipt_kind,
            MAX_DESCRIPTOR_REF_BYTES,
        )
    }
}

/// Descriptor shape validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DescriptorValidationError {
    /// Field that failed validation.
    pub field: &'static str,
    /// Invalid field value.
    pub value: String,
    /// Stable validation message.
    pub message: &'static str,
}

impl DescriptorValidationError {
    fn new(field: &'static str, value: impl Into<String>, message: &'static str) -> Self {
        Self {
            field,
            value: value.into(),
            message,
        }
    }
}

impl std::fmt::Display for DescriptorValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} `{}` is invalid: {}",
            self.field, self.value, self.message
        )
    }
}

impl std::error::Error for DescriptorValidationError {}

fn validate_stable_ref(
    operation_name: &str,
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), DescriptorValidationError> {
    validate_stable_token(field, value, max).map_err(|error| DescriptorValidationError {
        field: error.field,
        value: format!("{operation_name}:{}", error.value),
        message: error.message,
    })
}

fn validate_stable_token(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), DescriptorValidationError> {
    if value.is_empty() {
        return Err(DescriptorValidationError::new(field, value, "empty"));
    }
    if value.len() > max {
        return Err(DescriptorValidationError::new(field, value, "too long"));
    }
    if value
        .bytes()
        .any(|byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        return Err(DescriptorValidationError::new(
            field,
            value,
            "expected ASCII letters, digits, '.', '_' or '-'",
        ));
    }
    if value.starts_with('.') || value.ends_with('.') || value.contains("..") {
        return Err(DescriptorValidationError::new(
            field,
            value,
            "dot-separated tokens must be non-empty",
        ));
    }
    Ok(())
}
