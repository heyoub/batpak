//! Event delivery surfaces.
//!
//! Two canals ship under this module:
//!
//! * [`cursor`](crate::store::delivery::cursor) — pull-based, ordered delivery with optional durable
//!   checkpoints (per FREEZE-5). Without a checkpoint id the guarantee is
//!   process-local; with one it becomes durable at-least-once across
//!   restarts.
//! * [`subscription`](crate::store::delivery::subscription) — push-based, lossy fanout with a region filter
//!   applied at the writer push point (F8). Use
//!   [`Subscription::filtered_receiver`](crate::store::delivery::subscription::Subscription::filtered_receiver) for async /
//!   deadline-driven consumers; the raw
//!   [`Subscription::receiver`](crate::store::delivery::subscription::Subscription::receiver) accessor is retained under
//!   `#[doc(hidden)]` for back-compat and has identical semantics
//!   post-F8.

/// Shared delivery selector for typed reactor runners.
pub mod canal;
/// Pull-based cursor for ordered delivery with optional durable checkpoints.
pub mod cursor;
/// Delivery observation witnesses for composing at-least-once with
/// consumer-side idempotency.
pub mod observation;
/// Push-based (lossy) event subscription via broadcast channel.
pub mod subscription;
