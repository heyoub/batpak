//! Shared event fixtures for the split `raw_projection_mode*` integration
//! harnesses.
//!
//! Included via `#[path = "support/raw_projection_mode.rs"]` by every
//! `raw_projection_mode*` test binary. The harness was split out of a single
//! 923-line file (over the 500-line cap) along its replay-lane seam:
//!   * `raw_projection_mode` — raw-vs-value parity + watch lanes
//!   * `raw_projection_mode_flow_matrix` — flow-matrix + maybe-stale cache lanes
//!   * `raw_projection_mode_incremental` — incremental-apply replay lanes
//!
//! Only the two fixtures consumed by *every* split binary live here: the
//! [`CounterDelta`] payload that all replay lanes append/fold, and the [`KIND`]
//! event kind every lane filters on. Everything family-specific (the concrete
//! `EventSourced` counter states, the seeded-store builders, the flow-matrix
//! macro, `NOISE_KIND`) stays inlined in its owning binary so a binary crate
//! never inherits a `dead_code` surface (`pub` does not suppress `dead_code` in
//! a binary crate). Both fixtures here are referenced by all three binaries, so
//! this module carries no escape hatch.

use serde::{Deserialize, Serialize};

pub use batpak::event::EventKind;

/// Relevant event kind every replay lane folds into its counter state.
pub const KIND: EventKind = EventKind::custom(0xF, 0x31);

/// The msgpack/json payload appended by every replay lane and folded by every
/// `EventSourced` counter the split binaries define.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterDelta {
    pub amount: i64,
    pub label: String,
}
