//! Error types for the syncbat runtime shell.

use std::error::Error;
use std::fmt;

/// Error returned while assembling a [`crate::core::Core`].
#[derive(Debug, Eq, PartialEq)]
pub enum BuildError {
    /// An operation descriptor was registered more than once.
    DuplicateOperation {
        /// Duplicate operation name.
        name: String,
    },
    /// A handler was registered more than once for the same operation name.
    DuplicateHandler {
        /// Duplicate handler name.
        name: String,
    },
    /// A handler was registered without a matching operation descriptor.
    MissingDescriptor {
        /// Handler name without a descriptor.
        name: String,
    },
    /// An operation descriptor was registered without a matching handler.
    MissingHandler {
        /// Operation name without a handler.
        name: String,
    },
    /// A module descriptor failed shape validation.
    InvalidModule {
        /// Module name.
        name: String,
        /// Validation message.
        message: String,
    },
}

impl BuildError {
    /// Build a duplicate-operation error.
    #[must_use]
    pub fn duplicate_operation(name: impl Into<String>) -> Self {
        Self::DuplicateOperation { name: name.into() }
    }

    /// Build a duplicate-handler error.
    #[must_use]
    pub fn duplicate_handler(name: impl Into<String>) -> Self {
        Self::DuplicateHandler { name: name.into() }
    }

    /// Build a missing-descriptor error.
    #[must_use]
    pub fn missing_descriptor(name: impl Into<String>) -> Self {
        Self::MissingDescriptor { name: name.into() }
    }

    /// Build a missing-handler error.
    #[must_use]
    pub fn missing_handler(name: impl Into<String>) -> Self {
        Self::MissingHandler { name: name.into() }
    }

    /// Build an invalid-module error.
    #[must_use]
    pub fn invalid_module(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidModule {
            name: name.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateOperation { name } => {
                write!(f, "operation `{name}` is already registered")
            }
            Self::DuplicateHandler { name } => {
                write!(f, "handler for operation `{name}` is already registered")
            }
            Self::MissingDescriptor { name } => {
                write!(f, "handler `{name}` has no matching operation descriptor")
            }
            Self::MissingHandler { name } => {
                write!(f, "operation `{name}` has no registered handler")
            }
            Self::InvalidModule { name, message } => {
                write!(f, "module `{name}` is invalid: {message}")
            }
        }
    }
}

impl Error for BuildError {}

/// Error returned by synchronous operation dispatch.
#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeError {
    /// The requested operation name is not known to the runtime.
    UnknownOperation {
        /// Requested operation name.
        name: String,
    },
    /// The operation descriptor exists, but no handler is available.
    MissingHandler {
        /// Requested operation name.
        name: String,
    },
    /// The handler rejected the invocation.
    Handler {
        /// Operation name being handled.
        name: String,
        /// Handler-supplied error class.
        code: String,
        /// Handler-supplied error message.
        message: String,
    },
}

impl RuntimeError {
    /// Build an unknown-operation error.
    #[must_use]
    pub fn unknown_operation(name: impl Into<String>) -> Self {
        Self::UnknownOperation { name: name.into() }
    }

    /// Build a missing-handler error.
    #[must_use]
    pub fn missing_handler(name: impl Into<String>) -> Self {
        Self::MissingHandler { name: name.into() }
    }

    /// Build a handler error with an operation name and message.
    #[must_use]
    pub fn handler(
        name: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Handler {
            name: name.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperation { name } => write!(f, "unknown operation `{name}`"),
            Self::MissingHandler { name } => {
                write!(f, "operation `{name}` has no registered handler")
            }
            Self::Handler {
                name,
                code,
                message,
            } => {
                write!(
                    f,
                    "handler for operation `{name}` failed with {code}: {message}"
                )
            }
        }
    }
}

impl Error for RuntimeError {}
