use serde::{Deserialize, Serialize};
use std::fmt;

/// `OutcomeError`: structured error with kind, message, optional compensation.
///
/// **Retryability is derived, not stored.** Use [`OutcomeError::is_retryable`]
/// (which defers to [`ErrorKind::is_retryable`]) rather than reading a field.
/// Subclassing an additional retryable class happens through
/// [`ErrorKind::Custom`] with downstream-defined semantics — but the classifier
/// is always the kind, never a duplicated boolean on the error. See G9.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutcomeError {
    /// Classification of the error.
    pub kind: ErrorKind,
    /// Human-readable description of what went wrong.
    pub message: String,
    /// Optional compensation action to run when this error occurs.
    pub compensation: Option<super::wait::CompensationAction>,
}

impl OutcomeError {
    /// Build an `OutcomeError` with no compensation attached.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            compensation: None,
        }
    }

    /// Attach a compensation action to this error.
    pub fn with_compensation(mut self, action: super::wait::CompensationAction) -> Self {
        self.compensation = Some(action);
        self
    }

    /// Returns `true` when this error's kind is retryable.
    ///
    /// Derived from [`ErrorKind::is_retryable`] — there is no separate
    /// `retryable` field on `OutcomeError`, so the classifier cannot drift
    /// from the enum arm.
    pub fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}

/// ErrorKind: 8 domain kinds + Custom(u16) for product extension.
///
/// Products extend via `Custom(u16)` — same category:type encoding as
/// `EventKind`. Retryability is defined on the kind; see
/// [`ErrorKind::is_retryable`]. Custom codes default to non-retryable;
/// downstream wrappers that need retryable custom semantics can layer their
/// own classifier on top (the enum is `#[non_exhaustive]` precisely so that
/// extension points are explicit).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A requested resource does not exist.
    NotFound,
    /// An operation conflicts with existing state.
    Conflict,
    /// Input failed validation rules.
    Validation,
    /// A gate or policy explicitly rejected the operation.
    PolicyRejection,
    /// An operation was explicitly cancelled and then collapsed into an error.
    Cancelled,
    /// A persistence or storage layer failure.
    StorageError,
    /// An operation exceeded its time limit.
    Timeout,
    /// A non-terminal pending outcome was collapsed into an error.
    Pending,
    /// A serialization or deserialization failure.
    Serialization,
    /// An unexpected internal error.
    Internal,
    /// A batch outcome was collapsed into a single error.
    BatchCollapse,
    /// A product-defined error kind identified by a numeric code.
    Custom(u16),
}

impl ErrorKind {
    /// Returns true if this error kind is considered retryable.
    ///
    /// Single source for retryability classification (G9): every outcome
    /// combinator and every `OutcomeError::is_retryable` call funnels
    /// through this method. `StorageError` and `Timeout` are the core
    /// retryable kinds; `Custom` codes are non-retryable by default.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::StorageError | Self::Timeout)
    }

    /// Returns true if this error kind is a domain error (`NotFound`, `Conflict`, `Validation`, or `PolicyRejection`).
    pub fn is_domain(&self) -> bool {
        matches!(
            self,
            Self::NotFound
                | Self::Conflict
                | Self::Validation
                | Self::PolicyRejection
                | Self::Cancelled
        )
    }

    /// Returns true if this error kind is operational (`StorageError`, `Timeout`, `Serialization`, or `Internal`).
    pub fn is_operational(&self) -> bool {
        matches!(
            self,
            Self::StorageError
                | Self::Timeout
                | Self::Pending
                | Self::Serialization
                | Self::Internal
                | Self::BatchCollapse
        )
    }
}

impl fmt::Display for OutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}", self.kind, self.message)
    }
}
impl std::error::Error for OutcomeError {}
