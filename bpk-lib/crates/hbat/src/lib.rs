#![warn(missing_docs)]
//! Reference host library for the batpak family.
//!
//! This crate is the library half of the `hbat` reference host. It is not a
//! product daemon and not part of the BatPAK publish family. It exists so
//! TypeScript and other non-Rust clients have a concrete process to talk to
//! over `NETBAT/1`, and so the workspace can prove cross-language wire parity
//! against a known-good Rust handler.
//!
//! ## Reference terminal scope
//!
//! - [`heartbeat::HEARTBEAT_DESCRIPTOR`] (`system.heartbeat`) echoes a
//!   nonce and stamps a server clock value.
//! - [`bank::BANK_COMMIT_DESCRIPTOR`] (`bank.commit`) writes substrate
//!   events.
//! - [`bank::EVENT_GET_DESCRIPTOR`] (`event.get`) point-reads one known
//!   event id.
//! - [`bank::EVENT_QUERY_DESCRIPTOR`] (`event.query`) pages bounded,
//!   domain-neutral event summaries for external replay.
//! - [`receipt::RECEIPT_VERIFY_DESCRIPTOR`] (`receipt.verify`) checks
//!   ack-shaped append receipt fields.
//! - [`walk::EVENT_WALK_DESCRIPTOR`] (`event.walk`) walks bounded
//!   hash-chain ancestry.
//! - [`manifest::descriptors`] consumes inventory registrations for both
//!   event payloads and operations. Payload descriptors submit via
//!   [`hbat_event_descriptor!`]; operation descriptors use
//!   [`manifest::OperationDescriptorRegistration`] + `inventory::submit!`.
//!   Fixtures remain hand-authored on [`EventPayloadFixture`]; the macro
//!   wires field rows and fixture closures only.

/// `bank.commit` and `event.get` payload types + descriptors.
pub mod bank;
/// Declarative macro for payload [`manifest::EventDescriptorRegistration`]
/// inventory submissions.
#[macro_use]
mod descriptor;
/// Runtime handlers for [`bank`] operations. Capture `Arc<Store>`; pulled
/// into the binary via [`crate::main`] and not part of the library's
/// pure-data surface that `xtask` depends on.
pub mod handlers;
/// Heartbeat payload types and the registered operation descriptor.
pub mod heartbeat;
/// Manifest snapshot consumed by `xtask export-ts-manifest` and by the
/// `hbat` binary.
pub mod manifest;
/// `receipt.verify` payload types + descriptor.
pub mod receipt;
/// `event.walk` payload types + descriptor.
pub mod walk;

pub use bank::{
    BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, EventQueryAck,
    EventQueryRequest, EventSummary, BANK_COMMIT_DESCRIPTOR, BANK_COMMIT_INPUT_SCHEMA_REF,
    BANK_COMMIT_OPERATION_NAME, BANK_COMMIT_OUTPUT_SCHEMA_REF, BANK_COMMIT_RECEIPT_KIND,
    EVENT_GET_DESCRIPTOR, EVENT_GET_INPUT_SCHEMA_REF, EVENT_GET_OPERATION_NAME,
    EVENT_GET_OUTPUT_SCHEMA_REF, EVENT_GET_RECEIPT_KIND, EVENT_QUERY_DESCRIPTOR,
    EVENT_QUERY_INPUT_SCHEMA_REF, EVENT_QUERY_MAX_LIMIT, EVENT_QUERY_OPERATION_NAME,
    EVENT_QUERY_OUTPUT_SCHEMA_REF, EVENT_QUERY_RECEIPT_KIND, EVENT_QUERY_SUMMARY_SCHEMA_REF,
};
pub use handlers::{
    BankCommitHandler, EventGetHandler, EventQueryHandler, EventWalkHandler, ReceiptVerifyHandler,
};
pub use heartbeat::{
    handle_heartbeat, HeartbeatHandler, SystemHeartbeatAck, SystemHeartbeatRequest,
    HEARTBEAT_DESCRIPTOR, HEARTBEAT_OPERATION_NAME,
};
pub use manifest::{
    descriptors, EventDescriptor, FieldDescriptor, ManifestBuildError, ManifestErrorFixture,
    ManifestSnapshot, OperationDescriptorRecord, FIXTURE_NONCE, FIXTURE_SERVER_TS_MS,
    MANIFEST_VERSION,
};
pub use receipt::{
    ReceiptVerifyAck, ReceiptVerifyRequest, RECEIPT_VERIFY_DESCRIPTOR,
    RECEIPT_VERIFY_INPUT_SCHEMA_REF, RECEIPT_VERIFY_OPERATION_NAME,
    RECEIPT_VERIFY_OUTPUT_SCHEMA_REF, RECEIPT_VERIFY_RECEIPT_KIND,
};
pub use walk::{
    EventWalkAck, EventWalkRequest, EVENT_WALK_DESCRIPTOR, EVENT_WALK_INPUT_SCHEMA_REF,
    EVENT_WALK_MAX_LIMIT, EVENT_WALK_OPERATION_NAME, EVENT_WALK_OUTPUT_SCHEMA_REF,
    EVENT_WALK_RECEIPT_KIND,
};

/// Fixture-value supplier for an [`batpak::event::EventPayload`].
///
/// The `#[derive(EventPayload)]` macro emits no field metadata and no
/// fixture value; this trait is the Phase 0 shim that lets the manifest
/// export path serialize a known-good runtime value through both
/// canonical MessagePack (for `goldenPayloadHex`) and `serde_json` (for
/// the JSON `fixtureValue`) starting from the **same** Rust object.
///
/// Implementations must be deterministic — calling
/// [`fixture_value`](Self::fixture_value) twice must return equal values
/// — and must produce a value whose serde JSON shape is a Phase 0
/// subset: strings, safe integers (<= 2^53 - 1), booleans, null, arrays,
/// and maps with string keys.
pub trait EventPayloadFixture: batpak::event::EventPayload {
    /// Return the deterministic fixture value for this payload type.
    fn fixture_value() -> Self;
}
