#![warn(missing_docs)]
//! Claw kit facade for batpak-family sync operations.
//!
//! cb declares; sb runs; bp banks.
//!
//! Use this crate as `use downstream-kit as cb;` when declaring operation-kit
//! vocabulary. Runtime composition and invocation remain owned by
//! [`syncbat`].

use std::error::Error;
use std::fmt;

use batpak::guard::{Denial, Gate, GateSet};
use batpak::pipeline::Pipeline;

pub use syncbat::operation;
pub use syncbat::{
    EffectClass, OperationDescriptor, OperationRegisterItem, ReceiptEnvelope, ReceiptOutcome,
};

/// Stable batpak gate name for a required pass check.
pub const REQUIRED_PASS_GATE_NAME: &str = "downstream-kit.required_pass";
/// Stable batpak gate name for a required capability check.
pub const REQUIRED_CAPABILITY_GATE_NAME: &str = "downstream-kit.required_capability";
/// Machine-readable denial code for a missing pass.
pub const MISSING_PASS_CODE: &str = "DownstreamKit_MISSING_PASS";
/// Machine-readable denial code for a missing capability.
pub const MISSING_CAPABILITY_CODE: &str = "DownstreamKit_MISSING_CAPABILITY";

/// Lightweight validated reference to a pass declared by an operation kit.
pub type PassRef = Ref<Pass>;

/// Lightweight validated reference to a capability declared by an operation kit.
pub type CapabilityRef = Ref<Capability>;

/// Caller-provided requirement context used by downstream-kit requirement gates.
///
/// This trait is intentionally read-only. It lets downstream-kit declarations compile
/// into batpak gates while the caller keeps ownership of runtime admission,
/// dispatch, and evidence gathering.
pub trait GateContext {
    /// Return `true` when the requested pass is satisfied for this invocation.
    fn has_pass(&self, pass: PassRef) -> bool;

    /// Return `true` when the requested capability is satisfied for this invocation.
    fn has_capability(&self, capability: CapabilityRef) -> bool;
}

impl<T: GateContext + ?Sized> GateContext for &T {
    fn has_pass(&self, pass: PassRef) -> bool {
        (**self).has_pass(pass)
    }

    fn has_capability(&self, capability: CapabilityRef) -> bool {
        (**self).has_capability(capability)
    }
}

/// Concrete context for callers that already know the satisfied passes and
/// capabilities for an invocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RequirementEvidence {
    passes: Vec<PassRef>,
    capabilities: Vec<CapabilityRef>,
}

impl RequirementEvidence {
    /// Construct empty satisfied-requirement evidence.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            passes: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    /// Construct satisfied-requirement evidence from pass and capability iterators.
    #[must_use]
    pub fn from_refs(
        passes: impl IntoIterator<Item = PassRef>,
        capabilities: impl IntoIterator<Item = CapabilityRef>,
    ) -> Self {
        Self {
            passes: passes.into_iter().collect(),
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// Return a copy with one pass added.
    #[must_use]
    pub fn with_pass(mut self, pass: PassRef) -> Self {
        self.passes.push(pass);
        self
    }

    /// Return a copy with one capability added.
    #[must_use]
    pub fn with_capability(mut self, capability: CapabilityRef) -> Self {
        self.capabilities.push(capability);
        self
    }

    /// Borrow the satisfied passes in insertion order.
    #[must_use]
    pub fn passes(&self) -> &[PassRef] {
        &self.passes
    }

    /// Borrow the satisfied capabilities in insertion order.
    #[must_use]
    pub fn capabilities(&self) -> &[CapabilityRef] {
        &self.capabilities
    }
}

impl GateContext for RequirementEvidence {
    fn has_pass(&self, pass: PassRef) -> bool {
        self.passes.contains(&pass)
    }

    fn has_capability(&self, capability: CapabilityRef) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// Declared pass metadata for an operation kit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PassDescriptor {
    id: PassRef,
    title: Option<&'static str>,
}

impl PassDescriptor {
    /// Construct pass metadata from a validated pass reference.
    #[must_use]
    pub const fn new(id: PassRef) -> Self {
        Self { id, title: None }
    }

    /// Return a copy with a human-readable title.
    #[must_use]
    pub const fn with_title(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }

    /// Stable pass reference.
    #[must_use]
    pub const fn id(&self) -> PassRef {
        self.id
    }

    /// Optional human-readable title.
    #[must_use]
    pub const fn title(&self) -> Option<&'static str> {
        self.title
    }

    /// Compile this declaration into a batpak gate for one operation.
    #[must_use]
    pub fn required_gate(&self, operation_name: impl Into<String>) -> RequiredPassGate {
        RequiredPassGate::new(operation_name, self.id)
    }
}

/// Declared capability metadata for an operation kit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityDescriptor {
    id: CapabilityRef,
    title: Option<&'static str>,
}

impl CapabilityDescriptor {
    /// Construct capability metadata from a validated capability reference.
    #[must_use]
    pub const fn new(id: CapabilityRef) -> Self {
        Self { id, title: None }
    }

    /// Return a copy with a human-readable title.
    #[must_use]
    pub const fn with_title(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }

    /// Stable capability reference.
    #[must_use]
    pub const fn id(&self) -> CapabilityRef {
        self.id
    }

    /// Optional human-readable title.
    #[must_use]
    pub const fn title(&self) -> Option<&'static str> {
        self.title
    }

    /// Compile this declaration into a batpak gate for one operation.
    #[must_use]
    pub fn required_gate(&self, operation_name: impl Into<String>) -> RequiredCapabilityGate {
        RequiredCapabilityGate::new(operation_name, self.id)
    }
}

/// Batpak gate that denies when an invocation lacks a required pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredPassGate {
    operation_name: String,
    pass: PassRef,
}

impl RequiredPassGate {
    /// Construct a required-pass gate for one operation.
    #[must_use]
    pub fn new(operation_name: impl Into<String>, pass: PassRef) -> Self {
        Self {
            operation_name: operation_name.into(),
            pass,
        }
    }

    /// Stable operation name attached to denials from this gate.
    #[must_use]
    pub fn operation_name(&self) -> &str {
        &self.operation_name
    }

    /// Pass required by this gate.
    #[must_use]
    pub const fn pass(&self) -> PassRef {
        self.pass
    }
}

impl<Ctx: GateContext> Gate<Ctx> for RequiredPassGate {
    fn name(&self) -> &'static str {
        REQUIRED_PASS_GATE_NAME
    }

    fn evaluate(&self, ctx: &Ctx) -> Result<(), Denial> {
        if ctx.has_pass(self.pass) {
            return Ok(());
        }

        Err(Denial::new(
            REQUIRED_PASS_GATE_NAME,
            format!(
                "operation {} requires pass {}",
                self.operation_name, self.pass
            ),
        )
        .with_code(MISSING_PASS_CODE)
        .with_context("operation", self.operation_name.clone())
        .with_context("pass", self.pass.as_str()))
    }

    fn description(&self) -> &'static str {
        "requires a declared downstream-kit pass"
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequiredPassSetGate {
    operation_name: String,
    passes: Vec<PassRef>,
}

impl RequiredPassSetGate {
    fn new(operation_name: impl Into<String>, passes: impl IntoIterator<Item = PassRef>) -> Self {
        Self {
            operation_name: operation_name.into(),
            passes: passes.into_iter().collect(),
        }
    }
}

impl<Ctx: GateContext> Gate<Ctx> for RequiredPassSetGate {
    fn name(&self) -> &'static str {
        REQUIRED_PASS_GATE_NAME
    }

    fn evaluate(&self, ctx: &Ctx) -> Result<(), Denial> {
        for pass in &self.passes {
            if !ctx.has_pass(*pass) {
                return Err(Denial::new(
                    REQUIRED_PASS_GATE_NAME,
                    format!("operation {} requires pass {}", self.operation_name, pass),
                )
                .with_code(MISSING_PASS_CODE)
                .with_context("operation", self.operation_name.clone())
                .with_context("pass", pass.as_str()));
            }
        }
        Ok(())
    }

    fn description(&self) -> &'static str {
        "requires declared downstream-kit passes"
    }
}

/// Batpak gate that denies when an invocation lacks a required capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredCapabilityGate {
    operation_name: String,
    capability: CapabilityRef,
}

impl RequiredCapabilityGate {
    /// Construct a required-capability gate for one operation.
    #[must_use]
    pub fn new(operation_name: impl Into<String>, capability: CapabilityRef) -> Self {
        Self {
            operation_name: operation_name.into(),
            capability,
        }
    }

    /// Stable operation name attached to denials from this gate.
    #[must_use]
    pub fn operation_name(&self) -> &str {
        &self.operation_name
    }

    /// Capability required by this gate.
    #[must_use]
    pub const fn capability(&self) -> CapabilityRef {
        self.capability
    }
}

impl<Ctx: GateContext> Gate<Ctx> for RequiredCapabilityGate {
    fn name(&self) -> &'static str {
        REQUIRED_CAPABILITY_GATE_NAME
    }

    fn evaluate(&self, ctx: &Ctx) -> Result<(), Denial> {
        if ctx.has_capability(self.capability) {
            return Ok(());
        }

        Err(Denial::new(
            REQUIRED_CAPABILITY_GATE_NAME,
            format!(
                "operation {} requires capability {}",
                self.operation_name, self.capability
            ),
        )
        .with_code(MISSING_CAPABILITY_CODE)
        .with_context("operation", self.operation_name.clone())
        .with_context("capability", self.capability.as_str()))
    }

    fn description(&self) -> &'static str {
        "requires a declared downstream-kit capability"
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequiredCapabilitySetGate {
    operation_name: String,
    capabilities: Vec<CapabilityRef>,
}

impl RequiredCapabilitySetGate {
    fn new(
        operation_name: impl Into<String>,
        capabilities: impl IntoIterator<Item = CapabilityRef>,
    ) -> Self {
        Self {
            operation_name: operation_name.into(),
            capabilities: capabilities.into_iter().collect(),
        }
    }
}

impl<Ctx: GateContext> Gate<Ctx> for RequiredCapabilitySetGate {
    fn name(&self) -> &'static str {
        REQUIRED_CAPABILITY_GATE_NAME
    }

    fn evaluate(&self, ctx: &Ctx) -> Result<(), Denial> {
        for capability in &self.capabilities {
            if !ctx.has_capability(*capability) {
                return Err(Denial::new(
                    REQUIRED_CAPABILITY_GATE_NAME,
                    format!(
                        "operation {} requires capability {}",
                        self.operation_name, capability
                    ),
                )
                .with_code(MISSING_CAPABILITY_CODE)
                .with_context("operation", self.operation_name.clone())
                .with_context("capability", capability.as_str()));
            }
        }
        Ok(())
    }

    fn description(&self) -> &'static str {
        "requires declared downstream-kit capabilities"
    }
}

/// Claw kit operation declaration metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationKitItem<'a> {
    descriptor: OperationDescriptor,
    passes: &'a [PassRef],
    capabilities: &'a [CapabilityRef],
}

impl<'a> OperationKitItem<'a> {
    /// Construct operation-kit metadata.
    #[must_use]
    pub fn new(
        descriptor: OperationDescriptor,
        passes: &'a [PassRef],
        capabilities: &'a [CapabilityRef],
    ) -> Self {
        Self {
            descriptor,
            passes,
            capabilities,
        }
    }

    /// Runtime descriptor compiled from this declaration.
    #[must_use]
    pub fn descriptor(&self) -> &OperationDescriptor {
        &self.descriptor
    }

    /// Pass references declared by this operation.
    #[must_use]
    pub const fn passes(&self) -> &'a [PassRef] {
        self.passes
    }

    /// Capability references declared by this operation.
    #[must_use]
    pub const fn capabilities(&self) -> &'a [CapabilityRef] {
        self.capabilities
    }

    /// Compile required passes and capabilities into a batpak gate set.
    #[must_use]
    pub fn compile_gate_set<Ctx>(&self) -> GateSet<Ctx>
    where
        Ctx: GateContext + 'static,
    {
        let mut gates = GateSet::new();
        if !self.passes.is_empty() {
            gates.push(RequiredPassSetGate::new(
                self.descriptor.name().to_owned(),
                self.passes.iter().copied(),
            ));
        }
        if !self.capabilities.is_empty() {
            gates.push(RequiredCapabilitySetGate::new(
                self.descriptor.name().to_owned(),
                self.capabilities.iter().copied(),
            ));
        }
        gates
    }

    /// Compile required passes and capabilities into a batpak pipeline.
    #[must_use]
    pub fn compile_pipeline<Ctx>(&self) -> Pipeline<Ctx>
    where
        Ctx: GateContext + 'static,
    {
        Pipeline::new(self.compile_gate_set())
    }

    /// Build a syncbat register item from this operation and a handler.
    #[must_use]
    pub fn register_item(&self, handler: syncbat::handler::HandlerFn) -> OperationRegisterItem {
        OperationRegisterItem::new(self.descriptor.clone(), handler)
    }
}

/// Validation error for operation-kit references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefError {
    /// The reference string was empty.
    Empty,
    /// The reference exceeded the maximum supported length.
    TooLong {
        /// Maximum accepted byte length.
        max: usize,
        /// Actual byte length.
        actual: usize,
    },
    /// The reference contained a byte outside the allowed vocabulary.
    InvalidByte {
        /// Byte offset of the invalid byte.
        index: usize,
        /// Invalid byte.
        byte: u8,
    },
    /// The reference started or ended with punctuation instead of an
    /// alphanumeric token byte.
    InvalidBoundary {
        /// Byte offset of the invalid boundary byte.
        index: usize,
        /// Invalid boundary byte.
        byte: u8,
    },
    /// The reference contained two adjacent separator bytes.
    RepeatedSeparator {
        /// Byte offset of the repeated separator byte.
        index: usize,
        /// Repeated separator byte.
        byte: u8,
    },
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("reference must not be empty"),
            Self::TooLong { max, actual } => {
                write!(f, "reference length {actual} exceeds maximum {max}")
            }
            Self::InvalidByte { index, byte } => {
                write!(
                    f,
                    "reference contains invalid byte 0x{byte:02x} at offset {index}"
                )
            }
            Self::InvalidBoundary { index, byte } => {
                write!(
                    f,
                    "reference contains boundary separator byte 0x{byte:02x} at offset {index}"
                )
            }
            Self::RepeatedSeparator { index, byte } => {
                write!(
                    f,
                    "reference contains repeated separator byte 0x{byte:02x} at offset {index}"
                )
            }
        }
    }
}

impl Error for RefError {}

/// Validated operation-kit reference.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Ref<K> {
    value: &'static str,
    _kind: std::marker::PhantomData<K>,
}

impl<K> Ref<K> {
    /// Maximum accepted reference length in bytes.
    pub const MAX_LEN: usize = 128;

    /// Construct a validated reference.
    ///
    /// # Errors
    /// Returns [`RefError`] when the value is empty, too long, or contains a
    /// byte outside `[A-Za-z0-9._:-]`, starts or ends with punctuation, or
    /// contains adjacent separator bytes.
    pub const fn new(value: &'static str) -> Result<Self, RefError> {
        let bytes = value.as_bytes();
        if bytes.is_empty() {
            return Err(RefError::Empty);
        }
        if bytes.len() > Self::MAX_LEN {
            return Err(RefError::TooLong {
                max: Self::MAX_LEN,
                actual: bytes.len(),
            });
        }

        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            let valid = matches!(
                byte,
                b'a'..=b'z'
                    | b'A'..=b'Z'
                    | b'0'..=b'9'
                    | b'.'
                    | b'_'
                    | b':'
                    | b'-'
            );
            if !valid {
                return Err(RefError::InvalidByte { index, byte });
            }
            if !is_ref_alnum(byte) {
                if index == 0 || index + 1 == bytes.len() {
                    return Err(RefError::InvalidBoundary { index, byte });
                }
                if !is_ref_alnum(bytes[index - 1]) {
                    return Err(RefError::RepeatedSeparator { index, byte });
                }
            }
            index += 1;
        }

        Ok(Self {
            value,
            _kind: std::marker::PhantomData,
        })
    }

    /// Return the reference as a string slice.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.value
    }
}

const fn is_ref_alnum(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9')
}

impl<K> AsRef<str> for Ref<K> {
    fn as_ref(&self) -> &str {
        self.value
    }
}

impl<K> fmt::Display for Ref<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.value)
    }
}

/// Marker for pass references.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Pass {}

/// Marker for capability references.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Capability {}

/// Common imports for declaring claw kit operations.
pub mod prelude {
    pub use crate::{
        operation, CapabilityDescriptor, CapabilityRef, EffectClass, GateContext,
        OperationDescriptor, OperationKitItem, OperationRegisterItem, PassDescriptor, PassRef,
        ReceiptEnvelope, ReceiptOutcome, Ref, RefError, RequiredCapabilityGate, RequiredPassGate,
        RequirementEvidence, MISSING_CAPABILITY_CODE, MISSING_PASS_CODE,
        REQUIRED_CAPABILITY_GATE_NAME, REQUIRED_PASS_GATE_NAME,
    };
}
