#![deny(missing_docs)]
//! Generic deterministic module host for the batpak family.
//!
//! `hostbat` is the thin, content-identified shell that composes a
//! [`syncbat::Core`] from one or more **modules** and adds the things a raw
//! `syncbat` runtime has no concept of: content identity (`H_module` per module
//! and a host-composition [`HostFingerprint`]), modules that bundle operations,
//! an admission guard, lifecycle hooks, and supervised jobs as *one mountable
//! unit*, a generic [`Supervisor`] over the reviewed [`batpak::store::Spawn`]
//! seam, and a deterministic startup/shutdown hook schedule.
//!
//! It does **not** reimplement dispatch, receipts, or admission — those stay in
//! `syncbat`. `hostbat` lowers mounted modules into one [`syncbat::CoreBuilder`]
//! and delegates invocation to the composed `Core`. Meaning lives in the modules
//! a caller mounts (e.g. bvisor's boundary module); the host stays generic.
//!
//! # Shape
//!
//! ```text
//! HostModule::builder(id, version)        // declare ops + guard + hooks + jobs
//!     .operation(descriptor, handler)?
//!     .build()?                           // → sealed, content-identified module
//!
//! HostBuilder::new()
//!     .mount(module)?                     // cross-module collision + hash checks
//!     .build()?                           // → Host (one syncbat Core + supervisor)
//! ```
//!
//! The manifest is **derived from exactly the registered parts** — it is never
//! authored beside the implementation, so the declaration and the behavior it
//! attests cannot drift.

#[cfg(test)]
mod composition_tests;

#[cfg(test)]
mod composition_interface_tests;

#[cfg(test)]
mod event_payload_binding_tests;

#[cfg(test)]
mod append_validation_tests;

pub mod builder;
pub mod composition;
pub mod descriptor;
pub mod error;
pub mod event_payload_binding;
pub mod host;
pub mod identity;
pub mod interface;
pub mod manifest;
pub mod module;
pub mod schema;
pub mod subscription;
pub mod supervisor;
pub mod validating_effect_backend;

pub use builder::HostBuilder;
pub use composition::{CompositionSchema, HostCompositionManifest};
pub use descriptor::{GuardDescriptor, HookDescriptor, HookPhase, JobDescriptor};
pub use error::{HookFailure, HostError, HostRuntimeError, SchemaCollision};
pub use event_payload_binding::EventPayloadBinding;
pub use host::Host;
pub use identity::{Digest, HostFingerprint, InterfaceFingerprint, ModuleDigest};
pub use manifest::HostModuleManifest;
pub use module::{HostModule, HostModuleBuilder, JobBody, LifecycleHook};
pub use schema::{
    CanonicalEncoding, DiagnosticRustType, GoldenVector, SchemaDescriptor, SchemaId,
    SchemaRegistry, SchemaRole, SchemaVersion,
};
pub use subscription::{
    BackpressurePolicy, EventCategory, OperationStatusSelector, ProjectionId, ReceiptFilter,
    SubscriptionDelivery, SubscriptionDescriptor, SubscriptionId, SubscriptionSource,
    SUBSCRIPTION_WIRE_REQUIRES,
};
pub use supervisor::Supervisor;
pub use validating_effect_backend::ValidatingEffectBackend;
