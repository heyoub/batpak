#![deny(missing_docs)]
//! Sync-first runtime layer for batpak-family operation kits.
//!
//! This crate is intentionally small at birth. It establishes the runtime
//! layer boundary over batpak core without importing operation dialect,
//! network, protocol-profile, or rendering semantics.
//!
//! # Quick start
//!
//! Build a [`Core`], register one operation, invoke it synchronously.
//!
//! ```rust
//! use syncbat::{Core, EffectClass, Handler, HandlerError, HandlerResult, OperationDescriptor};
//!
//! /// Echoes the input bytes back unchanged.
//! struct EchoHandler;
//!
//! impl Handler for EchoHandler {
//!     fn handle(
//!         &mut self,
//!         input: &[u8],
//!         _cx: &mut syncbat::Ctx<'_>,
//!     ) -> HandlerResult {
//!         Ok(input.to_vec())
//!     }
//! }
//!
//! const ECHO: OperationDescriptor = OperationDescriptor::new(
//!     "echo",
//!     EffectClass::Inspect,
//!     "echo.request",
//!     "echo.ack",
//!     "receipt.echo.v1",
//! );
//!
//! let mut builder = Core::builder();
//! builder.register(ECHO.clone(), EchoHandler).expect("register");
//! let mut core = builder.build().expect("build");
//!
//! let result = core.invoke("echo", b"hi".to_vec()).expect("invoke");
//! assert_eq!(result.output(), b"hi");
//! ```
//!
//! # Operation names
//!
//! Operation names follow a stable grammar checked once by
//! [`OperationName::new`]. Downstream code never re-parses; passing a
//! plain `&str` to [`Core::invoke`] is fine, but pre-validating into
//! [`OperationName`] documents intent and removes per-call grammar
//! checks.
//!
//! ```rust
//! use syncbat::OperationName;
//!
//! let name = OperationName::new("bank.commit").expect("grammar");
//! assert_eq!(name.as_str(), "bank.commit");
//!
//! assert!(OperationName::new("bad..name").is_err());
//! ```

#[doc(hidden)]
pub extern crate self as syncbat;

pub mod admission;
pub mod builder;
pub mod core;
pub mod error;
pub mod handler;
pub mod module;
pub mod operation;
pub mod operation_name;
pub mod receipt;
pub mod register;
pub mod register_store;
pub mod store_sink;

pub use admission::{AdmissionDecision, AdmissionGuard};
pub use batpak_macros::operation;
pub use builder::CoreBuilder;
pub use core::{Checkout, CheckoutFrame, CheckoutResult, Core, Ctx};
pub use error::{BuildError, ReceiptSinkHandlerCause, RuntimeError};
pub use handler::{Handler, HandlerError, HandlerResult};
pub use module::Module;
pub use operation::{
    DescriptorValidationError, EffectClass, OperationDescriptor, OperationInput, OperationOutput,
    OperationRegisterItem, MAX_DESCRIPTOR_REF_BYTES, MAX_OPERATION_NAME_BYTES,
};
pub use operation_name::{OperationName, OperationNameError};
pub use receipt::{
    BatpakReceiptFields, ReceiptEnvelope, ReceiptExtensionDrawer, ReceiptHash, ReceiptHashPolicy,
    ReceiptHasher, ReceiptMetadata, ReceiptOutcome, ReceiptSink, ReceiptSinkError, RecordedReceipt,
    SYNCBAT_RECEIPT_EVENT_KIND,
};
pub use register::{CacheRegister, Register, RegisterValidationError};
pub use register_store::{
    rebuild_register_from_store, RegisterOperationActionV1, RegisterOperationRowV1,
    StoreRegisterCatalog, StoreRegisterCatalogError, SYNCBAT_REGISTER_EVENT_KIND,
};
pub use store_sink::{StoreReceiptSink, StoreReceiptSinkError};

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
