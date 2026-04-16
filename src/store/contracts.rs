use crate::coordinate::Coordinate;
use crate::event::{EventKind, StoredEvent};
use crate::store::{DiskPos, StoreError};
use serde::Serialize;

/// Reference to causation for batch items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CausationRef {
    /// No causation.
    #[default]
    None,
    /// Absolute event ID.
    Absolute(u128),
    /// Reference to previous item in same batch by index.
    PriorItem(usize),
}

/// Single item in a batch append operation.
#[derive(Clone, Debug)]
pub struct BatchAppendItem {
    /// Target coordinate (entity/scope) for this event.
    pub coord: Coordinate,
    /// Event kind classification.
    pub kind: EventKind,
    /// Pre-serialized payload bytes (MessagePack).
    pub payload_bytes: Vec<u8>,
    /// Append options (idempotency, correlation, etc.).
    pub options: AppendOptions,
    /// Causation reference for intra-batch linking.
    pub causation: CausationRef,
}

impl BatchAppendItem {
    /// Create a new batch item with serialized payload.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if payload serialization fails.
    pub fn new(
        coord: Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<Self, StoreError> {
        let payload_bytes =
            rmp_serde::to_vec_named(payload).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        Ok(Self {
            coord,
            kind,
            payload_bytes,
            options,
            causation,
        })
    }
}

/// AppendReceipt: proof an event was persisted.
#[derive(Clone, Debug)]
pub struct AppendReceipt {
    /// Unique ID of the persisted event.
    pub event_id: u128,
    /// Global sequence number assigned at commit time.
    pub sequence: u64,
    /// Location of the event frame on disk.
    pub disk_pos: DiskPos,
}

/// Optional caller-supplied branch hint for the committed event position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppendPositionHint {
    /// Parallel branch index within the DAG.
    pub lane: u32,
    /// Branch depth within the DAG.
    pub depth: u32,
}

impl AppendPositionHint {
    /// Create a new DAG lane/depth hint for append operations.
    pub const fn new(lane: u32, depth: u32) -> Self {
        Self { lane, depth }
    }
}

/// AppendOptions: CAS, idempotency, custom correlation/causation.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppendOptions {
    /// Expected entity sequence for compare-and-swap; `None` skips the CAS check.
    pub expected_sequence: Option<u32>,
    /// Idempotency key; duplicate appends with the same key return the original receipt.
    pub idempotency_key: Option<u128>,
    /// Custom correlation ID; defaults to the generated event ID if `None`.
    pub correlation_id: Option<u128>,
    /// ID of the event that caused this append; `None` for root-cause events.
    pub causation_id: Option<u128>,
    /// Optional caller-supplied branch hint; writer still owns HLC wall/counter and sequence.
    pub position_hint: Option<AppendPositionHint>,
    /// EventHeader flags (FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL, FLAG_REPLAY).
    /// Default: 0 (no flags); uses the same bit layout as `EventHeader::flags`.
    pub flags: u8,
}

impl AppendOptions {
    /// Create new AppendOptions with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set expected sequence for compare-and-swap (CAS) check.
    pub fn with_cas(mut self, seq: u32) -> Self {
        self.expected_sequence = Some(seq);
        self
    }

    /// Set idempotency key. Duplicate appends with the same key return the original receipt.
    pub fn with_idempotency(mut self, key: u128) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    /// Set EventHeader flags (bitwise OR of FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL, FLAG_REPLAY).
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    /// Set custom correlation ID.
    pub fn with_correlation(mut self, id: u128) -> Self {
        self.correlation_id = Some(id);
        self
    }

    /// Set custom causation ID.
    pub fn with_causation(mut self, id: u128) -> Self {
        self.causation_id = Some(id);
        self
    }

    /// Set the DAG lane/depth hint while leaving HLC and sequence to the writer.
    pub fn with_position_hint(mut self, hint: AppendPositionHint) -> Self {
        self.position_hint = Some(hint);
        self
    }
}

/// Predicate for filtering events during compaction. Returns true to keep, false to drop.
pub type RetentionPredicate = Box<dyn Fn(&StoredEvent<serde_json::Value>) -> bool + Send>;

/// CompactionStrategy: how compact() handles events during segment merging.
#[non_exhaustive]
pub enum CompactionStrategy {
    /// Merge sealed segments into one. No events removed.
    Merge,
    /// Merge + drop events failing the retention predicate.
    /// Dropped events are permanently lost.
    Retention(RetentionPredicate),
    /// Merge + write tombstone markers for dropped events.
    /// Downstream consumers can detect deletions.
    Tombstone(RetentionPredicate),
}

/// CompactionConfig: controls compact() behavior.
pub struct CompactionConfig {
    /// Strategy for handling events during compaction.
    pub strategy: CompactionStrategy,
    /// Minimum number of sealed segments before compaction runs.
    /// Below this threshold, compact() returns early.
    pub min_segments: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            strategy: CompactionStrategy::Merge,
            min_segments: 2,
        }
    }
}

/// Validate payload length fits in u32. Prevents silent truncation
/// when serialized payloads exceed 4GB (unlikely but possible with
/// pathological inputs or corrupted serialization).
pub(crate) fn checked_payload_len(payload_bytes: &[u8]) -> Result<u32, StoreError> {
    u32::try_from(payload_bytes.len())
        .map_err(|_| StoreError::ser_msg("payload size exceeds u32::MAX (4GB limit)"))
}
