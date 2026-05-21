#![warn(missing_docs)]
//! Reference host library for the batpak family.
//!
//! This crate is the library half of the `hbat` reference host. It is not a
//! product daemon and not part of the BatPAK publish family. It exists so
//! TypeScript and other non-Rust clients have a concrete process to talk to
//! over `NETBAT/1`, and so the workspace can prove cross-language wire parity
//! against a known-good Rust handler.
//!
//! ## Phase 0 scope
//!
//! - One operation: [`heartbeat::HEARTBEAT_DESCRIPTOR`] (`system.heartbeat`),
//!   echoing a nonce and stamping a server clock value.
//! - Two `EventPayload` types in [`heartbeat`]:
//!   [`heartbeat::SystemHeartbeatRequest`] and
//!   [`heartbeat::SystemHeartbeatAck`].
//! - A hand-built descriptor registry in [`manifest::descriptors`] consumed
//!   by both `xtask export-ts-manifest` and the `hbat` binary. The
//!   `#[derive(EventPayload)]` macro uses `inventory`, which is collected
//!   per binary; `xtask` cannot see registrations linked into the `hbat`
//!   binary. The explicit registry function is Phase 0 plumbing and will
//!   be replaced once a cross-binary descriptor-discovery story exists.

/// `bank.commit` and `event.get` payload types + descriptors.
pub mod bank;
/// Runtime handlers for [`bank`] operations. Capture `Arc<Store>`; pulled
/// into the binary via [`crate::main`] and not part of the library's
/// pure-data surface that `xtask` depends on.
pub mod handlers;
/// Heartbeat payload types and the registered operation descriptor.
pub mod heartbeat;
/// Manifest snapshot consumed by `xtask export-ts-manifest` and by the
/// `hbat` binary.
pub mod manifest;

pub use bank::{
    BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, BANK_COMMIT_DESCRIPTOR,
    BANK_COMMIT_INPUT_SCHEMA_REF, BANK_COMMIT_OPERATION_NAME, BANK_COMMIT_OUTPUT_SCHEMA_REF,
    BANK_COMMIT_RECEIPT_KIND, EVENT_GET_DESCRIPTOR, EVENT_GET_INPUT_SCHEMA_REF,
    EVENT_GET_OPERATION_NAME, EVENT_GET_OUTPUT_SCHEMA_REF, EVENT_GET_RECEIPT_KIND,
};
pub use handlers::{BankCommitHandler, EventGetHandler};
pub use heartbeat::{
    handle_heartbeat, HeartbeatHandler, SystemHeartbeatAck, SystemHeartbeatRequest,
    HEARTBEAT_DESCRIPTOR, HEARTBEAT_OPERATION_NAME,
};
pub use manifest::{
    descriptors, EventDescriptor, FieldDescriptor, ManifestBuildError, ManifestErrorFixture,
    ManifestSnapshot, OperationDescriptorRecord, FIXTURE_NONCE, FIXTURE_SERVER_TS_MS,
    MANIFEST_VERSION,
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
