use crate::coordinate::Coordinate;
use crate::event::{EventKind, EventPayload, StoredEvent};
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

impl CausationRef {
    /// Returns true when causation should fall back to the append options field.
    pub(crate) const fn uses_options_fallback(self) -> bool {
        matches!(self, Self::None)
    }

    /// Resolve the effective causation ID for a batch item.
    pub(crate) fn resolve(
        self,
        fallback: Option<u128>,
        item_index: usize,
        prior_event_id: impl FnOnce(usize) -> u128,
    ) -> Result<Option<u128>, StoreError> {
        match self {
            Self::None => Ok(fallback),
            // 0 is the wire sentinel for "no causation" — treat it as None.
            Self::Absolute(0) => Ok(None),
            Self::Absolute(id) => Ok(Some(id)),
            Self::PriorItem(prior_idx) => {
                if prior_idx >= item_index {
                    return Err(StoreError::Configuration(
                        "PriorItem causation must reference earlier batch item".into(),
                    ));
                }
                Ok(Some(prior_event_id(prior_idx)))
            }
        }
    }
}

/// Single item in a batch append operation.
#[derive(Clone, Debug)]
pub struct BatchAppendItem {
    /// Target coordinate (entity/scope) for this event.
    coord: Coordinate,
    /// Event kind classification.
    kind: EventKind,
    /// Pre-serialized payload bytes (MessagePack).
    payload_bytes: Vec<u8>,
    /// Append options (idempotency, correlation, etc.).
    options: AppendOptions,
    /// Causation reference for intra-batch linking.
    causation: CausationRef,
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

    /// Create a new batch item with a typed payload — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if payload serialization fails.
    pub fn typed<T: EventPayload>(
        coord: Coordinate,
        payload: &T,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<Self, StoreError> {
        Self::new(coord, T::KIND, payload, options, causation)
    }

    /// Low-level escape hatch for callers that already own canonical MessagePack bytes.
    ///
    /// Unlike [`BatchAppendItem::new`], this does not perform payload serialization.
    pub fn from_msgpack_bytes(
        coord: Coordinate,
        kind: EventKind,
        payload_bytes: Vec<u8>,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Self {
        Self {
            coord,
            kind,
            payload_bytes,
            options,
            causation,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (Coordinate, EventKind, Vec<u8>, AppendOptions, CausationRef) {
        (
            self.coord,
            self.kind,
            self.payload_bytes,
            self.options,
            self.causation,
        )
    }

    /// Borrow the append options for this item.
    pub fn options(&self) -> AppendOptions {
        self.options
    }

    /// Borrow the causation reference for this item.
    pub fn causation(&self) -> CausationRef {
        self.causation
    }

    /// Borrow the coordinate for this item.
    pub fn coord(&self) -> &Coordinate {
        &self.coord
    }

    /// Return the event kind for this item.
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// Borrow the encoded payload bytes for this item.
    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload_bytes
    }

    pub(crate) fn with_options(mut self, options: AppendOptions) -> Self {
        self.options = options;
        self
    }
}

/// AppendReceipt: proof an event was persisted.
#[derive(Clone, Copy, Debug)]
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
#[derive(Clone, Copy, Debug)]
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
    /// No-option baseline: all guards disabled, no hints, no custom IDs.
    /// `Default::default()` delegates here — one source of truth for the zero value.
    pub const fn new() -> Self {
        Self {
            expected_sequence: None,
            idempotency_key: None,
            correlation_id: None,
            causation_id: None,
            position_hint: None,
            flags: 0,
        }
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

    /// Set custom causation ID. Passing `0` is a no-op — 0 is the wire sentinel
    /// for "no causation" and is treated identically to not calling this method.
    pub fn with_causation(mut self, id: u128) -> Self {
        if id != 0 {
            self.causation_id = Some(id);
        }
        self
    }

    /// Set the DAG lane/depth hint while leaving HLC and sequence to the writer.
    pub fn with_position_hint(mut self, hint: AppendPositionHint) -> Self {
        self.position_hint = Some(hint);
        self
    }
}

impl Default for AppendOptions {
    fn default() -> Self {
        Self::new()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_causation_zero_is_noop() {
        let opts = AppendOptions::default().with_causation(0);
        assert_eq!(
            opts.causation_id, None,
            "0 is the wire sentinel — must not become Some(0)"
        );
    }

    #[test]
    fn with_causation_nonzero_is_recorded() {
        let opts = AppendOptions::default().with_causation(42);
        assert_eq!(opts.causation_id, Some(42));
    }

    #[test]
    fn causation_ref_absolute_zero_resolves_to_none() {
        let result = CausationRef::Absolute(0).resolve(None, 0, |_| unreachable!());
        assert_eq!(
            result.expect("resolve must not error"),
            None,
            "Absolute(0) must resolve to None"
        );
    }

    #[test]
    fn causation_ref_absolute_nonzero_resolves_to_some() {
        let result = CausationRef::Absolute(99).resolve(None, 0, |_| unreachable!());
        assert_eq!(result.expect("resolve must not error"), Some(99));
    }
}
