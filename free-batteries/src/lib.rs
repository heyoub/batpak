#![allow(unexpected_cfgs)]
//! free-batteries: Event Sourcing Runtime with DAG Causation Tracking.
//!
//! Free-batteries provides a complete event sourcing platform with:
//! - **Event Sourcing**: Immutable event log with hash chain integrity
//! - **DAG Causation**: Tracks causation relationships between events
//! - **Gate Evaluation**: Pluggable policy enforcement before event commitment
//! - **Persistent Storage**: Segment-based append-only store with fast querying
//!
//! The core pattern: acquire a [`Proposal`](crate::pipeline::Proposal), evaluate it through
//! [`Gate`](crate::guard::Gate) instances, then [`commit`](crate::pipeline::Pipeline::commit) to
//! the [`Store`](crate::store::Store).
//!
//! ```ignore
//! use free_batteries::prelude::*;
//!
//! let store = Store::open_default()?;
//! let gates = GateSet::new();
//! let pipeline = Pipeline::new(gates);
//!
//! let proposal = Proposal::new(payload);
//! let receipt = pipeline.evaluate(&context, proposal)?;
//! let committed = pipeline.commit(receipt, |p| {
//!     store.append(&coord, kind, &p)
//! })?;
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
/// Module declarations in DEPENDENCY ORDER: wire, coordinate, outcome, event, guard, pipeline, store, typestate, id, prelude.
/// [SPEC:src/lib.rs — Module declarations in DEPENDENCY ORDER]
pub mod wire;

/// compile_error guards for impossible configurations:
#[allow(unexpected_cfgs)]
#[cfg(feature = "async-store")]
compile_error!(
    "INVARIANT 2: free-batteries does not have an async Store API. \
     Async callers use spawn_blocking() or flume recv_async(). \
     See: src/store/subscription.rs for the async pattern."
);

#[allow(unexpected_cfgs)]
#[cfg(feature = "sha256")]
compile_error!(
    "INVARIANT 5: blake3 is the only hash. No HashAlgorithm enum. \
     One function: compute_hash(bytes) -> [u8; 32], behind feature = blake3."
);
