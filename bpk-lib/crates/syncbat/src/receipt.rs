//! Generic receipt envelope types for syncbat operation runs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::{error::Error, fmt};

use batpak::event::{EventKind, EventPayload};
use batpak::store::{EncodedBytes, ExtensionKey};
use serde::{Deserialize, Serialize};

use crate::operation::OperationDescriptor;

/// Batpak custom event kind used for syncbat receipt events.
///
/// Category `0xC` is a caller-defined category and is not used by the core
/// examples, which currently reserve their example payloads under category
/// `0x1`. Type id `0x5B7` is scoped to syncbat's generic receipt envelope.
pub const SYNCBAT_RECEIPT_EVENT_KIND: EventKind = EventKind::custom(0xC, 0x5B7);

/// Stable hash bytes carried by a syncbat receipt.
pub type ReceiptHash = [u8; 32];

/// Caller-owned raw byte hasher for runtime receipt input/output hashes.
pub trait ReceiptHasher {
    /// Return a stable hash for already-encoded operation bytes.
    fn hash(&self, bytes: &[u8]) -> ReceiptHash;
}

impl<F> ReceiptHasher for F
where
    F: Fn(&[u8]) -> ReceiptHash,
{
    fn hash(&self, bytes: &[u8]) -> ReceiptHash {
        self(bytes)
    }
}

/// Runtime policy for populating receipt input/output hashes.
#[derive(Clone, Default)]
#[non_exhaustive]
pub enum ReceiptHashPolicy {
    /// Defer hash population to a later layer; runtime receipts carry no byte
    /// hashes.
    #[default]
    Deferred,
    /// Hash raw handler input/output bytes with a deterministic caller-owned hasher.
    RawBytes(Arc<dyn ReceiptHasher>),
}

impl ReceiptHashPolicy {
    /// Build a raw-byte hash policy from a caller-owned deterministic hasher.
    #[must_use]
    pub fn raw_bytes(hasher: impl ReceiptHasher + 'static) -> Self {
        Self::RawBytes(Arc::new(hasher))
    }

    /// Return the configured raw-byte hash for `bytes`, when enabled.
    #[must_use]
    pub fn hash(&self, bytes: &[u8]) -> Option<ReceiptHash> {
        match self {
            Self::Deferred => None,
            Self::RawBytes(hasher) => Some(hasher.hash(bytes)),
        }
    }
}

/// Opaque extension drawer attached to a syncbat receipt.
///
/// Keys are profile-owned strings. Values are already-encoded bytes so this
/// layer does not impose a schema on higher-level operation kits.
pub type ReceiptExtensionDrawer = BTreeMap<String, Vec<u8>>;

/// Opaque receipt metadata a handler or admission guard attaches to the current
/// invocation via [`crate::Ctx`]. The runtime drains it into the recorded
/// [`ReceiptEnvelope`]'s drawers — `signed` into [`ReceiptEnvelope::signed_extensions`]
/// (copied into batpak receipt extensions by the store sink) and `local` into
/// [`ReceiptEnvelope::local_extensions`] (envelope body only). It exists so a
/// handler can stamp correlation/attempt metadata onto its receipt WITHOUT
/// owning the receipt envelope, preserving the runtime's sole ownership of
/// receipt persistence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReceiptMetadata {
    /// Entries destined for the envelope's signed drawer.
    pub signed: ReceiptExtensionDrawer,
    /// Entries kept only in the envelope's local drawer.
    pub local: ReceiptExtensionDrawer,
}

impl ReceiptMetadata {
    /// Return true when no metadata has been attached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.signed.is_empty() && self.local.is_empty()
    }
}

/// Runtime result recorded for a completed operation attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ReceiptOutcome {
    /// The operation completed and produced any expected output.
    Completed,
    /// The operation ran but failed before producing a usable result.
    Failed {
        /// Stable failure class.
        code: String,
        /// Human-readable failure detail.
        message: String,
    },
    /// The direct sink or policy layer declined to execute or publish the
    /// operation result.
    ///
    /// `Core` checkout dispatch emits `Completed` or `Failed`; `Denied` is
    /// reserved for direct receipt sinks, admission checks, and network guards
    /// that reject a call before handler execution.
    Denied {
        /// Stable denial class.
        code: String,
        /// Human-readable denial detail.
        message: String,
    },
}

impl ReceiptOutcome {
    /// Construct a failed outcome.
    #[must_use]
    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Failed {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Construct a denied outcome.
    #[must_use]
    pub fn denied(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Denied {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Return the stable outcome class used in receipt extensions.
    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed { .. } => "failed",
            Self::Denied { .. } => "denied",
        }
    }
}

/// Batpak append receipt fields associated with a persisted syncbat receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct BatpakReceiptFields {
    /// Unique ID of the persisted receipt event.
    pub event_id: batpak::id::EventId,
    /// Global sequence assigned by batpak at commit time.
    pub sequence: u64,
    /// Blake3 hash of the committed receipt payload bytes.
    pub content_hash: ReceiptHash,
    /// Signing-key identity reported by batpak.
    pub key_id: ReceiptHash,
    /// Detached receipt signature when store signing is enabled.
    pub signature: Option<[u8; 64]>,
    /// Opaque receipt extensions committed with the batpak append receipt.
    pub extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

impl From<batpak::store::AppendReceipt> for BatpakReceiptFields {
    fn from(receipt: batpak::store::AppendReceipt) -> Self {
        Self {
            event_id: receipt.event_id,
            sequence: receipt.global_sequence,
            content_hash: receipt.content_hash,
            key_id: receipt.key_id,
            signature: receipt.signature,
            extensions: receipt.extensions,
        }
    }
}

/// Generic syncbat receipt envelope persisted as an event payload.
///
/// The signed drawer is copied into batpak receipt extensions by
/// [`crate::store_sink::StoreReceiptSink`]. The local drawer remains only in
/// the syncbat envelope body for callers that need local, profile-owned
/// diagnostics without adding batpak receipt-extension keys.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ReceiptEnvelope {
    /// Stable operation descriptor name.
    pub descriptor_name: String,
    /// Stable receipt kind from the operation descriptor.
    pub receipt_kind: String,
    /// Optional hash of the operation input bytes.
    pub input_hash: Option<ReceiptHash>,
    /// Optional hash of the operation output bytes.
    pub output_hash: Option<ReceiptHash>,
    /// Runtime result for this operation attempt.
    pub outcome: ReceiptOutcome,
    /// Opaque extension drawer intended for batpak receipt extensions.
    pub signed_extensions: ReceiptExtensionDrawer,
    /// Opaque extension drawer kept in the syncbat envelope body.
    pub local_extensions: ReceiptExtensionDrawer,
}

impl ReceiptEnvelope {
    /// Construct an envelope from an operation descriptor.
    #[must_use]
    pub fn new(descriptor: &OperationDescriptor, outcome: ReceiptOutcome) -> Self {
        Self::from_descriptor(descriptor.name(), descriptor.receipt_kind(), outcome)
    }

    /// Construct an envelope from stable descriptor receipt fields.
    #[must_use]
    pub fn from_descriptor(
        descriptor_name: impl Into<String>,
        receipt_kind: impl Into<String>,
        outcome: ReceiptOutcome,
    ) -> Self {
        Self {
            descriptor_name: descriptor_name.into(),
            receipt_kind: receipt_kind.into(),
            input_hash: None,
            output_hash: None,
            outcome,
            signed_extensions: BTreeMap::new(),
            local_extensions: BTreeMap::new(),
        }
    }

    /// Attach an input hash.
    #[must_use]
    pub fn with_input_hash(mut self, hash: ReceiptHash) -> Self {
        self.input_hash = Some(hash);
        self
    }

    /// Attach an output hash.
    #[must_use]
    pub fn with_output_hash(mut self, hash: ReceiptHash) -> Self {
        self.output_hash = Some(hash);
        self
    }

    /// Insert one signed extension entry.
    #[must_use]
    pub fn with_signed_extension(
        mut self,
        key: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        self.signed_extensions.insert(key.into(), value.into());
        self
    }

    /// Insert one local extension entry.
    #[must_use]
    pub fn with_local_extension(
        mut self,
        key: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        self.local_extensions.insert(key.into(), value.into());
        self
    }
}

impl EventPayload for ReceiptEnvelope {
    const KIND: EventKind = SYNCBAT_RECEIPT_EVENT_KIND;
}

/// Receipt envelope plus sink-owned persistence metadata.
///
/// This type is returned by sinks after recording. The persisted event payload
/// remains [`ReceiptEnvelope`], so append-result metadata cannot accidentally
/// become part of the event body.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RecordedReceipt {
    /// Envelope body that was recorded.
    pub envelope: ReceiptEnvelope,
    /// Batpak receipt fields when the envelope was recorded through batpak.
    pub batpak_receipt: Option<BatpakReceiptFields>,
}

impl RecordedReceipt {
    /// Construct recorded receipt metadata for a receipt envelope.
    #[must_use]
    pub fn new(envelope: ReceiptEnvelope) -> Self {
        Self {
            envelope,
            batpak_receipt: None,
        }
    }

    /// Attach batpak receipt fields.
    #[must_use]
    pub fn with_batpak_receipt(mut self, receipt: impl Into<BatpakReceiptFields>) -> Self {
        self.batpak_receipt = Some(receipt.into());
        self
    }
}

/// Error returned when a receipt sink cannot record an envelope.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReceiptSinkError {
    message: String,
}

impl ReceiptSinkError {
    /// Construct a receipt-sink error from a displayable message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Return the sink error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ReceiptSinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ReceiptSinkError {}

/// Sink for completed syncbat receipt envelopes.
pub trait ReceiptSink {
    /// Persist a receipt envelope and return sink-owned persistence metadata.
    ///
    /// # Errors
    /// Returns [`ReceiptSinkError`] when the sink rejects or fails the write.
    fn record_receipt(
        &self,
        envelope: &ReceiptEnvelope,
    ) -> Result<RecordedReceipt, ReceiptSinkError>;
}

#[cfg(test)]
mod receipt_sink_error_tests {
    use super::ReceiptSinkError;

    #[test]
    fn message_returns_the_constructed_text() {
        // Pins the accessor: returning a constant ("xyzzy") would mask the real
        // sink failure reason from callers and receipts.
        let err = ReceiptSinkError::new("sink offline");
        assert_eq!(err.message(), "sink offline");
        assert_eq!(err.to_string(), "sink offline");
    }
}
