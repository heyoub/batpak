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
//! // `build()` fails closed without a receipt sink so receipts are never
//! // silently dropped; this example records none, so opt out explicitly.
//! builder.without_receipts();
//! let mut core = builder.build().expect("build");
//!
//! let result = core.invoke("echo", b"hi".to_vec()).expect("invoke");
//! assert_eq!(result.output(), b"hi");
//! ```
//!
//! # Runtime safety defaults
//!
//! The runtime fails closed where silence would lose evidence:
//!
//! - [`CoreBuilder::build`] refuses to build without a receipt sink, because a
//!   sinkless core silently drops every runtime receipt. State the sinkless
//!   intent explicitly with [`CoreBuilder::without_receipts`] (used above), or
//!   configure one with [`CoreBuilder::receipt_sink`].
//! - Receipt input/output hashing defaults to [`ReceiptHashPolicy::Blake3`], so
//!   every recorded receipt binds to the exact bytes that produced it;
//!   [`ReceiptHashPolicy::Deferred`] is the explicit opt-out for a layer that
//!   hashes the bytes itself.
//! - Capability tokens are enforced at checkout: a dispatched operation that
//!   declares a required token the [`Core`] was not granted fails closed. Grant
//!   tokens with [`CoreBuilder::grant_capability`] /
//!   [`CoreBuilder::grant_capabilities`]. (Effect-axis tokens auto-declared by
//!   the effect builders are ambient and need no explicit grant.)
//! - Every effect axis is an enforced boundary: an operation reaches an effect
//!   only through the matching `Ctx` capability handle, which records it into the
//!   observed row in the same step, and `checkout` fails closed when the observed
//!   row is not a subset of the declared row. `use_host_control` is a declared +
//!   subset-checked target axis like the read/append/query axes (observed host
//!   controls must be a subset of the declared targets), and `emit_receipt`
//!   stamps its opaque payload as observed evidence into the invocation's single
//!   banked receipt only after the backend mediates the emission.
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
pub mod effect;
pub mod effect_backend;
pub mod error;
pub mod handler;
pub mod module;
pub mod operation;
pub mod operation_name;
pub mod operation_status;
pub mod operation_status_sink;
pub mod receipt;
pub mod register;
pub mod register_store;
pub mod store_effect;
pub mod store_sink;
pub mod subscription_runtime;

pub use admission::{AdmissionDecision, AdmissionGuard};
pub use batpak_macros::operation;
pub use builder::CoreBuilder;
pub use core::{Checkout, CheckoutFrame, CheckoutResult, Core, CoreFactory, Ctx};
pub use effect::{
    append_target, EffectIdentity, EffectIdentityError, EventAppendHandle, EventReadHandle,
    HostControlHandle, ObservedEffectViolation, OperationEffectRow, ProjectionReadHandle,
    ReceiptEmitHandle,
};
pub use effect_backend::{EffectBackend, EffectError};
pub use error::{BuildError, ReceiptSinkHandlerCause, RuntimeError};
pub use handler::{Handler, HandlerError, HandlerResult};
pub use module::Module;
pub use operation::{
    DescriptorValidationError, EffectClass, OperationDescriptor, OperationInput, OperationOutput,
    OperationRegisterItem, MAX_DESCRIPTOR_REF_BYTES, MAX_OPERATION_NAME_BYTES,
};
pub use operation_name::{OperationName, OperationNameError};
pub use operation_status::{
    OperationStatusFactV1, OperationStatusLifecycle, OperationStatusView,
    SYNCBAT_OPERATION_STATUS_EVENT_KIND,
};
pub use operation_status_sink::{
    operation_status_entity, OperationStatusSink, OperationStatusSinkError,
    StoreOperationStatusSink,
};
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
pub use store_effect::StoreEffectBackend;
pub use store_sink::{StoreReceiptSink, StoreReceiptSinkError};
pub use subscription_runtime::{
    cursor_invalid_error, cursor_mismatch_error, unknown_subscription_error,
    CompositeSubscriptionRuntime, EntityStreamCursorV1, EntityStreamEnvelopeV1,
    EntityStreamRouteBinding, EntityStreamSession, EventStreamCursorV1, EventStreamEnvelopeV1,
    EventStreamSession, EventSubscriptionRuntime, OperationStatusRouteBinding,
    OperationStatusStreamCursorV1, OperationStatusStreamEnvelopeV1, OperationStatusStreamSession,
    ProjectionStreamCursorV1, ProjectionStreamEnvelopeV1, ProjectionStreamSession,
    ReceiptStreamCursorV1, ReceiptStreamEnvelopeV1, ReceiptStreamRouteBinding,
    ReceiptStreamSession, RuntimeCursor, SessionControl, SessionDelivery, SessionEnd, SessionError,
    SessionEventDelivery, SessionPoll, SessionWatermarkDelivery, SubscriptionId,
    SubscriptionRegistry, SubscriptionRoute, SubscriptionRuntimeConfig, SubscriptionRuntimeError,
    SubscriptionSession, SubscriptionSessionFactory, SubscriptionStore, TypedProjectionProjector,
    CURSOR_V1_LEN, ENTITY_STREAM_CURSOR_V1_LEN, OPERATION_STATUS_CURSOR_V1_LEN,
    PROJECTION_CURSOR_V1_LEN, RECEIPT_STREAM_CURSOR_V1_LEN, SOURCE_KIND_ENTITY_STREAM,
    SOURCE_KIND_EVENT_CATEGORY, SOURCE_KIND_OPERATION_STATUS, SOURCE_KIND_PROJECTION,
    SOURCE_KIND_RECEIPT_STREAM,
};

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
