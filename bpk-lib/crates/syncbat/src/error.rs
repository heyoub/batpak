//! Error types for the syncbat runtime shell.

use std::error::Error;
use std::fmt;

/// Error returned while assembling a [`crate::core::Core`].
///
/// `#[non_exhaustive]` so post-1.0 we can add validation variants
/// (e.g. cross-module descriptor checks, schema-version drift) without
/// breaking downstream exhaustive matches.
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
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
    /// An operation descriptor failed shape validation.
    InvalidOperation {
        /// Operation name.
        name: String,
        /// Validation message.
        message: String,
    },
    /// A handler name failed shape validation.
    InvalidHandler {
        /// Handler name.
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

    /// Build an invalid-operation error.
    #[must_use]
    pub fn invalid_operation(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidOperation {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Build an invalid-handler error.
    #[must_use]
    pub fn invalid_handler(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidHandler {
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
            Self::InvalidOperation { name, message } => {
                write!(f, "operation `{name}` is invalid: {message}")
            }
            Self::InvalidHandler { name, message } => {
                write!(f, "handler `{name}` is invalid: {message}")
            }
        }
    }
}

impl Error for BuildError {}

/// Handler failure preserved when receipt recording also fails.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ReceiptSinkHandlerCause {
    /// Handler-supplied error class.
    pub code: String,
    /// Handler-supplied error message.
    pub message: String,
}

impl ReceiptSinkHandlerCause {
    /// Build a handler cause for a receipt-sink failure.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Stable handler error class.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Handler-supplied error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Error returned by synchronous operation dispatch.
///
/// `#[non_exhaustive]` so the wire-error vocabulary can grow (e.g.
/// rate-limit, auth, schema-mismatch variants) without breaking
/// downstream matches that translate `RuntimeError` into transport-
/// layer error codes.
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
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
    /// Runtime policy denied the invocation. Admission guards and observed
    /// effect-row enforcement both record a `Denied` receipt before returning
    /// this variant.
    Denied {
        /// Operation name that was denied.
        name: String,
        /// Guard-supplied denial class.
        code: String,
        /// Guard-supplied denial message.
        message: String,
    },
    /// The configured receipt sink rejected a runtime-emitted receipt.
    ReceiptSink {
        /// Operation name whose receipt could not be recorded.
        name: String,
        /// Sink error message.
        message: String,
        /// Handler failure that preceded this sink failure, when present.
        caused_by_handler: Option<ReceiptSinkHandlerCause>,
    },
    /// The configured operation-status sink rejected a runtime-emitted fact.
    StatusSink {
        /// Operation name whose status fact could not be recorded.
        name: String,
        /// Sink error message.
        message: String,
        /// Handler failure that preceded this sink failure, when present.
        caused_by_handler: Option<ReceiptSinkHandlerCause>,
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

    /// Build an admission-denied error with an operation name, class, and message.
    #[must_use]
    pub fn denied(
        name: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Denied {
            name: name.into(),
            code: code.into(),
            message: message.into(),
        }
    }

    /// Build a receipt-sink error with an operation name and message.
    #[must_use]
    pub fn receipt_sink(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ReceiptSink {
            name: name.into(),
            message: message.into(),
            caused_by_handler: None,
        }
    }

    /// Build a receipt-sink error after a handler failure.
    #[must_use]
    pub fn receipt_sink_after_handler_failure(
        name: impl Into<String>,
        message: impl Into<String>,
        cause: ReceiptSinkHandlerCause,
    ) -> Self {
        Self::ReceiptSink {
            name: name.into(),
            message: message.into(),
            caused_by_handler: Some(cause),
        }
    }

    /// Build a status-sink error with an operation name and message.
    #[must_use]
    pub fn status_sink(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::StatusSink {
            name: name.into(),
            message: message.into(),
            caused_by_handler: None,
        }
    }

    /// Build a status-sink error after a handler failure.
    #[must_use]
    pub fn status_sink_after_handler_failure(
        name: impl Into<String>,
        message: impl Into<String>,
        cause: ReceiptSinkHandlerCause,
    ) -> Self {
        Self::StatusSink {
            name: name.into(),
            message: message.into(),
            caused_by_handler: Some(cause),
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
            Self::Denied {
                name,
                code,
                message,
            } => {
                write!(f, "operation `{name}` denied with {code}: {message}")
            }
            Self::ReceiptSink {
                name,
                message,
                caused_by_handler,
            } => {
                if let Some(cause) = caused_by_handler {
                    write!(
                        f,
                        "receipt sink for operation `{name}` failed after handler error {}: {}: {message}",
                        cause.code(),
                        cause.message()
                    )
                } else {
                    write!(f, "receipt sink for operation `{name}` failed: {message}")
                }
            }
            Self::StatusSink {
                name,
                message,
                caused_by_handler,
            } => {
                if let Some(cause) = caused_by_handler {
                    write!(
                        f,
                        "status sink for operation `{name}` failed after handler error {}: {}: {message}",
                        cause.code(),
                        cause.message()
                    )
                } else {
                    write!(f, "status sink for operation `{name}` failed: {message}")
                }
            }
        }
    }
}

impl Error for RuntimeError {}
