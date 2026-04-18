//! Event delivery surfaces.
//!
//! Two canals ship under this module:
//!
//! * [`cursor`] — pull-based, ordered delivery with optional durable
//!   checkpoints (per FREEZE-5). Without a checkpoint id the guarantee is
//!   process-local; with one it becomes durable at-least-once across
//!   restarts.
//! * [`subscription`] — push-based, lossy fanout with a region filter
//!   applied at the writer push point (F8). Use
//!   [`subscription::Subscription::filtered_receiver`] for async /
//!   deadline-driven consumers; the raw
//!   [`subscription::Subscription::receiver`] accessor is retained under
//!   `#[doc(hidden)]` for back-compat and has identical semantics
//!   post-F8.

/// Pull-based cursor for ordered delivery with optional durable checkpoints.
pub mod cursor;
/// Push-based (lossy) event subscription via broadcast channel.
pub mod subscription;
