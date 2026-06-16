//! Generic operation metadata for syncbat handlers.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::handler::HandlerFn;
use crate::operation_name::{OperationName, OperationNameError};

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

impl EffectClass {
    /// Stable lowercase catalog spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Compute => "compute",
            Self::Persist => "persist",
            Self::Emit => "emit",
            Self::Control => "control",
        }
    }

    /// Parse a stable lowercase catalog spelling.
    #[must_use]
    pub fn from_catalog_str(value: &str) -> Option<Self> {
        match value {
            "inspect" => Some(Self::Inspect),
            "compute" => Some(Self::Compute),
            "persist" => Some(Self::Persist),
            "emit" => Some(Self::Emit),
            "control" => Some(Self::Control),
            _ => None,
        }
    }
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
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OperationDescriptor {
    /// Stable operation name used for routing and receipts.
    name: DescriptorText,
    /// Optional human-readable title.
    title: Option<DescriptorText>,
    /// Runtime-facing effect class for receipt classification.
    pub effect: EffectClass,
    /// Stable string reference for the operation input schema.
    input_schema_ref: DescriptorText,
    /// Stable string reference for the operation output schema.
    output_schema_ref: DescriptorText,
    /// Stable receipt kind emitted for this operation.
    receipt_kind: DescriptorText,
}

#[derive(Clone, Debug, Eq)]
enum DescriptorText {
    Static(&'static str),
    Owned(Arc<str>),
}

impl DescriptorText {
    const fn static_str(value: &'static str) -> Self {
        Self::Static(value)
    }

    fn owned(value: impl Into<String>) -> Self {
        Self::Owned(Arc::from(value.into()))
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Static(value) => value,
            Self::Owned(value) => value.as_ref(),
        }
    }
}

impl PartialEq for DescriptorText {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Hash for DescriptorText {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
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
            name: DescriptorText::static_str(name),
            title: None,
            effect,
            input_schema_ref: DescriptorText::static_str(input_schema_ref),
            output_schema_ref: DescriptorText::static_str(output_schema_ref),
            receipt_kind: DescriptorText::static_str(receipt_kind),
        }
    }

    /// Construct an operation descriptor from stable string references with a
    /// human-readable title.
    #[must_use]
    pub const fn new_with_title(
        name: &'static str,
        effect: EffectClass,
        input_schema_ref: &'static str,
        output_schema_ref: &'static str,
        receipt_kind: &'static str,
        title: &'static str,
    ) -> Self {
        Self {
            name: DescriptorText::static_str(name),
            title: Some(DescriptorText::static_str(title)),
            effect,
            input_schema_ref: DescriptorText::static_str(input_schema_ref),
            output_schema_ref: DescriptorText::static_str(output_schema_ref),
            receipt_kind: DescriptorText::static_str(receipt_kind),
        }
    }

    /// Construct an operation descriptor from owned strings rebuilt from
    /// durable catalog rows.
    #[must_use]
    pub fn owned(
        name: impl Into<String>,
        effect: EffectClass,
        input_schema_ref: impl Into<String>,
        output_schema_ref: impl Into<String>,
        receipt_kind: impl Into<String>,
    ) -> Self {
        Self {
            name: DescriptorText::owned(name),
            title: None,
            effect,
            input_schema_ref: DescriptorText::owned(input_schema_ref),
            output_schema_ref: DescriptorText::owned(output_schema_ref),
            receipt_kind: DescriptorText::owned(receipt_kind),
        }
    }

    /// Return a copy of this descriptor with a human-readable title attached.
    #[must_use]
    pub fn with_title(mut self, title: &'static str) -> Self {
        self.title = Some(DescriptorText::static_str(title));
        self
    }

    /// Return a copy of this descriptor with an owned human-readable title
    /// attached.
    #[must_use]
    pub fn with_owned_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(DescriptorText::owned(title));
        self
    }

    /// Stable operation name used for routing and receipts.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Optional human-readable title.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_ref().map(DescriptorText::as_str)
    }

    /// Stable string reference for the operation input schema.
    #[must_use]
    pub fn input_schema_ref(&self) -> &str {
        self.input_schema_ref.as_str()
    }

    /// Stable string reference for the operation output schema.
    #[must_use]
    pub fn output_schema_ref(&self) -> &str {
        self.output_schema_ref.as_str()
    }

    /// Stable receipt kind emitted for this operation.
    #[must_use]
    pub fn receipt_kind(&self) -> &str {
        self.receipt_kind.as_str()
    }

    /// Validate descriptor fields before insertion into a live runtime catalog.
    ///
    /// # Errors
    /// Returns [`DescriptorValidationError`] when any stable identifier is
    /// empty, too long, or contains bytes outside syncbat's descriptor grammar.
    pub fn validate(&self) -> Result<(), DescriptorValidationError> {
        // Run the operation-name grammar through the single
        // [`OperationName`] constructor so every layer agrees on the rules.
        OperationName::new(self.name()).map_err(|error| {
            DescriptorValidationError::from_operation_name_error("name", self.name(), &error)
        })?;
        validate_stable_ref(
            self.name(),
            "input_schema_ref",
            self.input_schema_ref(),
            MAX_DESCRIPTOR_REF_BYTES,
        )?;
        validate_stable_ref(
            self.name(),
            "output_schema_ref",
            self.output_schema_ref(),
            MAX_DESCRIPTOR_REF_BYTES,
        )?;
        validate_stable_ref(
            self.name(),
            "receipt_kind",
            self.receipt_kind(),
            MAX_DESCRIPTOR_REF_BYTES,
        )
    }
}

/// Macro-generated operation registration item.
///
/// This is data plus a function pointer. It does not own runtime dispatch,
/// store setup, or receipt persistence; callers still choose when to register
/// it with a [`crate::CoreBuilder`].
#[derive(Clone)]
pub struct OperationRegisterItem {
    descriptor: OperationDescriptor,
    handler: HandlerFn,
}

impl OperationRegisterItem {
    /// Build an operation registration item.
    #[must_use]
    pub fn new(descriptor: OperationDescriptor, handler: HandlerFn) -> Self {
        Self {
            descriptor,
            handler,
        }
    }

    /// Descriptor emitted for this operation.
    #[must_use]
    pub fn descriptor(&self) -> &OperationDescriptor {
        &self.descriptor
    }

    /// Function-pointer handler emitted for this operation.
    #[must_use]
    pub fn handler(&self) -> HandlerFn {
        self.handler
    }

    /// Consume the item and return descriptor plus handler.
    #[must_use]
    pub fn into_parts(self) -> (OperationDescriptor, HandlerFn) {
        (self.descriptor, self.handler)
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

    /// Map an [`OperationNameError`] from the substrate-wide newtype into the
    /// descriptor-layer error shape so existing callers keep observing the
    /// same `field` + stable `message` columns.
    fn from_operation_name_error(
        field: &'static str,
        value: &str,
        error: &OperationNameError,
    ) -> Self {
        let message = match error {
            OperationNameError::Empty => "empty",
            OperationNameError::TooLong { .. } => "too long",
            OperationNameError::LeadingOrTrailingDot | OperationNameError::ConsecutiveDots => {
                "dot-separated tokens must be non-empty"
            }
            OperationNameError::IllegalCharacter { .. } => {
                "expected ASCII letters, digits, '.', '_' or '-'"
            }
        };
        Self::new(field, value, message)
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
    validate_stable_ref_token(field, value, max).map_err(|error| DescriptorValidationError {
        field: error.field,
        value: format!("{operation_name}:{}", error.value),
        message: error.message,
    })
}

/// Schema/receipt-ref grammar check.
///
/// Shares the operation-name grammar by intent but applies to a different
/// field with a larger byte bound. The operation-name path goes through
/// [`OperationName`] in `operation_name.rs` instead.
fn validate_stable_ref_token(
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

#[cfg(test)]
mod effect_class_tests {
    use super::EffectClass;

    #[test]
    fn every_variant_round_trips_through_catalog_str() {
        // Pins each `from_catalog_str` arm against its `as_str` spelling so a
        // deleted arm (e.g. "persist") is caught as a broken round-trip rather
        // than silently parsing to None.
        for class in [
            EffectClass::Inspect,
            EffectClass::Compute,
            EffectClass::Persist,
            EffectClass::Emit,
            EffectClass::Control,
        ] {
            assert_eq!(
                EffectClass::from_catalog_str(class.as_str()),
                Some(class),
                "round-trip failed for {}",
                class.as_str()
            );
        }
        assert_eq!(
            EffectClass::from_catalog_str("persist"),
            Some(EffectClass::Persist)
        );
        assert_eq!(EffectClass::from_catalog_str("nonsense"), None);
    }
}
