//! `Display` rendering for [`StoreError`].
//!
//! Split out of `store/error.rs` so the error module stays under the absolute
//! production file-size cap (split-don't-bump). The future-version refusals
//! share one render shape via [`StoreError::fmt_future_version`], kept off the
//! main `Display::fmt` match so adding a per-format refusal does not grow that
//! function past its complexity ratchet.

use super::{StoreError, StoreLockMode};
use crate::id::EntityIdType;

impl StoreError {
    /// Shared `Display` body for the on-disk future-version refusals. Each format
    /// renders the same "on disk is version N but this binary understands at most
    /// version M; upgrade the reader" shape with a format-specific subject. Kept
    /// out of the main `Display::fmt` match so adding a per-format refusal does
    /// not grow that function past its complexity ratchet.
    fn fmt_future_version(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdempotencyFutureVersion { stored, current } => write!(
                f,
                "durable idempotency store on disk is version {stored} but this binary understands \
                 at most version {current}; upgrade the reader"
            ),
            Self::MmapFutureVersion { found, supported } => write!(
                f,
                "mmap index on disk is version {found} but this binary understands at most version \
                 {supported}; refusing to rebuild from scan (a future writer may have written data \
                 this reader cannot interpret); upgrade the reader"
            ),
            Self::CheckpointFutureVersion { found, supported } => write!(
                f,
                "checkpoint on disk is version {found} but this binary understands at most version \
                 {supported}; refusing to rebuild from scan (a future writer may have written a \
                 snapshot this reader cannot interpret); upgrade the reader"
            ),
            Self::HiddenRangesFutureVersion {
                path,
                found,
                supported,
            } => write!(
                f,
                "hidden-ranges metadata at {} is version {found} but this binary understands at \
                 most version {supported}; refusing to open (a future writer may have recorded \
                 cancelled ranges this reader cannot interpret); upgrade the reader",
                path.display()
            ),
            Self::ForkEvidenceFutureVersion { found, supported } => write!(
                f,
                "fork evidence report is version {found} but this binary understands at most \
                 version {supported}; upgrade the reader"
            ),
            Self::ImportProvenanceFutureVersion { found, supported } => write!(
                f,
                "import provenance extension is version {found} but this binary understands at \
                 most version {supported}; upgrade the reader"
            ),
            // Reached only from the four future-version arms of `Display::fmt`.
            // The remaining variants are listed explicitly (not wildcarded) so a
            // newly-added variant trips a compile error here, and return the
            // empty render they can never actually produce on this path.
            // justifies: INV-ONDISK-FORWARD-COMPAT-CANONICAL
            Self::Io(_)
            | Self::StoreLocked { .. }
            | Self::Coordinate(_)
            | Self::CheckpointId(_)
            | Self::Serialization(_)
            | Self::CrcMismatch { .. }
            | Self::CorruptSegment { .. }
            | Self::NotFound(_)
            | Self::SequenceMismatch { .. }
            | Self::WriterCrashed
            | Self::WaitTimeout { .. }
            | Self::CacheFailed(_)
            | Self::SequenceGateViolation { .. }
            | Self::Configuration(_)
            | Self::PlatformProfileInvalid { .. }
            | Self::PlatformProfileMismatch { .. }
            | Self::PlatformAdmissionFailed { .. }
            | Self::EventPayloadRegistry(_)
            | Self::IdempotencyRequired
            | Self::VisibilityFenceActive
            | Self::VisibilityFenceNotActive
            | Self::VisibilityFenceCancelled
            | Self::BatchFailed { .. }
            | Self::BatchSyncFailed { .. }
            | Self::IdempotencyPartialBatch { .. }
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
            | Self::HiddenRangesCorrupt { .. }
            | Self::BatchItemTooLarge { .. }
            | Self::EntityClockOverflow { .. }
            | Self::InvalidClock { .. }
            | Self::CheckpointWriteFailed { .. }
            | Self::CursorCheckpointCorrupt { .. }
            | Self::CursorCheckpointRegionMismatch { .. }
            | Self::InvariantViolation { .. } => Ok(()),
            #[cfg(feature = "dangerous-test-hooks")]
            Self::FaultInjected(_) => Ok(()),
        }
    }

    /// Shared `Display` body for coordinate/causation/commit-metadata validation
    /// refusals. Grouped off the main `Display::fmt` match so the validation
    /// surface can grow without pushing `fmt` past its complexity ratchet.
    fn fmt_coordinate_violation(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCoordinate { index, reason } => match index {
                Some(i) => write!(f, "invalid coordinate at batch item {i}: {reason}"),
                None => write!(f, "invalid coordinate: {reason}"),
            },
            Self::ReservedKind { index, kind } => match index {
                Some(i) => write!(
                    f,
                    "reserved kind 0x{kind:04X} at batch item {i} cannot be appended through the public surface"
                ),
                None => write!(
                    f,
                    "reserved kind 0x{kind:04X} cannot be appended through the public surface"
                ),
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
            // Reached only from the coordinate-violation arm group of `Display::fmt`.
            // The remaining variants are listed explicitly (not wildcarded, per the
            // workspace `wildcard_enum_match_arm` lint) so a newly-added variant
            // trips a compile error here; they render in `Display::fmt` (or
            // `fmt_future_version`) and never actually reach this helper.
            Self::Io(_)
            | Self::StoreLocked { .. }
            | Self::Coordinate(_)
            | Self::CheckpointId(_)
            | Self::Serialization(_)
            | Self::CrcMismatch { .. }
            | Self::CorruptSegment { .. }
            | Self::NotFound(_)
            | Self::SequenceMismatch { .. }
            | Self::WriterCrashed
            | Self::WaitTimeout { .. }
            | Self::CacheFailed(_)
            | Self::SequenceGateViolation { .. }
            | Self::Configuration(_)
            | Self::PlatformProfileInvalid { .. }
            | Self::PlatformProfileMismatch { .. }
            | Self::PlatformAdmissionFailed { .. }
            | Self::EventPayloadRegistry(_)
            | Self::IdempotencyRequired
            | Self::VisibilityFenceActive
            | Self::VisibilityFenceNotActive
            | Self::VisibilityFenceCancelled
            | Self::BatchFailed { .. }
            | Self::BatchSyncFailed { .. }
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
            | Self::HiddenRangesCorrupt { .. }
            | Self::BatchItemTooLarge { .. }
            | Self::EntityClockOverflow { .. }
            | Self::InvalidClock { .. }
            | Self::CheckpointWriteFailed { .. }
            | Self::CursorCheckpointCorrupt { .. }
            | Self::CursorCheckpointRegionMismatch { .. }
            | Self::InvariantViolation { .. } => Ok(()),
            #[cfg(feature = "dangerous-test-hooks")]
            Self::FaultInjected(_) => Ok(()),
        }
    }
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
            Self::CheckpointId(e) => write!(f, "checkpoint id error: {e}"),
            Self::Serialization(e) => write!(f, "serialization error: {e}"),
            Self::CrcMismatch { segment_id, offset } => {
                write!(f, "CRC mismatch in segment {segment_id} at offset {offset}")
            }
            Self::CorruptSegment { segment_id, detail } => {
                write!(f, "corrupt segment {segment_id}: {detail}")
            }
            Self::NotFound(id) => write!(f, "event {:032x} not found", id.as_u128()),
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
            Self::PlatformProfileInvalid { path, kind } => write!(
                f,
                "platform profile at {} is invalid: {kind}",
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
            // All four on-disk future-version refusals share one Display shape;
            // delegate to a helper so this match stays within its complexity
            // ratchet rather than growing an arm per format.
            Self::IdempotencyFutureVersion { .. }
            | Self::MmapFutureVersion { .. }
            | Self::CheckpointFutureVersion { .. }
            | Self::HiddenRangesFutureVersion { .. }
            | Self::ForkEvidenceFutureVersion { .. }
            | Self::ImportProvenanceFutureVersion { .. } => self.fmt_future_version(f),
            Self::IdempotencyOverflowFailClosed { len, max_keys } => write!(
                f,
                "durable idempotency store at soft cap ({len}/{max_keys}); new keyed append \
                 refused (overflow policy fail-closed)"
            ),
            Self::InvalidPayloadVersion { kind } => write!(
                f,
                "typed append for kind 0x{kind:04X} declared PAYLOAD_VERSION 0; version 0 is the \
                 reserved legacy/untyped sentinel and is never a valid declared payload version"
            ),
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
            Self::InternerExhausted { count } => write!(
                f,
                "string interner has {count} entries, exhausting the u32 interner id domain"
            ),
            Self::DataDirMalformed { path } => {
                write!(
                    f,
                    "data directory contains unexpected file: {}",
                    path.display()
                )
            }
            Self::AncestryCorrupt { cycle_at } => {
                write!(f, "ancestry walk detected a cycle at event {:032x}", cycle_at.as_u128())
            }
            Self::RangeMalformed { start, end } => {
                write!(
                    f,
                    "malformed range: start={start} end={end} (start must be < end)"
                )
            }
            // Coordinate / causation / commit-metadata validation refusals share
            // one cohesive render group; delegate so this match stays within its
            // complexity ratchet rather than growing an arm per refusal.
            Self::InvalidCoordinate { .. }
            | Self::ReservedKind { .. }
            | Self::InvalidCausation { .. }
            | Self::InvalidCommitMetadata { .. }
            | Self::CoordinateNulByte
            | Self::CoordinatePathTraversal
            | Self::CoordinateControlChar => self.fmt_coordinate_violation(f),
            Self::HiddenRangesCorrupt { path, kind } => write!(
                f,
                "hidden-ranges metadata at {} is corrupt: {kind}",
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
            Self::InvariantViolation { kind } => write!(f, "invariant violation: {kind}"),
        }
    }
}
