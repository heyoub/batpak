use crate::coordinate::CoordinateError;
use crate::event::{
    EventPayloadRegistryError, ProjectionStateContract, StateExtent, UpcastChainRegistryError,
};
use crate::store::delivery::observation::CheckpointIdError;
use crate::store::stats::{HlcPoint, WatermarkKind};
use std::path::PathBuf;

mod display;
mod hidden_ranges;
mod invariant;
mod platform;

pub use hidden_ranges::HiddenRangesCorruption;
pub use invariant::StoreInvariant;
pub use platform::{ProfileInvalidKind, StoreLockMode};

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
    /// An invalid or malformed durable cursor checkpoint identity.
    CheckpointId(CheckpointIdError),
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
    NotFound(crate::id::EventId),
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
    /// A projection was materialized without declaring a growth contract.
    ProjectionStateContractUnspecified {
        /// Projection identity rejected at materialization time.
        projection: String,
    },
    /// A bounded projection could not report its current state extent.
    ProjectionStateExtentUnavailable {
        /// Projection identity rejected at materialization time.
        projection: String,
        /// Declared growth contract for the projection (boxed to keep
        /// `StoreError` small enough for the clippy large-Err threshold).
        declared: Box<ProjectionStateContract>,
        /// Actual extent report returned by the projection.
        actual: StateExtent,
    },
    /// A bounded projection exceeded its declared maximum cardinality.
    ProjectionStateBoundExceeded {
        /// Projection identity rejected at materialization time.
        projection: String,
        /// Declared growth contract for the projection (boxed to keep
        /// `StoreError` small enough for the clippy large-Err threshold).
        declared: Box<ProjectionStateContract>,
        /// Actual extent report returned by the projection.
        actual: StateExtent,
    },
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
        /// Typed rejection reason.
        kind: ProfileInvalidKind,
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
    /// A linked payload kind declares `PAYLOAD_VERSION > 1` but its registered
    /// `Upcast` steps do not form a complete `1 -> ... -> N` chain, so events
    /// stored at an uncovered version would be undecodable at read time. The
    /// store refuses to open (fail closed) under the default
    /// [`EventPayloadValidation::FailFast`](crate::event::EventPayloadValidation)
    /// rather than letting those historical events silently strand.
    UpcastChainIncomplete(UpcastChainRegistryError),
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
    /// The on-disk `index.idemp` durable idempotency store declares a format
    /// version newer than this binary understands. Like the schema-evolution
    /// `FutureVersion`, this is a hard error: a reader can never reconstruct a
    /// format it predates. Upgrade the reader.
    IdempotencyFutureVersion {
        /// Version stamped on the on-disk file.
        stored: u16,
        /// The maximum version this binary understands.
        current: u16,
    },
    /// The on-disk mmap index (`index.fbati`) declares a format version strictly
    /// newer than this binary understands. Unlike a corrupt or older artifact —
    /// which the cold-start flow safely rebuilds from the durable segments — a
    /// future-version artifact is a hard, canonical refusal: a future writer may
    /// have written segments or summaries this reader cannot interpret, so
    /// silently rebuilding from scan would risk a silent downgrade rather than a
    /// legally reachable state. Mirrors [`Self::IdempotencyFutureVersion`]. Upgrade the
    /// reader. justifies: INV-MMAP-SEALED-READS
    MmapFutureVersion {
        /// Version stamped on the on-disk file.
        found: u16,
        /// The maximum version this binary understands.
        supported: u16,
    },
    /// The on-disk checkpoint (`index.ckpt`) declares a format version strictly
    /// newer than this binary understands. Unlike a corrupt or older checkpoint —
    /// which the cold-start flow safely rebuilds from the durable segments — a
    /// future-version checkpoint is a hard, canonical refusal: a future writer
    /// may have written a snapshot this reader cannot interpret, so silently
    /// rebuilding from scan would risk a silent downgrade rather than a legally
    /// reachable state. Mirrors [`Self::MmapFutureVersion`]. Upgrade the reader.
    CheckpointFutureVersion {
        /// Version stamped on the on-disk file.
        found: u16,
        /// The maximum version this binary understands.
        supported: u16,
    },
    /// The on-disk hidden-ranges metadata (`visibility_ranges.fbv`) declares a
    /// format version strictly newer than this binary understands. Unlike a
    /// corrupt or older artifact — surfaced as [`Self::HiddenRangesCorrupt`] for the
    /// caller to remediate — a future-version artifact is a distinct canonical
    /// refusal: a future writer may have recorded cancelled ranges in a layout
    /// this reader cannot interpret, so treating it as remediable corruption (or
    /// silently empty) would risk resurrecting cancelled events. Mirrors
    /// [`Self::MmapFutureVersion`]. Upgrade the reader.
    HiddenRangesFutureVersion {
        /// Path of the future-version metadata file.
        path: PathBuf,
        /// Version stamped on the on-disk file.
        found: u16,
        /// The maximum version this binary understands.
        supported: u16,
    },
    /// A fork evidence report body declares a schema version strictly newer than
    /// this binary understands. Mirrors [`Self::MmapFutureVersion`]: silently
    /// accepting an unknown fork-evidence layout would mis-classify fork outcomes.
    ForkEvidenceFutureVersion {
        /// Version stamped on the wire artifact.
        found: u16,
        /// The maximum version this binary understands.
        supported: u16,
    },
    /// An import provenance extension body declares a schema version strictly
    /// newer than this binary understands. Mirrors [`Self::MmapFutureVersion`]:
    /// silently decoding unknown provenance would break import idempotency audits.
    ImportProvenanceFutureVersion {
        /// Version stamped on the wire artifact.
        found: u16,
        /// The maximum version this binary understands.
        supported: u16,
    },
    /// A new keyed append was refused because the durable idempotency store is
    /// at its soft cap and the configured `OverflowPolicy` is `FailClosed`
    /// (or `Backpressure`, which is treated as fail-closed). Existing
    /// within-window keys are never evicted, so prior retries remain no-ops;
    /// only genuinely new keys are rejected.
    IdempotencyOverflowFailClosed {
        /// Current number of durable keys.
        len: u64,
        /// Configured soft cap.
        max_keys: u64,
    },
    /// A typed append carried `EventPayload::PAYLOAD_VERSION == 0`. Version `0`
    /// is the reserved legacy/untyped sentinel and is never a valid declared
    /// version. The `#[derive(EventPayload)]` macro rejects this at compile
    /// time, but a hand-written `EventPayload` impl can still set it, so the
    /// typed-append seam rejects it at runtime before stamping the header
    /// (a non-zero declared version is what lets the decode seam tell a real
    /// typed frame apart from a legacy untyped one).
    InvalidPayloadVersion {
        /// The rejected event kind, as its raw 16-bit encoding.
        kind: u16,
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
    /// The `u32` interner id domain is exhausted: every one of the ~4 billion
    /// available `InternId` slots has
    /// been allocated, so no further entity/scope string can be interned.
    InternerExhausted {
        /// The interned-string count at the point of exhaustion (the last id
        /// successfully assigned before the domain overflowed).
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
        cycle_at: crate::id::EventId,
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
    /// A reserved system/effect/tombstone `EventKind` was submitted through the
    /// public raw-`kind` append surface. Reserved kinds are emitted only by the
    /// internal substrate (batch markers, lifecycle receipts, tombstones,
    /// gate-denial audit receipts) and cannot be forged by callers.
    ReservedKind {
        /// Optional position of the offending item within a batch.
        index: Option<usize>,
        /// The rejected reserved kind, as its raw 16-bit encoding.
        kind: u16,
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
        /// Typed corruption reason.
        kind: HiddenRangesCorruption,
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
        /// Typed invariant failure.
        kind: StoreInvariant,
    },
    /// An at-open hash-chain recompute (opted into via
    /// [`ChainVerification::Recompute`](crate::store::ChainVerification)) found
    /// the store is not intact, so the open failed closed. A plain `Crc` open
    /// trusts the per-frame CRC and never produces this; only the regulated
    /// recompute path — equivalently [`Store::verify_chain`](crate::store::Store::verify_chain)
    /// — recomputes blake3 over every committed event and refuses to hand back a
    /// tampered store.
    ChainVerificationFailed {
        /// Committed events whose recomputed blake3 content hash did NOT match
        /// the stored `event_hash` (content no longer matches its identity).
        content_hash_mismatches: usize,
        /// Non-genesis events whose `prev_hash` referenced no verified event (a
        /// dangling chain link).
        dangling_links: usize,
    },
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Coordinate(e) => Some(e),
            Self::CheckpointId(e) => Some(e),
            Self::Serialization(e) => Some(e.as_ref()),
            Self::CacheFailed(e) => Some(e.as_ref()),
            Self::PlatformProfileInvalid { kind, .. } => kind.source(),
            Self::HiddenRangesCorrupt { kind, .. } => kind.source(),
            Self::StoreLocked { .. }
            | Self::CrcMismatch { .. }
            | Self::CorruptSegment { .. }
            | Self::NotFound(_)
            | Self::SequenceMismatch { .. }
            | Self::WriterCrashed
            | Self::WaitTimeout { .. }
            | Self::SequenceGateViolation { .. }
            | Self::Configuration(_)
            | Self::ProjectionStateContractUnspecified { .. }
            | Self::ProjectionStateExtentUnavailable { .. }
            | Self::ProjectionStateBoundExceeded { .. }
            | Self::PlatformProfileMismatch { .. }
            | Self::PlatformAdmissionFailed { .. }
            | Self::IdempotencyRequired
            | Self::VisibilityFenceActive
            | Self::VisibilityFenceNotActive
            | Self::VisibilityFenceCancelled
            | Self::IdempotencyPartialBatch { .. }
            | Self::IdempotencyFutureVersion { .. }
            | Self::MmapFutureVersion { .. }
            | Self::CheckpointFutureVersion { .. }
            | Self::HiddenRangesFutureVersion { .. }
            | Self::ForkEvidenceFutureVersion { .. }
            | Self::ImportProvenanceFutureVersion { .. }
            | Self::IdempotencyOverflowFailClosed { .. }
            | Self::InvalidPayloadVersion { .. }
            | Self::CorruptFrame { .. }
            | Self::SegmentTooManyEntries { .. }
            | Self::InternerExhausted { .. }
            | Self::DataDirMalformed { .. }
            | Self::AncestryCorrupt { .. }
            | Self::RangeMalformed { .. }
            | Self::InvalidCoordinate { .. }
            | Self::ReservedKind { .. }
            | Self::InvalidCausation { .. }
            | Self::InvalidCommitMetadata { .. }
            | Self::CoordinateNulByte
            | Self::CoordinatePathTraversal
            | Self::CoordinateControlChar
            | Self::BatchItemTooLarge { .. }
            | Self::EntityClockOverflow { .. }
            | Self::InvalidClock { .. }
            | Self::CursorCheckpointCorrupt { .. }
            | Self::CursorCheckpointRegionMismatch { .. }
            | Self::InvariantViolation { .. }
            | Self::ChainVerificationFailed { .. } => None,
            Self::EventPayloadRegistry(error) => Some(error),
            Self::UpcastChainIncomplete(error) => Some(error),
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

    /// Segment-level corruption with caller-supplied detail.
    pub(crate) fn corrupt_segment_with_detail(segment_id: u64, detail: impl Into<String>) -> Self {
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
            | other @ CoordinateError::ScopeTooLong { .. }
            | other @ CoordinateError::ForbiddenSeparator => Self::Coordinate(other),
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
mod tests;
