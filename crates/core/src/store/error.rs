use crate::coordinate::CoordinateError;
use crate::event::EventPayloadRegistryError;
use crate::store::stats::{HlcPoint, WatermarkKind};
use std::path::PathBuf;

/// Store open mode for lifetime-held directory locking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreLockMode {
    /// Mutable open: writer thread active, exclusive lock required.
    Mutable,
    /// Read-only open: no writer thread, but still exclusive under the
    /// current store-ownership contract.
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
    /// A synchronous frontier wait timed out before the requested watermark
    /// crossed the target point.
    WaitTimeout {
        /// Watermark being observed.
        watermark: WatermarkKind,
        /// Target HLC point requested by the caller.
        target: HlcPoint,
        /// Requested wait duration in milliseconds.
        waited_ms: u64,
    },
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
    /// A configured platform profile file was unreadable or malformed.
    PlatformProfileInvalid {
        /// Path of the profile file.
        path: PathBuf,
        /// Human-readable rejection reason.
        reason: String,
    },
    /// The configured platform profile did not match current platform evidence.
    PlatformProfileMismatch {
        /// Path of the profile file.
        path: PathBuf,
        /// Human-readable mismatch description.
        reason: String,
    },
    /// Platform evidence could not be admitted for a required store capability.
    PlatformAdmissionFailed {
        /// Capability whose admission failed.
        capability: &'static str,
        /// Human-readable admission failure.
        reason: String,
    },
    /// Linked typed payloads claimed duplicate EventKind assignments.
    EventPayloadRegistry(EventPayloadRegistryError),
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
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "dangerous-test-hooks"))
    )]
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
    /// A batch item's serialized payload plus encoded receipt-extension bytes
    /// exceeded `single_append_max_bytes`.
    ///
    /// The per-item ceiling is independent of the batch-total cap: a single
    /// oversized item is rejected synchronously at `submit_batch` entry even
    /// when the sum of all item payloads and extensions stays under the batch cap.
    BatchItemTooLarge {
        /// Index of the offending item within the batch (0-based).
        index: usize,
        /// Serialized payload plus encoded receipt-extension size of the
        /// offending item, in bytes.
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
    /// An internal store invariant was violated during recovery or lifecycle
    /// bootstrap. Returned as an error so adversarial recovery inputs fail
    /// closed instead of panicking the process.
    InvariantViolation {
        /// Human-readable invariant failure.
        reason: String,
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
                    "store at {} is already locked; could not acquire {mode} access; ensure no other process holds this directory and remove {} only after verifying no owner is alive",
                    path.display(),
                    path.join(".batpak.lock").display()
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
            Self::WaitTimeout {
                watermark,
                target,
                waited_ms,
            } => write!(
                f,
                "wait for {watermark:?} watermark to reach {target:?} timed out after {waited_ms}ms"
            ),
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
            Self::PlatformProfileInvalid { path, reason } => write!(
                f,
                "platform profile at {} is invalid: {reason}",
                path.display()
            ),
            Self::PlatformProfileMismatch { path, reason } => write!(
                f,
                "platform profile at {} does not match current platform evidence: {reason}",
                path.display()
            ),
            Self::PlatformAdmissionFailed { capability, reason } => {
                write!(f, "platform admission failed for {capability}: {reason}")
            }
            Self::EventPayloadRegistry(error) => write!(f, "{error}"),
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
                "coordinate component contains forbidden path-traversal substring (`..` or `/`)"
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
                "batch item {index} append bytes {size} exceeds per-item ceiling {limit}"
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
            Self::InvariantViolation { reason } => write!(f, "invariant violation: {reason}"),
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
            | Self::WaitTimeout { .. }
            | Self::SequenceGateViolation { .. }
            | Self::Configuration(_)
            | Self::PlatformProfileInvalid { .. }
            | Self::PlatformProfileMismatch { .. }
            | Self::PlatformAdmissionFailed { .. }
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
            | Self::CursorCheckpointRegionMismatch { .. }
            | Self::InvariantViolation { .. } => None,
            Self::EventPayloadRegistry(error) => Some(error),
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

impl From<rmp_serde::encode::Error> for StoreError {
    fn from(e: rmp_serde::encode::Error) -> Self {
        Self::Serialization(Box::new(e))
    }
}

#[cfg(test)]
mod tests {
    use super::{StoreError, StoreLockMode};
    use std::error::Error as _;
    use std::io;

    fn assert_display_contains(error: &StoreError, needle: &str) {
        let display = error.to_string();
        assert!(
            display.contains(needle),
            "helper constructor display should contain {needle:?}, got {display:?}"
        );
    }

    #[test]
    fn batch_failed_helper_preserves_item_index_and_source() {
        let error = StoreError::batch_failed(
            3,
            StoreError::Io(io::Error::new(io::ErrorKind::TimedOut, "append timed out")),
        );

        assert!(matches!(
            &error,
            StoreError::BatchFailed {
                item_index: 3,
                source
            } if matches!(source.as_ref(), StoreError::Io(_))
        ));
        assert_display_contains(&error, "batch failed at item 3");
        assert_display_contains(&error, "append timed out");
        assert!(
            error
                .source()
                .is_some_and(|source| source.to_string().contains("append timed out")),
            "BatchFailed helper should expose the wrapped StoreError as source"
        );
    }

    #[test]
    fn batch_sync_failed_helper_preserves_count_and_source() {
        let error =
            StoreError::batch_sync_failed(4, StoreError::Io(io::Error::other("fsync failed")));

        assert!(matches!(
            &error,
            StoreError::BatchSyncFailed {
                item_count: 4,
                source
            } if matches!(source.as_ref(), StoreError::Io(_))
        ));
        assert_display_contains(&error, "batch sync failed after writing 4 items");
        assert_display_contains(&error, "fsync failed");
        assert!(
            error
                .source()
                .is_some_and(|source| source.to_string().contains("fsync failed")),
            "BatchSyncFailed helper should expose the wrapped StoreError as source"
        );
    }

    #[test]
    fn corrupt_magic_helper_builds_corrupt_segment() {
        let error = StoreError::corrupt_magic(9);

        assert!(
            matches!(
                error,
                StoreError::CorruptSegment {
                    segment_id: 9,
                    ref detail
                } if detail == "bad magic"
            ),
            "expected bad-magic CorruptSegment, got {error:?}"
        );
        assert_display_contains(&error, "corrupt segment 9");
        assert_display_contains(&error, "bad magic");
        assert!(error.source().is_none());
    }

    #[test]
    fn corrupt_eof_helper_builds_corrupt_segment() {
        let error = StoreError::corrupt_eof(11);

        assert!(
            matches!(
                error,
                StoreError::CorruptSegment {
                    segment_id: 11,
                    ref detail
                } if detail == "unexpected EOF during read"
            ),
            "expected EOF CorruptSegment, got {error:?}"
        );
        assert_display_contains(&error, "corrupt segment 11");
        assert_display_contains(&error, "unexpected EOF during read");
        assert!(error.source().is_none());
    }

    #[test]
    fn corrupt_version_helper_builds_corrupt_segment() {
        let error = StoreError::corrupt_version(12, 99);

        assert!(
            matches!(
                error,
                StoreError::CorruptSegment {
                    segment_id: 12,
                    ref detail
                } if detail.contains("unsupported segment version: 99")
            ),
            "expected version CorruptSegment, got {error:?}"
        );
        assert_display_contains(&error, "corrupt segment 12");
        assert_display_contains(&error, "unsupported segment version: 99");
        assert!(error.source().is_none());
    }

    #[test]
    fn cache_msg_helper_builds_cache_failed_without_typed_source() {
        let error = StoreError::cache_msg("cache metadata short read");

        assert!(matches!(error, StoreError::CacheFailed(_)));
        assert_display_contains(&error, "cache error");
        assert_display_contains(&error, "cache metadata short read");
        assert!(
            error
                .source()
                .is_some_and(|source| source.to_string().contains("cache metadata short read")),
            "CacheFailed helper should expose the boxed message error as source"
        );
    }

    #[test]
    fn cache_error_helper_builds_cache_failed_with_typed_source() {
        let error = StoreError::cache_error(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cache dir denied",
        ));

        assert!(matches!(error, StoreError::CacheFailed(_)));
        assert_display_contains(&error, "cache error");
        assert_display_contains(&error, "cache dir denied");
        assert!(
            error
                .source()
                .is_some_and(|source| source.to_string().contains("cache dir denied")),
            "CacheFailed typed helper should expose the wrapped source"
        );
    }

    #[test]
    fn ser_msg_helper_builds_serialization_error() {
        let error = StoreError::ser_msg("frame exceeds u32::MAX");

        assert!(matches!(error, StoreError::Serialization(_)));
        assert_display_contains(&error, "serialization error");
        assert_display_contains(&error, "frame exceeds u32::MAX");
        assert!(
            error
                .source()
                .is_some_and(|source| source.to_string().contains("frame exceeds u32::MAX")),
            "Serialization helper should expose the boxed message error as source"
        );
    }

    #[test]
    fn corrupt_frame_helper_builds_corrupt_segment_with_detail() {
        let error = StoreError::corrupt_frame(13, "valid CRC but malformed msgpack");

        assert!(
            matches!(
                error,
                StoreError::CorruptSegment {
                    segment_id: 13,
                    ref detail
                } if detail == "valid CRC but malformed msgpack"
            ),
            "expected detail-preserving CorruptSegment, got {error:?}"
        );
        assert_display_contains(&error, "corrupt segment 13");
        assert_display_contains(&error, "valid CRC but malformed msgpack");
        assert!(error.source().is_none());
    }

    #[test]
    fn store_locked_display_names_modes() {
        let read_only = StoreError::StoreLocked {
            path: "fixtures/store".into(),
            mode: StoreLockMode::ReadOnly,
        };
        let mutable = StoreError::StoreLocked {
            path: "fixtures/store".into(),
            mode: StoreLockMode::Mutable,
        };

        assert_display_contains(&read_only, "read-only");
        assert_display_contains(&mutable, "mutable");
    }
}
