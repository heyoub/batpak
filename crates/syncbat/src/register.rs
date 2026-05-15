//! Durable-facing operation catalog and cache projection.
//!
//! [`Register`] owns the catalog data that can be persisted or reconstructed
//! from persisted records. [`CacheRegister`] is a borrowed, hot lookup view over
//! a register; it is an optimization surface, not the source of truth.

use crate::operation::OperationDescriptor;
use std::collections::BTreeMap;
use std::fmt;

/// Catalog validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegisterValidationError {
    /// More than one descriptor used the same operation name.
    DuplicateOperationName {
        /// Duplicate operation name.
        name: String,
    },
}

impl fmt::Display for RegisterValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateOperationName { name } => {
                write!(f, "duplicate operation name `{name}`")
            }
        }
    }
}

impl std::error::Error for RegisterValidationError {}

/// Durable-facing catalog of operation descriptors keyed by operation name.
#[derive(Default)]
pub struct Register {
    operations: BTreeMap<String, OperationDescriptor>,
}

impl Register {
    /// Create an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a catalog from `(operation_name, descriptor)` pairs.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterValidationError::DuplicateOperationName`] when the
    /// same operation name appears more than once.
    pub fn from_operations<I>(operations: I) -> Result<Self, RegisterValidationError>
    where
        I: IntoIterator<Item = OperationDescriptor>,
    {
        let mut register = Self::new();
        for descriptor in operations {
            register.insert_operation(descriptor)?;
        }
        Ok(register)
    }

    /// Insert one operation descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterValidationError::DuplicateOperationName`] if the
    /// operation name is already present.
    pub fn insert_operation(
        &mut self,
        descriptor: OperationDescriptor,
    ) -> Result<(), RegisterValidationError> {
        let name = descriptor.name().to_owned();
        if self.operations.contains_key(&name) {
            return Err(RegisterValidationError::DuplicateOperationName { name });
        }
        self.operations.insert(name, descriptor);
        Ok(())
    }

    /// Return an operation descriptor by name.
    #[must_use]
    pub fn operation(&self, name: &str) -> Option<&OperationDescriptor> {
        self.operations.get(name)
    }

    /// Return true when an operation name exists in the catalog.
    #[must_use]
    pub fn contains_operation(&self, name: &str) -> bool {
        self.operations.contains_key(name)
    }

    /// Number of cataloged operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Return true when the catalog contains no operations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Iterate operation names in deterministic key order.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.operations.keys().map(String::as_str)
    }

    /// Iterate operation descriptors in deterministic key order.
    pub fn operations(&self) -> impl Iterator<Item = (&str, &OperationDescriptor)> + '_ {
        self.operations
            .iter()
            .map(|(name, descriptor)| (name.as_str(), descriptor))
    }

    /// Borrow the underlying deterministic operation map.
    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, OperationDescriptor> {
        &self.operations
    }

    /// Consume the catalog and return its deterministic operation map.
    #[must_use]
    pub fn into_map(self) -> BTreeMap<String, OperationDescriptor> {
        self.operations
    }
}

/// Hot lookup projection over a [`Register`].
///
/// This type borrows descriptor data from a register and can be rebuilt at any
/// time from that durable-facing catalog. It should not be serialized or
/// treated as the source of record for what operations exist.
#[derive(Default)]
pub struct CacheRegister<'a> {
    operations: BTreeMap<&'a str, &'a OperationDescriptor>,
}

impl<'a> CacheRegister<'a> {
    /// Build a hot lookup projection from a register.
    #[must_use]
    pub fn from_register(register: &'a Register) -> Self {
        let operations = register.operations().collect();
        Self { operations }
    }

    /// Return an operation descriptor by name.
    #[must_use]
    pub fn operation(&self, name: &str) -> Option<&'a OperationDescriptor> {
        self.operations.get(name).copied()
    }

    /// Return true when the projection contains an operation name.
    #[must_use]
    pub fn contains_operation(&self, name: &str) -> bool {
        self.operations.contains_key(name)
    }

    /// Number of projected operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Return true when the projection contains no operations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Iterate projected operation names in deterministic key order.
    pub fn names(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.operations.keys().copied()
    }

    /// Iterate projected operation descriptors in deterministic key order.
    pub fn operations(&self) -> impl Iterator<Item = (&'a str, &'a OperationDescriptor)> + '_ {
        self.operations
            .iter()
            .map(|(name, descriptor)| (*name, *descriptor))
    }
}
