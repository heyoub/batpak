use serde::{Deserialize, Serialize};
use std::fmt;

/// OutcomeError: structured error with kind, message, optional compensation.
/// [SPEC:src/outcome/error.rs]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutcomeError {
    pub kind: ErrorKind,
    pub message: String,
    pub compensation: Option<super::wait::CompensationAction>,
    pub retryable: bool,
}

/// ErrorKind: 8 domain kinds + Custom(u16) for product extension.
/// Products extend via Custom(u16) — same category:type encoding as EventKind.
/// [SPEC:src/outcome/error.rs — ErrorKind]

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    NotFound,
    Conflict,
    Validation,
    PolicyRejection,
    StorageError,
    Timeout,
    Serialization,
    Internal,
    Custom(u16),
}

impl ErrorKind {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::StorageError | Self::Timeout)
    }

    pub fn is_domain(&self) -> bool {
        matches!(
            self,
            Self::NotFound | Self::Conflict | Self::Validation | Self::PolicyRejection
        )
    }

    pub fn is_operational(&self) -> bool {
        matches!(
            self,
            Self::StorageError | Self::Timeout | Self::Serialization | Self::Internal
        )
    }
}

impl fmt::Display for OutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}", self.kind, self.message)
    }
}
impl std::error::Error for OutcomeError {}
