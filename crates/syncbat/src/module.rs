//! Data-oriented module descriptors.
//!
//! A [`Module`] lists operation descriptors as data. It performs only shape
//! validation: operation names must be unique. Concrete handlers are registered
//! on the runtime builder so module descriptors stay declarative.

use crate::operation::OperationDescriptor;
use crate::register::{validate_module_name, Register, RegisterValidationError};
use std::collections::BTreeMap;

/// Data-oriented module descriptor.
pub struct Module {
    name: String,
    operations: BTreeMap<String, OperationDescriptor>,
}

impl Module {
    /// Create an empty module descriptor.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            operations: BTreeMap::new(),
        }
    }

    /// Build a module descriptor from operations.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterValidationError::DuplicateOperationName`] when an
    /// operation name is repeated.
    pub fn from_operations<I>(
        name: impl Into<String>,
        operations: I,
    ) -> Result<Self, RegisterValidationError>
    where
        I: IntoIterator<Item = OperationDescriptor>,
    {
        let mut module = Self::new(name);
        for descriptor in operations {
            module.insert_operation(descriptor)?;
        }
        module.validate()?;
        Ok(module)
    }

    /// Stable module name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Insert one operation descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterValidationError::DuplicateOperationName`] when the
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

    /// Validate this module descriptor.
    ///
    /// # Errors
    ///
    pub fn validate(&self) -> Result<(), RegisterValidationError> {
        validate_module_name(&self.name)?;
        for descriptor in self.operations.values() {
            descriptor
                .validate()
                .map_err(|source| RegisterValidationError::InvalidDescriptor {
                    name: descriptor.name().to_owned(),
                    source,
                })?;
        }
        Ok(())
    }

    /// Return an operation descriptor by name.
    #[must_use]
    pub fn operation(&self, name: &str) -> Option<&OperationDescriptor> {
        self.operations.get(name)
    }

    /// Iterate operation descriptors in deterministic key order.
    pub fn operations(&self) -> impl Iterator<Item = (&str, &OperationDescriptor)> + '_ {
        self.operations
            .iter()
            .map(|(name, descriptor)| (name.as_str(), descriptor))
    }

    /// Number of listed operations.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }

    /// Consume this module into a durable-facing operation register.
    ///
    /// # Errors
    ///
    /// Returns a validation error if the module is malformed.
    pub fn into_register(self) -> Result<Register, RegisterValidationError> {
        self.validate()?;
        Register::from_operations(self.operations.into_values())
    }
}
