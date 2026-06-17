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
mod frontier_api;
mod gate;
mod hidden_ranges;
/// In-memory 2D event index, rebuilt from segments on startup.
pub mod index;
mod lifecycle;
mod lifecycle_api;
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
    CompactionConfig, CompactionStrategy, DenialReceipt, EncodedBytes, ExtensionKey,
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
pub use gate::DurabilityGate;
pub use platform::clock::{Clock, SystemClock};
pub use projection::watch::{CursorWatcherError, ProjectionWatcher, WatcherError};
pub use projection::{
    CacheCapabilities, CacheMeta, Freshness, NativeCache, NoCache, ProjectionCache,
};
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
    LockLeafSymlinkProtection, MmapAdmissionSummary, MmapEvidence, ParentDirSyncAdmissionSummary,
    ParentDirSyncEvidence, PlatformAdmissionSummary, PlatformEvidenceSummary, StoreDiagnostics,
    StoreLockAdmissionSummary, StorePathEvidenceSummary, StorePathStatusEvidence, StoreStats,
    WatermarkKind, WriterPressure,
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
use crate::guard::{Denial, GateSet};
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
// justifies: INV-STORE-SYNC-ONLY, ADR-0001; async-store is not a declared feature in src/store/mod.rs; this compile_error guard must survive cargo check by silencing the unexpected cfg name
#[allow(unexpected_cfgs)]
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
