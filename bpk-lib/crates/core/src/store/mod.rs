mod ancestry;
mod append;
pub mod backup_envelope;
mod chain_walk;
/// Cold-start recovery reports and artifact readers.
pub mod cold_start;
mod compaction_report;
mod config;
/// The explicit crypto-shred erasure op (`Store::shred_scope`); opt-in
/// `payload-encryption` only.
#[cfg(feature = "payload-encryption")]
mod crypto_shred_api;
/// Push subscriptions (lossy) and pull cursors (ordered, with optional durable
/// checkpoints) for event delivery.
pub mod delivery;
mod diagnostics_api;
mod dir_lock;
mod error;
/// Fault injection framework for testing failure scenarios.
#[cfg(feature = "dangerous-test-hooks")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "dangerous-test-hooks"))
)]
pub mod fault;
mod file_classification;
mod fork_report;
mod frontier_api;
mod gate;
// `pub(crate)` (was `mod`) so the feature-gated `crate::__fuzz` module can name
// `hidden_ranges::{load_cancelled_ranges, VISIBILITY_RANGES_FILENAME}`. Crate-
// internal only; no public API-surface change.
pub(crate) mod hidden_ranges;
mod import;
mod import_api;
/// In-memory 2D event index, rebuilt from segments on startup.
pub mod index;
/// Per-scope payload key material for opt-in crypto-shred encryption.
#[cfg(feature = "payload-encryption")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "payload-encryption"))
)]
pub mod keyscope;
mod lifecycle;
mod lifecycle_api;
mod lifecycle_close;
mod open;
mod platform;
/// Projection cache traits and built-in backends (NoCache, NativeCache).
pub mod projection;
mod projection_run;
/// Typed reactor output batch — accumulator handed to typed reactor handlers.
pub mod reaction;
mod reactor_delivery;
/// Typed reactor public surface + shared internal canal runner.
pub mod reactor_typed;
mod read_api;
mod read_walk;
mod receipt_verification;
#[cfg(test)]
mod runtime_contracts;
/// On-disk segment format, frame encoding/decoding, and compaction helpers.
pub mod segment;
mod signing;
/// Cooperative single-thread seeded simulation runtime (test hooks only).
#[cfg(feature = "dangerous-test-hooks")]
pub(crate) mod sim;
mod snapshot_report;
/// Runtime statistics and diagnostic snapshots.
pub mod stats;
mod store_resource_report;
mod subscriber_frontier;
#[cfg(feature = "dangerous-test-hooks")]
mod test_support;
mod watch_api;
pub(crate) mod write;
mod write_api;

pub use ancestry::{AncestorWalk, AncestryBoundary};
pub use append::{
    AppendOptions, AppendPositionHint, AppendReceipt, BatchAppendItem, CausationRef,
    CompactionConfig, CompactionStrategy, DenialReceipt, DenialRequest, EncodedBytes, ExtensionKey,
    ExtensionKeyError, ReceiptExtensionKey, ReceiptExtensionNamespace, ReceiptExtensionValue,
    RetentionPredicate, SigningDowngradeBody, SigningDowngradeReason, SigningExtensionNamespace,
    SIGNING_DOWNGRADE_SCHEMA_VERSION,
};
pub use chain_walk::{
    ChainWalkEvidenceReport, ChainWalkFinding, ChainWalkHash, ChainWalkMode, ChainWalkReportBody,
    ChainWalkReportError, ChainWalkRequest, ChainWalkStartRef, CHAIN_WALK_REPORT_SCHEMA_VERSION,
};
pub use compaction_report::{
    compaction_strategy_shape, report_for_run, report_skipped, CompactionEvidenceHash,
    CompactionEvidenceReport, CompactionReportBody, CompactionReportFinding,
    CompactionStrategyShape, COMPACTION_REPORT_SCHEMA_VERSION,
};
pub use config::{
    BatchConfig, ChainVerification, IdempotencyRetention, IndexConfig, IndexTopology,
    OpenReportObserver, OverflowPolicy, SigningPolicy, StoreConfig, SyncConfig, SyncMode,
    WriterConfig,
};
pub use delivery::canal::{Canal, CanalBatch, CanalClosed, CanalHandle, CanalItem, ReactorCanal};
pub use delivery::cursor::{
    Cursor, CursorGapConfig, CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle,
    GapObservation,
};
pub use delivery::observation::{
    AtLeastOnce, CheckpointId, CheckpointIdError, IdempotencyKey, ObservedOnce,
    MAX_CHECKPOINT_ID_LEN,
};
pub use delivery::subscription::Subscription;
pub use error::{
    HiddenRangesCorruption, ProfileInvalidKind, StoreError, StoreInvariant, StoreLockMode,
};
#[cfg(feature = "dangerous-test-hooks")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "dangerous-test-hooks"))
)]
pub use fault::{
    CountdownAction, CountdownInjector, FaultInjector, InjectionPoint, ProbabilisticInjector,
};
pub use fork_report::{
    decode_fork_evidence_wire, encode_fork_evidence_wire, fork_report_body_hash, CopyPreference,
    ForkCopyStrategy, ForkEvidenceHash, ForkFinding, ForkOptions, ForkReport, ForkReportBody,
    ForkStrategyCounts, FORK_EVIDENCE_REPORT_SCHEMA_VERSION,
};
pub use gate::DurabilityGate;
pub use import::{
    decode_import_provenance_wire, encode_import_provenance_wire, provenance,
    provenance_from_extensions, ImportFilter, ImportOptions, ImportProvenance, ImportReport,
    ImportSelector, SourceNamespace, IMPORT_PROVENANCE_SCHEMA_VERSION,
};
pub use index::IndexEntry;
#[cfg(feature = "payload-encryption")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "payload-encryption"))
)]
pub use keyscope::{
    scope_for, KeyScope, KeyScopeGranularity, KeyStore, KeyStoreError, PayloadKey, ShredScope,
};
/// Test-only global-allocator shims. Re-exported so dedicated single-test
/// binaries can install one as `#[global_allocator]`. Compiled out unless the
/// `alloc-count` or `fault-alloc` feature is enabled.
#[cfg(any(feature = "alloc-count", feature = "fault-alloc"))]
pub use platform::alloc;
pub use platform::clock::{Clock, SystemClock};
pub use platform::spawn::{JobHandle, JobStatus, JoinError, Spawn, SpawnError, ThreadSpawn};
pub use projection::flow::ReplayInput;
pub use projection::watch::{CursorWatcherError, ProjectionWatcher, WatcherError};
pub use projection::{
    CacheCapabilities, CacheMeta, Freshness, NativeCache, NoCache, ProjectionCache,
};
/// Three projection states returned by [`Store::project_fused3`].
pub type ProjectionFusion3<First, Second, Third> = (Option<First>, Option<Second>, Option<Third>);
pub use projection_run::{
    ProjectionEvidenceRegistry, ProjectionRunCacheStatus, ProjectionRunCheckpointRef,
    ProjectionRunEvidenceReport, ProjectionRunFinding, ProjectionRunFreshnessStatus,
    ProjectionRunFrontierKind, ProjectionRunHash, ProjectionRunInputFrontier,
    ProjectionRunOutputHash, ProjectionRunReplayMode, ProjectionRunReportBody,
    ProjectionRunReportError, ProjectionRunRequestedFreshness, ProjectionSourceRef,
    PROJECTION_RUN_REPORT_SCHEMA_VERSION,
};
pub use reaction::ReactionBatch;
pub use reactor_typed::{ReactorConfig, ReactorError, TypedReactorHandle};
pub use read_api::ChainVerificationReport;
#[cfg(feature = "payload-encryption")]
pub use read_api::{DeliveryPayload, ReadDisposition};
pub use read_walk::{
    ReadWalkDroppedCount, ReadWalkEvidenceReport, ReadWalkFinding, ReadWalkFreshnessIntent,
    ReadWalkFrontierKind, ReadWalkHash, ReadWalkInputFrontier, ReadWalkProofRef, ReadWalkProofRefs,
    ReadWalkReplayMode, ReadWalkReportBody, ReadWalkReportError, ReadWalkRequest,
    ReadWalkSourceRef, READ_WALK_REPORT_SCHEMA_VERSION,
};
pub use receipt_verification::{ReceiptVerification, ReceiptVerificationError};
pub use signing::SigningKey;
/// The canonical deterministic [`Clock`](crate::store::Clock) for simulators.
///
/// Exported only under `dangerous-test-hooks` (the same gate the rest of the
/// simulation runtime lives behind) so downstream deterministic simulators
/// construct one logical clock instead of re-implementing the trait.
#[cfg(feature = "dangerous-test-hooks")]
pub use sim::SimClock;
pub use snapshot_report::{
    snapshot_report_body_hash, SnapshotEvidenceHash, SnapshotEvidenceReport, SnapshotFenceTokenRef,
    SnapshotFileKind, SnapshotFinding, SnapshotReportBody, SnapshotWatermarkRef,
    SNAPSHOT_EVIDENCE_REPORT_SCHEMA_VERSION,
};
pub use stats::{
    ActiveSegmentReadEvidence, ClockEvidence, FrontierView, HlcPoint, HostEvidenceSummary,
    LaneFrontierView, LockLeafSymlinkProtection, MmapAdmissionSummary, MmapEvidence,
    ParentDirSyncAdmissionSummary, ParentDirSyncEvidence, PlatformAdmissionSummary,
    PlatformEvidenceSummary, StoreDiagnostics, StoreLockAdmissionSummary, StorePathEvidenceSummary,
    StorePathStatusEvidence, StoreStats, WatermarkKind, WriterPressure,
};
pub use store_resource_report::{
    store_data_dir_identity_hash, store_resource_evidence_report_from_diagnostics,
    store_resource_report_body_from_diagnostics, store_resource_report_body_hash,
    StoreResourceEvidenceReport, StoreResourceFrontierBody, StoreResourceHash,
    StoreResourceReportBody, StoreResourceReportError, StoreResourceRestartPolicyShape,
    STORE_RESOURCE_REPORT_SCHEMA_VERSION,
};
pub use subscriber_frontier::{
    LossPrecision, SubscriberDeliveryState, SubscriberFrontierEvidenceReport,
    SubscriberFrontierFinding, SubscriberFrontierHash, SubscriberFrontierReportBody,
    SubscriberFrontierReportError, SubscriberFrontierRequest, SubscriberFrontierSource,
    SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION,
};
pub use watch_api::ReactLoopHandle;
pub use write::control::{AppendTicket, BatchAppendTicket, Outbox, VisibilityFence};
pub use write::writer::{Notification, RestartPolicy};

use crate::coordinate::{Coordinate, KindFilter, Region};
use crate::event::{
    self, EventKind, EventPayload, EventPayloadValidation, EventSourced, StoredEvent,
};
use index::StoreIndex;
use open::timestamp_us_for_hlc;
use parking_lot::Mutex;
use projection::registry::ProjectionRegistry;
use segment::scan::Reader;
use serde::Serialize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use write::control::AppendSubmission;
use write::fanout::{ReactorSubscriberList, SubscriberList};
use write::writer::{WatermarkAdvanceHandle, WatermarkState, WriterCommand, WriterHandle};
// ProjectionCache re-exported above via pub use, no separate use needed.

#[cfg(test)]
const TEST_WRITER_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) fn recv_writer_reply<T>(
    rx: &flume::Receiver<Result<T, StoreError>>,
) -> Result<T, StoreError> {
    #[cfg(test)]
    let received = rx
        .recv_timeout(TEST_WRITER_REPLY_TIMEOUT)
        .map_err(|_| StoreError::WriterCrashed)?;
    #[cfg(not(test))]
    let received = rx.recv().map_err(|_| StoreError::WriterCrashed)?;
    received
}

/// Store: the runtime. Sync API. Send + Sync.
/// Invariant 2: all methods are sync; async integration lives in channels.
// async-store is intentionally undeclared in Cargo.toml; build.rs registers the
// cfg via `cargo::rustc-check-cfg` so this INV-STORE-SYNC-ONLY guard (ADR-0001)
// compiles warning-free in src/store/mod.rs.
#[cfg(feature = "async-store")]
compile_error!("INVARIANT 2: Store API is sync. Use spawn_blocking or flume recv_async.");

/// Sealing module for [`StoreState`] (mirrors `typestate::transition::sealed`).
///
/// `Sealed` is a marker that only this crate's typestate markers implement, so
/// downstream code cannot add new [`StoreState`] implementors.
#[doc(hidden)]
pub mod sealed {
    /// Sealed marker implemented by every [`super::StoreState`] type.
    pub trait Sealed {}
}

/// Sealed per-state teardown contract for the [`Store`] typestate.
///
/// This is what lets the single generic `Drop for Store<State>` reach the
/// writer (when there is one) by `&mut` without moving any field out of the
/// store. Only [`Open`] carries a writer handle; the other markers are ZSTs
/// whose teardown is a no-op.
///
/// The trait is `pub` so it can bound the `State` parameter on the public
/// [`Store`]/[`ProjectionEvidenceRegistry`] types, but it is **sealed** via the
/// private `sealed::Sealed` supertrait: downstream code can neither implement
/// it for new markers nor obtain a `StoreState` value to call its methods on
/// (the marker constructors — e.g. `Open`'s field — are crate-private). No
/// method signature names `WriterHandle`, so the crate-private writer type is
/// never exposed in the public API either.
pub trait StoreState: sealed::Sealed {
    /// Drain + join the owned writer when `should_shutdown` is set.
    fn shutdown_writer(&mut self, should_shutdown: bool);

    /// Queue length of the owned writer's command channel, if any.
    ///
    /// Returns the channel length only for [`Open`]; the other states carry no
    /// writer. Domain-neutral `usize` so the crate-private `WriterHandle` is
    /// never named in the (sealed) trait signature.
    fn writer_queue_len(&self) -> Option<usize>;
}

/// Typestate marker for an open store.
///
/// `Open` is the only state that owns the writer handle: encoding the
/// invariant "an open store always has a writer" directly in the type, so
/// `writer_ref` can return `&WriterHandle` with no `Option`, no `expect`.
pub struct Open(pub(crate) WriterHandle);

/// Typestate marker for a cleanly closed store.
pub struct Closed;

/// Typestate marker for a read-only store handle.
pub struct ReadOnly;

impl sealed::Sealed for Open {}
impl sealed::Sealed for Closed {}
impl sealed::Sealed for ReadOnly {}

impl StoreState for Open {
    fn shutdown_writer(&mut self, should_shutdown: bool) {
        if !should_shutdown {
            return;
        }
        let writer = &mut self.0;
        tracing::warn!(
            "Store dropped without explicit close(); draining writer before releasing store lock"
        );
        let (tx, rx) = flume::bounded(1);
        if writer
            .tx
            .send(WriterCommand::Shutdown { respond: tx })
            .is_ok()
        {
            wait_for_drop_shutdown_ack(&rx);
        }
        join_drop_shutdown_writer(writer);
    }

    fn writer_queue_len(&self) -> Option<usize> {
        Some(self.0.tx.len())
    }
}

impl StoreState for Closed {
    fn shutdown_writer(&mut self, _should_shutdown: bool) {}
    // A cleanly-closed store owns no writer, so it reports no command-queue
    // length. `Store<Closed>` is never constructed, but `Closed` is a public ZST
    // and this `StoreState` method is directly callable on the bare marker — so
    // the `Some(0)`/`Some(1)` constant mutants ARE observable and are killed by
    // `closed_state_reports_no_writer_queue` (tests/mutation_kill_core-store.rs).
    fn writer_queue_len(&self) -> Option<usize> {
        None
    }
}

impl StoreState for ReadOnly {
    fn shutdown_writer(&mut self, _should_shutdown: bool) {}
    fn writer_queue_len(&self) -> Option<usize> {
        None
    }
}

/// The main event store handle. Sync API; all methods are blocking. Send + Sync.
pub struct Store<State: StoreState = Open> {
    pub(crate) index: Arc<StoreIndex>,
    pub(crate) reader: Arc<Reader>,
    pub(crate) cache: Box<dyn ProjectionCache>,
    pub(crate) watermark_handle: WatermarkAdvanceHandle,
    pub(crate) projection_registry: ProjectionRegistry,
    pub(crate) lifecycle_gate: Mutex<()>,
    pub(crate) config: Arc<StoreConfig>,
    pub(crate) runtime: Arc<config::ValidatedStoreConfig>,
    pub(crate) should_shutdown_on_drop: bool,
    pub(crate) open_report: Option<cold_start::rebuild::OpenIndexReport>,
    pub(crate) cumulative_reserved_kind_fallbacks: segment::sidx::ReservedKindFallbackStats,
    /// Loaded crypto-shred keyset, present only when `payload_encryption` is
    /// configured (`None` disables encryption). Rehydrated from disk at open
    /// (Stage B); Stage C reads/mints/destroys through it on the append/read
    /// paths. An [`Arc`]`<`[`Mutex`]`>` — the SAME handle the background writer
    /// holds through the runtime — so an append that mints a key on the writer
    /// thread is immediately visible to a decrypt-on-read under `&self`.
    #[cfg(feature = "payload-encryption")]
    pub(crate) key_store: Option<Arc<Mutex<keyscope::KeyStore>>>,
    /// Typestate payload: carries the writer handle when (and only when) the
    /// store is [`Open`]; a ZST for the other states.
    pub(crate) state: State,
    pub(crate) _store_lock: dir_lock::StoreDirLock,
}

/// Safety net: if Store is dropped without calling close(), send Shutdown to the
/// writer thread and wait for it to drain pending events before releasing the
/// directory lock.
/// close(self) is still the preferred explicit path for guaranteed clean shutdown.
impl<State: StoreState> Drop for Store<State> {
    fn drop(&mut self) {
        self.state.shutdown_writer(self.should_shutdown_on_drop);
    }
}

fn wait_for_drop_shutdown_ack(rx: &flume::Receiver<Result<(), StoreError>>) {
    let _ack = rx.recv();
}

fn join_drop_shutdown_writer(writer: &mut WriterHandle) {
    let _join_result = writer.join();
}

#[cfg(feature = "dangerous-test-hooks")]
impl Store<Open> {
    /// Test-only: abandon this store the way a power loss would, WITHOUT the
    /// clean-shutdown drain.
    ///
    /// A normal `drop`/`close` sends `Shutdown` to the writer, which drains the
    /// queue, writes a SIDX footer, and fsyncs — defeating any pre-fsync crash
    /// scenario. This hook instead disables the drop-time shutdown and quiesces
    /// the writer by closing its command channel (NOT by sending `Shutdown`), so
    /// the writer loop simply ends with no final sync/footer. The
    /// write-but-unsynced tail therefore stays exactly where the durability seam
    /// left it; the caller then drives [`crate::store::platform::fs::StoreFs::crash`]
    /// (via the installed sim filesystem) to truncate it, modelling power loss.
    ///
    /// Consumes the store; reopen over the same data directory to recover.
    pub(crate) fn abandon_without_shutdown(mut self) {
        self.should_shutdown_on_drop = false;
        // Close the writer's command channel WITHOUT sending Shutdown: the writer
        // loop ends naturally (no drain, no footer, no final sync), then we join
        // to quiescence so no thread is mid-fsync when the caller crashes the
        // filesystem.
        self.state.0.close_channel_and_join();
        // `self` (should_shutdown_on_drop=false) drops here as an inert handle:
        // `Open::shutdown_writer(false)` no-ops and the quiesced writer in
        // `state.0` then drops inertly.
    }
}

#[cfg(all(test, feature = "dangerous-test-hooks"))]
mod writer_queue_len_tests {
    //! Pins `<Open as StoreState>::writer_queue_len` through the public
    //! `diagnostics()` surface. A cooperative store does not drain its command
    //! queue until pumped, so several un-awaited `submit`s leave a known,
    //! non-trivial backlog — catching the `None`, `Some(0)`, and `Some(1)`
    //! constant mutants of the `Open` impl.
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::store::{Store, StoreConfig};

    #[test]
    fn open_diagnostics_reports_the_live_writer_backlog_and_capacity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = StoreConfig::new(dir.path()).with_writer_channel_capacity(32);
        let store = Store::open_cooperative(config).expect("open cooperative store");
        let coord = Coordinate::new("entity:queue-len", "scope:mutation").expect("coord");

        // Submit without awaiting: cooperative mode does not pump here, so each
        // command stays queued in the writer mailbox.
        let mut tickets = Vec::new();
        for n in 0..4u32 {
            tickets.push(
                store
                    .submit(&coord, EventKind::DATA, &serde_json::json!({ "n": n }))
                    .expect("submit"),
            );
        }

        let pressure = store.diagnostics().writer_pressure;
        let mut failures: Vec<String> = Vec::new();
        if pressure.queue_len < 2 {
            failures.push(format!(
                "Open writer_queue_len must reflect the real backlog (>=2 after 4 un-awaited \
                 submits), got {}",
                pressure.queue_len
            ));
        }
        if pressure.capacity != 32 {
            failures.push(format!(
                "Open writer pressure capacity must be the configured 32 (None mutant would \
                 report 0), got {}",
                pressure.capacity
            ));
        }
        assert!(
            failures.is_empty(),
            "writer pressure mismatches: {failures:?}"
        );

        // Drain inline and drop inertly — no Shutdown drain (which would deadlock
        // a cooperative store with a backlog and no pumping thread).
        drop(tickets);
        store.abandon_without_shutdown();
    }
}

#[cfg(test)]
mod open_writer_queue_len_direct_tests {
    //! Non-feature-gated companion to `writer_queue_len_tests`: pins
    //! `<Open as StoreState>::writer_queue_len` WITHOUT `dangerous-test-hooks`,
    //! so the `--no-default-features` repo-wide mutation surface — where the
    //! cooperative-mode direct-assert test is not compiled — still catches the
    //! `Some(0)` constant mutant of the `Open` impl by assertion.
    //!
    //! It builds an `Open` over a writer handle with NO draining thread
    //! (`from_parts_for_test` => `WriterDrive::Threaded { thread: None }`),
    //! enqueues a known command backlog, and reads the queue length DIRECTLY.
    //! There is no writer thread and no wait loop, so under the mutant the test
    //! fails an assertion in microseconds rather than being noticed only by a
    //! spin-wait that hangs to the cargo-mutants test-timeout.
    use super::{Open, StoreError, StoreState, SubscriberList, WriterCommand, WriterHandle};
    use std::sync::Arc;

    #[test]
    fn open_writer_queue_len_reports_the_undrained_command_backlog() {
        // `from_parts_for_test` yields `WriterDrive::Threaded { thread: None }`:
        // nothing consumes the command channel, so every enqueued command stays
        // counted by `tx.len()`. Keep `_command_rx` alive so the channel stays
        // connected and the messages stay queued through the observation.
        let (tx, _command_rx) = flume::bounded::<WriterCommand>(16);
        let open = Open(WriterHandle::from_parts_for_test(
            tx,
            Arc::new(SubscriberList::new()),
        ));

        let (ack_tx, _ack_rx) = flume::bounded::<Result<(), StoreError>>(1);
        let mut setup_failures: Vec<String> = Vec::new();
        for n in 0..3u32 {
            if open
                .0
                .tx
                .send(WriterCommand::Shutdown {
                    respond: ack_tx.clone(),
                })
                .is_err()
            {
                setup_failures.push(format!("could not enqueue writer command {n}"));
            }
        }

        // Single, non-blocking read of the mutated method.
        let observed = open.writer_queue_len();

        assert!(
            setup_failures.is_empty(),
            "writer-command enqueue setup failed: {setup_failures:?}"
        );
        assert_eq!(
            observed,
            Some(3),
            "Open::writer_queue_len must report the live writer command-channel \
             backlog (3 un-drained commands); the Some(0) mutant fabricates an \
             empty queue and the None mutant claims there is no writer at all"
        );
    }
}
