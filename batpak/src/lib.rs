#![allow(unexpected_cfgs)]
// cast_possible_truncation and cast_sign_loss are enforced via [lints.clippy] in Cargo.toml.
// Each intentional cast has an inline #[allow] with a justification comment.
//! batpak: Event Sourcing Runtime with DAG Causation Tracking.
//!
//! Batpak provides a complete event sourcing platform with:
//! - **Event Sourcing**: Immutable event log with hash chain integrity
//! - **DAG Causation**: Tracks causation relationships between events
//! - **Gate Evaluation**: Pluggable policy enforcement before event commitment
//! - **Persistent Storage**: Segment-based append-only store with fast querying
//!
//! The core pattern: acquire a [`Proposal`](crate::pipeline::Proposal), evaluate it through
//! [`Gate`](crate::guard::Gate) instances, then [`commit`](crate::pipeline::Pipeline::commit) to
//! the [`Store`](crate::store::Store).
//!
//! ```no_run
//! use batpak::prelude::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Store::open_default()?;
//! let gates: GateSet<()> = GateSet::new();
//! let pipeline = Pipeline::new(gates);
//!
//! let coord = Coordinate::new("entity:1", "scope:test")?;
//! let kind = EventKind::custom(0xF, 1);
//! let payload = serde_json::json!({"hello": "world"});
//!
//! let proposal = Proposal::new(payload.clone());
//! let receipt = pipeline.evaluate(&(), proposal)?;
//! let committed = pipeline.commit(receipt, |p| -> Result<_, StoreError> {
//!     let r = store.append(&coord, kind, &p)?;
//!     Ok(Committed { payload: p, event_id: r.event_id, sequence: r.sequence, hash: [0u8; 32] })
//! })?;
//! # Ok(())
//! # }
//! ```
//!
//! **Reading order:**
//! 1. [`coordinate`]: Identify entities and scopes
//! 2. [`event`]: Structure your events
//! 3. [`guard`]: Build policy gates
//! 4. [`pipeline`]: Propose and commit
//! 5. [`store`]: Persist and query

pub mod coordinate;
pub mod event;
pub mod guard;
pub mod id;
pub mod outcome;
pub mod pipeline;
pub mod prelude;
pub mod store;
pub mod typestate;
/// Module declarations in DEPENDENCY ORDER:
/// wire → coordinate → outcome → event → guard → pipeline → store → typestate → id → prelude
/// [SPEC:src/lib.rs — Module declarations in DEPENDENCY ORDER]
pub mod wire; // serde helpers — no deps, must come first

/// compile_error guards for impossible configurations:
#[allow(unexpected_cfgs)]
#[cfg(feature = "async-store")]
compile_error!(
    "INVARIANT 2: batpak does not have an async Store API. \
     Async callers use spawn_blocking() or flume recv_async(). \
     See: src/store/subscription.rs for the async pattern."
);

#[allow(unexpected_cfgs)]
#[cfg(feature = "sha256")]
compile_error!(
    "INVARIANT 5: blake3 is the only hash. No HashAlgorithm enum. \
     One function: compute_hash(bytes) -> [u8; 32], behind feature = blake3."
);
