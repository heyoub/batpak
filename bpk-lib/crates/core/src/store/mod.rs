mod ancestry;
mod append;
pub mod backup_envelope;
mod chain_walk;
/// Cold-start recovery reports and artifact readers.
pub mod cold_start;
mod compaction_report;
mod config;
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
mod lifecycle;
mod lifecycle_api;
mod lifecycle_close;
mod lifecycle_fork;
mod open;
mod platform;
/// Projection cache traits and built-in backends (NoCache, NativeCache).
pub mod projection;
mod projection_run;
/// Typed reactor output batch — accumulator handed to typed reactor handlers.
pub mod reaction;
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
    BatchConfig, IdempotencyRetention, IndexConfig, IndexTopology, OpenReportObserver,
    OverflowPolicy, StoreConfig, SyncConfig, SyncMode, WriterConfig,
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
    fork_report_body_hash, ForkCopyStrategy, ForkEvidenceHash, ForkFinding, ForkOptions,
    ForkReport, ForkReportBody, ForkStrategyCounts, FORK_EVIDENCE_REPORT_SCHEMA_VERSION,
};
pub use gate::DurabilityGate;
pub use import::{
    provenance, provenance_from_extensions, ImportFilter, ImportOptions, ImportProvenance,
    ImportReport, ImportSelector, IMPORT_PROVENANCE_SCHEMA_VERSION,
};
/// Test-only global-allocator shims. Re-exported so dedicated single-test
/// binaries can install one as `#[global_allocator]`. Compiled out unless the
/// `alloc-count` or `fault-alloc` feature is enabled.
#[cfg(any(feature = "alloc-count", feature = "fault-alloc"))]
pub use platform::alloc;
pub use platform::clock::{Clock, SystemClock};
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
pub use read_walk::{
    ReadWalkDroppedCount, ReadWalkEvidenceReport, ReadWalkFinding, ReadWalkFreshnessIntent,
    ReadWalkFrontierKind, ReadWalkHash, ReadWalkInputFrontier, ReadWalkProofRef, ReadWalkProofRefs,
    ReadWalkReplayMode, ReadWalkReportBody, ReadWalkReportError, ReadWalkRequest,
    ReadWalkSourceRef, READ_WALK_REPORT_SCHEMA_VERSION,
};
pub use receipt_verification::{ReceiptVerification, ReceiptVerificationError};
pub use signing::SigningKey;
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

/// Typestate marker for an open store.
pub struct Open;

/// Typestate marker for a cleanly closed store.
pub struct Closed;

/// Typestate marker for a read-only store handle.
pub struct ReadOnly;

/// The main event store handle. Sync API; all methods are blocking. Send + Sync.
pub struct Store<State = Open> {
    pub(crate) index: Arc<StoreIndex>,
    pub(crate) reader: Arc<Reader>,
    pub(crate) cache: Box<dyn ProjectionCache>,
    pub(crate) writer: Option<WriterHandle>,
    pub(crate) watermark_handle: WatermarkAdvanceHandle,
    pub(crate) projection_registry: ProjectionRegistry,
    pub(crate) lifecycle_gate: Mutex<()>,
    pub(crate) config: Arc<StoreConfig>,
    pub(crate) runtime: Arc<config::ValidatedStoreConfig>,
    pub(crate) should_shutdown_on_drop: bool,
    pub(crate) open_report: Option<cold_start::rebuild::OpenIndexReport>,
    pub(crate) cumulative_reserved_kind_fallbacks: segment::sidx::ReservedKindFallbackStats,
    pub(crate) _state: std::marker::PhantomData<State>,
    pub(crate) _store_lock: dir_lock::StoreDirLock,
}

/// Safety net: if Store is dropped without calling close(), send Shutdown to the
/// writer thread and wait for it to drain pending events before releasing the
/// directory lock.
/// close(self) is still the preferred explicit path for guaranteed clean shutdown.
impl<State> Drop for Store<State> {
    fn drop(&mut self) {
        if !self.should_shutdown_on_drop {
            return;
        }
        let Some(mut writer) = self.writer.take() else {
            return;
        };
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
        join_drop_shutdown_writer(&mut writer);
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
        // Take the writer handle and close its command channel WITHOUT sending
        // Shutdown: the writer loop ends naturally (no drain, no footer, no final
        // sync), then we join to quiescence so no thread is mid-fsync when the
        // caller crashes the filesystem.
        if let Some(writer) = self.writer.take() {
            writer.close_channel_and_join();
        }
        // `self` (writer already taken, should_shutdown_on_drop=false) drops here
        // as an inert handle: Drop returns immediately.
    }
}
