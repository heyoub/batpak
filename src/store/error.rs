use crate::coordinate::CoordinateError;

/// Stage of batch processing when failure occurred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchStage {
    /// Pre-write checks (CAS, idempotency, entity locks).
    Validation,
    /// Payload serialization.
    Encoding,
    /// Segment file write.
    Writing,
    /// fsync to disk.
    Syncing,
    /// In-memory index update.
    Indexing,
}

/// StoreError: every error the store can produce.
/// [SPEC:src/store/error.rs — StoreError variants]
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// A filesystem or OS-level I/O failure.
    Io(std::io::Error),
    /// An invalid or malformed coordinate (entity/scope).
    Coordinate(CoordinateError),
    /// MessagePack serialization or deserialization failed.
    Serialization(Box<dyn std::error::Error + Send + Sync>),
    /// CRC32 checksum did not match the frame data.
    CrcMismatch {
        /// Segment file where the mismatch occurred.
        segment_id: u64,
        /// Byte offset of the corrupt frame within the segment.
        offset: u64,
    },
    /// Segment file has unrecoverable structural corruption.
    CorruptSegment {
        /// Segment file that is corrupt.
        segment_id: u64,
        /// Human-readable description of the corruption.
        detail: String,
    },
    /// No event with the given ID exists in the index.
    NotFound(u128),
    /// CAS check failed: the entity's current sequence did not match the expected value.
    SequenceMismatch {
        /// Entity whose sequence was checked.
        entity: String,
        /// Sequence value provided by the caller.
        expected: u32,
        /// Actual current sequence of the entity.
        actual: u32,
    },
    /// The writer thread has crashed and is no longer processing commands.
    WriterCrashed,
    /// A projection cache operation failed.
    CacheFailed(Box<dyn std::error::Error + Send + Sync>),
    /// A StoreConfig field has an invalid value.
    Configuration(String),
    /// Group commit (batch > 1) requires an idempotency key on every append.
    IdempotencyRequired,
    /// Batch append failed at a specific item.
    BatchFailed {
        /// Index of the item that failed (0-based).
        item_index: usize,
        /// Stage of processing when the failure occurred.
        stage: BatchStage,
        /// The underlying error.
        source: Box<StoreError>,
    },
    /// A fault was injected by the test-support fault injection framework.
    #[cfg(feature = "test-support")]
    FaultInjected(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Coordinate(e) => write!(f, "coordinate error: {e}"),
            Self::Serialization(e) => write!(f, "serialization error: {e}"),
            Self::CrcMismatch { segment_id, offset } => {
                write!(f, "CRC mismatch in segment {segment_id} at offset {offset}")
            }
            Self::CorruptSegment { segment_id, detail } => {
                write!(f, "corrupt segment {segment_id}: {detail}")
            }
            Self::NotFound(id) => write!(f, "event {id:032x} not found"),
            Self::SequenceMismatch {
                entity,
                expected,
                actual,
            } => write!(
                f,
                "CAS failed for {entity}: expected seq {expected}, got {actual}"
            ),
            Self::WriterCrashed => write!(f, "writer thread crashed"),
            Self::CacheFailed(e) => write!(f, "cache error: {e}"),
            Self::Configuration(msg) => write!(f, "invalid config: {msg}"),
            Self::IdempotencyRequired => write!(
                f,
                "group commit (batch > 1) requires an idempotency key on every append"
            ),
            Self::BatchFailed {
                item_index,
                stage,
                source,
            } => write!(
                f,
                "batch failed at item {} during {:?}: {}",
                item_index, stage, source
            ),
            #[cfg(feature = "test-support")]
            Self::FaultInjected(msg) => write!(f, "fault injected: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Coordinate(e) => Some(e),
            Self::Serialization(e) => Some(e.as_ref()),
            Self::CacheFailed(e) => Some(e.as_ref()),
            Self::CrcMismatch { .. }
            | Self::CorruptSegment { .. }
            | Self::NotFound(_)
            | Self::SequenceMismatch { .. }
            | Self::WriterCrashed
            | Self::Configuration(_)
            | Self::IdempotencyRequired
            | Self::BatchFailed { .. } => None,
            #[cfg(feature = "test-support")]
            Self::FaultInjected(_) => None,
        }
    }
}

impl StoreError {
    /// Segment has a bad magic number (not a valid batpak segment).
    pub(crate) fn corrupt_magic(segment_id: u64) -> Self {
        Self::CorruptSegment {
            segment_id,
            detail: "bad magic".into(),
        }
    }

    /// Unexpected EOF during frame read.
    pub(crate) fn corrupt_eof(segment_id: u64) -> Self {
        Self::CorruptSegment {
            segment_id,
            detail: "unexpected EOF during read".into(),
        }
    }

    /// Segment has an unsupported version number.
    pub(crate) fn corrupt_version(segment_id: u64, version: u16) -> Self {
        Self::CorruptSegment {
            segment_id,
            detail: format!("unsupported segment version: {version}"),
        }
    }

    /// Cache operation failed with a message (no underlying typed error).
    pub(crate) fn cache_msg(msg: &str) -> Self {
        Self::CacheFailed(msg.into())
    }

    /// Serialization failed with a message (no underlying typed error).
    pub(crate) fn ser_msg(msg: &str) -> Self {
        Self::Serialization(msg.into())
    }

    /// Frame deserialization failed.
    pub(crate) fn corrupt_frame(segment_id: u64, detail: impl Into<String>) -> Self {
        Self::CorruptSegment {
            segment_id,
            detail: detail.into(),
        }
    }
}

impl From<CoordinateError> for StoreError {
    fn from(e: CoordinateError) -> Self {
        Self::Coordinate(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
