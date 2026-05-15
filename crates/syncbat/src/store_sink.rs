//! Batpak-backed receipt sink for syncbat receipt envelopes.

use std::sync::Arc;
use std::{error::Error, fmt};

use batpak::coordinate::Coordinate;
use batpak::store::{AppendOptions, Open, Store, StoreError};

use crate::receipt::{
    ReceiptEnvelope, ReceiptExtensionDrawer, ReceiptSink, ReceiptSinkError, RecordedReceipt,
};
use crate::{receipt_extension_key, receipt_extension_value};

const EXT_DESCRIPTOR: &str = "descriptor";
const EXT_OUTCOME: &str = "outcome";
const EXT_INPUT_HASH: &str = "input";
const EXT_OUTPUT_HASH: &str = "output";
const EXT_SIGNED_DRAWER: &str = "signed";

/// Error returned by [`StoreReceiptSink`].
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreReceiptSinkError {
    /// A syncbat receipt-extension key could not be constructed.
    ExtensionKey(batpak::store::ExtensionKeyError),
    /// Batpak failed to append the typed receipt event.
    Store(StoreError),
}

impl fmt::Display for StoreReceiptSinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExtensionKey(error) => {
                write!(f, "invalid syncbat receipt extension key: {error:?}")
            }
            Self::Store(error) => write!(f, "batpak receipt append failed: {error}"),
        }
    }
}

impl Error for StoreReceiptSinkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ExtensionKey(_) => None,
            Self::Store(error) => Some(error),
        }
    }
}

impl From<batpak::store::ExtensionKeyError> for StoreReceiptSinkError {
    fn from(error: batpak::store::ExtensionKeyError) -> Self {
        Self::ExtensionKey(error)
    }
}

impl From<StoreError> for StoreReceiptSinkError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Batpak-backed syncbat receipt sink.
///
/// The sink writes [`ReceiptEnvelope`] as a typed batpak event using
/// [`Store::append_typed_with_options`]. It stores signed receipt metadata in
/// `syncbat.*` batpak receipt extensions so the batpak append receipt signs the
/// descriptor, outcome, optional input/output hashes, and signed extension
/// drawer alongside the receipt event body.
pub struct StoreReceiptSink {
    store: Arc<Store<Open>>,
    coordinate: Coordinate,
    base_options: AppendOptions,
}

impl StoreReceiptSink {
    /// Construct a sink for one receipt coordinate.
    #[must_use]
    pub fn new(store: Arc<Store<Open>>, coordinate: Coordinate) -> Self {
        Self {
            store,
            coordinate,
            base_options: AppendOptions::new(),
        }
    }

    /// Set append options used as the baseline for every receipt write.
    ///
    /// Syncbat receipt extensions are added after these options, so any
    /// conflicting `syncbat.*` keys in `options` are overwritten by the
    /// envelope being recorded.
    #[must_use]
    pub fn with_options(mut self, options: AppendOptions) -> Self {
        self.base_options = options;
        self
    }

    fn options_for(
        &self,
        envelope: &ReceiptEnvelope,
    ) -> Result<AppendOptions, StoreReceiptSinkError> {
        let mut options = self
            .base_options
            .clone()
            .with_receipt_extension(
                receipt_extension_key(EXT_DESCRIPTOR)?,
                receipt_extension_value(envelope.descriptor_name.as_bytes().to_vec()),
            )
            .with_receipt_extension(
                receipt_extension_key(EXT_OUTCOME)?,
                receipt_extension_value(envelope.outcome.class().as_bytes().to_vec()),
            );

        if let Some(hash) = envelope.input_hash {
            options = options.with_receipt_extension(
                receipt_extension_key(EXT_INPUT_HASH)?,
                receipt_extension_value(hash),
            );
        }

        if let Some(hash) = envelope.output_hash {
            options = options.with_receipt_extension(
                receipt_extension_key(EXT_OUTPUT_HASH)?,
                receipt_extension_value(hash),
            );
        }

        if !envelope.signed_extensions.is_empty() {
            options = options.with_receipt_extension(
                receipt_extension_key(EXT_SIGNED_DRAWER)?,
                receipt_extension_value(encode_extension_drawer(&envelope.signed_extensions)),
            );
        }

        Ok(options)
    }

    /// Record a receipt envelope and return the typed store-backed sink error.
    ///
    /// # Errors
    /// Returns [`StoreReceiptSinkError`] when syncbat receipt extension
    /// construction fails or batpak rejects the append.
    pub fn record_typed(
        &self,
        envelope: &ReceiptEnvelope,
    ) -> Result<RecordedReceipt, StoreReceiptSinkError> {
        let options = self.options_for(envelope)?;
        let append_receipt =
            self.store
                .append_typed_with_options(&self.coordinate, envelope, options)?;
        Ok(RecordedReceipt::new(envelope.clone()).with_batpak_receipt(append_receipt))
    }
}

impl ReceiptSink for StoreReceiptSink {
    fn record_receipt(
        &self,
        envelope: &ReceiptEnvelope,
    ) -> Result<RecordedReceipt, ReceiptSinkError> {
        self.record_typed(envelope).map_err(ReceiptSinkError::from)
    }
}

impl From<StoreReceiptSinkError> for ReceiptSinkError {
    fn from(error: StoreReceiptSinkError) -> Self {
        Self::new(error.to_string())
    }
}

fn encode_extension_drawer(drawer: &ReceiptExtensionDrawer) -> Vec<u8> {
    let mut out = Vec::from("syncbat.drawer.v1\0".as_bytes());
    for (key, value) in drawer {
        extend_len_prefixed(&mut out, key.as_bytes());
        extend_len_prefixed(&mut out, value);
    }
    out
}

fn extend_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}
