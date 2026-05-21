use super::StoreError;

/// Typed reason hidden-range metadata could not be admitted.
#[derive(Debug)]
#[non_exhaustive]
pub enum HiddenRangesCorruption {
    /// Reading the visibility-ranges file failed.
    ReadFailed(std::io::Error),
    /// The file was shorter than the fixed header.
    TooShort {
        /// Bytes available in the file.
        actual: usize,
        /// Bytes required for the fixed header.
        required: usize,
    },
    /// The file did not start with the visibility-ranges magic.
    BadMagic,
    /// The visibility-ranges version is unsupported.
    UnsupportedVersion {
        /// Version observed on disk.
        observed: u16,
        /// Version this crate accepts.
        expected: u16,
    },
    /// The stored CRC did not match the decoded body.
    CrcMismatch {
        /// CRC stored in the metadata header.
        stored: u32,
        /// CRC computed from the metadata body.
        computed: u32,
    },
    /// MessagePack decoding of the visibility body failed.
    DecodeFailed(rmp_serde::decode::Error),
    /// Decoded ranges were structurally malformed.
    MalformedEntries {
        /// Precise range normalization error.
        source: Box<StoreError>,
    },
}

impl std::fmt::Display for HiddenRangesCorruption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadFailed(error) => {
                write!(f, "failed to read visibility-ranges metadata: {error}")
            }
            Self::TooShort { .. } => write!(f, "visibility-ranges file too short"),
            Self::BadMagic => write!(f, "visibility-ranges file has wrong magic"),
            Self::UnsupportedVersion { observed, .. } => {
                write!(f, "unsupported visibility-ranges version: {observed}")
            }
            Self::CrcMismatch { .. } => write!(f, "visibility-ranges CRC mismatch"),
            Self::DecodeFailed(error) => {
                write!(f, "visibility-ranges deserialisation failed: {error}")
            }
            Self::MalformedEntries { source } => {
                write!(
                    f,
                    "visibility-ranges file contained malformed entries: {source}"
                )
            }
        }
    }
}

impl HiddenRangesCorruption {
    pub(super) fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadFailed(error) => Some(error),
            Self::DecodeFailed(error) => Some(error),
            Self::MalformedEntries { source } => Some(source.as_ref()),
            Self::TooShort { .. }
            | Self::BadMagic
            | Self::UnsupportedVersion { .. }
            | Self::CrcMismatch { .. } => None,
        }
    }
}
