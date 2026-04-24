use crate::coordinate::CoordinateError;
use std::path::PathBuf;

/// Store open mode for lifetime-held directory locking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreLockMode {
    /// Mutable open: writer thread active, exclusive lock required.
    Mutable,
    /// Read-only open: no writer thread, but still exclusive in the first
    /// hardening wave until shared semantics are explicitly designed.
    ReadOnly,
}

/// StoreError: every error the store can produce.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// A filesystem or OS-level I/O failure.
    Io(std::io::Error),
    /// The store directory is already locked by another open handle.
    StoreLocked {
        /// Store root that could not be opened in the requested mode.
        path: PathBuf,
        /// Requested open mode that could not acquire its lifetime-held lock.
        mode: StoreLockMode,
    },
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
    /// A visibility-watermark publish request violated sequence-gate bounds.
    SequenceGateViolation {
        /// Human-readable operation that attempted the publish.
        operation: &'static str,
        /// Requested exclusive upper bound for visibility.
        requested: u64,
        /// Allocator watermark at the time of the request.
        allocated: u64,
        /// Current visible watermark at the time of the request.
        visible: u64,
    },
    /// A StoreConfig field has an invalid value.
    Configuration(String),
    /// Group commit (batch > 1) requires an idempotency key on every append.
    IdempotencyRequired,
    /// A visibility fence is already active on this store.
    VisibilityFenceActive,
    /// No matching visibility fence is currently active.
    VisibilityFenceNotActive,
    /// The visibility fence was cancelled before it published its writes.
    VisibilityFenceCancelled,
    /// Batch append failed at a specific item.
    BatchFailed {
        /// Index of the item that failed (0-based).
        item_index: usize,
        /// The underlying error.
        source: Box<StoreError>,
    },
    /// Batch append reached the durability boundary but segment sync failed
    /// before the batch became visible.
    BatchSyncFailed {
        /// Number of items in the batch whose final sync failed.
        item_count: usize,
        /// The underlying sync error.
        source: Box<StoreError>,
    },
    /// A fault was injected by the dangerous-test-hooks fault injection framework.
    #[cfg(feature = "dangerous-test-hooks")]
    FaultInjected(String),
    /// A batch mixes items with and without idempotency keys.
    /// Batches must be homogeneous: either every item carries an idempotency
    /// key or none do.
    IdempotencyPartialBatch {
        /// Human-readable description of the offending batch layout.
        reason: String,
    },
    /// A segment frame is corrupt (length field beyond buffer, bad CRC region, etc.).
    CorruptFrame {
        /// Segment file where the frame lives.
        segment_id: u64,
        /// Byte offset of the corrupt frame within the segment.
        offset: u64,
        /// Human-readable description of the corruption.
        reason: String,
    },
    /// A segment contains more entries than the on-disk footer format can address.
    SegmentTooManyEntries {
        /// Segment file whose entry count overflowed.
        segment_id: u64,
        /// Actual entry count that exceeded the supported `u32` range.
        count: u64,
    },
    /// The data directory contains a file that does not match the expected
    /// segment-filename convention (`NNNNNN.fbat`).
    DataDirMalformed {
        /// Path of the file that could not be recognised.
        path: std::path::PathBuf,
    },
    /// The ancestry walk detected a cycle in the hash chain.
    AncestryCorrupt {
        /// Event id at which the cycle was closed.
        cycle_at: u128,
    },
    /// A caller-supplied visibility range is malformed (`start >= end`).
    RangeMalformed {
        /// Invalid range start.
        start: u64,
        /// Invalid range end.
        end: u64,
    },
    /// A coordinate supplied at the API boundary failed revalidation.
    InvalidCoordinate {
        /// Optional position of the offending item within a batch.
        index: Option<usize>,
        /// Human-readable description of the rejection reason.
        reason: String,
    },
    /// A batch `CausationRef::PriorItem` pointed at a non-earlier item.
    InvalidCausation {
        /// Index the caller referenced.
        prior_idx: usize,
        /// Index of the item that issued the reference.
        item_index: usize,
        /// Human-readable description of the rejection reason.
        reason: String,
    },
    /// A `CommitMetadata` value failed validation.
    InvalidCommitMetadata {
        /// Human-readable description of the rejection reason.
        reason: String,
    },
    /// A coordinate component contained a forbidden NUL (`'\0'`) byte.
    CoordinateNulByte,
    /// A coordinate component contained a forbidden path-traversal substring
    /// (`..` or `/`).
    CoordinatePathTraversal,
    /// A coordinate component contained a forbidden ASCII control character.
    CoordinateControlChar,
    /// The on-disk hidden-ranges metadata file is present but unreadable.
    ///
    /// Cold start must fail closed: a corrupt visibility-ranges file cannot be
    /// silently treated as empty, because doing so would resurrect cancelled
    /// events. The caller must remediate (repair or manually clear the file)
    /// before proceeding.
    HiddenRangesCorrupt {
        /// Path of the unreadable metadata file.
        path: PathBuf,
        /// Human-readable description of why the file could not be parsed.
        reason: String,
    },
    /// A batch item's serialized payload exceeded `single_append_max_bytes`.
    ///
    /// The per-item ceiling is independent of the batch-total cap: a single
    /// oversized item is rejected synchronously at `submit_batch` entry even
    /// when the sum of all item payloads stays under the batch cap.
    BatchItemTooLarge {
        /// Index of the offending item within the batch (0-based).
        index: usize,
        /// Serialized payload size of the offending item, in bytes.
        size: usize,
        /// Configured per-item ceiling (`single_append_max_bytes`).
        limit: usize,
    },
    /// An entity's per-entity clock reached `u32::MAX`; further appends to
    /// that entity are rejected rather than silently saturating. See F1 /
    /// the INVARIANT doc at the mutation site in
    /// `store/write/writer.rs::precompute_batch_items`.
    EntityClockOverflow {
        /// Entity whose clock would have overflowed.
        entity: String,
    },
    /// The configured custom clock produced an invalid negative timestamp.
    ///
    /// Production `SystemTime` is normalized by the store runtime clock path
    /// and cannot produce this. This variant exists for caller-supplied test
    /// or integration clocks installed through `StoreConfig::with_clock`.
    InvalidClock {
        /// Rejected timestamp in microseconds since Unix epoch.
        timestamp_us: i64,
        /// Human-readable rejection reason.
        reason: String,
    },
    /// A durable cursor checkpoint write failed. Only raised for cursors
    /// constructed with `checkpoint_id: Some(_)`; in-memory cursors have
    /// no file write and cannot produce this error. See G5.
    CheckpointWriteFailed {
        /// The cursor checkpoint identifier (i.e. the file stem under
        /// `{data_dir}/cursors/{id}.ckpt`).
        id: String,
        /// The underlying I/O error from temp-file creation, write,
        /// fsync, or rename.
        source: std::io::Error,
    },
    /// A durable cursor checkpoint file exists but cannot be decoded.
    ///
    /// Durable resume must fail closed: silently treating a corrupt
    /// checkpoint as missing would rewind the worker to position 0 and
    /// re-deliver an unbounded prefix while claiming the durable surface
    /// was still intact.
    CursorCheckpointCorrupt {
        /// Path of the corrupt checkpoint file.
        path: PathBuf,
        /// Human-readable decode failure.
        reason: String,
    },
    /// A durable cursor checkpoint belongs to a different logical consumer.
    ///
    /// Checkpoints are bound to the exact region identity of the cursor
    /// that created them. Reusing a `checkpoint_id` across different
    /// regions must fail closed rather than silently resuming from an
    /// unrelated global position.
    CursorCheckpointRegionMismatch {
        /// Path of the mismatched checkpoint file.
        path: PathBuf,
        /// Region identity encoded in the checkpoint on disk.
        stored: Option<String>,
        /// Region identity expected by the caller.
        expected: String,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::StoreLocked { path, mode } => {
                let mode = match mode {
                    StoreLockMode::Mutable => "mutable",
                    StoreLockMode::ReadOnly => "read-only",
                };
                write!(
                    f,
                    "store at {} is already locked; could not acquire {mode} access",
                    path.display()
                )
            }
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
            Self::SequenceGateViolation {
                operation,
                requested,
                allocated,
                visible,
            } => write!(
                f,
                "sequence gate rejected {operation} publish({requested}) with allocated={allocated} visible={visible}"
            ),
            Self::Configuration(msg) => write!(f, "invalid config: {msg}"),
            Self::IdempotencyRequired => write!(
                f,
                "group commit (batch > 1) requires an idempotency key on every append"
            ),
            Self::VisibilityFenceActive => write!(f, "a visibility fence is already active"),
            Self::VisibilityFenceNotActive => {
                write!(f, "no matching visibility fence is currently active")
            }
            Self::VisibilityFenceCancelled => {
                write!(f, "visibility fence was cancelled before publish")
            }
            Self::BatchFailed { item_index, source } => {
                write!(f, "batch failed at item {}: {}", item_index, source)
            }
            Self::BatchSyncFailed { item_count, source } => {
                write!(
                    f,
                    "batch sync failed after writing {} items: {}",
                    item_count, source
                )
            }
            #[cfg(feature = "dangerous-test-hooks")]
            Self::FaultInjected(msg) => write!(f, "fault injected: {msg}"),
            Self::IdempotencyPartialBatch { reason } => {
                write!(f, "batch rejected: {reason}")
            }
            Self::CorruptFrame {
                segment_id,
                offset,
                reason,
            } => write!(
                f,
                "corrupt frame in segment {segment_id} at offset {offset}: {reason}"
            ),
            Self::SegmentTooManyEntries { segment_id, count } => write!(
                f,
                "segment {segment_id} has {count} entries, exceeding the u32 footer capacity"
            ),
            Self::DataDirMalformed { path } => {
                write!(
                    f,
                    "data directory contains unexpected file: {}",
                    path.display()
                )
            }
            Self::AncestryCorrupt { cycle_at } => {
                write!(f, "ancestry walk detected a cycle at event {cycle_at:032x}")
            }
            Self::RangeMalformed { start, end } => {
                write!(
                    f,
                    "malformed range: start={start} end={end} (start must be < end)"
                )
            }
            Self::InvalidCoordinate { index, reason } => match index {
                Some(i) => write!(f, "invalid coordinate at batch item {i}: {reason}"),
                None => write!(f, "invalid coordinate: {reason}"),
            },
            Self::InvalidCausation {
                prior_idx,
                item_index,
                reason,
            } => write!(
                f,
                "invalid causation at item {item_index} referencing prior {prior_idx}: {reason}"
            ),
            Self::InvalidCommitMetadata { reason } => {
                write!(f, "invalid commit metadata: {reason}")
            }
            Self::CoordinateNulByte => {
                write!(f, "coordinate component contains forbidden NUL byte")
            }
            Self::CoordinatePathTraversal => write!(
                f,
                "coordinate component contains forbidden path-traversal substring"
            ),
            Self::CoordinateControlChar => write!(
                f,
                "coordinate component contains forbidden ASCII control character"
            ),
            Self::HiddenRangesCorrupt { path, reason } => write!(
                f,
                "hidden-ranges metadata at {} is corrupt: {reason}",
                path.display()
            ),
            Self::BatchItemTooLarge { index, size, limit } => write!(
                f,
                "batch item {index} payload size {size} exceeds per-item ceiling {limit}"
            ),
            Self::EntityClockOverflow { entity } => write!(
                f,
                "entity {entity} per-entity clock reached u32::MAX; further appends rejected"
            ),
            Self::InvalidClock {
                timestamp_us,
                reason,
            } => write!(
                f,
                "custom clock returned invalid timestamp_us {timestamp_us}: {reason}"
            ),
            Self::CheckpointWriteFailed { id, source } => {
                write!(f, "cursor checkpoint {id} write failed: {source}")
            }
            Self::CursorCheckpointCorrupt { path, reason } => write!(
                f,
                "durable cursor checkpoint at {} is corrupt: {reason}",
                path.display()
            ),
            Self::CursorCheckpointRegionMismatch {
                path,
                stored,
                expected,
            } => write!(
                f,
                "durable cursor checkpoint at {} belongs to region {:?}, expected {}",
                path.display(),
                stored,
                expected
            ),
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
            Self::StoreLocked { .. }
            | Self::CrcMismatch { .. }
            | Self::CorruptSegment { .. }
            | Self::NotFound(_)
            | Self::SequenceMismatch { .. }
            | Self::WriterCrashed
            | Self::SequenceGateViolation { .. }
            | Self::Configuration(_)
            | Self::IdempotencyRequired
            | Self::VisibilityFenceActive
            | Self::VisibilityFenceNotActive
            | Self::VisibilityFenceCancelled
            | Self::IdempotencyPartialBatch { .. }
            | Self::CorruptFrame { .. }
            | Self::SegmentTooManyEntries { .. }
            | Self::DataDirMalformed { .. }
            | Self::AncestryCorrupt { .. }
            | Self::RangeMalformed { .. }
            | Self::InvalidCoordinate { .. }
            | Self::InvalidCausation { .. }
            | Self::InvalidCommitMetadata { .. }
            | Self::CoordinateNulByte
            | Self::CoordinatePathTraversal
            | Self::CoordinateControlChar
            | Self::HiddenRangesCorrupt { .. }
            | Self::BatchItemTooLarge { .. }
            | Self::EntityClockOverflow { .. }
            | Self::InvalidClock { .. }
            | Self::CursorCheckpointCorrupt { .. }
            | Self::CursorCheckpointRegionMismatch { .. } => None,
            Self::BatchFailed { source, .. } | Self::BatchSyncFailed { source, .. } => {
                Some(source.as_ref())
            }
            Self::CheckpointWriteFailed { source, .. } => Some(source),
            #[cfg(feature = "dangerous-test-hooks")]
            Self::FaultInjected(_) => None,
        }
    }
}

impl StoreError {
    pub(crate) fn batch_failed(item_index: usize, source: impl Into<Box<StoreError>>) -> Self {
        Self::BatchFailed {
            item_index,
            source: source.into(),
        }
    }

    pub(crate) fn batch_sync_failed(item_count: usize, source: impl Into<Box<StoreError>>) -> Self {
        Self::BatchSyncFailed {
            item_count,
            source: source.into(),
        }
    }

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

    /// Cache operation failed with a typed error (IO, serialization, etc.).
    pub(crate) fn cache_error(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::CacheFailed(Box::new(err))
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
        // Route the hardening-specific errors to their dedicated variants so
        // callers can match precise failure modes without stringly parsing.
        match e {
            CoordinateError::NulByte => Self::CoordinateNulByte,
            CoordinateError::ControlChar => Self::CoordinateControlChar,
            CoordinateError::PathTraversal => Self::CoordinatePathTraversal,
            other @ CoordinateError::EmptyEntity
            | other @ CoordinateError::EmptyScope
            | other @ CoordinateError::EntityTooLong { .. }
            | other @ CoordinateError::ScopeTooLong { .. } => Self::Coordinate(other),
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
