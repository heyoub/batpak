#![deny(missing_docs)]
// justifies: INV-STORE-SYNC-ONLY, ADR-0001; impossible-feature guards in src/lib.rs (async-store, sha256) use cfg attributes for features intentionally not declared in Cargo.toml; item-level allow is unreliable for cfg checks on some toolchain versions so we silence at crate root.
#![allow(unexpected_cfgs)]
// justifies: docs.rs builds with --cfg docsrs from Cargo.toml so feature-gated public API can show doc(cfg) badges; local stable docs add batpak_stable_docs to avoid nightly-only attributes.
#![cfg_attr(all(docsrs, not(batpak_stable_docs)), feature(doc_cfg))]
// justifies: src/lib.rs makes production expect() sites deliberate invariant escape hatches instead of ambient convenience panics.
#![cfg_attr(not(test), deny(clippy::expect_used))]
// cast_possible_truncation and cast_sign_loss are enforced via [lints.clippy] in Cargo.toml.
// Each intentional cast has an inline #[allow] with a justification comment.
//! Sync-first event sourcing for Rust: append-only segments, causal metadata,
//! caller-defined gates, and typed projections.
//!
//! Batpak stores immutable events in segment files, tracks causation metadata,
//! evaluates caller-defined gates before commit, and rebuilds typed projections through
//! a synchronous API that does not require an async runtime.
//!
//! Most callers start with typed payloads: derive [`EventPayload`], append with
//! [`Store::append_typed`](crate::store::Store::append_typed), and read through
//! the in-memory index. Gates and [`Pipeline`](crate::pipeline::Pipeline) can be
//! added when a write needs caller-owned evaluation before commit.
//!
//! ```no_run
//! use batpak::prelude::*;
//!
//! #[derive(serde::Serialize, serde::Deserialize, EventPayload)]
//! #[batpak(category = 0xF, type_id = 1)]
//! struct PlayerMoved {
//!     x: i32,
//!     y: i32,
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let store = Store::open(StoreConfig::new(dir.path()))?;
//! let coord = Coordinate::new("player:alice", "room:dungeon")?;
//!
//! let receipt = store.append_typed(&coord, &PlayerMoved { x: 10, y: 20 })?;
//! let stored = store.get(receipt.event_id)?;
//!
//! assert_eq!(stored.coordinate.entity(), "player:alice");
//! assert_eq!(stored.event.header.event_id, receipt.event_id);
//! # Ok(())
//! # }
//! ```
//!
//! **Reading order:**
//! 1. [`coordinate`]: Identify entities and scopes
//! 2. [`event`]: Structure your events
//! 3. [`artifact`]: Canonical body-vs-envelope digests for signed attachments
//! 4. [`registry`]: Attested immutable rows (lifecycle, supersession, drift, verification)
//! 5. [`transition`]: Generic state transition evidence (events and structural reports)
//! 6. [`reservation`]: Generic reservation ledger (reserve/commit/refund/expire/orphan)
//! 7. [`guard`]: Build caller-defined gates
//! 8. [`pipeline`]: Propose and commit
//! 9. [`store`]: Persist and query

/// Crate-level substrate: canonical artifact body digest vs envelope digest.
pub mod artifact;
/// Entity and scope addressing for events.
pub mod coordinate;
/// Stable named-field MessagePack encoding helpers.
pub mod encoding;
/// Event types, headers, and sourcing traits.
pub mod event;
mod evidence;
/// Caller-defined gate evaluation before event commitment.
pub mod guard;
/// UUID v7 identifier generation.
pub mod id;
/// Result-like type for pipeline operations.
pub mod outcome;
/// Propose-evaluate-commit workflow.
pub mod pipeline;
/// Common re-exports for convenient use.
pub mod prelude;
/// Crate-level substrate: generic signed registry rows composing artifact envelopes.
pub mod registry;
/// Crate-level substrate: generic reservation ledger mechanics.
pub mod reservation;
/// Deterministic schema/fixture snapshot drift evidence.
pub mod schema;
/// Persistent event storage and querying.
pub mod store;
/// Crate-level substrate: generic state transition events and reports.
pub mod transition;
/// Compile-time state machine transitions.
pub mod typestate;
/// Module declarations in DEPENDENCY ORDER:
/// wire → coordinate → outcome → event → guard → pipeline → store → typestate → id → prelude
/// Serde serialization helpers.
pub mod wire; // serde helpers — no deps, must come first

/// Back-compatible alias for batpak-scoped named-field MessagePack helpers.
///
/// This is not a cross-protocol canonicalization surface; protocols with their
/// own canonical byte rules must apply those rules outside batpak core.
pub use crate::encoding as canonical;

/// Internal types referenced by `#[derive(EventPayload)]` generated code.
/// Not part of the public API; may change without notice.
#[doc(hidden)]
pub mod __private {
    pub use batpak_macros_support::{
        assert_no_kind_collisions, inventory, scan_for_kind_collisions, EventPayloadRegistration,
    };
}

// Self-alias for path hygiene in derive-generated code.
// `batpak-macros` emits absolute `::batpak::...` paths (see ADR-0010 and
// `crates/macros/src/lib.rs:151-166`). `pub extern crate self as batpak;`
// makes `::batpak::...` resolve to `self::...` from inside the library
// crate itself, so `#[derive(EventPayload)]` / `#[derive(EventSourced)]` /
// `#[derive(MultiEventReactor)]` all work identically in downstream crates
// AND in in-workspace tests/examples/unit modules. The downstream fixture
// at `fixtures/downstream/` proves the outward direction; the in-crate
// test added with this seam proves the inward direction.
#[doc(hidden)]
pub extern crate self as batpak;

// Crate-root re-exports for the typed payload binding and dispatch-layer
// derives. Traits live in `batpak::event::...`; the derive macros are
// generated by the `batpak-macros` proc-macro crate. Mirroring both at the
// crate root follows the serde pattern: a single `use batpak::EventPayload`
// (or `EventSourced`) brings in both the trait (type namespace) and the
// derive (macro namespace).
pub use crate::event::{EventPayload, EventSourced, MultiReactive};
pub use batpak_macros::{EventPayload, EventSourced, MultiEventReactor};

/// compile_error guards for impossible configurations:
// justifies: INV-STORE-SYNC-ONLY, ADR-0001; async-store is not a declared feature in src/lib.rs; this guard must survive cargo check without the crate-level lint silencing the cfg reference
#[allow(unexpected_cfgs)]
#[cfg(feature = "async-store")]
compile_error!(
    "INVARIANT 2: batpak does not have an async Store API. \
     Async callers use spawn_blocking() or flume recv_async(). \
     See: src/store/delivery/subscription.rs for the async pattern."
);

// justifies: INV-STORE-SYNC-ONLY; sha256 is not a declared feature in src/lib.rs; this compile_error guard requires the cfg reference to reach codegen
#[allow(unexpected_cfgs)]
#[cfg(feature = "sha256")]
compile_error!(
    "INVARIANT 5: blake3 is the only hash. No HashAlgorithm enum. \
     One function: compute_hash(bytes) -> [u8; 32], blake3 is a mandatory dependency."
);
