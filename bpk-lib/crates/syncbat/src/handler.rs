//! Synchronous byte-oriented handler contracts.

use std::{error::Error, fmt};

use crate::core::Ctx;

/// Result type returned by syncbat handlers.
pub type HandlerResult = Result<Vec<u8>, HandlerError>;

/// Function-pointer handler emitted by operation declaration macros.
pub type HandlerFn = for<'a> fn(&[u8], &mut Ctx<'a>) -> HandlerResult;

/// Error returned when a handler cannot produce output bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HandlerError {
    /// Input bytes could not be decoded or validated by the handler.
    InvalidInput(String),
    /// The handler failed while executing the operation.
    Failed(String),
}

impl HandlerError {
    /// Construct an invalid-input handler error.
    #[must_use]
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    /// Construct an execution-failure handler error.
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    /// Return a stable string code for the error class.
    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::Failed(_) => "failed",
        }
    }

    /// Return the handler-provided error message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidInput(message) | Self::Failed(message) => message,
        }
    }
}

impl fmt::Display for HandlerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class(), self.message())
    }
}

impl Error for HandlerError {}

/// Synchronous handler contract for byte-oriented operations.
pub trait Handler {
    /// Handle input bytes with mutable runtime context and return output bytes.
    ///
    /// # Errors
    /// Returns [`HandlerError`] when input decoding, validation, or operation
    /// execution fails.
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult;
}

impl<F> Handler for F
where
    F: for<'a> FnMut(&[u8], &mut Ctx<'a>) -> HandlerResult,
{
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        self(input, cx)
    }
}
