use crate::coordinate::Coordinate;
use crate::event::{EventKind, EventPayload, StoredEvent};
use crate::id::{CausationId, CorrelationId, EventId, IdempotencyKey};
use crate::store::gate::DurabilityGate;
use crate::store::index::DiskPos;
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::marker::PhantomData;

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

    /// Construct an `Absolute` variant from a typed
    /// [`crate::id::CausationId`]. Prefer this at public call-sites over
    /// the raw-u128 variant so the ID class cannot be mixed up.
    pub fn absolute_typed(id: crate::id::CausationId) -> Self {
        use crate::id::EntityIdType;
        Self::Absolute(id.as_u128())
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
                    return Err(StoreError::InvalidCausation {
                        prior_idx,
                        item_index,
                        reason: "PriorItem causation must reference earlier batch item".into(),
                    });
                }
                Ok(Some(prior_event_id(prior_idx)))
            }
        }
    }
}

/// Single item in a batch append operation.
///
/// If the embedded [`AppendOptions`] includes a [`DurabilityGate`], batch
/// append ignores that per-item gate. Use `Store::append_batch_with_options`
/// to apply one batch-level gate to the last event in the batch.
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
        let payload_bytes = crate::encoding::to_bytes(payload)
            .map_err(|e| StoreError::Serialization(Box::new(e)))?;
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
        self.options.clone()
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

    /// Attach one receipt extension to this batch item.
    #[must_use]
    pub fn with_extension(mut self, key: ExtensionKey, bytes: impl Into<EncodedBytes>) -> Self {
        self.options = self.options.with_extension(key, bytes);
        self
    }

    /// Attach one typed receipt extension to this batch item.
    #[must_use]
    pub fn with_receipt_extension<P: ReceiptExtensionNamespace>(
        mut self,
        key: ReceiptExtensionKey<P>,
        value: ReceiptExtensionValue<P>,
    ) -> Self {
        self.options = self.options.with_receipt_extension(key, value);
        self
    }

    /// Replace this batch item's receipt extension map.
    #[must_use]
    pub fn with_extensions(mut self, extensions: BTreeMap<ExtensionKey, EncodedBytes>) -> Self {
        self.options = self.options.with_extensions(extensions);
        self
    }
}

/// AppendReceipt: witness that an event was persisted.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AppendReceipt {
    /// Unique ID of the persisted event.
    pub event_id: EventId,
    /// Global sequence number assigned at commit time.
    pub sequence: u64,
    /// Location of the event frame on disk.
    pub(crate) disk_pos: DiskPos,
    /// Blake3 hash of the committed payload bytes.
    pub content_hash: [u8; 32],
    /// Signing-key identity. All zeros when receipt signing is disabled.
    pub key_id: [u8; 32],
    /// Detached Ed25519 signature over the receipt fields.
    pub signature: Option<[u8; 64]>,
    /// Typed side-data attached to the receipt envelope.
    pub extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

/// Receipt returned when a denial trace is persisted as `SYSTEM_DENIAL`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DenialReceipt {
    /// Unique ID of the persisted denial event.
    pub event_id: EventId,
    /// Global sequence number assigned at commit time.
    pub sequence: u64,
    /// Location of the denial frame on disk.
    pub(crate) disk_pos: DiskPos,
    /// Blake3 hash of the denial payload bytes.
    pub content_hash: [u8; 32],
    /// Signing-key identity. All zeros when receipt signing is disabled.
    pub key_id: [u8; 32],
    /// Detached Ed25519 signature over the receipt fields.
    pub signature: Option<[u8; 64]>,
    /// Typed side-data attached to the denial receipt envelope.
    pub extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

/// Encoded extension payload bytes.
pub type EncodedBytes = Vec<u8>;

/// Schema version for the signing-downgrade receipt extension body.
pub const SIGNING_DOWNGRADE_SCHEMA_VERSION: u16 = 1;

/// Receipt extension body emitted when a configured signing registry cannot
/// build the signature cover.
///
/// This evidence is unsigned by definition: it is attached only after cover
/// construction has failed, so `signature` remains `None` and `key_id` remains
/// all zeroes. An unsigned receipt without this extension remains the canonical
/// "no signing keys configured" shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SigningDowngradeBody {
    /// Signing-downgrade extension schema version.
    pub schema_version: u16,
    /// Typed downgrade reason.
    pub reason: SigningDowngradeReason,
}

/// Typed reason receipt signing downgraded to an unsigned receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SigningDowngradeReason {
    /// Signature-cover construction failed before signing could run.
    CoverBuildFailed {
        /// Human-readable cover-build failure.
        encoding_error: String,
    },
}

impl AppendReceipt {
    /// Location of the committed event frame on disk.
    #[must_use]
    pub const fn disk_pos(&self) -> DiskPos {
        self.disk_pos
    }

    /// Decode the substrate signing-downgrade receipt extension when present.
    ///
    /// `None` means either the receipt was signed, signing was never configured,
    /// or the extension bytes are malformed. Malformed extension bytes still
    /// remain covered by normal receipt-extension preservation and signature
    /// verification paths.
    #[must_use]
    pub fn signing_downgrade(&self) -> Option<SigningDowngradeBody> {
        self.extensions
            .get(&signing_downgrade_extension_key())
            .and_then(|bytes| crate::encoding::from_bytes(bytes).ok())
    }
}

impl DenialReceipt {
    /// Location of the committed denial frame on disk.
    #[must_use]
    pub const fn disk_pos(&self) -> DiskPos {
        self.disk_pos
    }
}

pub(crate) fn encoded_receipt_extensions_len(
    extensions: &BTreeMap<ExtensionKey, EncodedBytes>,
) -> Result<usize, StoreError> {
    if extensions.is_empty() {
        return Ok(0);
    }
    crate::canonical::to_bytes(extensions)
        .map(|bytes| bytes.len())
        .map_err(|error| StoreError::Serialization(Box::new(error)))
}

pub(crate) fn checked_append_bytes(
    payload_len: usize,
    extensions: &BTreeMap<ExtensionKey, EncodedBytes>,
) -> Result<usize, StoreError> {
    let extension_len = encoded_receipt_extensions_len(extensions)?;
    payload_len
        .checked_add(extension_len)
        .ok_or_else(|| StoreError::ser_msg("append bytes overflow usize"))
}

/// Receipt extension key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExtensionKey(String);

/// Marker trait for typed receipt-extension namespaces.
pub trait ReceiptExtensionNamespace {
    /// Namespace prefix owned by this extension family.
    const PREFIX: &'static str;
}

/// Substrate-owned receipt-extension namespace for signing evidence.
pub struct SigningExtensionNamespace;

impl ReceiptExtensionNamespace for SigningExtensionNamespace {
    const PREFIX: &'static str = "batpak.signing";
}

/// Extension key branded by a receipt-extension namespace marker.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReceiptExtensionKey<P: ReceiptExtensionNamespace> {
    raw: ExtensionKey,
    _namespace: PhantomData<P>,
}

impl<P: ReceiptExtensionNamespace> Clone for ReceiptExtensionKey<P> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            _namespace: PhantomData,
        }
    }
}

impl<P: ReceiptExtensionNamespace> ReceiptExtensionKey<P> {
    /// Construct `<P::PREFIX>.<field>` as a validated typed extension key.
    ///
    /// # Errors
    /// Returns [`ExtensionKeyError`] when the composed key fails normal
    /// extension-key validation.
    pub fn new(field: impl AsRef<str>) -> Result<Self, ExtensionKeyError> {
        let raw = ExtensionKey::new(format!("{}.{}", P::PREFIX, field.as_ref()))?;
        Ok(Self {
            raw,
            _namespace: PhantomData,
        })
    }

    /// Borrow the raw validated extension key.
    #[must_use]
    pub fn as_key(&self) -> &ExtensionKey {
        &self.raw
    }
}

/// Extension value branded by a receipt-extension namespace marker.
#[derive(Debug, PartialEq, Eq)]
pub struct ReceiptExtensionValue<P: ReceiptExtensionNamespace> {
    bytes: EncodedBytes,
    _namespace: PhantomData<P>,
}

impl<P: ReceiptExtensionNamespace> Clone for ReceiptExtensionValue<P> {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            _namespace: PhantomData,
        }
    }
}

impl<P: ReceiptExtensionNamespace> ReceiptExtensionValue<P> {
    /// Construct a typed extension value from already encoded bytes.
    pub fn new(bytes: impl Into<EncodedBytes>) -> Self {
        Self {
            bytes: bytes.into(),
            _namespace: PhantomData,
        }
    }
}

/// Validation failures for [`ExtensionKey`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtensionKeyError {
    /// The key was empty.
    Empty,
    /// The key must be ASCII.
    NonAscii,
    /// The key exceeded the maximum supported length.
    TooLong,
    /// The key must contain exactly one namespace separator (`.`) with
    /// non-empty prefix and field segments.
    InvalidNamespaceFormat,
    /// `batpak.*` is reserved for substrate-owned keys.
    ReservedNamespace,
}

impl std::fmt::Display for ExtensionKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "extension key is empty"),
            Self::NonAscii => write!(f, "extension key must be ASCII"),
            Self::TooLong => write!(f, "extension key exceeds maximum length"),
            Self::InvalidNamespaceFormat => {
                write!(
                    f,
                    "extension key must have exactly one non-empty namespace separator"
                )
            }
            Self::ReservedNamespace => {
                write!(f, "extension key uses the reserved batpak namespace")
            }
        }
    }
}

impl std::error::Error for ExtensionKeyError {}

impl ExtensionKey {
    const MAX_LEN: usize = 256;

    /// Construct a validated extension key in `<prefix>.<key>` form.
    ///
    /// # Errors
    /// Returns [`ExtensionKeyError`] when the key is empty, non-ASCII, lacks a
    /// single namespace separator, or uses the reserved `batpak.*` namespace.
    pub fn new(key: impl Into<String>) -> Result<Self, ExtensionKeyError> {
        let key = key.into();
        if key.is_empty() {
            return Err(ExtensionKeyError::Empty);
        }
        if !key.is_ascii() {
            return Err(ExtensionKeyError::NonAscii);
        }
        if key.len() > Self::MAX_LEN {
            return Err(ExtensionKeyError::TooLong);
        }
        let Some((prefix, field)) = key.split_once('.') else {
            return Err(ExtensionKeyError::InvalidNamespaceFormat);
        };
        if prefix.is_empty() || field.is_empty() || field.contains('.') {
            return Err(ExtensionKeyError::InvalidNamespaceFormat);
        }
        if prefix == "batpak" {
            return Err(ExtensionKeyError::ReservedNamespace);
        }
        Ok(Self(key))
    }

    /// Construct a substrate-owned reserved extension key.
    #[must_use]
    pub(crate) fn reserved(key: impl Into<String>) -> Self {
        let key = key.into();
        debug_assert!(key.starts_with("batpak."));
        debug_assert!(key.is_ascii());
        debug_assert!(key.len() <= Self::MAX_LEN);
        debug_assert!(key.split('.').all(|segment| !segment.is_empty()));
        Self(key)
    }

    /// Borrow the validated extension key as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Optional caller-supplied branch hint for the committed event position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppendPositionHint {
    /// Parallel branch index within the DAG.
    pub lane: u32,
    /// Branch depth within the DAG, or the parent depth when
    /// [`branch_root`](Self::branch_root) is `true`.
    pub depth: u32,
    /// Whether this append starts a new branch at `depth + 1`.
    pub branch_root: bool,
}

impl AppendPositionHint {
    /// Create a new DAG lane/depth hint for append operations.
    pub const fn new(lane: u32, depth: u32) -> Self {
        Self {
            lane,
            depth,
            branch_root: false,
        }
    }

    /// Create a hint for the first event on a forked lane.
    pub const fn branch_root(lane: u32, parent_depth: u32) -> Self {
        Self {
            lane,
            depth: parent_depth,
            branch_root: true,
        }
    }
}

/// AppendOptions: CAS, idempotency, custom correlation/causation.
#[derive(Clone, Debug)]
pub struct AppendOptions {
    /// Expected entity sequence for compare-and-swap; `None` skips the CAS check.
    pub expected_sequence: Option<u32>,
    /// Idempotency key; duplicate appends with the same key return the original receipt.
    pub idempotency_key: Option<IdempotencyKey>,
    /// Custom correlation ID; defaults to the generated event ID if `None`.
    pub correlation_id: Option<CorrelationId>,
    /// ID of the event that caused this append; `None` for root-cause events.
    pub causation_id: Option<CausationId>,
    /// Optional caller-supplied branch hint; writer still owns HLC wall/counter and sequence.
    pub position_hint: Option<AppendPositionHint>,
    /// EventHeader flags (FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL, FLAG_REPLAY).
    /// Default: 0 (no flags); uses the same bit layout as `EventHeader::flags`.
    pub flags: u8,
    /// Optional append-time wait for a frontier watermark.
    pub gate: Option<DurabilityGate>,
    /// Caller-supplied receipt extensions. The store treats these as opaque
    /// bytes, signs them as part of the receipt cover, and leaves semantic
    /// validation to profile-aware code layered above batpak.
    pub extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

impl AppendOptions {
    /// No-option baseline: all guards disabled, no hints, no custom IDs.
    /// `Default::default()` delegates here — one source of truth for the zero value.
    pub fn new() -> Self {
        Self {
            expected_sequence: None,
            idempotency_key: None,
            correlation_id: None,
            causation_id: None,
            position_hint: None,
            flags: 0,
            gate: None,
            extensions: BTreeMap::new(),
        }
    }

    /// Set expected sequence for compare-and-swap (CAS) check.
    pub fn with_cas(mut self, seq: u32) -> Self {
        self.expected_sequence = Some(seq);
        self
    }

    /// Set idempotency key. Duplicate appends with the same key return the original receipt.
    ///
    /// Accepts the typed [`IdempotencyKey`] newtype; pass `IdempotencyKey::from(raw_u128)`
    /// if a wire-decode path holds the value as a raw integer.
    pub fn with_idempotency(mut self, key: IdempotencyKey) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    /// Set EventHeader flags (bitwise OR of FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL, FLAG_REPLAY).
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    /// Set custom correlation ID. The typed newtype makes it structurally
    /// impossible to pass (e.g.) an [`crate::id::EventId`] where a
    /// correlation id was intended.
    pub fn with_correlation(mut self, id: CorrelationId) -> Self {
        self.correlation_id = Some(id);
        self
    }

    /// Set custom causation ID. Passing a [`CausationId`] wrapping `0` is a
    /// no-op — 0 is the wire sentinel for "no causation" and is treated
    /// identically to not calling this method.
    pub fn with_causation(mut self, id: CausationId) -> Self {
        use crate::id::EntityIdType;
        if id.as_u128() != 0 {
            self.causation_id = Some(id);
        }
        self
    }

    /// Set the DAG lane/depth hint while leaving HLC and sequence to the writer.
    pub fn with_position_hint(mut self, hint: AppendPositionHint) -> Self {
        self.position_hint = Some(hint);
        self
    }

    /// Set an append-time durability gate.
    pub fn with_gate(mut self, gate: DurabilityGate) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Attach one opaque receipt extension payload.
    #[must_use]
    pub fn with_extension(mut self, key: ExtensionKey, bytes: impl Into<EncodedBytes>) -> Self {
        self.extensions.insert(key, bytes.into());
        self
    }

    /// Attach one typed opaque receipt extension payload.
    #[must_use]
    pub fn with_receipt_extension<P: ReceiptExtensionNamespace>(
        mut self,
        key: ReceiptExtensionKey<P>,
        value: ReceiptExtensionValue<P>,
    ) -> Self {
        self.extensions.insert(key.raw, value.bytes);
        self
    }

    /// Replace the full opaque receipt extension map.
    #[must_use]
    pub fn with_extensions(mut self, extensions: BTreeMap<ExtensionKey, EncodedBytes>) -> Self {
        self.extensions = extensions;
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

pub(crate) fn signing_downgrade_extension_key() -> ExtensionKey {
    ExtensionKey::reserved("batpak.signing.downgrade")
}

impl SigningDowngradeBody {
    pub(crate) fn cover_build_failed(error: impl Into<String>) -> Self {
        Self {
            schema_version: SIGNING_DOWNGRADE_SCHEMA_VERSION,
            reason: SigningDowngradeReason::CoverBuildFailed {
                encoding_error: error.into(),
            },
        }
    }

    pub(crate) fn encode_extension(&self) -> Result<EncodedBytes, StoreError> {
        crate::encoding::to_bytes(self).map_err(|error| StoreError::Serialization(Box::new(error)))
    }
}

#[cfg(test)]
#[path = "append_tests.rs"]
mod tests;
