#![warn(missing_docs)]
//! Sync-first runtime layer for batpak-family operation kits.
//!
//! This crate is intentionally small at birth. It establishes the runtime
//! layer boundary over batpak core without importing operation dialect,
//! network, protocol-profile, or rendering semantics.

#[doc(hidden)]
pub extern crate self as syncbat;

pub mod builder;
pub mod core;
pub mod error;
pub mod handler;
pub mod module;
pub mod operation;
pub mod receipt;
pub mod register;
pub mod store_sink;

pub use builder::CoreBuilder;
pub use core::{Checkout, CheckoutFrame, CheckoutResult, Core, Cx, InvokeResult};
pub use error::{BuildError, RuntimeError};
pub use handler::{Handler, HandlerError, HandlerResult};
pub use module::Module;
pub use operation::{
    DescriptorValidationError, EffectClass, OperationDescriptor, OperationInput, OperationOutput,
    MAX_DESCRIPTOR_REF_BYTES, MAX_OPERATION_NAME_BYTES,
};
pub use receipt::{
    BatpakReceiptFields, ReceiptEnvelope, ReceiptExtensionDrawer, ReceiptHash, ReceiptHashPolicy,
    ReceiptHasher, ReceiptOutcome, ReceiptSink, ReceiptSinkError, RecordedReceipt,
    SYNCBAT_RECEIPT_EVENT_KIND,
};
pub use register::{CacheRegister, Register, RegisterValidationError};
pub use store_sink::{StoreReceiptSink, StoreReceiptSinkError};
pub use syncbat_macros::operation;

/// Receipt-extension namespace owned by the syncbat runtime layer.
pub const SYNCBAT_EXTENSION_NAMESPACE: &str = "syncbat";

/// Marker type for syncbat-owned receipt extensions.
pub enum SyncbatReceiptNamespace {}

impl batpak::store::ReceiptExtensionNamespace for SyncbatReceiptNamespace {
    const PREFIX: &'static str = SYNCBAT_EXTENSION_NAMESPACE;
}

/// Validated syncbat-owned receipt-extension key.
pub type SyncbatReceiptExtensionKey = batpak::store::ReceiptExtensionKey<SyncbatReceiptNamespace>;

/// Encoded syncbat-owned receipt-extension value.
pub type SyncbatReceiptExtensionValue =
    batpak::store::ReceiptExtensionValue<SyncbatReceiptNamespace>;

/// Construct a syncbat receipt-extension key.
///
/// # Errors
/// Returns [`batpak::store::ExtensionKeyError`] when the field would produce
/// an invalid batpak receipt-extension key.
pub fn receipt_extension_key(
    field: impl AsRef<str>,
) -> Result<SyncbatReceiptExtensionKey, batpak::store::ExtensionKeyError> {
    SyncbatReceiptExtensionKey::new(field)
}

/// Wrap already-encoded bytes as a syncbat receipt-extension value.
#[must_use]
pub fn receipt_extension_value(
    bytes: impl Into<batpak::store::EncodedBytes>,
) -> SyncbatReceiptExtensionValue {
    SyncbatReceiptExtensionValue::new(bytes)
}
