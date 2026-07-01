#![deny(missing_docs)]
// The impossible-feature guards below (async-store, sha256) reference features
// intentionally not declared in Cargo.toml; build.rs registers them via
// `cargo::rustc-check-cfg` so rustc recognizes them without any cfg-lint allow.
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
//! Use it when you need a tamper-evident, replayable record of what happened:
//! every event is hash-bound to its per-entity ancestor with Blake3, every
//! accepted write returns a verifiable (optionally Ed25519-signed) receipt,
//! and projections are derived views rebuilt from the log by construction.
//!
//! Most callers start with the eight-job path: open a [`Store`](crate::store::Store), append typed
//! events, page commit order with
//! [`Store::query_entries_after`](crate::store::Store::query_entries_after),
//! point-read with [`Store::get`](crate::store::Store::get), walk bounded
//! hash-chain ancestry with
//! [`Store::walk_ancestors`](crate::store::Store::walk_ancestors), verify
//! receipts with
//! [`Store::verify_append_receipt`](crate::store::Store::verify_append_receipt),
//! project derived state with [`Store::project`](crate::store::Store::project),
//! then close the store. Gates and [`Pipeline`](crate::pipeline::Pipeline) are
//! advanced batteries for caller-owned evaluation before commit.
//!
//! ```no_run
//! use batpak::prelude::*;
//!
//! #[derive(serde::Serialize, serde::Deserialize, EventPayload)]
//! #[batpak(category = 0xF, type_id = 1)]
//! struct ThingHappened {
//!     value: i64,
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let store = Store::open(StoreConfig::new(dir.path()))?;
//! let coord = Coordinate::new("entity:a", "scope:1")?;
//!
//! let receipt = store.append_typed(&coord, &ThingHappened { value: 42 })?;
//! let stored = store.get(receipt.event_id)?;
//!
//! assert_eq!(stored.coordinate.entity(), "entity:a");
//! assert_eq!(stored.event.header.event_id, receipt.event_id);
//! # Ok(())
//! # }
//! ```
//!
//! **Reading order:**
//! 1. [`coordinate`]: Identify entities and scopes.
//! 2. [`event`]: Structure typed payloads and projection inputs.
//! 3. [`store`]: Persist, page, point-read, walk, verify, and project.
//! 4. [`guard`] and [`pipeline`]: Add caller-defined write evaluation.
//! 5. [`artifact`], [`registry`], [`transition`], [`reservation`], and
//!    [`schema`]: Advanced substrate batteries for envelopes, ledgers,
//!    transition evidence, reservation mechanics, and drift reports.
//!
//! **Fail-closed defaults (verifiability).** A store refuses to open on an
//! ambiguous or undecodable payload registry:
//! [`EventPayloadValidation`](crate::event::EventPayloadValidation) defaults to
//! `FailFast`, so a duplicate-kind collision or an incomplete upcast chain is
//! rejected at [`Store::open`](crate::store::Store::open) (opt out explicitly
//! with `Warn`/`Silent`). A binary that registers `EventPayload` types but may
//! never open a store should call
//! [`verify_registry`](crate::event::verify_registry) once at startup (or enable
//! the non-default `startup-registry-check` feature for automatic enforcement),
//! since the derive's own collision test is `#[cfg(test)]`-only and a release
//! binary would otherwise see no check. Receipt signing is governed by
//! [`SigningPolicy`](crate::store::SigningPolicy): the default `Optional`
//! permits a keyless store, while `Required` refuses to open without a signing
//! key so an unsigned receipt is never accepted. A configured signer fails the
//! append closed rather than silently emitting an unsigned receipt unless
//! [`StoreConfig::with_signing_downgrade_allowed`](crate::store::StoreConfig::with_signing_downgrade_allowed)
//! opts in.
//!
//! **On-demand integrity.** [`Store::verify_chain`](crate::store::Store::verify_chain)
//! recomputes the full blake3 hash chain over every committed event and returns
//! a [`ChainVerificationReport`](crate::store::ChainVerificationReport); opt into
//! [`ChainVerification::Recompute`](crate::store::ChainVerification::Recompute)
//! to run that pass automatically at open and fail closed on tamper. For
//! ancestry, [`Store::walk_ancestors_outcome`](crate::store::Store::walk_ancestors_outcome)
//! returns an [`AncestorWalk`](crate::store::AncestorWalk) whose
//! [`AncestryBoundary`](crate::store::AncestryBoundary) makes a truncated lineage
//! (for example, a retention-dropped mid-chain parent) observable instead of
//! indistinguishable from a complete walk to genesis.

// Width invariant: batpak stores ids/offsets/lengths compactly as `u32`/`u64` and
// converts to `usize` only transiently for slicing. `u32 -> usize` is lossless only
// when `usize` is at least 32 bits, so a <32-bit target is rejected here at compile
// time. This makes the `usize::try_from(u32).unwrap_or(..)` conversions throughout
// the store provably-total (their `Err` arm is unreachable) without any `#[allow]`.
#[cfg(target_pointer_width = "16")]
compile_error!("batpak requires a >=32-bit target: u32 ids/offsets must fit usize losslessly");

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
        assert_no_kind_collisions, inventory, scan_for_kind_collisions, upcast_steps_for,
        EventPayloadRegistration, UpcastRegistration,
    };
}

/// Fuzz-only decode entry points for the workspace-excluded `batpak-fuzz`
/// cargo-fuzz crate (GAUNT-FUZZ-1). Gated behind `dangerous-test-hooks` and
/// `#[doc(hidden)]`: a default build never compiles it, so there is no
/// production API-surface change. See the module docs for the wrapper contract.
#[cfg(feature = "dangerous-test-hooks")]
#[doc(hidden)]
pub mod __fuzz;

/// Deterministic-simulation entry points for the `sim_is_deterministic`
/// integration test (GAUNT-SIM-2c). Gated behind `dangerous-test-hooks` and
/// `#[doc(hidden)]`: a default build never compiles it, so there is no
/// production API-surface change. Exposes only the seeded-workload driver and
/// `BATPAK_SEED` replay helper over the `pub(crate)` simulation backends.
#[cfg(feature = "dangerous-test-hooks")]
#[doc(hidden)]
pub mod __sim {
    pub use crate::store::sim::corpus::{
        assert_corpus_rows_current, graduate_corpus_cell, graduate_corpus_seed,
        run_fork_isolation_corpus_cell, run_import_reapply_corpus_cell, verify_corpus_row,
        verify_corpus_row_cell, CorpusReplayPublic, CorpusRowDescriptor, GraduationRequest,
    };
    pub use crate::store::sim::fork_hostile::{
        run_fork_dest_equals_source, run_fork_enospc_mid_copy, run_fork_stale_dest,
        run_fork_symlink_dest, DestEqualsSourceOutcome, EnospcMidCopyOutcome, StaleDestOutcome,
        SymlinkDestOutcome, STALE_RANGES_FILE, STALE_SEGMENT_FILE,
    };
    pub use crate::store::sim::fork_recovery::{
        fork_fault_replay_seed, run_seeded_fork_fault_public, ForkFaultOutcomePublic,
    };
    pub use crate::store::sim::import_recovery::{
        import_fault_replay_seed, run_seeded_import_fault_public, ImportFaultOutcomePublic,
    };
    pub use crate::store::sim::recovery::{
        recovery_replay_seed, run_seeded_recovery, RecoveryOutcomePublic,
    };
    pub use crate::store::sim::recovery_matrix::{
        matrix_replay_seed, run_recovery_matrix, MatrixCell, RecoveredClass,
    };
    pub use crate::store::sim::{replay_seed, run_seeded_workload};
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
// async-store is intentionally undeclared in Cargo.toml; build.rs registers the
// cfg via `cargo::rustc-check-cfg` so this INV-STORE-SYNC-ONLY guard (ADR-0001)
// compiles warning-free in src/lib.rs.
#[cfg(feature = "async-store")]
compile_error!(
    "INVARIANT 2: batpak does not have an async Store API. \
     Async callers use spawn_blocking() or flume recv_async(). \
     See: src/store/delivery/subscription.rs for the async pattern."
);

// sha256 is intentionally undeclared in Cargo.toml; build.rs registers the cfg
// via `cargo::rustc-check-cfg` so this blake3-only guard (INV-STORE-SYNC-ONLY)
// compiles warning-free in src/lib.rs.
#[cfg(feature = "sha256")]
compile_error!(
    "INVARIANT 5: blake3 is the only hash. No HashAlgorithm enum. \
     One function: compute_hash(bytes) -> [u8; 32], blake3 is a mandatory dependency."
);
