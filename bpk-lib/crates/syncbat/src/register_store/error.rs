//! Errors returned by batpak-backed syncbat register catalog operations.

use std::{error::Error, fmt};

use batpak::event::TypedDecodeError;
use batpak::store::StoreError;

use crate::operation::DescriptorValidationError;
use crate::register::RegisterValidationError;

/// Error returned by batpak-backed syncbat register catalog operations.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreRegisterCatalogError {
    /// Batpak store operation failed.
    Store(StoreError),
    /// Stored payload could not be decoded as a register row.
    Decode(TypedDecodeError),
    /// Catalog row used an unsupported schema version.
    InvalidSchemaVersion {
        /// Unsupported schema version.
        version: u16,
    },
    /// Catalog row used an unsupported action.
    InvalidAction {
        /// Unsupported action.
        action: String,
    },
    /// Catalog row used an unsupported effect spelling.
    InvalidEffect {
        /// Unsupported effect spelling.
        effect: String,
    },
    /// Catalog row decoded but did not validate as an operation descriptor.
    InvalidDescriptor(DescriptorValidationError),
    /// Catalog row is not well-formed for its declared lifecycle action.
    InvalidLifecycleRow {
        /// Operation name carried by the malformed row.
        name: String,
        /// Stable action spelling.
        action: String,
        /// Stable conflict explanation.
        reason: &'static str,
    },
    /// Rebuilt catalog contains an invalid lifecycle transition.
    CatalogConflict {
        /// Conflicting operation name.
        name: String,
        /// Stable action spelling.
        action: String,
        /// Stable conflict explanation.
        reason: &'static str,
    },
    /// Rebuilt register rejected the catalog rows.
    Register(RegisterValidationError),
}

impl fmt::Display for StoreRegisterCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "batpak register catalog operation failed: {error}"),
            Self::Decode(error) => write!(f, "register catalog row decode failed: {error}"),
            Self::InvalidSchemaVersion { version } => {
                write!(f, "unsupported register catalog schema version {version}")
            }
            Self::InvalidAction { action } => {
                write!(f, "unsupported register catalog action `{action}`")
            }
            Self::InvalidEffect { effect } => {
                write!(f, "unsupported register catalog effect `{effect}`")
            }
            Self::InvalidDescriptor(error) => {
                write!(f, "invalid register catalog descriptor: {error}")
            }
            Self::InvalidLifecycleRow {
                name,
                action,
                reason,
            } => {
                write!(
                    f,
                    "invalid register catalog `{action}` row for `{name}`: {reason}"
                )
            }
            Self::CatalogConflict {
                name,
                action,
                reason,
            } => {
                write!(
                    f,
                    "conflicting register catalog `{action}` transition for `{name}`: {reason}"
                )
            }
            Self::Register(error) => write!(f, "rebuilt register rejected catalog: {error}"),
        }
    }
}

impl Error for StoreRegisterCatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::InvalidDescriptor(error) => Some(error),
            Self::Register(error) => Some(error),
            Self::InvalidSchemaVersion { .. }
            | Self::InvalidAction { .. }
            | Self::InvalidEffect { .. }
            | Self::InvalidLifecycleRow { .. }
            | Self::CatalogConflict { .. } => None,
        }
    }
}

impl From<StoreError> for StoreRegisterCatalogError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<TypedDecodeError> for StoreRegisterCatalogError {
    fn from(error: TypedDecodeError) -> Self {
        Self::Decode(error)
    }
}
